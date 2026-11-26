//! The browser runs on this machine and reaches the target through a SOCKS proxy.

use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::browser::{self, Edit, Family, Gui, Net, Plan};
use crate::certs;
use crate::cli::{Browse, ProfileArg};
use crate::host::{self, DEBUG_ID, Endpoint, HostEvent, Limiter, Session};
use crate::proto::{Call, Event};
use crate::target::Target;
use crate::util::{self, PortSpec};
use crate::{profile, socks, sys};

/// Removes a directory when dropped.
pub struct TempDir(pub PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The profile to use, and for one that only this session uses, the guard that deletes it.
fn profile_dir(
    cli: &Browse,
    s: &Session,
    browser: &browser::Resolved,
) -> Result<(profile::Claimed, Option<profile::Discard>), String> {
    let kept = |name: &str| profile::kept_dir().join(s.target.slug()).join(&browser.name).join(util::slug(name));
    let path_of = |p: &str| -> Result<PathBuf, String> {
        if util::is_path(p) {
            Ok(util::expand_home(p, util::home().as_deref()))
        } else {
            profile::check_name(p)?;
            Ok(kept(p))
        }
    };
    let own = |name: Option<&str>| {
        profile::check_kept_system(&kept("system"))?;
        profile::find_own(browser, name, browser::Os::HERE, util::home().as_deref())
    };
    let (fork, open) = match cli.profile_arg() {
        ProfileArg::Kept(p) => {
            if let Some(name) = profile::system_name(&p) {
                return Ok((profile::claim_own(&own(name)?, browser.family, &s.label)?, None));
            }
            let path = path_of(&p)?;
            if !profile::is_run_profile(&path) {
                let claimed = profile::claim(&path, browser.family, &s.label)?;
                if !util::is_path(&p) {
                    sys::set_mode(&path, 0o700);
                }
                return Ok((claimed, None));
            }
            // The profile that `rembro run` shares between the sessions it starts for links.
            let dir = path.parent().unwrap_or(&path);
            if !dir.is_dir() {
                return Err(format!("{} is gone: the `rembro run` that this link came from has ended", dir.display()));
            }
            let claimed = profile::claim(&path, browser.family, &s.label)?;
            // Checked after the claim, so that the directory can't be swapped in between.
            if !sys::is_private_dir(dir) {
                return Err(format!("{} is not a directory that only you can open", dir.display()));
            }
            let discard = (!claimed.busy).then(|| profile::Discard(path));
            return Ok((claimed, discard));
        }
        ProfileArg::Temp => (None, false),
        ProfileArg::Fork(p) => {
            let (src, directory) = match profile::system_name(&p) {
                Some(name) => {
                    let own = own(name)?;
                    if cfg!(windows) && matches!(browser.name.as_str(), "chrome" | "edge" | "brave") {
                        eprintln!(
                            "rembro: on Windows, {} decrypts the cookies it protects with App-Bound Encryption only in \
                             its own data directory, so this copy may start signed out of sites",
                            browser.name
                        );
                    }
                    (own.path, own.directory)
                }
                None => (path_of(&p)?, None),
            };
            if !util::is_path(&p) && !src.is_dir() {
                return Err(format!(
                    "there is no {p} profile for {} in {}; `rembro profiles` lists the profiles there are",
                    s.label, browser.name
                ));
            }
            let open = profile::is_open(&src);
            if open {
                eprintln!(
                    "rembro: {p} is open in another window, so this browser starts from what that one last saved"
                );
            }
            (Some((src, directory)), open)
        }
    };
    let (fork, directory) = match &fork {
        Some((src, directory)) => (Some(src.as_path()), directory.as_deref()),
        None => (None, None),
    };
    let claimed = profile::create_temp(&std::env::temp_dir(), fork, directory, &s.label).map_err(|e| {
        // On Windows, Chromium doesn't let other programs read its cookies while it runs.
        if open { format!("{e}. Close the browser that has it open, and try again") } else { e }
    })?;
    let discard = profile::Discard(claimed.path.clone());
    Ok((claimed, Some(discard)))
}

/// The port and browser path in Chromium's DevToolsActivePort file, once it is listening.
fn devtools_port(profile: &Path) -> Option<(u16, String)> {
    let text = fs::read_to_string(profile.join("DevToolsActivePort")).ok()?;
    let mut lines = text.lines();
    Some((lines.next()?.trim().parse().ok()?, lines.next()?.to_string()))
}

/// Opens a page in the headless Chromium that has this profile open.
fn new_tab(profile: &Path, url: &str) -> Result<(), String> {
    let (port, _) = devtools_port(profile).ok_or("the browser is not listening for pages yet")?;
    let conn = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(|e| e.to_string())?;
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    browser::new_tab(conn, url)
}

/// Opens pages in the headless Chromium that has this profile open, once it is listening.
fn open_later(profile: &Path, urls: Vec<String>) {
    let profile = profile.to_path_buf();
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        for url in urls {
            while let Err(e) = new_tab(&profile, &url) {
                if Instant::now() >= deadline {
                    eprintln!("rembro: could not open {url}: {e}");
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }
        }
    });
}

