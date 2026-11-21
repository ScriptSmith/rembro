//! Keeping rembro's changes out of browser profiles that it did not create, and telling
//! its own running sessions apart from browsers started some other way.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::browser::Family;
use crate::{certs, util};

/// Present in profiles that rembro created.
const OWNER: &str = "rembro-profile";
/// While a rembro session has the profile open: rembro's PID and the target.
const SESSION: &str = "rembro-session";
/// Settings of a Firefox profile that rembro did not create, to put back after a session.
const RESTORE: &str = "rembro-restore.json";

pub fn mark_created(profile: &Path) {
    let _ = fs::write(
        profile.join(OWNER),
        "rembro created this browser profile and may change its settings each time it starts the browser.\n",
    );
}

pub fn created_by_rembro(profile: &Path) -> bool {
    profile.join(OWNER).exists() || profile.starts_with(util::data_dir().join("rembro/profiles"))
}

/// The target of the running rembro session that has this profile open, if there is one.
pub fn session_target(profile: &Path) -> Option<String> {
    let text = fs::read_to_string(profile.join(SESSION)).ok()?;
    let (pid, target) = text.split_once('\n')?;
    let cmdline = fs::read(format!("/proc/{}/cmdline", pid.trim())).ok()?;
    String::from_utf8_lossy(&cmdline).contains("rembro").then(|| target.trim().to_string())
}

/// Removes the mark of a rembro session that ended without removing it.
pub fn clear_stale_session(profile: &Path) {
    if profile.join(SESSION).exists() && session_target(profile).is_none() {
        let _ = fs::remove_file(profile.join(SESSION));
    }
}

/// Records that this rembro session has the profile open, until dropped.
pub struct SessionMark(PathBuf);

impl SessionMark {
    pub fn new(profile: &Path, target: &str) -> SessionMark {
        let path = profile.join(SESSION);
        let _ = fs::write(&path, format!("{}\n{target}\n", std::process::id()));
        SessionMark(path)
    }
}

impl Drop for SessionMark {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
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

    #[test]
    fn sessions() {
        let dir = util::mkdtemp(&std::env::temp_dir(), "rembro-test-").unwrap();
        assert_eq!(session_target(&dir), None);
        {
            let _mark = SessionMark::new(&dir, "sbx:qut");
            // This test binary's name contains "rembro".
            assert_eq!(session_target(&dir).as_deref(), Some("sbx:qut"));
        }
        assert_eq!(session_target(&dir), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
