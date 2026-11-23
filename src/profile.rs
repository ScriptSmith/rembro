//! Browser profiles: temporary and kept ones, keeping rembro's changes out of profiles that
//! it did not create, telling its own running sessions apart from browsers started some
//! other way, and `rembro profiles`.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::browser::Family;
use crate::cli::Profiles;
use crate::{certs, sys, util};

/// Present in profiles that rembro created.
const OWNER: &str = "rembro-profile";
/// While a rembro session has the profile open: rembro's PID and the target.
const SESSION: &str = "rembro-session";
/// Locked by the rembro session that has the profile open, so that two sessions starting at
/// once can't both take it.
const LOCK: &str = "rembro-session.lock";
/// Settings of a Firefox profile that rembro did not create, to put back after a session.
const RESTORE: &str = "rembro-restore.json";
/// Made and removed in a profile when a session forks it.
const FORKED: &str = "rembro-forked";
/// The start of a temporary profile's directory name, in the system's temporary directory.
const TEMP_PREFIX: &str = "rembro-profile-";
/// The start of the name of a `RunDir`, in the system's temporary directory.
const RUN_PREFIX: &str = "rembro-run-";
/// The name of the profile in a `RunDir`.
const RUN_PROFILE: &str = "profile";

/// Whether a running browser already has the profile open on this machine.
#[cfg(unix)]
pub fn in_use(profile: &Path, family: Family) -> bool {
    let link = match family {
        Family::Chromium => profile.join("SingletonLock"),
        Family::Firefox => profile.join("lock"),
        _ => return false,
    };
    let Ok(target) = fs::read_link(link) else {
        // Firefox on macOS makes no symlink, and locks .parentlock instead.
        return family == Family::Firefox && sys::fcntl_locked(&profile.join(".parentlock"));
    };
    let target = target.to_string_lossy();
    // Chromium writes HOST-PID; Firefox writes ADDRESS:+PID.
    let (host, pid) = match family {
        Family::Chromium => match target.rsplit_once('-') {
            Some((h, p)) => (Some(h), p),
            None => return false,
        },
        _ => match target.rsplit_once('+') {
            Some((_, p)) => (None, p),
            None => return false,
        },
    };
    if host.is_some_and(|h| h != sys::hostname()) {
        return false;
    }
    pid.parse::<u32>().is_ok_and(sys::pid_alive)
}

/// Whether a running browser already has the profile open on this machine. On Windows,
/// Chromium and Firefox keep a lock file open without sharing it, and it goes when they exit.
#[cfg(windows)]
pub fn in_use(profile: &Path, family: Family) -> bool {
    let lock = match family {
        Family::Chromium => profile.join("lockfile"),
        Family::Firefox => profile.join("parent.lock"),
        _ => return false,
    };
    lock.exists() && fs::OpenOptions::new().write(true).open(&lock).is_err()
}

/// A profile directory that a session is to use.
pub struct Claimed {
    pub path: PathBuf,
    /// A browser already has it open.
    pub busy: bool,
    /// Marks the profile as this session's, unless a browser already had it open.
    pub mark: Option<SessionMark>,
}

/// Creates the profile at `path` if it does not exist, and marks it as this session's. When
/// another rembro session has it and has not started its browser yet, waits for that browser,
/// so that this session hands it the pages rather than starting a second browser that could
/// take the profile from it.
pub fn claim(path: &Path, family: Family, target: &str) -> Result<Claimed, String> {
    claim_within(path, family, target, Duration::from_secs(30))
}

