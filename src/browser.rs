//! Browsers rembro knows how to find and configure.

use std::path::Path;

use clap::ValueEnum;

use crate::util;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Family {
    /// Chromium, Chrome, Brave, Edge, Vivaldi and other Chromium builds
    Chromium,
    /// Firefox, LibreWolf, Zen and other Firefox builds
    Firefox,
    /// GNOME Web
    Epiphany,
    /// WebKitGTK's MiniBrowser
    Minibrowser,
    /// Playwright's WebKit build
    PlaywrightWebkit,
}

struct Known {
    name: &'static str,
    family: Family,
    candidates: &'static [&'static str],
}

const KNOWN: &[Known] = &[
    Known { name: "chromium", family: Family::Chromium, candidates: &["chromium", "chromium-browser"] },
    Known {
        name: "chrome",
        family: Family::Chromium,
        candidates: &["google-chrome-stable", "google-chrome", "/opt/google/chrome/chrome"],
    },
    Known { name: "brave", family: Family::Chromium, candidates: &["brave-browser", "brave", "brave-browser-stable"] },
    Known { name: "edge", family: Family::Chromium, candidates: &["microsoft-edge-stable", "microsoft-edge"] },
    Known { name: "vivaldi", family: Family::Chromium, candidates: &["vivaldi-stable", "vivaldi"] },
    Known {
        name: "playwright-chromium",
        family: Family::Chromium,
        candidates: &["~/.cache/ms-playwright/chromium-*/chrome-linux*/chrome"],
    },
    Known { name: "firefox", family: Family::Firefox, candidates: &["firefox", "firefox-esr"] },
    Known { name: "librewolf", family: Family::Firefox, candidates: &["librewolf"] },
    Known { name: "zen", family: Family::Firefox, candidates: &["zen-browser", "zen"] },
    Known {
        name: "playwright-firefox",
        family: Family::Firefox,
        candidates: &["~/.cache/ms-playwright/firefox-*/firefox/firefox"],
    },
    Known { name: "epiphany", family: Family::Epiphany, candidates: &["epiphany", "epiphany-browser"] },
    Known {
        name: "minibrowser",
        family: Family::Minibrowser,
        candidates: &[
            "/usr/lib/webkitgtk-6.0/MiniBrowser",
            "/usr/libexec/webkitgtk-6.0/MiniBrowser",
            "/usr/lib/*/webkitgtk-6.0/MiniBrowser",
            "/usr/lib/webkit2gtk-4.1/MiniBrowser",
            "/usr/libexec/webkit2gtk-4.1/MiniBrowser",
            "/usr/lib/*/webkit2gtk-4.1/MiniBrowser",
        ],
    },
    Known {
        name: "playwright-webkit",
        family: Family::PlaywrightWebkit,
        candidates: &["~/.cache/ms-playwright/webkit-*/pw_run.sh"],
    },
];

const DEFAULT_ORDER: &[&str] = &[
    "chromium",
    "chrome",
    "brave",
    "edge",
    "vivaldi",
    "playwright-chromium",
    "firefox",
    "librewolf",
    "zen",
    "playwright-firefox",
    "epiphany",
    "minibrowser",
    "playwright-webkit",
];

pub const NAMES: &str = "auto, chromium, chrome, brave, edge, vivaldi, playwright-chromium, firefox, librewolf, zen, \
playwright-firefox, webkit, epiphany, minibrowser, playwright-webkit, or a command or path";

/// A browser to look for: the first candidate that exists wins.
pub struct Choice {
    pub name: String,
    pub family: Family,
    pub candidates: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub name: String,
    pub family: Family,
    pub path: String,
}

fn guess_family(name: &str) -> Option<Family> {
    let n = name.to_ascii_lowercase();
    if ["chrom", "brave", "edge", "vivaldi", "opera", "thorium", "yandex", "helium"].iter().any(|k| n.contains(k)) {
        Some(Family::Chromium)
    } else if ["firefox", "librewolf", "zen", "floorp", "waterfox", "icecat", "mullvad"].iter().any(|k| n.contains(k)) {
        Some(Family::Firefox)
    } else if n.contains("epiphany") {
        Some(Family::Epiphany)
    } else if n == "pw_run.sh" {
        Some(Family::PlaywrightWebkit)
    } else if n.contains("minibrowser") {
        Some(Family::Minibrowser)
    } else {
        None
    }
}

