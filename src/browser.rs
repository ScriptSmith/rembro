//! Browsers rembro knows how to find and configure.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::stealth::{self, Stealth};
use crate::{profile, util};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

/// The system to find browsers for: this machine's, or Linux in a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    pub const HERE: Os = if cfg!(target_os = "macos") {
        Os::Macos
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Linux
    };
}

struct Known {
    name: &'static str,
    family: Family,
    /// Commands and paths to try on Linux, here or in a target.
    linux: &'static [&'static str],
    macos: &'static [&'static str],
    windows: &'static [&'static str],
}

impl Known {
    fn candidates(&self, os: Os) -> &'static [&'static str] {
        match os {
            Os::Linux => self.linux,
            Os::Macos => self.macos,
            Os::Windows => self.windows,
        }
    }
}

const KNOWN: &[Known] = &[
    Known {
        name: "chromium",
        family: Family::Chromium,
        linux: &["chromium", "chromium-browser"],
        macos: &[
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "~/Applications/Chromium.app/Contents/MacOS/Chromium",
        ],
        windows: &[
            r"%LOCALAPPDATA%\Chromium\Application\chrome.exe",
            r"%ProgramFiles%\Chromium\Application\chrome.exe",
        ],
    },
    Known {
        name: "chrome",
        family: Family::Chromium,
        linux: &["google-chrome-stable", "google-chrome", "/opt/google/chrome/chrome"],
        macos: &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "~/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ],
        windows: &[
            r"%ProgramFiles%\Google\Chrome\Application\chrome.exe",
            r"%ProgramFiles(x86)%\Google\Chrome\Application\chrome.exe",
            r"%LOCALAPPDATA%\Google\Chrome\Application\chrome.exe",
        ],
    },
    Known {
        name: "brave",
        family: Family::Chromium,
        linux: &["brave-browser", "brave", "brave-browser-stable"],
        macos: &[
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "~/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ],
        windows: &[
            r"%ProgramFiles%\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"%ProgramFiles(x86)%\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\Application\brave.exe",
        ],
    },
    Known {
        name: "edge",
        family: Family::Chromium,
        linux: &["microsoft-edge-stable", "microsoft-edge"],
        macos: &["/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"],
        windows: &[
            r"%ProgramFiles(x86)%\Microsoft\Edge\Application\msedge.exe",
            r"%ProgramFiles%\Microsoft\Edge\Application\msedge.exe",
        ],
    },
    Known {
        name: "vivaldi",
        family: Family::Chromium,
        linux: &["vivaldi-stable", "vivaldi"],
        macos: &["/Applications/Vivaldi.app/Contents/MacOS/Vivaldi"],
        windows: &[
            r"%LOCALAPPDATA%\Vivaldi\Application\vivaldi.exe",
            r"%ProgramFiles%\Vivaldi\Application\vivaldi.exe",
        ],
    },
    Known {
        name: "playwright-chromium",
        family: Family::Chromium,
        linux: &["~/.cache/ms-playwright/chromium-*/chrome-linux*/chrome"],
        macos: &["~/Library/Caches/ms-playwright/chromium-*/chrome-mac*/*.app/Contents/MacOS/*"],
        windows: &[r"%LOCALAPPDATA%\ms-playwright\chromium-*\chrome-win*\chrome.exe"],
    },
    Known {
        name: "firefox",
        family: Family::Firefox,
        linux: &["firefox", "firefox-esr"],
        macos: &[
            "/Applications/Firefox.app/Contents/MacOS/firefox",
            "~/Applications/Firefox.app/Contents/MacOS/firefox",
        ],
        windows: &[
            r"%ProgramFiles%\Mozilla Firefox\firefox.exe",
            r"%ProgramFiles(x86)%\Mozilla Firefox\firefox.exe",
            r"%LOCALAPPDATA%\Mozilla Firefox\firefox.exe",
        ],
    },
    Known {
        name: "librewolf",
        family: Family::Firefox,
        linux: &["librewolf"],
        macos: &["/Applications/LibreWolf.app/Contents/MacOS/librewolf"],
        windows: &[r"%ProgramFiles%\LibreWolf\librewolf.exe", r"%LOCALAPPDATA%\LibreWolf\librewolf.exe"],
    },
    Known {
        name: "zen",
        family: Family::Firefox,
        linux: &["zen-browser", "zen"],
        macos: &["/Applications/Zen.app/Contents/MacOS/zen", "/Applications/Zen Browser.app/Contents/MacOS/zen"],
        windows: &[r"%ProgramFiles%\Zen Browser\zen.exe"],
    },
    Known {
        name: "playwright-firefox",
        family: Family::Firefox,
        linux: &["~/.cache/ms-playwright/firefox-*/firefox/firefox"],
        macos: &["~/Library/Caches/ms-playwright/firefox-*/firefox/Nightly.app/Contents/MacOS/firefox"],
        windows: &[r"%LOCALAPPDATA%\ms-playwright\firefox-*\firefox\firefox.exe"],
    },
    Known {
        name: "epiphany",
        family: Family::Epiphany,
        linux: &["epiphany", "epiphany-browser"],
        macos: &[],
        windows: &[],
    },
    Known {
        name: "minibrowser",
        family: Family::Minibrowser,
        linux: &[
            "/usr/lib/webkitgtk-6.0/MiniBrowser",
            "/usr/libexec/webkitgtk-6.0/MiniBrowser",
            "/usr/lib/*/webkitgtk-6.0/MiniBrowser",
            "/usr/lib/webkit2gtk-4.1/MiniBrowser",
            "/usr/libexec/webkit2gtk-4.1/MiniBrowser",
            "/usr/lib/*/webkit2gtk-4.1/MiniBrowser",
        ],
        macos: &[],
        windows: &[],
    },
    // Playwright's WebKit takes no proxy, so it is of use only in a target.
    Known {
        name: "playwright-webkit",
        family: Family::PlaywrightWebkit,
        linux: &["~/.cache/ms-playwright/webkit-*/pw_run.sh"],
        macos: &[],
        windows: &[],
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

/// Where a browser keeps its own profiles: Chromium's user data directory, or the directory
/// with Firefox's profiles.ini. `$XDG_CONFIG_HOME` is that variable, or ~/.config when it is
/// not set.
struct Own {
    name: &'static str,
    /// The ID of its flatpak, which is also the name of the command that runs it.
    flatpak: &'static str,
    linux: &'static [&'static str],
    macos: &'static [&'static str],
    windows: &'static [&'static str],
}