fn claim_within(path: &Path, family: Family, target: &str, wait: Duration) -> Result<Claimed, String> {
    let started = Instant::now();
    let mut told = false;
    loop {
        if in_use(path, family) {
            return Ok(Claimed { path: path.to_path_buf(), busy: true, mark: None });
        }
        let new = fs::read_dir(path).map(|mut d| d.next().is_none()).unwrap_or(true);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        sys::create_private_dir(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        if new {
            mark_created(path);
        }
        if let Some(mark) = SessionMark::claim(path, target) {
            return Ok(Claimed { path: path.to_path_buf(), busy: false, mark: Some(mark) });
        }
        // Only Chromium's and Firefox's browsers can be seen to start.
        if !matches!(family, Family::Chromium | Family::Firefox) || started.elapsed() >= wait {
            return Ok(Claimed { path: path.to_path_buf(), busy: true, mark: None });
        }
        if !told && started.elapsed() >= Duration::from_secs(2) {
            eprintln!("rembro: waiting for the rembro session that has {} to start its browser", path.display());
            told = true;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Creates a temporary profile in `base` and marks it as this session's. With `fork`, it
/// starts as a copy of that profile. The caller deletes it when the session ends.
pub fn create_temp(base: &Path, fork: Option<&Path>, target: &str) -> Result<Claimed, String> {
    if let Some(src) = fork
        && !src.is_dir()
    {
        return Err(format!("there is no profile at {}", src.display()));
    }
    let path = util::mkdtemp(base, TEMP_PREFIX).map_err(|e| format!("mkdtemp: {e}"))?;
    mark_created(&path);
    // Locked before the copy, so that `rembro profiles --prune` leaves it alone.
    let mark = SessionMark::claim(&path, target);
    if let Some(src) = fork {
        if let Err(e) = copy_profile(src, &path) {
            let _ = fs::remove_dir_all(&path);
            return Err(format!("cannot copy {}: {e}", src.display()));
        }
        // Forking a profile is using it, as far as `rembro profiles --prune` is concerned.
        if src.join(OWNER).exists() {
            touch(src);
        }
    }
    Ok(Claimed { path, busy: false, mark })
}

/// Changes the profile directory's time, which `rembro profiles` shows as its last use, as
/// a session's mark does when it comes and goes.
fn touch(profile: &Path) {
    let file = profile.join(FORKED);
    if fs::write(&file, "").is_ok() {
        let _ = fs::remove_file(file);
    }
}

/// Deletes a profile. It is moved aside first, so that a session that starts meanwhile
/// creates a new profile rather than taking one that is half deleted.
fn remove_profile(path: &Path) -> io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let aside = path.with_file_name(format!(".{name}.rembro-deleting-{}", std::process::id()));
    match fs::rename(path, &aside) {
        Ok(()) => fs::remove_dir_all(aside),
        Err(_) => fs::remove_dir_all(path),
    }
}

/// The private directory, in the system's temporary directory, that holds the profile for the
/// links that a program under `rembro run` opens. The sessions that `rembro run` starts for
/// them share that profile, and the one whose browser has it open deletes it when it ends.
/// The directory goes with `rembro run`, or after it with the last of those sessions, and
/// stays in place while `rembro run` runs, so a profile is never made at a path that other
/// users of this machine could have taken.
pub struct RunDir {
    dir: PathBuf,
    _mark: Option<SessionMark>,
}

impl RunDir {
    pub fn create() -> Result<RunDir, String> {
        let dir = util::mkdtemp(&std::env::temp_dir(), RUN_PREFIX).map_err(|e| format!("mkdtemp: {e}"))?;
        mark_created(&dir);
        let _mark = SessionMark::claim(&dir, "rembro run");
        Ok(RunDir { dir, _mark })
    }

    pub fn profile(&self) -> PathBuf {
        self.dir.join(RUN_PROFILE)
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        if !run_profile_open(&self.profile()) {
            let _ = remove_profile(&self.dir);
        }
    }
}

/// Whether a session has the profile in a `RunDir` open, or is creating it.
fn run_profile_open(profile: &Path) -> bool {
    profile.exists() && (!profile.join(OWNER).exists() || is_open(profile))
}

/// Whether `path` is the profile in a `RunDir`.
pub fn is_run_profile(path: &Path) -> bool {
    path.file_name() == Some(RUN_PROFILE.as_ref())
        && path.parent().is_some_and(|dir| {
            dir.parent() == Some(std::env::temp_dir().as_path())
                && dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with(RUN_PREFIX))
        })
}

/// Deletes a temporary profile, or the profile in a `RunDir`, when dropped. The `RunDir`
/// goes too once `rembro run` has ended.
pub struct Discard(pub PathBuf);

impl Drop for Discard {
    fn drop(&mut self) {
        let _ = remove_profile(&self.0);
        if !is_run_profile(&self.0) {
            return;
        }
        // Once `rembro run` has ended, and unless another session has made the profile again.
        if let Some(dir) = self.0.parent()
            && session_target(dir).is_none()
            && !run_profile_open(&self.0)
        {
            let _ = remove_profile(dir);
        }
    }
}

/// What a copy of a profile leaves out at its top level: rembro's files, the locks of the
/// browser and rembro session that have it open, crash reports, metrics, and installers of
/// Chromium components that it has already installed.
const NOT_COPIED: &[&str] = &[
    OWNER,
    SESSION,
    LOCK,
    RESTORE,
    FORKED,
    "DevToolsActivePort",
    "lockfile",
    "parent.lock",
    ".parentlock",
    // In a copy of a profile that Firefox has open, it would restore these tabs as after a crash.
    "sessionstore-backups",
    "Crashpad",
    "BrowserMetrics",
    "DeferredBrowserMetrics",
    "component_crx_cache",
];
/// Caches, left out wherever they are, which the browser fills again.
const CACHES: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "GPUPersistentCache",
    "GrShaderCache",
    "GraphiteDawnCache",
    "ShaderCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "cache2",
    "startupCache",
    "shader-cache",
];

/// Copies a profile, which a browser may have open, into the empty directory `dst`.
fn copy_profile(src: &Path, dst: &Path) -> io::Result<()> {
    fn copy_dir(src: &Path, dst: &Path, top: bool) -> io::Result<()> {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if CACHES.contains(&name.as_ref()) || (top && NOT_COPIED.contains(&name.as_ref())) {
                continue;
            }
            // Symbolic links are left out too: in a profile, they are Chromium's and Firefox's locks.
            let kind = entry.file_type()?;
            let to = dst.join(entry.file_name());
            let result = if kind.is_dir() {
                fs::create_dir_all(&to).and_then(|()| copy_dir(&entry.path(), &to, false))
            } else if kind.is_file() {
                fs::copy(entry.path(), &to).map(|_| ())
            } else {
                Ok(())
            };
            match result {
                // The browser that has it open removed it.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                r => r?,
            }
        }
        Ok(())
    }
    copy_dir(src, dst, true)?;
    // Chromium marks a profile as crashed while it has it open, and offers to restore the
    // tabs of a copy made then.
    for entry in fs::read_dir(dst)?.flatten() {
        let prefs = entry.path().join("Preferences");
        let Ok(text) = fs::read(&prefs) else { continue };
        let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&text) else { continue };
        if let Some(p) = json.get_mut("profile").and_then(|p| p.as_object_mut())
            && p.get("exit_type").is_some_and(|t| t != "Normal")
        {
            p.insert("exit_type".into(), "Normal".into());
            p.insert("exited_cleanly".into(), true.into());
            fs::write(&prefs, serde_json::to_vec(&json)?)?;
        }
    }
    Ok(())
}