fn apply_files(files: Vec<(String, Edit)>) -> Result<(), String> {
    for (path, edit) in files {
        let data = match edit {
            Edit::Replace(d) => d,
            Edit::Block(block) => browser::apply_block(fs::read_to_string(&path).ok().as_deref(), &block).into_bytes(),
        };
        if let Some(parent) = Path::new(&path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&path, data).map_err(|e| format!("cannot write {path}: {e}"))?;
    }
    Ok(())
}

fn free_port() -> Result<u16, String> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| e.to_string())
}

/// Waits for the browser's debugging server and joins the target's listener to it.
fn connect_debugger(s: &Session, family: Family, profile: &Path, port: u16, target_port: u16, verbose: bool) {
    let handler = s.handler.clone();
    let profile = profile.to_path_buf();
    let label = s.label.clone();
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let found = match family {
                Family::Chromium => devtools_port(&profile),
                _ => TcpStream::connect((Ipv4Addr::LOCALHOST, port)).ok().map(|_| (port, "/session".to_string())),
            };
            if let Some((port, path)) = found {
                let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
                if TcpStream::connect(addr).is_ok() {
                    // Chromium builds the WebSocket URLs it hands out from the Host header,
                    // which has to keep naming the target's port.
                    let endpoint = match family {
                        Family::Chromium => Endpoint::Tcp(addr),
                        _ => Endpoint::Http { addr, target_port },
                    };
                    handler.set_forward(DEBUG_ID, endpoint);
                    if verbose {
                        eprintln!("rembro: {label}'s 127.0.0.1:{target_port} now reaches the browser's {addr}");
                    }
                    if family == Family::Chromium {
                        println!("  ws://127.0.0.1:{target_port}{path}");
                    }
                    return;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        eprintln!("rembro: the browser did not open its debugging port; is the profile already open?");
    });
}

/// Sets up what the target side of a local-browser session needs, and returns the
/// debugging port there. Runs again after a reconnect, when `previous` is the port used so far.
fn prepare_target(
    b: &Browse,
    s: &Session,
    debug: Option<Option<u16>>,
    previous: Option<u16>,
) -> Result<Option<u16>, String> {
    if b.target_proxy {
        s.call::<serde_json::Value>(&Call::Configure { target_proxy: true })?;
    }
    s.call::<String>(&Call::Shim)?;
    match debug {
        Some(requested) => {
            let port = requested.or(previous).unwrap_or(9222);
            Ok(Some(host::debug_listener(s, port, requested.is_none())?))
        }
        None => Ok(None),
    }
}

