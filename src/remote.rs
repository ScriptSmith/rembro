//! Programs that run in the target with their windows and sound shown here, and the
//! browser in that mode.

use std::any::Any;
use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::browser::{self, Edit, Family, Gui, Net, Plan, Resolved};
use crate::cli::{Browse, DisplayArg, GuiOpts};
use crate::host::{self, Endpoint, HostEvent, Limiter, PULSE_ID, Session, WAYLAND_ID, X11_ID};
use crate::local::TempDir;
use crate::mux::Sock;
use crate::proto::{Addr, Call, Event, Open};
use crate::target::Target;
use crate::util::{self, PortSpec};
use crate::{fetch, sys, x11};

fn resolve(s: &Session, choice: &str, family: Option<Family>) -> Result<Resolved, String> {
    let choices = browser::choices(choice, family, browser::Os::Linux)?;
    let groups: Vec<Vec<String>> = choices.iter().map(|c| c.candidates.clone()).collect();
    let found: Option<(usize, String)> = s.call(&Call::Find { groups })?;
    let (i, path) = found.ok_or_else(|| {
        format!(
            "no browser found for {choice:?} in {label}; install one with 'rembro install {label}', \
             or pass --browser ({})",
            browser::NAMES,
            label = s.label
        )
    })?;
    let c = &choices[i];
    Ok(Resolved { name: c.name.clone(), family: c.family, path })
}

fn find(s: &Session, name: &str) -> Result<Option<String>, String> {
    let found: Option<(usize, String)> = s.call(&Call::Find { groups: vec![vec![name.into()]] })?;
    Ok(found.map(|f| f.1))
}

fn host_has(cmd: &str) -> bool {
    util::find_executable(cmd, util::home().as_deref()).is_some()
}

/// Copies `data` to `path` in the target.
pub fn upload(s: &Session, data: &[u8], path: &str, mode: u32) -> Result<(), String> {
    let (a, mut b) = Sock::pair().map_err(|e| e.to_string())?;
    let store = Open::Store { path: path.into(), size: data.len() as u64, mode };
    s.conn.mux().open(&store, a, |_, _| ())?;
    b.write_all(data).map_err(|e| format!("upload: {e}"))?;
    b.shutdown(Shutdown::Write);
    let mut reply = String::new();
    let _ = b.read_to_string(&mut reply);
    if reply.trim() == "ok" { Ok(()) } else { Err(format!("upload to {path} failed: {}", reply.trim())) }
}

/// A waypipe the target can run: its own, or a static build that rembro uploads.
fn target_waypipe(s: &Session) -> Result<String, String> {
    if let Some(p) = find(s, "waypipe")? {
        return Ok(p);
    }
    let local = fetch::tool("waypipe", &s.info.arch, s.verbose)?;
    let data = fs::read(&local).map_err(|e| format!("cannot read {}: {e}", local.display()))?;
    let path = format!("{}/waypipe-{}", s.info.agent_dir, &fetch::hex(&fetch::sha256(&data))[..16]);
    if s.call::<Option<u64>>(&Call::Stat { path: path.clone() })? != Some(data.len() as u64) {
        if s.verbose {
            eprintln!("rembro: copying waypipe to {} ({} KiB)", s.label, data.len() / 1024);
        }
        upload(s, &data, &path, 0o755)?;
    }
    Ok(path)
}