/// The browsers `browser` may refer to, in order of preference.
pub fn choices(browser: &str, family: Option<Family>) -> Result<Vec<Choice>, String> {
    let names: &[&str] = match browser {
        "auto" | "" => DEFAULT_ORDER,
        "chromium" => &["chromium", "playwright-chromium"],
        "firefox" => &["firefox", "playwright-firefox"],
        "webkit" => &["epiphany", "minibrowser", "playwright-webkit"],
        other => match KNOWN.iter().find(|k| k.name == other) {
            Some(k) => std::slice::from_ref(&k.name),
            None => {
                let base = Path::new(other).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                let family = family.or_else(|| guess_family(&base)).ok_or_else(|| {
                    format!("cannot tell which kind of browser {other:?} is; pass --family, or use one of {NAMES}")
                })?;
                let name =
                    if family == Family::PlaywrightWebkit { "playwright-webkit".into() } else { util::slug(&base) };
                return Ok(vec![Choice { name, family, candidates: vec![other.to_string()] }]);
            }
        },
    };
    Ok(names
        .iter()
        .filter_map(|n| KNOWN.iter().find(|k| k.name == *n))
        .filter(|k| family.is_none_or(|f| f == k.family))
        .map(|k| Choice {
            name: k.name.to_string(),
            family: k.family,
            candidates: k.candidates.iter().map(|c| c.to_string()).collect(),
        })
        .collect())
}

pub fn resolve_local(browser: &str, family: Option<Family>) -> Result<Resolved, String> {
    let home = util::home();
    for c in choices(browser, family)? {
        for cand in &c.candidates {
            if let Some(p) = util::find_executable(cand, home.as_deref()) {
                return Ok(Resolved { name: c.name, family: c.family, path: p.to_string_lossy().into_owned() });
            }
        }
    }
    Err(format!("no browser found for {browser:?} on this machine; install one or pass --browser ({NAMES})"))
}

pub enum Net {
    /// The browser runs here and reaches the target through rembro's SOCKS server.
    Proxy { socks_port: u16, loopback_only: bool },
    /// The browser runs in the target and uses its network as it is.
    Native,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gui {
    Host,
    Wayland,
    X11,
}

pub struct Plan<'a> {
    pub browser: &'a Resolved,
    pub profile: &'a str,
    pub net: Net,
    pub gui: Gui,
    pub debug_port: Option<u16>,
    pub urls: &'a [String],
    pub extra: &'a [String],
    pub root: bool,
    pub small_shm: bool,
    /// Start a new process rather than handing the pages to one already running.
    pub new_instance: bool,
    pub verbose: bool,
}

pub enum Edit {
    /// Replace the file.
    Replace(Vec<u8>),
    /// Replace rembro's block in the file, keeping the rest.
    Block(String),
}

#[derive(Default)]
pub struct Launch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub unset: Vec<String>,
    pub files: Vec<(String, Edit)>,
}

pub const BLOCK_BEGIN: &str = "// rembro begin: rewritten each time rembro starts this profile";
pub const BLOCK_END: &str = "// rembro end";