/// Whether a Chromium or Firefox browser has the profile open.
pub fn is_open_by_browser(profile: &Path) -> bool {
    in_use(profile, Family::Chromium) || in_use(profile, Family::Firefox)
}

/// Whether a browser or a rembro session has the profile open. Its lock goes untouched, so
/// that a session claiming the profile at the same moment doesn't find it taken.
pub fn is_open(profile: &Path) -> bool {
    is_open_by_browser(profile) || session_target(profile).is_some()
}

pub fn mark_created(profile: &Path) {
    let _ = fs::write(
        profile.join(OWNER),
        "rembro created this browser profile and may change its settings each time it starts the browser.\n",
    );
}

pub fn created_by_rembro(profile: &Path) -> bool {
    profile.join(OWNER).exists() || profile.starts_with(kept_dir())
}

/// The target of the running rembro session that has this profile open, if there is one.
pub fn session_target(profile: &Path) -> Option<String> {
    let text = fs::read_to_string(profile.join(SESSION)).ok()?;
    let (pid, target) = text.split_once('\n')?;
    sys::process_name(pid.trim().parse().ok()?)?.contains("rembro").then(|| target.trim().to_string())
}

/// Records that this rembro session has the profile open, until dropped.
pub struct SessionMark {
    path: PathBuf,
    /// Dropped after `path` is removed, which lets the next session in.
    _lock: Option<File>,
}

impl SessionMark {
    /// Marks the profile as this session's, or returns None if another rembro session that
    /// is still running holds it, or the profile is gone. The lock goes with this process, so
    /// a session that was killed leaves nothing in the way.
    pub fn claim(profile: &Path, target: &str) -> Option<SessionMark> {
        if !profile.is_dir() {
            return None;
        }
        let lock = match lock_path(profile).map(|p| util::lock_file(&p)) {
            Some(Ok(Some(f))) => Some(f),
            Some(Ok(None)) => return None,
            Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => return None,
            // A file system without locks, or a lock file that can't be opened: go on without one.
            _ => None,
        };
        let path = profile.join(SESSION);
        let _ = fs::write(&path, format!("{}\n{target}\n", std::process::id()));
        Some(SessionMark { path, _lock: lock })
    }
}

