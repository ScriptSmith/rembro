//! --stealth: leaving out what pages can tell an automated browser by, and giving a browser
//! that runs here the target's timezone and language.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::browser::{Edit, Family, Gui, Launch, Os, Resolved};
use crate::cli::Browse;
use crate::host::Session;
use crate::proto::Call;
use crate::remote;
use crate::target::Target;

/// What --stealth gives the browser, besides leaving flags out.
#[derive(Default, Debug)]
pub struct Stealth {
    /// A headless Chromium's user agent, without "HeadlessChrome".
    pub user_agent: Option<String>,
    /// The target's timezone, such as "America/New_York", for a browser that runs here.
    pub timezone: Option<String>,
    /// The target's language as a BCP 47 tag, such as "de-DE", for a browser that runs here.
    pub language: Option<String>,
}

/// Prints the target's timezone and locale settings as KEY=VALUE lines.
const SETTINGS: &str = r#"printf '%s\n' "TZ=$TZ" "LC_ALL=$LC_ALL" "LC_MESSAGES=$LC_MESSAGES" "LANG=$LANG" \
  "localtime=$(readlink /etc/localtime 2>/dev/null)" "timezone=$(cat /etc/timezone 2>/dev/null)"
sed -n 's/^LANG=/conf=/p' /etc/locale.conf /etc/default/locale 2>/dev/null"#;