const OWN: &[Own] = &[
    Own {
        name: "chromium",
        flatpak: "org.chromium.Chromium",
        linux: &[
            "$XDG_CONFIG_HOME/chromium",
            "~/snap/chromium/common/chromium",
            "~/.var/app/org.chromium.Chromium/config/chromium",
        ],
        macos: &["~/Library/Application Support/Chromium"],
        windows: &[r"%LOCALAPPDATA%\Chromium\User Data"],
    },
    Own {
        name: "chrome",
        flatpak: "com.google.Chrome",
        linux: &["$XDG_CONFIG_HOME/google-chrome", "~/.var/app/com.google.Chrome/config/google-chrome"],
        macos: &["~/Library/Application Support/Google/Chrome"],
        windows: &[r"%LOCALAPPDATA%\Google\Chrome\User Data"],
    },
    Own {
        name: "brave",
        flatpak: "com.brave.Browser",
        linux: &[
            "$XDG_CONFIG_HOME/BraveSoftware/Brave-Browser",
            "~/snap/brave/current/.config/BraveSoftware/Brave-Browser",
            "~/.var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser",
        ],
        macos: &["~/Library/Application Support/BraveSoftware/Brave-Browser"],
        windows: &[r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data"],
    },
    Own {
        name: "edge",
        flatpak: "com.microsoft.Edge",
        linux: &["$XDG_CONFIG_HOME/microsoft-edge", "~/.var/app/com.microsoft.Edge/config/microsoft-edge"],
        macos: &["~/Library/Application Support/Microsoft Edge"],
        windows: &[r"%LOCALAPPDATA%\Microsoft\Edge\User Data"],
    },
    Own {
        name: "vivaldi",
        flatpak: "com.vivaldi.Vivaldi",
        linux: &[
            "$XDG_CONFIG_HOME/vivaldi",
            "~/snap/vivaldi/current/.config/vivaldi",
            "~/.var/app/com.vivaldi.Vivaldi/config/vivaldi",
        ],
        macos: &["~/Library/Application Support/Vivaldi"],
        windows: &[r"%LOCALAPPDATA%\Vivaldi\User Data"],
    },
    // Firefox keeps using ~/.mozilla once it has profiles there, and puts new ones in $XDG_CONFIG_HOME.
    Own {
        name: "firefox",
        flatpak: "org.mozilla.firefox",
        linux: &[
            "~/.mozilla/firefox",
            "$XDG_CONFIG_HOME/mozilla/firefox",
            "~/snap/firefox/common/.mozilla/firefox",
            "~/.var/app/org.mozilla.firefox/.mozilla/firefox",
            "~/.var/app/org.mozilla.firefox/config/mozilla/firefox",
        ],
        macos: &["~/Library/Application Support/Firefox"],
        windows: &[r"%APPDATA%\Mozilla\Firefox"],
    },
    Own {
        name: "librewolf",
        flatpak: "io.gitlab.librewolf-community",
        linux: &["~/.librewolf", "$XDG_CONFIG_HOME/librewolf", "~/.var/app/io.gitlab.librewolf-community/.librewolf"],
        macos: &["~/Library/Application Support/librewolf"],
        windows: &[r"%APPDATA%\librewolf"],
    },
    Own {
        name: "zen",
        flatpak: "app.zen_browser.zen",
        linux: &["~/.zen", "$XDG_CONFIG_HOME/zen", "~/.var/app/app.zen_browser.zen/.zen"],
        macos: &["~/Library/Application Support/zen"],
        windows: &[r"%APPDATA%\zen"],
    },
];

pub const OWN_NAMES: &str = "chromium, chrome, brave, edge, vivaldi, firefox, librewolf and zen";

/// The browser in `OWN` that `name` or the executable `exe` is.
fn own(name: &str, exe: &str) -> Option<&'static Own> {
    let file = Path::new(exe).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    OWN.iter().find(|o| o.name == name || o.flatpak == name || o.flatpak == file)
}