/// The file a session locks while it has the profile open. It stays in place, since removing
/// it would let two sessions lock different files, so for a profile that rembro did not
/// create it lives in rembro's own directory, named after the profile's path.
fn lock_path(profile: &Path) -> Option<PathBuf> {
    if created_by_rembro(profile) {
        return Some(profile.join(LOCK));
    }
    let profile = fs::canonicalize(profile).unwrap_or_else(|_| profile.to_path_buf());
    let name = format!("{:016x}.lock", util::fnv64(profile.to_string_lossy().as_bytes()));
    let dir = util::data_dir().join("rembro/locks");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(name);
    path.is_absolute().then_some(path)
}

impl Drop for SessionMark {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    /// user.js as it was, or None if there was none.
    user_js: Option<String>,
    /// For each pref rembro sets, its line in prefs.js before, if it had one.
    prefs: Vec<(String, Option<String>)>,
}

fn pref_line_key(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("user_pref(\"")?.split_once('"').map(|(k, _)| k)
}

/// The pref names in a block of `user_pref(...)` lines.
pub fn pref_keys(block: &str) -> Vec<String> {
    block.lines().filter_map(pref_line_key).map(String::from).collect()
}

/// Puts back a Firefox profile's settings when dropped.
pub struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        if let Err(e) = restore(&self.0) {
            eprintln!("rembro: could not put back the settings in {}: {e}", self.0.display());
        }
    }
}

/// Saves the user.js and the prefs that rembro is about to change in a Firefox profile it
/// did not create. The returned guard puts them back; if rembro dies first, the next
/// launch with this profile does it.
pub fn save_firefox(profile: &Path, keys: &[String]) -> Result<Restore, String> {
    let user_js = fs::read_to_string(profile.join("user.js")).ok();
    let prefs_js = fs::read_to_string(profile.join("prefs.js")).unwrap_or_default();
    let prefs = keys
        .iter()
        .map(|k| (k.clone(), prefs_js.lines().find(|l| pref_line_key(l) == Some(k)).map(String::from)))
        .collect();
    let saved = serde_json::to_vec(&Saved { user_js, prefs }).unwrap();
    fs::write(profile.join(RESTORE), saved).map_err(|e| format!("cannot write {}: {e}", profile.display()))?;
    Ok(Restore(profile.to_path_buf()))
}

/// Puts back settings saved by `save_firefox`, if a session left some behind.
pub fn restore(profile: &Path) -> Result<(), String> {
    let path = profile.join(RESTORE);
    let Ok(data) = fs::read(&path) else { return Ok(()) };
    let saved: Saved = serde_json::from_slice(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let user_js = profile.join("user.js");
    match &saved.user_js {
        Some(text) => fs::write(&user_js, text),
        None => fs::remove_file(&user_js)
            .or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }),
    }
    .map_err(|e| format!("{}: {e}", user_js.display()))?;
    // Firefox copied rembro's user.js prefs into prefs.js, so those lines go back too.
    let prefs_path = profile.join("prefs.js");
    if let Ok(text) = fs::read_to_string(&prefs_path) {
        let ours: Vec<&str> = saved.prefs.iter().map(|(k, _)| k.as_str()).collect();
        let mut out: String = text
            .lines()
            .filter(|l| pref_line_key(l).is_none_or(|k| !ours.contains(&k)))
            .map(|l| format!("{l}\n"))
            .collect();
        for line in saved.prefs.iter().filter_map(|(_, l)| l.as_ref()) {
            out.push_str(line);
            out.push('\n');
        }
        fs::write(&prefs_path, out).map_err(|e| format!("{}: {e}", prefs_path.display()))?;
    }
    fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Takes the target's CAs back out of a profile that rembro did not create, when dropped.
pub struct Untrust(pub Family, pub PathBuf);

impl Drop for Untrust {
    fn drop(&mut self) {
        let result = match self.0 {
            Family::Chromium => certs::apply_chromium(&self.1, &[]),
            Family::Firefox => certs::apply_firefox(&self.1, &[]),
            _ => Ok(()),
        };
        if let Err(e) = result {
            eprintln!("rembro: could not remove the target's CAs from {}: {e}", self.1.display());
        }
    }
}