pub fn run(b: &Browse, s: &Session, ports: &PortSpec) -> Result<i32, String> {
    let browser = browser::resolve_local(&b.browser, b.family)?;
    if s.verbose {
        eprintln!("rembro: using {} ({})", browser.name, browser.path);
    }
    let debug = b.debug_port()?;
    if debug.is_some() && browser::debug_endpoint(browser.family, 0).is_none() {
        return Err(format!("--debug needs a Chromium or Firefox browser, not {}", browser.name));
    }
    if debug.is_some() {
        profile::check_debug(&b.profile_arg(), &browser)?;
    }
    if debug.is_some() && s.target != Target::Local {
        eprintln!(
            "rembro: the browser runs on this machine, so programs in {} that drive it can read files it can open \
             here; use --remote to keep it inside the target",
            s.label
        );
    }
    if b.target_proxy && s.info.proxy.is_none() {
        eprintln!("rembro: {} sets no HTTP proxy, so --target-proxy changes nothing", s.label);
    }
    let socks_port =
        socks::serve(s.conn.clone(), s.verbose).map_err(|e| format!("cannot start the SOCKS server: {e}"))?;
    let (claimed, discard) = profile_dir(b, s, &browser)?;
    let profile = claimed.path;
    let busy = claimed.busy;
    let _session = claimed.mark;
    let directory = claimed.directory;
    // The CA certificates go into the Default profile's store.
    if b.trust_target_ca && directory.as_deref().is_some_and(|d| d != "Default") {
        return Err(format!(
            "--trust-target-ca works only with Chromium's Default profile, and this one is in {}",
            directory.unwrap_or_default()
        ));
    }
    let temporary = discard.is_some();
    // Declared after the mark, so that the profile goes before the next session can take it.
    let _discard = discard;
    if busy && debug.is_some() {
        return Err(format!(
            "{} is already open in another window, which cannot be given a debugging port; close it, or use --fork \
             instead of --profile to start from a copy of it",
            profile.display()
        ));
    }
    // A running browser that is not a rembro session for this target would load the pages
    // from some other network.
    if busy {
        let why = match profile::session_target(&profile) {
            Some(t) if t == s.label => None,
            Some(t) => Some(format!("is open in a rembro session for {t}, so the pages would load from {t}'s network")),
            None => Some(format!(
                "is open in a browser that rembro did not start, so the pages would load from this machine's network \
                 instead of {}'s",
                s.label
            )),
        };
        if let Some(why) = why {
            if !b.force_profile {
                return Err(format!(
                    "{} {why}. Close that browser first, or pass --force-profile to open them there anyway",
                    profile.display()
                ));
            }
            eprintln!("rembro: {} {why}; opening them there because of --force-profile", profile.display());
        }
    }
    let owned = profile::created_by_rembro(&profile);
    if !busy && let Err(e) = profile::restore(&profile) {
        eprintln!("rembro: could not put back the settings an earlier session changed: {e}");
    }
    // Asking for the browser's own profile is asking for rembro to change it and put it back.
    let own = matches!(b.profile_arg(), ProfileArg::Kept(p) if profile::system_name(&p).is_some());
    if browser.family == Family::Firefox && !owned && !busy && !b.force_profile && !own {
        return Err(format!(
            "{} is a Firefox profile that rembro did not create. rembro's proxy settings would stay in it, and \
             Firefox started on its own would then use a proxy that no longer exists. Pass --force-profile to use it \
             anyway; rembro puts the profile's own settings back when the browser exits",
            profile.display()
        ));
    }

    if debug.is_some() {
        s.forward(DEBUG_ID, Endpoint::Pending("the browser is still starting"));
    }
    let mut target_debug = prepare_target(b, s, debug, None)?;
    let mut urls = b.urls.clone();
    let mut seen = None;
    let exclude: Vec<u16> = target_debug.into_iter().collect();
    if urls.is_empty() && !busy {
        let listening;
        (urls, listening) = host::server_urls(s, ports, &exclude)?;
        seen = Some(listening);
    }
    let mut _page_dir = None;
    if let Some(port) = target_debug
        && let Some(page) = host::announce_debug(s, browser.family, port)
        && !b.headless
    {
        let dir = util::mkdtemp(&util::runtime_base(), "rembro-").map_err(|e| format!("mkdtemp: {e}"))?;
        let path = dir.join("attach.html");
        fs::write(&path, page).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        urls.insert(0, util::file_url(&path.to_string_lossy()));
        _page_dir = Some(TempDir(dir));
    }

    let host_debug = match (target_debug, browser.family) {
        (None, _) => None,
        (Some(_), Family::Chromium) => Some(0),
        (Some(_), _) => Some(free_port()?),
    };
    let profile_str = profile.to_string_lossy().into_owned();
    let launch_for = |urls: &[String], first: bool| -> Result<browser::Launch, String> {
        browser::launch(&Plan {
            browser: &browser,
            profile: &profile_str,
            profile_directory: directory.as_deref(),
            net: Net::Proxy { socks_port, loopback_only: b.loopback_only },
            gui: if b.headless { Gui::Headless } else { Gui::Host },
            debug_port: if first { host_debug } else { None },
            urls,
            extra: if first { &b.browser_args } else { &[] },
            root: false,
            small_shm: false,
            new_instance: first && !busy,
            temporary,
            verbose: s.verbose,
        })
    };
    let launch = launch_for(&urls, true)?;
    // Guards that undo rembro's changes to a profile it did not create, once the browser exits.
    let mut _restore = None;
    let mut _untrust = None;
    if !busy {
        if !owned && browser.family == Family::Firefox {
            let keys: Vec<String> = launch
                .files
                .iter()
                .filter_map(|(_, e)| if let Edit::Block(block) = e { Some(profile::pref_keys(block)) } else { None })
                .flatten()
                .collect();
            _restore = Some(profile::save_firefox(&profile, &keys)?);
        }
        if !owned && b.trust_target_ca {
            _untrust = Some(profile::Untrust(browser.family, profile.clone()));
        }
        trust_target_cas(s, &browser, &profile, b.trust_target_ca, busy);
        apply_files(launch.files)?;
    } else if b.trust_target_ca {
        eprintln!("rembro: the profile is open in another window, so its trusted CAs stay as they were");
    }
    // Left by an earlier browser, it would name a port that nothing here listens on.
    if !busy {
        let _ = fs::remove_file(profile.join("DevToolsActivePort"));
    }
    if busy && !b.force_profile {
        eprintln!("rembro: this profile is open in another rembro window for {}, so the pages open there", s.label);
    }
    // A Chromium with a debugging endpoint takes the pages through it, which is the only way
    // headless Chromium takes them. Other browsers get them from a second launch, below.
    if busy
        && browser.family == Family::Chromium
        && let Some(first) = urls.first()
        && new_tab(&profile, first).is_ok()
    {
        for url in &urls[1..] {
            if let Err(e) = new_tab(&profile, url) {
                eprintln!("rembro: could not open {url}: {e}");
            }
        }
        return Ok(0);
    }
    if s.verbose {
        eprintln!("rembro: SOCKS server on 127.0.0.1:{socks_port}; profile {}", profile.display());
    }

    let spawn = |argv: &[String], env: &[(String, String)], unset: &[String], quiet: bool| {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]).stdin(Stdio::null());
        sys::own_group(&mut cmd);
        for k in unset {
            cmd.env_remove(k);
        }
        cmd.envs(env.iter().cloned());
        if quiet {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        sys::unblock_signals(&mut cmd);
        cmd.spawn().map_err(|e| format!("cannot run {}: {e}", argv[0]))
    };
    let mut child = spawn(&launch.argv, &launch.env, &launch.unset, s.quiet)?;
    let pid = child.id();
    let tx = s.sender.clone();
    thread::spawn(move || {
        let (code, signal) = match child.wait() {
            Ok(st) => (st.code(), sys::exit_signal(&st)),
            Err(_) => (Some(1), None),
        };
        let _ = tx.send(HostEvent::BrowserExited(code, signal));
    });
    if let (Some(port), Some(target_port)) = (host_debug, target_debug) {
        connect_debugger(s, browser.family, &profile, port, target_port, s.verbose);
    }
    if !launch.later.is_empty() {
        open_later(&profile, launch.later);
    }
    // Opens more pages by starting the browser again, which hands them to the running one.
    // Headless browsers take no such hand-off.
    let open = |url: &str| {
        if s.verbose {
            eprintln!("rembro: opening {url}");
        }
        if b.headless {
            if browser.family == Family::Chromium {
                open_later(&profile, vec![url.to_string()]);
            } else {
                eprintln!(
                    "rembro: headless {} takes no more pages once it has started, so {url} stays closed",
                    browser.name
                );
            }
            return;
        }
        let result = launch_for(&[url.to_string()], false).and_then(|l| spawn(&l.argv, &l.env, &l.unset, !s.verbose));
        match result {
            Ok(mut c) => {
                thread::spawn(move || c.wait());
            }
            Err(e) => eprintln!("rembro: could not open {url}: {e}"),
        }
    };
    if host::can_watch(b, &browser) {
        host::watch_ports(s, ports.clone(), exclude, seen);
    }

    let term = |sig| sys::signal_group(pid, sig);
    let mut limiter = Limiter::new();
    loop {
        match s.events.recv() {
            Ok(HostEvent::BrowserExited(code, signal)) => return Ok(host::exit_code(code, signal)),
            Ok(HostEvent::Signal(sig)) => {
                host::wait_exit(s, |e| matches!(e, HostEvent::BrowserExited(..)), term);
                return Ok(128 + sig);
            }
            Ok(HostEvent::AgentClosed) => {
                eprintln!("rembro: lost the connection to {}; reconnecting while the browser stays open", s.label);
                if debug.is_some() {
                    s.forward(DEBUG_ID, Endpoint::Pending("rembro is reconnecting"));
                }
                s.reconnect();
            }
            Ok(HostEvent::Reconnected(link)) => {
                s.install(link);
                match prepare_target(b, s, debug, target_debug) {
                    Ok(port) => {
                        if port != target_debug
                            && let Some(p) = port
                        {
                            eprintln!("rembro: the debugging endpoint in {} moved to port {p}", s.label);
                        }
                        target_debug = port;
                        if let (Some(host_port), Some(target_port)) = (host_debug, target_debug) {
                            connect_debugger(s, browser.family, &profile, host_port, target_port, s.verbose);
                        }
                        eprintln!("rembro: reconnected to {}", s.label);
                    }
                    Err(e) => eprintln!("rembro: reconnected to {}, but setting it up failed: {e}", s.label),
                }
            }
            Ok(HostEvent::Agent(Event::Open { url })) => {
                if limiter.allow() {
                    open(&url);
                }
            }
            Ok(HostEvent::NewServer(url)) => {
                eprintln!("rembro: opening {url}, which just started in {}", s.label);
                open(&url);
            }
            Ok(HostEvent::Agent(_)) => {}
            Err(_) => return Ok(1),
        }
    }
}