/// Starts a waypipe client here and returns it with its socket.
fn waypipe_client(dir: &Path, quiet: bool) -> Result<(Child, PathBuf), String> {
    let sock = dir.join("waypipe.sock");
    let mut cmd = Command::new("waypipe");
    cmd.args(["--no-gpu", "-s"]).arg(&sock).arg("client").stdin(Stdio::null());
    sys::own_group(&mut cmd);
    if quiet {
        cmd.stderr(Stdio::null());
    }
    sys::unblock_signals(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("cannot run waypipe: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sys::is_socket(&sock) {
        if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
            return Err("the waypipe client did not start".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok((child, sock))
}

/// This machine's PulseAudio server: the first in PULSE_SERVER if it is a `unix:` socket or a
/// `tcp:` address, or the usual socket in XDG_RUNTIME_DIR.
fn pulse_server() -> Option<Endpoint> {
    let server = match std::env::var("PULSE_SERVER") {
        Ok(v) => v.split_whitespace().next()?.to_string(),
        Err(_) => format!("unix:{}", Path::new(&std::env::var("XDG_RUNTIME_DIR").ok()?).join("pulse/native").display()),
    };
    if let Some(path) = server.strip_prefix("unix:") {
        return sys::is_socket(Path::new(path)).then(|| Endpoint::Unix(path.into()));
    }
    let addr = server.strip_prefix("tcp:").or_else(|| server.strip_prefix("tcp6:"))?;
    let (host, port) = match addr.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, p.parse().ok()?),
        _ => (addr, 4713),
    };
    let addr = (host.trim_matches(['[', ']']), port).to_socket_addrs().ok()?.next()?;
    Some(Endpoint::Tcp(addr))
}

fn pulse_cookie() -> Option<Vec<u8>> {
    let home = util::home();
    let candidates = [
        std::env::var("PULSE_COOKIE").ok().map(PathBuf::from),
        Some(util::expand_home("~/.config/pulse/cookie", home.as_deref())),
        Some(util::expand_home("~/.pulse-cookie", home.as_deref())),
    ];
    candidates.into_iter().flatten().find_map(|p| fs::read(p).ok())
}

pub fn write(s: &Session, path: &str, data: Vec<u8>, mode: u32) -> Result<String, String> {
    s.call(&Call::WriteFile { path: path.into(), data, mode })
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Revokes an untrusted X11 authorization when the session ends.
struct Revoke(x11::Display, Option<Vec<u8>>, u32);

impl Drop for Revoke {
    fn drop(&mut self) {
        self.0.revoke(self.1.as_deref(), self.2);
    }
}

/// Everything a program in the target needs to show windows and play sound here.
#[derive(Default)]
pub struct GuiSetup {
    pub gui: Option<Gui>,
    /// Command prefix that runs the program under waypipe.
    pub wrapper: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Ports the setup listens on in the target.
    pub exclude: Vec<u16>,
    _guards: Vec<Box<dyn Any>>,
}

/// How windows from the target reach a display.
enum Shown {
    /// Through waypipe, with the target's waypipe.
    Wayland(String),
    /// Forwarded to this machine's X server.
    X11,
    /// On the target's own display, such as WSLg's in WSL.
    Target(Gui),
}

/// Why there is no display here, and what to do about it.
fn no_display() -> (&'static str, &'static str) {
    if cfg!(target_os = "macos") {
        ("this Mac has no X11 display", "; install XQuartz from https://www.xquartz.org, then log out and back in")
    } else if cfg!(windows) {
        ("this machine has no X11 display", "; start an X server such as VcXsrv or X410, and set DISPLAY=127.0.0.1:0")
    } else {
        ("this session has neither a Wayland nor an X11 display", "")
    }
}

/// Picks how windows are shown. None means no display.
fn choose_display(opts: &GuiOpts, s: &Session, required: bool) -> Result<Option<Shown>, String> {
    if opts.display == DisplayArg::None {
        return Ok(None);
    }
    let wayland_here = std::env::var_os("WAYLAND_DISPLAY").is_some() && host_has("waypipe");
    let x11_here = x11::Display::from_env().is_some();
    if opts.display != DisplayArg::X11 && wayland_here {
        match target_waypipe(s) {
            Ok(path) => return Ok(Some(Shown::Wayland(path))),
            Err(e) if opts.display == DisplayArg::Wayland => {
                return Err(format!("{} has no waypipe, and rembro could not provide one: {e}", s.label));
            }
            Err(e) => eprintln!("rembro: {} has no waypipe, and rembro could not provide one: {e}", s.label),
        }
    } else if opts.display == DisplayArg::Wayland {
        return Err(if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "waypipe is not installed on this machine".into()
        } else {
            "this session has no Wayland display (WAYLAND_DISPLAY is not set)".into()
        });
    }
    if x11_here {
        return Ok(Some(Shown::X11));
    }
    if opts.display == DisplayArg::Auto
        && let Some(gui) =
            s.info.wayland_display.as_ref().map(|_| Gui::Wayland).or(s.info.display.as_ref().map(|_| Gui::X11))
    {
        if s.verbose {
            eprintln!("rembro: this machine has no display, so windows go to {}'s own", s.label);
        }
        return Ok(Some(Shown::Target(gui)));
    }
    let (why, hint) = no_display();
    if required {
        return Err(format!("{why} to show windows on{hint}"));
    }
    eprintln!("rembro: {why}, so the program runs without one");
    Ok(None)
}

/// Sets up the display, sound and runtime directory for programs started in `dir`.
pub fn setup_gui(s: &Session, opts: &GuiOpts, dir: &str, required: bool) -> Result<GuiSetup, String> {
    let mut g = GuiSetup::default();
    if s.info.runtime_dir.is_none() {
        g.env.push(("XDG_RUNTIME_DIR".into(), dir.into()));
    }
    match choose_display(opts, s, required)? {
        None => {}
        Some(Shown::Target(gui)) => g.gui = Some(gui),
        Some(Shown::Wayland(waypipe)) => {
            let host_dir = util::mkdtemp(&util::runtime_base(), "rembro-").map_err(|e| format!("mkdtemp: {e}"))?;
            let (child, sock) = waypipe_client(&host_dir, s.quiet)?;
            g._guards.push(Box::new(KillOnDrop(child)));
            g._guards.push(Box::new(TempDir(host_dir)));
            s.forward(WAYLAND_ID, Endpoint::Unix(sock));
            let target_sock = format!("{dir}/waypipe.sock");
            s.call::<u16>(&Call::Listen { id: WAYLAND_ID, addr: Addr::Unix { path: target_sock.clone() } })?;
            g.wrapper = vec![
                waypipe,
                "--no-gpu".into(),
                "--title-prefix".into(),
                format!("[{}] ", s.label),
                "-s".into(),
                target_sock,
                "server".into(),
                "--".into(),
            ];
            g.gui = Some(Gui::Wayland);
        }
        Some(Shown::X11) => {
            let display = x11::Display::from_env().ok_or("DISPLAY is not set")?;
            let cookie = display.cookie();
            let mut give = cookie.clone();
            let mut trusted = true;
            if !opts.x11_trusted {
                match display.untrusted(cookie.as_deref()) {
                    Ok((id, c)) => {
                        give = Some(c);
                        trusted = false;
                        g._guards.push(Box::new(Revoke(display.clone(), cookie.clone(), id)));
                    }
                    Err(e) => eprintln!("rembro: this X server cannot make an untrusted connection ({e})"),
                }
            }
            if trusted && s.target != Target::Local {
                eprintln!("rembro: programs in {} can see and type into every window on this X11 display", s.label);
            }
            s.forward(X11_ID, Endpoint::X11(display, give.clone()));
            let number = (10..100)
                .find(|n| {
                    s.call::<u16>(&Call::Listen {
                        id: X11_ID,
                        addr: Addr::Tcp { host: "127.0.0.1".into(), port: 6000 + n },
                    })
                    .is_ok()
                })
                .ok_or_else(|| format!("no free X display number in {}", s.label))?;
            g.exclude.push(6000 + number);
            g.env.push(("DISPLAY".into(), format!("127.0.0.1:{number}")));
            if let Some(cookie) = give {
                let path = write(s, &format!("{dir}/Xauthority"), x11::xauthority(number as u32, &cookie), 0o600)?;
                g.env.push(("XAUTHORITY".into(), path));
            }
            g.env.push(("QT_X11_NO_MITSHM".into(), "1".into()));
            g.gui = Some(Gui::X11);
        }
    }
    if !opts.no_audio
        && let Some(server) = pulse_server()
    {
        s.forward(PULSE_ID, server);
        let target_sock = format!("{dir}/pulse.sock");
        s.call::<u16>(&Call::Listen { id: PULSE_ID, addr: Addr::Unix { path: target_sock.clone() } })?;
        g.env.push(("PULSE_SERVER".into(), format!("unix:{target_sock}")));
        if let Some(cookie) = pulse_cookie() {
            g.env.push(("PULSE_COOKIE".into(), write(s, &format!("{dir}/pulse-cookie"), cookie, 0o600)?));
        }
    }
    Ok(g)
}

fn profile_dir(b: &Browse, s: &Session, browser: &str, dir: &str) -> Result<String, String> {
    let path = if b.temp_profile {
        format!("{dir}/profile")
    } else {
        match &b.profile {
            Some(p) if p.contains('/') || p == "~" => p.clone(),
            p => {
                let profile = p.as_deref().unwrap_or(if b.debug.is_some() { "agent" } else { "default" });
                format!("~/.local/share/rembro/profiles/{browser}/{}", util::slug(profile))
            }
        }
    };
    s.call(&Call::Mkdir { path })
}

pub fn run(b: &Browse, s: &Session, ports: &PortSpec) -> Result<i32, String> {
    let dir: String = s.call(&Call::Mkdtemp)?;
    let browser = resolve(s, &b.browser, b.family)?;
    if s.verbose {
        eprintln!("rembro: using {} ({}) in {}", browser.name, browser.path, s.label);
    }
    let debug = b.debug_port()?;
    if debug.is_some() && browser::debug_endpoint(browser.family, 0).is_none() {
        return Err(format!("--debug needs a Chromium or Firefox browser, not {}", browser.name));
    }
    let mut gui_opts = b.gui.clone();
    if gui_opts.display == DisplayArg::Auto
        && x11::Display::from_env().is_some()
        && let Some(major) = old_chromium(s, &browser)
    {
        eprintln!(
            "rembro: Chromium {major} in {} predates its Wayland support, so the window goes through X11",
            s.label
        );
        gui_opts.display = DisplayArg::X11;
    }
    let gui = setup_gui(s, &gui_opts, &dir, true)?;
    let gui_kind = gui.gui.unwrap_or(Gui::X11);
    let started = Instant::now();
    s.call::<String>(&Call::Shim)?;

    let profile = profile_dir(b, s, &browser.name, &dir)?;
    let mut exclude = gui.exclude.clone();
    let mut debug_port = None;
    if let Some(requested) = debug {
        let port = match requested {
            Some(p) => p,
            None if s.call::<bool>(&Call::PortFree { port: 9222 })? => 9222,
            None => s.call::<u16>(&Call::FreePort)?,
        };
        if !s.call::<bool>(&Call::PortFree { port })? {
            return Err(format!("port {port} is already in use in {}", s.label));
        }
        exclude.push(port);
        debug_port = Some(port);
    }
    let mut urls = b.urls.clone();
    let mut seen = None;
    if urls.is_empty() {
        let listening;
        (urls, listening) = host::server_urls(s, ports, &exclude)?;
        seen = Some(listening);
    }
    if let Some(port) = debug_port
        && let Some(page) = host::announce_debug(s, browser.family, port)
    {
        let path = write(s, &format!("{dir}/attach.html"), page.into_bytes(), 0o644)?;
        urls.push(util::file_url(&path));
    }

    let launch_for = |urls: &[String], first: bool| -> Result<browser::Launch, String> {
        browser::launch(&Plan {
            browser: &browser,
            profile: &profile,
            net: Net::Native,
            gui: gui_kind,
            debug_port: if first { debug_port } else { None },
            urls,
            extra: if first { &b.browser_args } else { &[] },
            root: s.info.uid == 0,
            small_shm: s.info.shm_bytes.is_some_and(|b| b < 1 << 30),
            new_instance: first,
            verbose: s.verbose,
        })
    };
    let launch = launch_for(&urls, true)?;
    if s.info.uid == 0 && browser.family == Family::Chromium {
        eprintln!("rembro: {} runs this as root, so Chromium's sandbox is off", s.label);
    }
    for (path, edit) in launch.files {
        let data = match edit {
            Edit::Replace(d) => d,
            Edit::Block(block) => {
                let old: Option<Vec<u8>> = s.call(&Call::ReadFile { path: path.clone() })?;
                let old = old.map(|b| String::from_utf8_lossy(&b).into_owned());
                browser::apply_block(old.as_deref(), &block).into_bytes()
            }
        };
        write(s, &path, data, 0o644)?;
    }
    let spawn = |argv: Vec<String>, env: Vec<(String, String)>, unset: Vec<String>| -> Result<u32, String> {
        let mut full = gui.wrapper.clone();
        full.extend(argv);
        let mut all_env = gui.env.clone();
        all_env.extend(env);
        s.call(&Call::Spawn { argv: full, env: all_env, unset, shim: true })
    };
    let proc = spawn(launch.argv, launch.env, launch.unset)?;
    // Opens more pages by starting the browser again, which hands them to the running one.
    let open = |url: &str| {
        if s.verbose {
            eprintln!("rembro: opening {url}");
        }
        if let Ok(l) = launch_for(&[url.to_string()], false)
            && let Err(e) = spawn(l.argv, l.env, l.unset)
        {
            eprintln!("rembro: could not open {url}: {e}");
        }
    };
    if b.watching() {
        host::watch_ports(s, ports.clone(), exclude.clone(), seen);
    }

    let mut announced = debug_port.is_none();
    let mut limiter = Limiter::new();
    loop {
        match s.events.recv() {
            Ok(HostEvent::Agent(Event::Output { proc: p, stream, text })) => {
                if p == proc
                    && !announced
                    && let Some(url) = debug_banner(&text)
                {
                    announced = true;
                    if browser.family == Family::Chromium {
                        println!("  {url}");
                    }
                }
                if !s.quiet && p == proc {
                    host::relay_output(stream, &text);
                }
            }
            Ok(HostEvent::Agent(Event::Exited { proc: p, code, signal })) if p == proc => {
                let status = host::exit_code(code, signal);
                if status != 0 && gui_kind == Gui::Wayland && started.elapsed() < Duration::from_secs(20) {
                    eprintln!(
                        "rembro: the browser exited with status {status} right after starting; if it cannot use \
                         Wayland, try --display x11"
                    );
                }
                return Ok(status);
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
            Ok(HostEvent::Signal(sig)) => {
                host::wait_exit(
                    s,
                    // The agent ends the browser itself when its connection closes.
                    |e| {
                        matches!(e, HostEvent::AgentClosed)
                            || matches!(e, HostEvent::Agent(Event::Exited { proc: p, .. }) if *p == proc)
                    },
                    |sig| {
                        let _ = s.call::<serde_json::Value>(&Call::Kill { proc, signal: sig });
                    },
                );
                return Ok(128 + sig);
            }
            Ok(HostEvent::AgentClosed) | Err(_) => {
                eprintln!("rembro: the connection to {} closed", s.label);
                return Ok(1);
            }
            Ok(_) => {}
        }
    }
}

/// The major version of a Chromium in the target that is too old for Wayland: before
/// version 98, distributions built Chromium without it, and those builds crash under Wayland.
fn old_chromium(s: &Session, browser: &Resolved) -> Option<u32> {
    if !["chromium", "chrome", "edge", "brave", "playwright-chromium"].contains(&browser.name.as_str()) {
        return None;
    }
    let argv = vec![browser.path.clone(), "--version".into()];
    let (_, out) = s.call::<(Option<i32>, String)>(&Call::Capture { argv, timeout_ms: 10_000 }).ok()?;
    let major = chromium_major(&out)?;
    (major < 98).then_some(major)
}

/// The major version in output such as "Chromium 90.0.4430.212 built on Debian 10.9".
fn chromium_major(version: &str) -> Option<u32> {
    version
        .split_whitespace()
        .find(|w| {
            w.split('.').count() >= 3 && w.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// The endpoint in a browser's "listening" line, if `line` is one.
fn debug_banner(line: &str) -> Option<&str> {
    let line = line.trim_end();
    line.strip_prefix("DevTools listening on ").or_else(|| line.strip_prefix("WebDriver BiDi listening on "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_versions() {
        assert_eq!(chromium_major("Chromium 90.0.4430.212 built on Debian 10.9, running on Debian 10.13"), Some(90));
        assert_eq!(chromium_major("Google Chrome 131.0.6778.85 "), Some(131));
        assert_eq!(chromium_major("Brave Browser 131.1.73.89"), Some(131));
        assert_eq!(chromium_major("nonsense 1.2"), None);
    }
}