/// Where rembro keeps profiles on this machine, as TARGET/BROWSER/NAME.
pub fn kept_dir() -> PathBuf {
    util::data_dir().join("rembro/profiles")
}

/// A profile that `rembro profiles` lists.
struct Found {
    path: PathBuf,
    /// TARGET/BROWSER/NAME, or a temporary profile's directory name.
    name: String,
    size: u64,
    used: SystemTime,
    open: bool,
    /// rembro created it, so --prune may delete it.
    ours: bool,
}

impl Found {
    fn new(path: PathBuf, name: String) -> Found {
        Found {
            size: dir_size(&path),
            used: last_used(&path),
            open: open_now(&path),
            ours: path.join(OWNER).exists(),
            name,
            path,
        }
    }

    fn days(&self, now: SystemTime) -> u64 {
        days_between(self.used, now)
    }
}

/// When a session last used the profile. A session creates and removes its mark in the
/// profile, and a fork does the same, so the directory's time is when one last came or went.
fn last_used(profile: &Path) -> SystemTime {
    fs::metadata(profile).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

fn days_between(then: SystemTime, now: SystemTime) -> u64 {
    now.duration_since(then).map(|d| d.as_secs() / 86400).unwrap_or(0)
}

/// Whether a browser or a rembro session has the profile open, or for a `RunDir`, whether
/// `rembro run` or a session with its profile is still running.
fn open_now(path: &Path) -> bool {
    is_open(path) || run_profile_open(&path.join(RUN_PROFILE))
}

/// Whether `dir` is a browser profile rather than a directory of them.
fn is_profile(dir: &Path) -> bool {
    [OWNER, "Local State", "prefs.js", "times.json"].iter().any(|f| dir.join(f).exists())
}

/// The directories in `dir`, leaving out those that `remove_profile` is deleting.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// The profiles kept in `dir`, and the temporary ones in `temp` that sessions which were
/// killed left behind.
fn find(dir: &Path, temp: &Path) -> (Vec<Found>, Vec<Found>) {
    let mut kept = Vec::new();
    let mut add = |profile: PathBuf| {
        let name = profile.strip_prefix(dir).unwrap_or(&profile).to_string_lossy().into_owned();
        kept.push(Found::new(profile, name));
    };
    for target in subdirs(dir) {
        for browser in subdirs(&target) {
            // --remote keeps profiles in BROWSER/NAME in the target, which can be this machine.
            if is_profile(&browser) {
                add(browser);
                continue;
            }
            subdirs(&browser).into_iter().for_each(&mut add);
        }
    }
    let left = subdirs(temp)
        .into_iter()
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(TEMP_PREFIX) || n.starts_with(RUN_PREFIX)
            })
        })
        .map(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            Found::new(p, name)
        })
        .filter(|f| f.ours && !f.open)
        .collect();
    (kept, left)
}

fn size(bytes: u64) -> String {
    let mut n = bytes as f64;
    let mut unit = 0;
    while n >= 1024.0 && unit < 4 {
        n /= 1024.0;
        unit += 1;
    }
    let suffix = ["B", "K", "M", "G", "T"][unit];
    if unit > 0 && n < 10.0 { format!("{n:.1}{suffix}") } else { format!("{n:.0}{suffix}") }
}

fn ago(days: u64) -> String {
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        n => format!("{n} days ago"),
    }
}

/// Deletes the profiles in `kept` that rembro created and no session has used for `days`
/// days, and the temporary ones in `left`. Returns those it removed.
fn prune<'a>(dir: &Path, kept: &'a [Found], left: &'a [Found], days: u64, now: SystemTime) -> Vec<&'a Found> {
    let mut removed = Vec::new();
    let kept = kept.iter().filter(|f| f.ours && !f.open && f.days(now) >= days).map(|f| (f, true));
    for (f, is_kept) in kept.chain(left.iter().map(|f| (f, false))) {
        // Checked again, since a session may have used it after `find` looked.
        if open_now(&f.path) || (is_kept && days_between(last_used(&f.path), now) < days) {
            continue;
        }
        // Locked first, so that no session starts with it while it goes.
        let Some(_mark) = SessionMark::claim(&f.path, "rembro profiles") else { continue };
        if is_open_by_browser(&f.path) || run_profile_open(&f.path.join(RUN_PROFILE)) {
            continue;
        }
        match remove_profile(&f.path) {
            Ok(()) => removed.push(f),
            Err(e) => eprintln!("rembro: cannot remove {}: {e}", f.path.display()),
        }
    }
    // The TARGET and BROWSER directories that are left empty.
    for target in subdirs(dir) {
        for browser in subdirs(&target) {
            let _ = fs::remove_dir(browser);
        }
        let _ = fs::remove_dir(target);
    }
    removed
}