/// The name in `OWN` of the browser that `name` or the executable `exe` is.
pub fn own_name(name: &str, exe: &str) -> Option<&'static str> {
    own(name, exe).map(|o| o.name)
}

/// The directories where a browser may keep its own profiles on `os`, most likely first. A
/// snap's or flatpak's come first when `exe`, the browser's executable, is one. None if rembro
/// knows of none for this browser.
pub fn own_profile_dirs(
    name: &str,
    exe: &str,
    os: Os,
    home: Option<&str>,
    var: &dyn Fn(&str) -> Option<String>,
) -> Option<Vec<PathBuf>> {
    let o = own(name, exe)?;
    let mut patterns: Vec<&str> = match os {
        Os::Linux => o.linux.to_vec(),
        Os::Macos => o.macos.to_vec(),
        Os::Windows => o.windows.to_vec(),
    };
    let packaged = if exe.starts_with("/snap/") {
        Some("~/snap/")
    } else if exe.contains("/flatpak/") || Path::new(exe).file_name().is_some_and(|f| f == o.flatpak) {
        Some("~/.var/app/")
    } else {
        None
    };
    if let Some(prefix) = packaged {
        patterns.sort_by_key(|p| !p.starts_with(prefix));
    }
    Some(patterns.iter().filter_map(|p| expand_own(p, home, var)).collect())
}

/// Expands the `~`, `$XDG_CONFIG_HOME` or `%NAME%` at the start of a directory in `OWN`.
fn expand_own(pattern: &str, home: Option<&str>, var: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(rest) = pattern.strip_prefix("$XDG_CONFIG_HOME/") {
        let base = match var("XDG_CONFIG_HOME") {
            Some(d) if Path::new(&d).is_absolute() => PathBuf::from(d),
            _ => Path::new(home?).join(".config"),
        };
        return Some(base.join(rest));
    }
    if let Some(rest) = pattern.strip_prefix('%') {
        let (name, rest) = rest.split_once('%')?;
        return Some(PathBuf::from(format!("{}{rest}", var(name).filter(|v| !v.is_empty())?)));
    }
    Some(Path::new(home?).join(pattern.strip_prefix("~/")?))
}

/// Whether the browser opens a debugging port when it runs on its own data directory. Google
/// Chrome stopped doing so in version 136 (https://developer.chrome.com/blog/remote-debugging-port),
/// and Edge followed. Chromium's own builds still do.
pub fn debugs_own_profile(name: &str, exe: &str) -> bool {
    !matches!(own_name(name, exe), Some("chrome" | "edge"))
}