/// Makes the profile trust the CA certificates added in the target, or with `trust` false,
/// removes trust that an earlier launch added. Problems are warnings, not errors.
fn trust_target_cas(s: &Session, browser: &browser::Resolved, profile: &Path, trust: bool, busy: bool) {
    if !trust && !certs::has_rembro_trust(profile) {
        return;
    }
    if busy {
        return;
    }
    let mut names = Vec::new();
    let mut ders = Vec::new();
    if trust {
        match s.call::<Vec<(String, String)>>(&Call::ExtraCas) {
            Ok(cas) => {
                for (source, pem) in cas {
                    let found = certs::parse_pem(&pem);
                    if !found.is_empty() {
                        names.push(source);
                        ders.extend(found);
                    }
                }
            }
            Err(e) => return eprintln!("rembro: cannot read {}'s CA certificates: {e}", s.label),
        }
        if ders.is_empty() {
            eprintln!("rembro: {} has no added CA certificates to trust", s.label);
        }
    }
    let result = match browser.family {
        Family::Chromium => certs::apply_chromium(profile, &ders),
        Family::Firefox => certs::apply_firefox(profile, &ders),
        _ if trust => Err(format!("--trust-target-ca works with Chromium and Firefox, not {}", browser.name)),
        _ => Ok(()),
    };
    match result {
        Ok(()) if !ders.is_empty() => {
            let count =
                if ders.len() == 1 { "1 CA certificate".into() } else { format!("{} CA certificates", ders.len()) };
            eprintln!("rembro: this profile trusts {count} from {}: {}", s.label, names.join(", "))
        }
        Ok(()) => {}
        Err(e) => eprintln!("rembro: could not update the profile's trusted CAs: {e}"),
    }
}