/// Exits with 0 when Chromium on Linux finds Widevine as it starts: in the profile, $2, where
/// Chromium downloads it, or beside the browser, $1, or beside what $1 runs when it is a
/// script, which is where Google Chrome ships it.
const WIDEVINE: &str = r#"[ -d "$2/WidevineCdm" ] && exit 0
exe=$(readlink -f "$1") || exit 1
[ -d "${exe%/*}/WidevineCdm" ] && exit 0
[ "$(head -c 2 "$exe")" = '#!' ] || exit 1
set -f
for word in $(sed -n 's/^[[:space:]]*exec[[:space:]]//p' "$exe" | tr "\"'" '  '); do
  case $word in /*) word=$(readlink -f "$word") && [ -d "${word%/*}/WidevineCdm" ] && exit 0 ;; esac
done
exit 1"#;

/// Refuses what --stealth can't do with this browser at all.
pub fn refuse(family: Family, debug: bool) -> Result<(), String> {
    if family == Family::Firefox && debug {
        return Err("--stealth can't be used with --debug in Firefox, which sets navigator.webdriver to true while \
                    its remote agent runs (Mozilla bug 1719505); leave out --debug, or use Chromium"
            .into());
    }
    Ok(())
}

/// Works out what --stealth gives the browser, and says what it can't hide in this session.
pub fn prepare(b: &Browse, s: &Session, browser: &Resolved) -> Result<Option<Stealth>, String> {
    if !b.stealth {
        return Ok(None);
    }
    refuse(browser.family, b.debug.is_some())?;
    let mut st = Stealth::default();
    if b.headless && browser.family == Family::Chromium {
        // Chromium blanks the high-entropy user agent hints when --user-agent replaces its own.
        eprintln!(
            "rembro: --stealth can't hide every sign of headless Chromium, such as its 800x600 screen, and the empty \
             architecture and full version in navigator.userAgentData once its user agent is changed"
        );
        if !b.browser_args.iter().any(|a| a.starts_with("--user-agent=")) {
            let (version, os) = if b.remote {
                (target_version(s, &browser.path), Os::Linux)
            } else {
                (local_version(&browser.path), Os::HERE)
            };
            let ua =
                version.and_then(|v| chromium_version(browser, &v)).map(|(major, edge)| user_agent(major, edge, os));
            st.user_agent = Some(ua.ok_or_else(|| {
                format!(
                    "cannot tell which version of Chromium {} is, so pages would see HeadlessChrome in its user \
                     agent; leave out --headless, or give the browser a --user-agent after --",
                    browser.name
                )
            })?);
        }
    }
    if !b.remote && !b.loopback_only && s.target != Target::Local {
        match_target(s, browser, &mut st);
    }
    Ok(Some(st))
}

/// Says when pages in a browser in the target will find WebGL missing or rendered in software.
pub fn check_webgl(s: &Session, family: Family, gui: Gui) {
    if gui == Gui::X11 && family == Family::Chromium {
        // The GPU's buffers can't reach this machine's X server through a forwarded connection.
        return eprintln!("rembro: through X11, Chromium in {} can't use a GPU, so pages find WebGL missing", s.label);
    }
    let has = |path: &str| s.call::<Option<u64>>(&Call::Stat { path: path.into() }).ok().flatten().is_some();
    if !has("/dev/dri") && !has("/dev/dxg") {
        eprintln!(
            "rembro: {} has no GPU, so pages there find WebGL missing, or rendered in software by SwiftShader or \
             llvmpipe",
            s.label
        );
    }
}

/// Says when pages will find Widevine missing from a Chromium on Linux, which loads it only as
/// it starts, so a profile without it goes without it for the whole session.
pub fn check_widevine(s: &Session, browser: &Resolved, profile: &str, remote: bool) {
    if browser.family != Family::Chromium || !remote && Os::HERE != Os::Linux {
        return;
    }
    let found = if remote {
        let argv = ["sh", "-c", WIDEVINE, "sh", &browser.path, profile].map(String::from).into();
        matches!(s.call::<(Option<i32>, String)>(&Call::Capture { argv, timeout_ms: 10_000 }), Ok((Some(0), _)))
    } else {
        widevine_here(&browser.path, profile)
    };
    if !found {
        eprintln!(
            "rembro: this profile has no Widevine, and Chromium on Linux loads it only as it starts, so pages in this \
             session find it missing. To get it, keep a profile with --profile NAME and let one session run until \
             Chromium has downloaded Widevine, a minute or so; later sessions with --profile NAME or --fork NAME have it"
        );
    }
}

/// Whether a Chromium here finds Widevine as it starts.
fn widevine_here(browser: &str, profile: &str) -> bool {
    Command::new("sh")
        .args(["-c", WIDEVINE, "sh", browser, profile])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|st| st.success())
}

/// Takes the target's timezone and language for the browser, where they name a place and a
/// language. Otherwise the browser keeps this machine's, which suits a container that shares
/// this machine's network and has no timezone of its own.
fn match_target(s: &Session, browser: &Resolved, st: &mut Stealth) {
    let argv = vec!["sh".into(), "-c".into(), SETTINGS.into()];
    let text = match s.call::<(Option<i32>, String)>(&Call::Capture { argv, timeout_ms: 10_000 }) {
        Ok((_, out)) => out,
        Err(e) => return eprintln!("rembro: cannot read {}'s timezone and language: {e}", s.label),
    };
    let label = &s.label;
    match zone(&text) {
        // On Windows, Chromium and Firefox take the timezone from Windows and ignore TZ.
        Some(z) if cfg!(windows) => {
            eprintln!(
                "rembro: {label}'s timezone is {z}, but browsers on Windows ignore TZ, so this one keeps this machine's"
            )
        }
        Some(z) => st.timezone = Some(z),
        None if s.verbose => {
            eprintln!("rembro: {label} names no geographic timezone, so the browser keeps this machine's")
        }
        None => {}
    }
    match language(&text) {
        Some(l) if cfg!(target_os = "macos") && browser.family == Family::Chromium => eprintln!(
            "rembro: {label}'s language is {l}, but Chromium on macOS takes its language from macOS, so this one \
             keeps this machine's"
        ),
        Some(l) if !matches!(browser.family, Family::Chromium | Family::Firefox) => {
            eprintln!("rembro: {label}'s language is {l}, but {} keeps this machine's", browser.name)
        }
        Some(l) => st.language = Some(l),
        None if s.verbose => eprintln!("rembro: {label} sets no language, so the browser keeps this machine's"),
        None => {}
    }
}

/// The value of a KEY=VALUE line in the output of SETTINGS, if it is not empty.
fn setting<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
        .map(|v| v.trim().trim_matches(['"', '\'']))
        .filter(|v| !v.is_empty())
}

/// The target's timezone, when it names a place: from TZ, or else the /etc/localtime link,
/// or else /etc/timezone.
fn zone(text: &str) -> Option<String> {
    let name = match setting(text, "TZ").map(|tz| tz.strip_prefix(':').unwrap_or(tz)) {
        None | Some("/etc/localtime") => setting(text, "localtime").or_else(|| setting(text, "timezone"))?,
        Some(tz) => tz,
    };
    let name = name.find("zoneinfo/").map_or(name, |i| &name[i + "zoneinfo/".len()..]);
    let name = name.strip_prefix("posix/").or_else(|| name.strip_prefix("right/")).unwrap_or(name);
    geographic(name).then(|| name.to_string())
}

/// Whether a zone is an Area/City name, such as Europe/Berlin, rather than UTC, Etc/GMT+5 or
/// a rule such as EST5EDT.
fn geographic(zone: &str) -> bool {
    zone.contains('/')
        && !zone.starts_with("Etc/")
        && zone.split('/').all(|part| {
            part.starts_with(|c: char| c.is_ascii_uppercase())
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
        })
}

/// The target's language: from LC_ALL, LC_MESSAGES or LANG, whichever is set first, or else
/// from the system's locale file.
fn language(text: &str) -> Option<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG", "conf"].iter().find_map(|k| setting(text, k))?;
    tag(locale)
}

/// The BCP 47 tag for a locale such as de_DE.UTF-8, or None for C, POSIX and C.UTF-8.
fn tag(locale: &str) -> Option<String> {
    let name = locale.split(['.', '@']).next()?;
    let (lang, region) = match name.split_once('_') {
        Some((l, r)) => (l, Some(r)),
        None => (name, None),
    };
    if !(2..=3).contains(&lang.len()) || !lang.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    match region {
        None => Some(lang.into()),
        Some(r)
            if r.len() == 2 && r.bytes().all(|b| b.is_ascii_uppercase())
                || r.len() == 3 && r.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some(format!("{lang}-{r}"))
        }
        Some(_) => None,
    }
}

/// Firefox's intl.accept_languages for a language, in the form Chromium uses by default:
/// de-DE gives "de-DE, de, en-US, en".
pub fn accept_languages(tag: &str) -> String {
    let lang = tag.split('-').next().unwrap_or(tag);
    let mut list = vec![tag];
    if lang != tag {
        list.push(lang);
    }
    if lang != "en" {
        list.extend(["en-US", "en"]);
    }
    list.join(", ")
}

/// The --version output of a Chromium here. On Windows, where chrome.exe doesn't print its
/// version, the name of the directory beside it that is named after the version.
fn local_version(path: &str) -> Option<String> {
    if cfg!(windows) {
        let dir = Path::new(path).parent()?;
        return fs::read_dir(dir).ok()?.flatten().find_map(|e| {
            let name = e.file_name().to_string_lossy().trim_end_matches(".manifest").to_string();
            remote::chromium_major(&name).is_some().then_some(name)
        });
    }
    let out = Command::new(path).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The --version output of a Chromium in the target.
fn target_version(s: &Session, path: &str) -> Option<String> {
    let argv = vec![path.to_string(), "--version".into()];
    s.call::<(Option<i32>, String)>(&Call::Capture { argv, timeout_ms: 10_000 }).ok().map(|(_, out)| out)
}

/// The major version of Chromium that a browser is built on, and whether it is Edge, from
/// the output of its --version. Vivaldi and Opera print versions of their own, so they get none.
fn chromium_version(browser: &Resolved, version: &str) -> Option<(u32, bool)> {
    let products = ["Chromium", "Google Chrome", "Brave Browser", "Microsoft Edge"];
    let known = ["chromium", "chrome", "brave", "edge", "playwright-chromium"].contains(&browser.name.as_str());
    if !known && !products.iter().any(|p| version.trim_start().starts_with(p)) {
        return None;
    }
    let edge = browser.name == "edge" || version.contains("Edge");
    Some((remote::chromium_major(version)?, edge))
}

/// The user agent of a windowed Chromium. Chromium reports only its major version and a fixed
/// platform for each system.
fn user_agent(major: u32, edge: bool, os: Os) -> String {
    let platform = match os {
        Os::Linux => "X11; Linux x86_64",
        Os::Macos => "Macintosh; Intel Mac OS X 10_15_7",
        Os::Windows => "Windows NT 10.0; Win64; x64",
    };
    let edge = if edge { format!(" Edg/{major}.0.0.0") } else { String::new() };
    format!("Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36{edge}")
}

/// What a launch with --stealth does differently from the same launch without it.
pub fn changes(plain: &Launch, stealthy: &Launch) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(plain.argv.iter().filter(|a| !stealthy.argv.contains(a)).map(|a| format!("leaves out {a}")));
    out.extend(stealthy.argv.iter().filter(|a| !plain.argv.contains(a)).map(|a| format!("adds {a}")));
    out.extend(stealthy.env.iter().filter(|e| !plain.env.contains(e)).map(|(k, v)| format!("sets {k}={v}")));
    let lines = |l: &Launch| -> Vec<String> {
        l.files
            .iter()
            .filter_map(|(_, e)| if let Edit::Block(b) = e { Some(b.lines().map(String::from)) } else { None })
            .flatten()
            .collect()
    };
    let (before, after) = (lines(plain), lines(stealthy));
    out.extend(before.iter().filter(|l| !after.contains(l)).map(|l| format!("leaves out {l}")));
    out.extend(after.iter().filter(|l| !before.contains(l)).map(|l| format!("adds {l}")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones() {
        let z = |text: &str| zone(text);
        assert_eq!(
            z("TZ=America/New_York\nlocaltime=/usr/share/zoneinfo/Etc/UTC\n").as_deref(),
            Some("America/New_York")
        );
        assert_eq!(z("TZ=:/usr/share/zoneinfo/Europe/Berlin\n").as_deref(), Some("Europe/Berlin"));
        // TZ decides, even when it names no place.
        assert_eq!(z("TZ=UTC\nlocaltime=/usr/share/zoneinfo/Europe/Berlin\n"), None);
        assert_eq!(z("TZ=EST5EDT\n"), None);
        assert_eq!(z("TZ=:/etc/localtime\nlocaltime=/usr/share/zoneinfo/Asia/Tokyo\n").as_deref(), Some("Asia/Tokyo"));
        assert_eq!(
            z("TZ=\nlocaltime=../usr/share/zoneinfo/America/Argentina/Buenos_Aires\n").as_deref(),
            Some("America/Argentina/Buenos_Aires")
        );
        assert_eq!(
            z("TZ=\nlocaltime=/usr/share/zoneinfo/posix/America/Port-au-Prince\n").as_deref(),
            Some("America/Port-au-Prince")
        );
        // Docker images usually link Etc/UTC, or have no link and no /etc/timezone.
        assert_eq!(z("TZ=\nlocaltime=/usr/share/zoneinfo/Etc/UTC\ntimezone=Europe/Paris\n"), None);
        assert_eq!(z("TZ=\nlocaltime=\ntimezone=Europe/Paris\n").as_deref(), Some("Europe/Paris"));
        assert_eq!(z("TZ=\nlocaltime=\ntimezone=Etc/UTC\n"), None);
        assert_eq!(z("TZ=\nlocaltime=\ntimezone=\n"), None);
        assert_eq!(z("TZ=\nlocaltime=/usr/share/zoneinfo/UTC\n"), None);
        assert_eq!(z("TZ=../../etc/passwd\n"), None);
    }

    #[test]
    fn languages() {
        let l = |text: &str| language(text);
        assert_eq!(l("LC_ALL=\nLC_MESSAGES=\nLANG=de_DE.UTF-8\n").as_deref(), Some("de-DE"));
        assert_eq!(l("LC_ALL=fr_CA.UTF-8\nLANG=de_DE.UTF-8\n").as_deref(), Some("fr-CA"));
        assert_eq!(l("LC_MESSAGES=sr_RS@latin\nLANG=C\n").as_deref(), Some("sr-RS"));
        assert_eq!(l("LANG=es_419.UTF-8\n").as_deref(), Some("es-419"));
        // An explicit C locale decides, so the locale file doesn't apply.
        for unset in ["LANG=C.UTF-8\nconf=de_DE.UTF-8\n", "LANG=POSIX\n", "LC_ALL=C\nLANG=de_DE.UTF-8\n", "LANG=\n"] {
            assert_eq!(l(unset), None, "{unset}");
        }
        assert_eq!(l("LANG=\nconf=\"pt_BR.UTF-8\"\n").as_deref(), Some("pt-BR"));
        assert_eq!(l("LANG=en_us\n"), None);
        assert_eq!(accept_languages("de-DE"), "de-DE, de, en-US, en");
        assert_eq!(accept_languages("en-GB"), "en-GB, en");
        assert_eq!(accept_languages("fr"), "fr, en-US, en");
    }

    #[test]
    fn user_agents() {
        let b = |name: &str| Resolved { name: name.into(), family: Family::Chromium, path: String::new() };
        let ua = |name: &str, version: &str, os| {
            chromium_version(&b(name), version).map(|(major, edge)| user_agent(major, edge, os))
        };
        assert_eq!(
            ua("chromium", "Chromium 157.0.8103.38 Arch Linux\n", Os::Linux).unwrap(),
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36"
        );
        assert!(ua("edge", "Microsoft Edge 131.0.2903.86", Os::Windows).unwrap().ends_with(" Edg/131.0.0.0"));
        assert!(ua("chrome", "131.0.6778.86", Os::Macos).unwrap().contains("(Macintosh; Intel Mac OS X 10_15_7)"));
        assert!(ua("brave", "Brave Browser 131.1.73.89", Os::Linux).unwrap().contains(" Chrome/131.0.0.0 "));
        assert!(ua("thorium", "Google Chrome for Testing 130.0.6723.58", Os::Linux).is_some());
        assert_eq!(ua("vivaldi", "Vivaldi 7.0.3495.27 stable", Os::Linux), None);
        assert_eq!(ua("chromium", "nonsense", Os::Linux), None);
    }

    #[test]
    fn refusals() {
        assert!(refuse(Family::Firefox, true).unwrap_err().contains("navigator.webdriver"));
        assert!(refuse(Family::Firefox, false).is_ok());
        assert!(refuse(Family::Chromium, true).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn widevine() {
        let dir = crate::util::mkdtemp(&std::env::temp_dir(), "rembro-test-").unwrap();
        let path = |p: &str| dir.join(p).to_string_lossy().into_owned();
        for d in ["profile", "bin", "opt/chrome/WidevineCdm", "opt/chromium"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        fs::write(dir.join("opt/chrome/chrome"), "\x7fELF").unwrap();
        fs::write(dir.join("opt/chromium/chromium"), "\x7fELF").unwrap();
        std::os::unix::fs::symlink(dir.join("opt/chrome/chrome"), dir.join("bin/chrome")).unwrap();
        let wrapper = |to: &str| format!("#!/bin/sh\nexec '{}' $FLAGS \"$@\"\n", path(to));
        fs::write(dir.join("bin/google-chrome"), wrapper("opt/chrome/chrome")).unwrap();
        fs::write(dir.join("bin/chromium"), wrapper("opt/chromium/chromium")).unwrap();
        let has = |browser: &str| widevine_here(&path(browser), &path("profile"));
        // Google Chrome ships it, found through a symbolic link or a script that runs it.
        for chrome in ["opt/chrome/chrome", "bin/chrome", "bin/google-chrome"] {
            assert!(has(chrome), "{chrome}");
        }
        assert!(!has("opt/chromium/chromium") && !has("bin/chromium"));
        // Chromium downloaded it into the profile.
        fs::create_dir(dir.join("profile/WidevineCdm")).unwrap();
        assert!(has("bin/chromium"));
        fs::remove_dir_all(dir).unwrap();
    }
}