/// For `rembro profiles`: each browser that keeps profiles of its own, with its family and the
/// directories where they may be on this machine.
pub fn own_profile_dirs_here() -> Vec<(&'static str, Family, Vec<PathBuf>)> {
    let home = util::home();
    OWN.iter()
        .filter_map(|o| {
            let known = KNOWN.iter().find(|k| k.name == o.name)?;
            let exe = known
                .candidates(Os::HERE)
                .iter()
                .find_map(|c| util::find_executable(c, home.as_deref()))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dirs = own_profile_dirs(o.name, &exe, Os::HERE, home.as_deref(), &|k| std::env::var(k).ok())?;
            Some((o.name, known.family, dirs))
        })
        .collect()
}

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

/// The browsers `browser` may refer to on `os`, in order of preference.
pub fn choices(browser: &str, family: Option<Family>, os: Os) -> Result<Vec<Choice>, String> {
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
            candidates: k.candidates(os).iter().map(|c| c.to_string()).collect(),
        })
        .collect())
}

pub fn resolve_local(browser: &str, family: Option<Family>) -> Result<Resolved, String> {
    let home = util::home();
    for c in choices(browser, family, Os::HERE)? {
        for cand in &c.candidates {
            if let Some(p) = util::find_executable(cand, home.as_deref()) {
                return Ok(Resolved { name: c.name, family: c.family, path: p.to_string_lossy().into_owned() });
            }
        }
    }
    Err(format!("no browser found for {browser:?} on this machine; install one or pass --browser ({NAMES})"))
}

#[derive(Clone, Copy)]
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
    /// No window at all.
    Headless,
}

#[derive(Clone, Copy)]
pub struct Plan<'a> {
    pub browser: &'a Resolved,
    pub profile: &'a str,
    /// For one of Chromium's own profiles, its directory in `profile`, such as `Profile 1`.
    pub profile_directory: Option<&'a str>,
    pub net: Net,
    pub gui: Gui,
    pub debug_port: Option<u16>,
    pub urls: &'a [String],
    pub extra: &'a [String],
    pub root: bool,
    pub small_shm: bool,
    /// Start a new process rather than handing the pages to one already running.
    pub new_instance: bool,
    /// The profile is deleted when the session ends.
    pub temporary: bool,
    pub verbose: bool,
    /// With --stealth: leave out what pages can see, and use these settings.
    pub stealth: Option<&'a Stealth>,
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
    /// Pages that could not go on the command line, to open with `new_tab` once the browser runs.
    pub later: Vec<String>,
    /// What --stealth changed, for --verbose.
    pub stealth: Vec<String>,
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

/// Firefox's prefs.js without the lines that rembro's block in `existing` set and `block`
/// doesn't, or None if it has none of them. Lines that the browser changed since stay.
pub fn forget_prefs(prefs_js: &str, existing: Option<&str>, block: &str) -> Option<String> {
    let keep = profile::pref_keys(block);
    let mut inside = false;
    let mut gone = Vec::new();
    for line in existing.unwrap_or("").lines() {
        if line.starts_with("// rembro begin") {
            inside = true;
        } else if line == BLOCK_END {
            inside = false;
        } else if inside {
            let keys = profile::pref_keys(line);
            if !keys.is_empty() && keys.iter().all(|k| !keep.contains(k)) {
                gone.push(line.trim());
            }
        }
    }
    let mut changed = false;
    let out = prefs_js
        .lines()
        .filter(|l| {
            let drop = gone.contains(&l.trim());
            changed |= drop;
            !drop
        })
        .map(|l| format!("{l}\n"))
        .collect();
    changed.then_some(out)
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
        Gui::Host | Gui::Headless => {}
        Gui::Wayland => l.env.push(("GDK_BACKEND".into(), "wayland".into())),
        Gui::X11 => {
            l.env.push(("GDK_BACKEND".into(), "x11".into()));
            l.unset.push("WAYLAND_DISPLAY".into());
        }
    }
}

pub fn launch(p: &Plan) -> Result<Launch, String> {
    let mut l = build(p)?;
    if p.stealth.is_some() {
        l.stealth = stealth::changes(&build(&Plan { stealth: None, ..*p })?, &l);
    }
    Ok(l)
}