/// `rembro profiles`: lists the profiles rembro keeps on this machine, or with --prune,
/// deletes those that no session has used for that many days.
pub fn command(p: &Profiles) -> Result<i32, String> {
    let dir = kept_dir();
    let temp = std::env::temp_dir();
    let (kept, left) = find(&dir, &temp);
    let now = SystemTime::now();
    let Some(days) = p.prune else {
        let row = |f: &Found, note: &str| println!("{:>6}  {:<12}  {}{note}", size(f.size), ago(f.days(now)), f.name);
        if kept.is_empty() {
            println!("No profiles in {}", dir.display());
        } else {
            println!("Profiles in {}:", dir.display());
        }
        for f in &kept {
            row(
                f,
                match (f.open, f.ours) {
                    (true, _) => "  (open)",
                    (false, false) => "  (no rembro-profile mark, so --prune keeps it)",
                    (false, true) => "",
                },
            );
        }
        if !left.is_empty() {
            println!("Temporary profiles left in {} by sessions that were killed:", temp.display());
        }
        for f in &left {
            row(f, "");
        }
        return Ok(0);
    };
    let removed = prune(&dir, &kept, &left, days, now);
    for f in &removed {
        println!("Removed {} ({}, last used {})", f.name, size(f.size), ago(f.days(now)));
    }
    let freed = size(removed.iter().map(|f| f.size).sum());
    match removed.len() {
        0 if days == 1 => println!("No profile has gone unused for a day"),
        0 => println!("No profile has gone unused for {days} days"),
        1 => println!("Freed {freed}"),
        n => println!("Removed {n} profiles and freed {freed}"),
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firefox_settings_come_back() {
        let dir = util::mkdtemp(&std::env::temp_dir(), "rembro-test-").unwrap();
        let prefs = "// Mozilla User Preferences\nuser_pref(\"network.proxy.type\", 4);\nuser_pref(\"other\", 1);\n";
        fs::write(dir.join("prefs.js"), prefs).unwrap();
        fs::write(dir.join("user.js"), "user_pref(\"mine\", true);\n").unwrap();
        let keys = pref_keys("user_pref(\"network.proxy.type\", 1);\nuser_pref(\"network.proxy.socks_port\", 9);\n");
        let guard = save_firefox(&dir, &keys).unwrap();
        // What a session leaves behind: rembro's block in user.js, copied into prefs.js.
        fs::write(dir.join("user.js"), "user_pref(\"mine\", true);\n// rembro begin\n// rembro end\n").unwrap();
        let during = "user_pref(\"network.proxy.socks_port\", 9);\nuser_pref(\"network.proxy.type\", 1);\nuser_pref(\"other\", 2);\n";
        fs::write(dir.join("prefs.js"), during).unwrap();
        drop(guard);
        assert_eq!(fs::read_to_string(dir.join("user.js")).unwrap(), "user_pref(\"mine\", true);\n");
        let after = fs::read_to_string(dir.join("prefs.js")).unwrap();
        assert!(after.contains("user_pref(\"network.proxy.type\", 4);"));
        assert!(after.contains("user_pref(\"other\", 2);"));
        assert!(!after.contains("socks_port"));
        assert!(!dir.join(RESTORE).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    /// Claims the profile, allowing for a process that another test thread is starting: until
    /// it execs, it holds a copy of each open file, and so the locks of files that were closed.
    fn claim_soon(profile: &Path) -> Option<SessionMark> {
        (0..50).find_map(|_| {
            SessionMark::claim(profile, "sbx:qut").or_else(|| {
                thread::sleep(Duration::from_millis(20));
                None
            })
        })
    }

    /// Windows opens a directory only with backup semantics, and changes its times only
    /// through a handle that may write its attributes.
    fn set_dir_modified(dir: &Path, time: SystemTime) -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            options.access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        }
        options.open(dir)?.set_modified(time)
    }

    #[test]
    fn sessions() {
        let dir = util::mkdtemp(&std::env::temp_dir(), "rembro-test-").unwrap();
        mark_created(&dir);
        assert_eq!(session_target(&dir), None);
        {
            let _mark = SessionMark::claim(&dir, "sbx:qut").unwrap();
            // This test binary's name contains "rembro".
            assert_eq!(session_target(&dir).as_deref(), Some("sbx:qut"));
            assert!(SessionMark::claim(&dir, "sbx:qut").is_none());
        }
        assert_eq!(session_target(&dir), None);
        assert!(claim_soon(&dir).is_some());
        // A profile that is gone can't be claimed.
        assert!(SessionMark::claim(&dir.join("gone"), "sbx:qut").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn kept_profiles() {
        let dir = util::mkdtemp(&std::env::temp_dir(), "rembro-test-").unwrap();
        let path = dir.join("work");
        let first = claim(&path, Family::Chromium, "sbx:qut").unwrap();
        assert!(!first.busy);
        assert!(path.join(OWNER).exists());
        assert!(is_open(&path));
        // Another session waits for the first one's browser, and finds the profile open.
        let second = claim_within(&path, Family::Chromium, "sbx:qut", Duration::from_millis(300)).unwrap();
        assert!(second.busy && second.mark.is_none());
        // It takes the profile if the first session ends while it waits.
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            drop(first);
        });
        let third = claim_within(&path, Family::Chromium, "sbx:qut", Duration::from_secs(5)).unwrap();
        assert!(!third.busy && third.mark.is_some());
        release.join().unwrap();
        drop(third);
        assert!(!is_open(&path));
        // A browser rembro did not start has it open.
        #[cfg(unix)]
        {
            let lock = format!("{}-{}", sys::hostname(), std::process::id());
            std::os::unix::fs::symlink(lock, path.join("SingletonLock")).unwrap();
            assert!(claim(&path, Family::Chromium, "sbx:qut").unwrap().busy);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forks() {
        let dir = util::mkdtemp(&std::env::temp_dir(), "rembro-test-").unwrap();
        let src = dir.join("src");
        let open = claim(&src, Family::Chromium, "sbx:qut").unwrap();
        fs::create_dir_all(src.join("Default/Cache")).unwrap();
        fs::write(src.join("Default/Cookies"), "cookies").unwrap();
        fs::write(src.join("Default/Cache/data_0"), "cached").unwrap();
        fs::write(src.join("Default/Preferences"), r#"{"profile":{"exit_type":"Crashed","name":"Me"}}"#).unwrap();
        fs::write(src.join("DevToolsActivePort"), "9222\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("host-1", src.join("SingletonLock")).unwrap();
        let month_ago = SystemTime::now() - Duration::from_secs(30 * 86400);
        set_dir_modified(&src, month_ago).unwrap();

        let fork = create_temp(&dir, Some(&src), "sbx:qut").unwrap();
        assert!(!fork.busy && fork.mark.is_some());
        assert!(fork.path.file_name().unwrap().to_string_lossy().starts_with(TEMP_PREFIX));
        assert_eq!(fs::read_to_string(fork.path.join("Default/Cookies")).unwrap(), "cookies");
        assert!(!fork.path.join("Default/Cache").exists());
        assert!(!fork.path.join("DevToolsActivePort").exists());
        assert!(fs::symlink_metadata(fork.path.join("SingletonLock")).is_err());
        let prefs: serde_json::Value =
            serde_json::from_slice(&fs::read(fork.path.join("Default/Preferences")).unwrap()).unwrap();
        assert_eq!(prefs["profile"]["exit_type"], "Normal");
        assert_eq!(prefs["profile"]["name"], "Me");
        // The fork is a session of its own, so the source being open does not make it busy.
        assert!(is_open(&fork.path));
        // Forking counts as using the source.
        assert_eq!(days_between(last_used(&src), SystemTime::now()), 0);
        assert!(create_temp(&dir, Some(&dir.join("missing")), "sbx:qut").is_err());
        drop(open);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn run_profiles() {
        let temp = std::env::temp_dir();
        assert!(is_run_profile(&temp.join("rembro-run-Ab12Cd/profile")));
        assert!(!is_run_profile(&temp.join("rembro-run-Ab12Cd/other")));
        assert!(!is_run_profile(&temp.join("rembro-profile-Ab12Cd")));
        assert!(!is_run_profile(&temp.join("x/rembro-run-Ab12Cd/profile")));

        // `rembro run` ends while a session has the profile open, and that session then ends.
        let run = RunDir::create().unwrap();
        let (dir, profile) = (run.dir.clone(), run.profile());
        assert!(sys::is_private_dir(&dir));
        let session = claim(&profile, Family::Chromium, "sbx:qut").unwrap();
        drop(run);
        assert!(profile.exists());
        drop(Discard(profile.clone()));
        drop(session);
        assert!(!dir.exists());

        // A session ends while `rembro run` goes on, and `rembro run` ends after it.
        let run = RunDir::create().unwrap();
        let (dir, profile) = (run.dir.clone(), run.profile());
        let session = claim(&profile, Family::Chromium, "sbx:qut").unwrap();
        drop(Discard(profile.clone()));
        drop(session);
        assert!(!profile.exists() && dir.exists());
        drop(run);
        assert!(!dir.exists());
    }

    #[test]
    fn pruning() {
        let root = util::mkdtemp(&std::env::temp_dir(), "rembro-test-").unwrap();
        let (dir, temp) = (root.join("profiles"), root.join("tmp"));
        fs::create_dir_all(&temp).unwrap();
        let profile = |name: &str| {
            let p = dir.join(name);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("Local State"), "{}").unwrap();
            mark_created(&p);
            p
        };
        let old = profile("sbx-old/chromium/default");
        let _new = profile("sbx-qut/chromium/default");
        let open = profile("sbx-qut/chromium/work");
        let _session = SessionMark::claim(&open, "sbx:qut").unwrap();
        let copied = dir.join("sbx-qut/firefox/default");
        fs::create_dir_all(copied.join("Default")).unwrap();
        let remote = profile("chromium/default");
        fs::create_dir_all(remote.join("Default")).unwrap();
        // A --remote profile from rembro 0.2, which had no mark.
        let unmarked = dir.join("firefox/default");
        fs::create_dir_all(unmarked.join("storage")).unwrap();
        fs::write(unmarked.join("prefs.js"), "").unwrap();
        fs::create_dir_all(dir.join("sbx-qut/chromium/.gone.rembro-deleting-1")).unwrap();
        let left = create_temp(&temp, None, "sbx:qut").unwrap();
        let left_path = left.path.clone();
        drop(left);
        let running = create_temp(&temp, None, "sbx:qut").unwrap();
        // A `rembro run` that is still going, and one that was killed.
        let run_dir = |held: bool| {
            let d = util::mkdtemp(&temp, RUN_PREFIX).unwrap();
            mark_created(&d);
            let mark = SessionMark::claim(&d, "rembro run").unwrap();
            fs::create_dir(d.join(RUN_PROFILE)).unwrap();
            mark_created(&d.join(RUN_PROFILE));
            (d, held.then_some(mark))
        };
        let (live_run, _live) = run_dir(true);
        let (dead_run, _) = run_dir(false);

        let (kept, left) = find(&dir, &temp);
        let names: Vec<&str> = kept.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names.len(), 6, "{names:?}");
        assert!(kept.iter().any(|f| f.path == remote));
        assert!(kept.iter().any(|f| f.path == unmarked && !f.ours));
        assert!(kept.iter().any(|f| f.path == copied && !f.ours));
        assert!(kept.iter().any(|f| f.path == open && f.open));
        let mut left_paths: Vec<PathBuf> = left.iter().map(|f| f.path.clone()).collect();
        left_paths.sort();
        let mut expected = vec![left_path.clone(), dead_run.clone()];
        expected.sort();
        assert_eq!(left_paths, expected);

        // As if a month has gone by: all but the open and the unmarked profiles go.
        let later = SystemTime::now() + Duration::from_secs(30 * 86400);
        let removed: Vec<PathBuf> = prune(&dir, &kept, &left, 30, later).iter().map(|f| f.path.clone()).collect();
        assert_eq!(removed.len(), 5, "{removed:?}");
        assert!(!old.exists() && !dir.join("sbx-old").exists() && !remote.exists());
        assert!(open.exists() && copied.exists() && unmarked.exists() && running.path.exists());
        assert!(!left_path.exists() && !dead_run.exists() && live_run.exists());
        drop(running);
        fs::remove_dir_all(root).unwrap();
    }
}