/// Puts `block` between rembro's markers in `existing`, replacing any earlier block.
pub fn apply_block(existing: Option<&str>, block: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in existing.unwrap_or("").lines() {
        if line.starts_with("// rembro begin") {
            skipping = true;
        } else if skipping && line == BLOCK_END {
            skipping = false;
        } else if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(BLOCK_BEGIN);
    out.push('\n');
    out.push_str(block);
    out.push_str(BLOCK_END);
    out.push('\n');
    out
}

fn pac(socks_port: u16) -> String {
    format!(
        "function FindProxyForURL(url, host) {{\n  \
         if (host == \"localhost\" || dnsDomainIs(host, \".localhost\") || shExpMatch(host, \"127.*\") ||\n      \
         host == \"::1\" || host == \"[::1]\")\n    \
         return \"SOCKS5 127.0.0.1:{socks_port}\";\n  \
         return \"DIRECT\";\n}}\n"
    )
}

fn gui_env(l: &mut Launch, gui: Gui) {
    match gui {
        Gui::Host => {}
        Gui::Wayland => l.env.push(("GDK_BACKEND".into(), "wayland".into())),
        Gui::X11 => {
            l.env.push(("GDK_BACKEND".into(), "x11".into()));
            l.unset.push("WAYLAND_DISPLAY".into());
        }
    }
}

pub fn launch(p: &Plan) -> Result<Launch, String> {
    let mut l = Launch { argv: vec![p.browser.path.clone()], ..Default::default() };
    let profile = p.profile.trim_end_matches('/');
    let unsupported_debug = || {
        format!(
            "--debug needs a Chromium or Firefox browser; {} has no debugging protocol that rembro can expose",
            p.browser.name
        )
    };
    match p.browser.family {
        Family::Chromium => {
            l.argv.extend([
                format!("--user-data-dir={profile}"),
                "--no-first-run".into(),
                "--no-default-browser-check".into(),
            ]);
            if let Net::Proxy { socks_port, loopback_only } = p.net {
                l.argv.push(format!("--proxy-server=socks5://127.0.0.1:{socks_port}"));
                // Later rules win: <-loopback> sends loopback to the proxy, and * sends the rest direct.
                l.argv.push(
                    if loopback_only { "--proxy-bypass-list=*;<-loopback>" } else { "--proxy-bypass-list=<-loopback>" }
                        .into(),
                );
                if !loopback_only {
                    // Keep the DNS prefetcher and WebRTC from going around the proxy.
                    l.argv.push("--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1".into());
                    l.argv.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
                }
            }
            match p.gui {
                Gui::Host => {}
                Gui::Wayland | Gui::X11 => {
                    l.argv.push(format!("--ozone-platform={}", if p.gui == Gui::Wayland { "wayland" } else { "x11" }));
                    l.argv.extend(["--disable-gpu".into(), "--password-store=basic".into()]);
                    if !p.verbose {
                        l.argv.push("--log-level=3".into());
                    }
                    if p.gui == Gui::X11 {
                        // A forwarded display cannot share memory with the X server.
                        l.argv.push("--no-xshm".into());
                        l.unset.push("WAYLAND_DISPLAY".into());
                    }
                }
            }
            if p.root {
                l.argv.push("--no-sandbox".into());
            }
            if p.small_shm {
                l.argv.push("--disable-dev-shm-usage".into());
            }
            if let Some(port) = p.debug_port {
                l.argv.push(format!("--remote-debugging-port={port}"));
            }
        }
        Family::Firefox => {
            l.argv.extend(["--profile".into(), profile.to_string()]);
            if p.new_instance {
                l.argv.push("--new-instance".into());
            }
            if let Some(port) = p.debug_port {
                l.argv.extend(["--remote-debugging-port".into(), port.to_string()]);
            }
            let mut prefs: Vec<(&str, String)> = vec![
                ("browser.shell.checkDefaultBrowser", "false".into()),
                ("browser.aboutwelcome.enabled", "false".into()),
                ("trailhead.firstrun.didSeeAboutWelcome", "true".into()),
                ("browser.startup.homepage_override.mstone", "\"ignore\"".into()),
                ("datareporting.policy.dataSubmissionPolicyBypassNotification", "true".into()),
                ("toolkit.telemetry.reportingpolicy.firstRun", "false".into()),
            ];
            if let Net::Proxy { socks_port, loopback_only } = p.net {
                prefs.push(("network.proxy.allow_hijacking_localhost", "true".into()));
                prefs.push(("network.proxy.failover_direct", "false".into()));
                if loopback_only {
                    let pac_path = format!("{profile}/rembro.pac");
                    prefs.push(("network.proxy.type", "2".into()));
                    prefs.push(("network.proxy.autoconfig_url", format!("{:?}", util::file_url(&pac_path))));
                    l.files.push((pac_path, Edit::Replace(pac(socks_port).into_bytes())));
                } else {
                    prefs.extend([
                        ("network.proxy.type", "1".into()),
                        ("network.proxy.socks", "\"127.0.0.1\"".into()),
                        ("network.proxy.socks_port", socks_port.to_string()),
                        ("network.proxy.socks_version", "5".into()),
                        ("network.proxy.socks_remote_dns", "true".into()),
                        ("network.proxy.socks5_remote_dns", "true".into()),
                        ("network.proxy.http", "\"\"".into()),
                        ("network.proxy.ssl", "\"\"".into()),
                        ("network.proxy.share_proxy_settings", "false".into()),
                        ("network.proxy.no_proxies_on", "\"\"".into()),
                        ("network.trr.mode", "5".into()),
                        ("network.dns.disablePrefetch", "true".into()),
                        ("network.http.http3.enable", "false".into()),
                        ("media.peerconnection.ice.proxy_only_if_behind_proxy", "true".into()),
                    ]);
                }
            }
            let block: String = prefs.iter().map(|(k, v)| format!("user_pref(\"{k}\", {v});\n")).collect();
            l.files.push((format!("{profile}/user.js"), Edit::Block(block)));
            match p.gui {
                Gui::Host => {}
                Gui::Wayland => l.env.push(("MOZ_ENABLE_WAYLAND".into(), "1".into())),
                Gui::X11 => {
                    l.env.push(("MOZ_ENABLE_WAYLAND".into(), "0".into()));
                    l.unset.push("WAYLAND_DISPLAY".into());
                }
            }
        }
        Family::Epiphany => {
            if p.debug_port.is_some() {
                return Err(unsupported_debug());
            }
            l.argv.push(format!("--profile={profile}"));
            if let Net::Proxy { socks_port, loopback_only } = p.net {
                // Epiphany has no proxy options of its own; it follows GNOME's proxy settings,
                // so give it a private settings file that names rembro's SOCKS server.
                let config = format!("{profile}/rembro-config");
                let settings = if loopback_only {
                    let pac_path = format!("{profile}/rembro.pac");
                    l.files.push((pac_path.clone(), Edit::Replace(pac(socks_port).into_bytes())));
                    format!(
                        "[system/proxy]\nmode='auto'\nautoconfig-url='{}'\nignore-hosts=@as []\n",
                        util::file_url(&pac_path)
                    )
                } else {
                    format!(
                        "[system/proxy]\nmode='manual'\nignore-hosts=@as []\n\n[system/proxy/socks]\nhost='127.0.0.1'\nport={socks_port}\n"
                    )
                };
                l.files.push((format!("{config}/glib-2.0/settings/keyfile"), Edit::Replace(settings.into_bytes())));
                l.env.extend([
                    ("XDG_CONFIG_HOME".into(), config),
                    ("GSETTINGS_BACKEND".into(), "keyfile".into()),
                    ("GIO_USE_PROXY_RESOLVER".into(), "gnome".into()),
                ]);
            }
            gui_env(&mut l, p.gui);
        }
        Family::Minibrowser => {
            if p.debug_port.is_some() {
                return Err(unsupported_debug());
            }
            l.argv.push(format!("--profile-dir={profile}"));
            if let Net::Proxy { socks_port, loopback_only } = p.net {
                if loopback_only {
                    return Err("MiniBrowser cannot send only loopback through a proxy; drop --loopback-only".into());
                }
                l.argv.push(format!("--proxy=socks5://127.0.0.1:{socks_port}"));
            }
            gui_env(&mut l, p.gui);
        }
        Family::PlaywrightWebkit => {
            if p.debug_port.is_some() {
                return Err(unsupported_debug());
            }
            if matches!(p.net, Net::Proxy { .. }) {
                return Err("Playwright's WebKit cannot be given a proxy on the command line; use --remote, or \
                            -b epiphany or -b minibrowser"
                    .into());
            }
            l.argv.push(format!("--user-data-dir={profile}"));
            gui_env(&mut l, p.gui);
        }
    }
    l.argv.extend(p.extra.iter().cloned());
    l.argv.extend(p.urls.iter().cloned());
    Ok(l)
}

/// Which debugging protocol a family speaks, and the endpoint URL for a port.
pub fn debug_endpoint(family: Family, port: u16) -> Option<(&'static str, String)> {
    match family {
        Family::Chromium => Some(("CDP", format!("http://127.0.0.1:{port}"))),
        Family::Firefox => Some(("WebDriver BiDi", format!("ws://127.0.0.1:{port}/session"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block() {
        let first = apply_block(Some("user_pref(\"a\", 1);\n"), "user_pref(\"b\", 2);\n");
        let second = apply_block(Some(&first), "user_pref(\"b\", 3);\n");
        assert_eq!(second, format!("user_pref(\"a\", 1);\n{BLOCK_BEGIN}\nuser_pref(\"b\", 3);\n{BLOCK_END}\n"));
    }

    #[test]
    fn families() {
        assert_eq!(guess_family("google-chrome-unstable"), Some(Family::Chromium));
        assert_eq!(guess_family("floorp"), Some(Family::Firefox));
        assert_eq!(guess_family("lynx"), None);
        assert!(choices("lynx", None).is_err());
        assert_eq!(choices("lynx", Some(Family::Chromium)).unwrap()[0].name, "lynx");
        assert_eq!(choices("auto", Some(Family::Firefox)).unwrap()[0].name, "firefox");
    }

    #[test]
    fn chromium_flags() {
        let b = Resolved { name: "chromium".into(), family: Family::Chromium, path: "/usr/bin/chromium".into() };
        let urls = ["http://localhost:5173".to_string()];
        let l = launch(&Plan {
            browser: &b,
            profile: "/p",
            net: Net::Proxy { socks_port: 1080, loopback_only: true },
            gui: Gui::Host,
            debug_port: Some(0),
            urls: &urls,
            extra: &[],
            root: false,
            small_shm: false,
            new_instance: true,
            verbose: false,
        })
        .unwrap();
        assert!(l.argv.contains(&"--proxy-bypass-list=*;<-loopback>".to_string()));
        assert!(l.argv.contains(&"--remote-debugging-port=0".to_string()));
        assert_eq!(l.argv.last().unwrap(), "http://localhost:5173");
    }
}