fn build(p: &Plan) -> Result<Launch, String> {
    let mut l = Launch { argv: vec![p.browser.path.clone()], ..Default::default() };
    let profile = p.profile.trim_end_matches('/');
    let unsupported_debug = || {
        format!(
            "--debug needs a Chromium or Firefox browser; {} has no debugging protocol that rembro can expose",
            p.browser.name
        )
    };
    if p.gui == Gui::Headless && !matches!(p.browser.family, Family::Chromium | Family::Firefox) {
        return Err(format!("--headless needs a Chromium or Firefox browser, not {}", p.browser.name));
    }
    match p.browser.family {
        Family::Chromium => {
            l.argv.extend([
                format!("--user-data-dir={profile}"),
                "--no-first-run".into(),
                "--no-default-browser-check".into(),
            ]);
            if let Some(dir) = p.profile_directory {
                l.argv.push(format!("--profile-directory={dir}"));
            }
            if p.temporary && p.stealth.is_none() {
                // Chromium would otherwise download over 100 MB of components, such as
                // Widevine, into each new profile. Pages can tell when Widevine is missing.
                l.argv.push("--disable-component-update".into());
            }
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
                Gui::Headless => l.argv.extend([
                    "--headless".into(),
                    "--mute-audio".into(),
                    // As Playwright does, so that pages lay out for a mouse rather than a touch screen.
                    "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4"
                        .into(),
                ]),
                Gui::Wayland | Gui::X11 => {
                    l.argv.push(format!("--ozone-platform={}", if p.gui == Gui::Wayland { "wayland" } else { "x11" }));
                    // Pages then find no WebGL, so --stealth lets Chromium use a GPU in the target,
                    // which it can through Wayland.
                    if p.stealth.is_none() {
                        l.argv.push("--disable-gpu".into());
                    }
                    l.argv.push("--password-store=basic".into());
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
            if p.gui == Gui::Headless
                && let Some(st) = p.stealth
            {
                // Headless Chromium puts HeadlessChrome in its user agent, and renders WebGL with
                // SwiftShader even when there is a GPU. --enable-gpu alone turns WebGL off where
                // there is none, so SwiftShader stays the fallback.
                l.argv.extend(st.user_agent.iter().map(|ua| format!("--user-agent={ua}")));
                l.argv.extend(["--enable-gpu".into(), "--enable-unsafe-swiftshader".into()]);
            }
            if p.root {
                l.argv.push("--no-sandbox".into());
            }
            if p.small_shm {
                l.argv.push("--disable-dev-shm-usage".into());
            }
            if let Some(port) = p.debug_port {
                l.argv.push(format!("--remote-debugging-port={port}"));
                // Playwright's flags for the same problem: without them, a page in a minimized,
                // covered or background window is throttled, and screenshots of it never finish.
                // Pages can tell that their timers keep full speed there, so --stealth leaves out
                // all but the screenshot feature.
                l.argv.push("--enable-features=CDPScreenshotNewSurface".into());
                if p.stealth.is_none() {
                    l.argv.extend([
                        "--disable-backgrounding-occluded-windows".into(),
                        "--disable-renderer-backgrounding".into(),
                        "--disable-background-timer-throttling".into(),
                    ]);
                } else {
                    // A debugging port sets navigator.webdriver.
                    l.argv.push("--disable-blink-features=AutomationControlled".into());
                }
            }
            // Chromium takes its language from --lang on Windows, and from the environment on Linux.
            if let Some(lang) = p.stealth.and_then(|st| st.language.as_deref()) {
                if Os::HERE == Os::Windows {
                    l.argv.push(format!("--lang={lang}"));
                } else {
                    l.env.push(("LANGUAGE".into(), lang.replace('-', "_")));
                }
            }
        }
        Family::Firefox => {
            l.argv.extend(["--profile".into(), profile.to_string()]);
            if p.new_instance {
                l.argv.push("--new-instance".into());
            }
            if p.gui == Gui::Headless {
                l.argv.push("--headless".into());
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
            // Firefox copies user.js into prefs.js, so these are set either way: Playwright's
            // values with --debug, which keep timers in background tabs at full speed, and
            // Firefox's defaults otherwise.
            let (timeout, budget) = if p.debug_port.is_some() { ("0", "false") } else { ("1000", "true") };
            prefs.extend([
                ("dom.min_background_timeout_value", timeout.into()),
                ("dom.min_background_timeout_value_without_budget_throttling", timeout.into()),
                ("dom.timeout.enable_budget_timer_throttling", budget.into()),
            ]);
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
            if let Some(lang) = p.stealth.and_then(|st| st.language.as_deref()) {
                prefs.push(("intl.locale.requested", format!("{lang:?}")));
                prefs.push(("intl.accept_languages", format!("{:?}", stealth::accept_languages(lang))));
            }
            let block: String = prefs.iter().map(|(k, v)| format!("user_pref(\"{k}\", {v});\n")).collect();
            l.files.push((format!("{profile}/user.js"), Edit::Block(block)));
            match p.gui {
                Gui::Host | Gui::Headless => {}
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
    if let Some(tz) = p.stealth.and_then(|st| st.timezone.as_deref()) {
        l.env.push(("TZ".into(), tz.into()));
    }
    l.argv.extend(p.extra.iter().cloned());
    // Headless Chromium refuses to start with more than one page on its command line.
    let first = if p.gui == Gui::Headless && p.browser.family == Family::Chromium { 1 } else { p.urls.len() };
    let (now, later) = p.urls.split_at(first.min(p.urls.len()));
    l.argv.extend(now.iter().cloned());
    l.later = later.to_vec();
    Ok(l)
}

/// Asks a running Chromium to open `url` in a new tab, over a connection to its debugging
/// endpoint. This is how pages reach headless Chromium, which a second launch can't hand them to.
pub fn new_tab(mut conn: impl Read + Write, url: &str) -> Result<(), String> {
    let request = format!(
        "PUT /json/new?{} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        util::percent_encode(url)
    );
    conn.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut status = String::new();
    BufReader::new(conn.take(1024)).read_line(&mut status).map_err(|e| e.to_string())?;
    match status.split_whitespace().nth(1) {
        Some("200") => Ok(()),
        _ => Err(format!("the browser answered {:?}", status.trim())),
    }
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
        assert!(choices("lynx", None, Os::Linux).is_err());
        assert_eq!(choices("lynx", Some(Family::Chromium), Os::Linux).unwrap()[0].name, "lynx");
        assert_eq!(choices("auto", Some(Family::Firefox), Os::Linux).unwrap()[0].name, "firefox");
        assert_eq!(guess_family("Google Chrome for Testing"), Some(Family::Chromium));
        assert_eq!(guess_family("msedge.exe"), Some(Family::Chromium));
        let mac = choices("chrome", None, Os::Macos).unwrap();
        assert!(mac[0].candidates[0].ends_with("/Contents/MacOS/Google Chrome"));
        assert!(choices("epiphany", None, Os::Windows).unwrap()[0].candidates.is_empty());
    }

    #[test]
    fn own_profiles() {
        let (home, h) = (Some("/home/u"), Path::new("/home/u"));
        let xdg = std::env::temp_dir().join("xdg");
        let var = |k: &str| (k == "XDG_CONFIG_HOME").then(|| xdg.to_string_lossy().into_owned());
        let dirs = own_profile_dirs("chromium", "/usr/bin/chromium", Os::Linux, home, &var).unwrap();
        assert_eq!(dirs[..2], [xdg.join("chromium"), h.join("snap/chromium/common/chromium")]);
        // A snap's or flatpak's own directory comes first for that snap or flatpak.
        let dirs = own_profile_dirs("chromium", "/snap/bin/chromium", Os::Linux, home, &|_| None).unwrap();
        assert_eq!(dirs[..2], [h.join("snap/chromium/common/chromium"), h.join(".config").join("chromium")]);
        let exe = "/var/lib/flatpak/exports/bin/org.mozilla.firefox";
        let dirs = own_profile_dirs("org.mozilla.firefox", exe, Os::Linux, home, &|_| None).unwrap();
        assert_eq!(dirs[0], h.join(".var/app/org.mozilla.firefox/.mozilla/firefox"));
        assert_eq!(
            dirs[1..3],
            [h.join(".var/app/org.mozilla.firefox/config/mozilla/firefox"), h.join(".mozilla/firefox")]
        );
        assert_eq!(own_name("org.mozilla.firefox", exe), Some("firefox"));
        let local = |k: &str| (k == "LOCALAPPDATA").then(|| r"C:\Users\u\AppData\Local".to_string());
        let dirs = own_profile_dirs("edge", "msedge.exe", Os::Windows, None, &local).unwrap();
        assert_eq!(dirs, [PathBuf::from(r"C:\Users\u\AppData\Local\Microsoft\Edge\User Data")]);
        assert!(own_profile_dirs("firefox", "firefox.exe", Os::Windows, None, &|_| None).unwrap().is_empty());
        assert!(own_profile_dirs("playwright-chromium", "", Os::Linux, home, &|_| None).is_none());
        assert!(own_profile_dirs("epiphany", "/usr/bin/epiphany", Os::Linux, home, &|_| None).is_none());
        // Google Chrome and Edge ignore --remote-debugging-port on their own data directory.
        assert!(!debugs_own_profile("chrome", "/usr/bin/google-chrome-stable"));
        assert!(!debugs_own_profile("com.google.Chrome", "/var/lib/flatpak/exports/bin/com.google.Chrome"));
        assert!(!debugs_own_profile("edge", "/usr/bin/microsoft-edge-stable"));
        assert!(debugs_own_profile("chromium", "/usr/bin/chromium"));
        assert!(debugs_own_profile("brave", "/usr/bin/brave-browser"));
    }

    #[test]
    fn chromium_flags() {
        let b = Resolved { name: "chromium".into(), family: Family::Chromium, path: "/usr/bin/chromium".into() };
        let urls = ["http://localhost:5173".to_string()];
        let l = launch(&Plan {
            browser: &b,
            profile: "/p",
            profile_directory: None,
            net: Net::Proxy { socks_port: 1080, loopback_only: true },
            gui: Gui::Host,
            debug_port: Some(0),
            urls: &urls,
            extra: &[],
            root: false,
            small_shm: false,
            new_instance: true,
            temporary: false,
            verbose: false,
            stealth: None,
        })
        .unwrap();
        assert!(l.argv.contains(&"--proxy-bypass-list=*;<-loopback>".to_string()));
        assert!(l.argv.contains(&"--remote-debugging-port=0".to_string()));
        assert!(l.argv.contains(&"--enable-features=CDPScreenshotNewSurface".to_string()));
        assert_eq!(l.argv.last().unwrap(), "http://localhost:5173");
        assert!(l.later.is_empty());

        let urls = ["http://localhost:5173".to_string(), "http://localhost:8080".to_string()];
        let l = launch(&Plan {
            browser: &b,
            profile: "/p",
            profile_directory: None,
            net: Net::Native,
            gui: Gui::Headless,
            debug_port: Some(9222),
            urls: &urls,
            extra: &[],
            root: false,
            small_shm: false,
            new_instance: true,
            temporary: false,
            verbose: false,
            stealth: None,
        })
        .unwrap();
        assert!(l.argv.contains(&"--headless".to_string()));
        assert_eq!(l.argv.last().unwrap(), "http://localhost:5173");
        assert_eq!(l.later, ["http://localhost:8080"]);
        assert!(!l.argv.iter().any(|a| a.starts_with("--profile-directory")));

        let l = launch(&Plan {
            browser: &b,
            profile: "/home/u/.config/chromium",
            profile_directory: Some("Profile 1"),
            net: Net::Native,
            gui: Gui::Host,
            debug_port: None,
            urls: &[],
            extra: &[],
            root: false,
            small_shm: false,
            new_instance: true,
            temporary: false,
            verbose: false,
            stealth: None,
        })
        .unwrap();
        assert!(l.argv.contains(&"--user-data-dir=/home/u/.config/chromium".to_string()));
        assert!(l.argv.contains(&"--profile-directory=Profile 1".to_string()));
    }

    fn plan<'a>(b: &'a Resolved, urls: &'a [String]) -> Plan<'a> {
        Plan {
            browser: b,
            profile: "/p",
            profile_directory: None,
            net: Net::Proxy { socks_port: 1080, loopback_only: false },
            gui: Gui::Host,
            debug_port: None,
            urls,
            extra: &[],
            root: false,
            small_shm: false,
            new_instance: true,
            temporary: true,
            verbose: false,
            stealth: None,
        }
    }

    #[test]
    fn stealth_chromium_flags() {
        let b = Resolved { name: "chromium".into(), family: Family::Chromium, path: "/usr/bin/chromium".into() };
        let ua =
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
        let st = Stealth {
            user_agent: Some(ua.into()),
            timezone: Some("America/New_York".into()),
            language: Some("de-DE".into()),
        };
        let has = |l: &Launch, a: &str| l.argv.iter().any(|x| x == a);
        let headless = Plan { gui: Gui::Headless, debug_port: Some(0), ..plan(&b, &[]) };

        let l = launch(&headless).unwrap();
        for a in [
            "--disable-component-update",
            "--enable-features=CDPScreenshotNewSurface",
            "--disable-background-timer-throttling",
            "--disable-backgrounding-occluded-windows",
            "--disable-renderer-backgrounding",
        ] {
            assert!(has(&l, a), "{a}");
        }
        assert!(!l.argv.iter().any(|a| a.starts_with("--user-agent")));
        assert!(l.env.is_empty() && l.stealth.is_empty());

        let l = launch(&Plan { stealth: Some(&st), ..headless }).unwrap();
        for a in [
            "--disable-component-update",
            "--disable-background-timer-throttling",
            "--disable-backgrounding-occluded-windows",
            "--disable-renderer-backgrounding",
        ] {
            assert!(!has(&l, a), "{a}");
        }
        // What pages can't see, and what keeps this machine's address and DNS out of sight, stays.
        for a in [
            "--enable-features=CDPScreenshotNewSurface",
            "--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
            "--mute-audio",
            "--disable-blink-features=AutomationControlled",
            "--enable-gpu",
            &format!("--user-agent={ua}"),
        ] {
            assert!(has(&l, a), "{a}");
        }
        assert!(l.argv.iter().any(|a| a.starts_with("--blink-settings=")));
        assert!(l.env.contains(&("TZ".into(), "America/New_York".into())));
        if Os::HERE == Os::Windows {
            assert!(has(&l, "--lang=de-DE"));
        } else {
            assert!(l.env.contains(&("LANGUAGE".into(), "de_DE".into())));
        }
        assert!(l.stealth.contains(&"leaves out --disable-component-update".to_string()));
        assert!(l.stealth.contains(&"sets TZ=America/New_York".to_string()));

        // A debugging port sets navigator.webdriver, with a window or without.
        let windowed = Plan { stealth: Some(&st), gui: Gui::Host, ..headless };
        assert!(has(&launch(&windowed).unwrap(), "--disable-blink-features=AutomationControlled"));
        let l = launch(&Plan { debug_port: None, ..windowed }).unwrap();
        assert!(!l.argv.iter().any(|a| a.starts_with("--disable-blink-features")));

        // In the target, with a window: no --disable-gpu, so a GPU there is used.
        let remote = Plan { net: Net::Native, gui: Gui::X11, ..plan(&b, &[]) };
        assert!(has(&launch(&remote).unwrap(), "--disable-gpu"));
        let l = launch(&Plan { stealth: Some(&Stealth::default()), ..remote }).unwrap();
        assert!(!has(&l, "--disable-gpu") && has(&l, "--password-store=basic"));
        assert_eq!(l.stealth, ["leaves out --disable-component-update", "leaves out --disable-gpu"]);
    }

    #[test]
    fn stealth_firefox_prefs() {
        let b = Resolved { name: "firefox".into(), family: Family::Firefox, path: "/usr/bin/firefox".into() };
        let block = |l: &Launch| match &l.files.last().unwrap().1 {
            Edit::Block(b) => b.clone(),
            Edit::Replace(_) => panic!("user.js is a block"),
        };
        let st =
            Stealth { language: Some("de-DE".into()), timezone: Some("Europe/Berlin".into()), ..Default::default() };
        let l = launch(&Plan { stealth: Some(&st), ..plan(&b, &[]) }).unwrap();
        let prefs = block(&l);
        assert!(prefs.contains("user_pref(\"intl.locale.requested\", \"de-DE\");\n"));
        assert!(prefs.contains("user_pref(\"intl.accept_languages\", \"de-DE, de, en-US, en\");\n"));
        assert!(prefs.contains("user_pref(\"network.http.http3.enable\", false);"));
        assert!(prefs.contains("user_pref(\"dom.min_background_timeout_value\", 1000);"));
        assert_eq!(l.env, [("TZ".to_string(), "Europe/Berlin".to_string())]);

        // A later session without --stealth takes the language back out of prefs.js, unless
        // the browser changed it since.
        let user_js = apply_block(None, &prefs);
        let plain = block(&launch(&plan(&b, &[])).unwrap());
        let prefs_js = "user_pref(\"a\", 1);\nuser_pref(\"intl.accept_languages\", \"de-DE, de, en-US, en\");\n\
                        user_pref(\"intl.locale.requested\", \"fr\");\nuser_pref(\"network.proxy.type\", 1);\n";
        assert_eq!(
            forget_prefs(prefs_js, Some(&user_js), &plain).unwrap(),
            "user_pref(\"a\", 1);\nuser_pref(\"intl.locale.requested\", \"fr\");\nuser_pref(\"network.proxy.type\", 1);\n"
        );
        assert_eq!(forget_prefs(prefs_js, Some(&apply_block(None, &plain)), &plain), None);
        assert_eq!(forget_prefs(prefs_js, None, &plain), None);
    }
}
