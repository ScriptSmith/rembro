use std::cmp::Ordering;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Makes `cmd` start with no signals blocked. rembro blocks the signals it waits for, and
/// children would otherwise inherit that, leaving them deaf to SIGTERM and Ctrl-C.
pub fn unblock_signals(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
}

/// Quotes `s` for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Splits a command line into words the way a shell would, without expansions.
pub fn split_words(s: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("unterminated ' quote".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => cur.push(c),
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => return Err("unterminated \" quote".into()),
                        },
                        Some(c) => cur.push(c),
                        None => return Err("unterminated \" quote".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(c) = chars.next() {
                    cur.push(c);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Compares strings so that runs of digits order by value: chromium-999 < chromium-1200.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let nb = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (da, db) = (trim_zeros(&a[..na]), trim_zeros(&b[..nb]));
                let ord = da.len().cmp(&db.len()).then_with(|| da.cmp(db));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[na..];
                b = &b[nb..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|&&c| c == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
}

/// Matches `name` against a pattern where `*` stands for any run of characters.
pub fn wildcard(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.len() >= part.len() && rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Expands `*` in the components of an absolute path, newest version first.
pub fn glob(pattern: &Path) -> Vec<PathBuf> {
    let mut found = vec![PathBuf::from("/")];
    for comp in pattern.components().skip(1) {
        let comp = comp.as_os_str().to_string_lossy();
        let mut next = Vec::new();
        for dir in &found {
            if comp.contains('*') {
                let Ok(entries) = fs::read_dir(dir) else { continue };
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| wildcard(&comp, n))
                    .collect();
                names.sort_by(|a, b| natural_cmp(b, a));
                next.extend(names.into_iter().map(|n| dir.join(n)));
            } else {
                let p = dir.join(&*comp);
                if p.symlink_metadata().is_ok() {
                    next.push(p);
                }
            }
        }
        found = next;
    }
    found
}

pub fn expand_home(p: &str, home: Option<&str>) -> PathBuf {
    match (p.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => Path::new(h).join(rest),
        _ if p == "~" && home.is_some() => PathBuf::from(home.unwrap()),
        _ => PathBuf::from(p),
    }
}

pub fn home() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty())
}

pub fn is_executable(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Ubuntu installs shell scripts that only say to install a snap, which never runs in a container.
fn is_snap_stub(p: &Path) -> bool {
    let Ok(head) = fs::read(p).map(|b| b.into_iter().take(1024).collect::<Vec<_>>()) else { return false };
    if !head.starts_with(b"#!") {
        return false;
    }
    let text = String::from_utf8_lossy(&head);
    let Some(name) = p.file_name() else { return false };
    text.contains("/snap/bin/") && !is_executable(&Path::new("/snap/bin").join(name))
}

/// Finds an executable: a command name looked up in PATH, or a path that may contain `*` and `~`.
pub fn find_executable(candidate: &str, home: Option<&str>) -> Option<PathBuf> {
    if candidate.contains('/') {
        let path = expand_home(candidate, home);
        let paths = if candidate.contains('*') { glob(&path) } else { vec![path] };
        return paths.into_iter().find(|p| is_executable(p) && !is_snap_stub(p));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(candidate)).find(|p| is_executable(p) && !is_snap_stub(p))
}

/// A listening socket from /proc/net/tcp or tcp6.
pub struct Listener {
    pub addr: IpAddr,
    pub port: u16,
}

fn parse_proc_addr(s: &str) -> Option<SocketAddr> {
    let (ip, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(u32::from_str_radix(ip, 16).ok()?.to_ne_bytes())),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                let w = u32::from_str_radix(&ip[i * 8..i * 8 + 8], 16).ok()?;
                b[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(b))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Rows of /proc/net/tcp and tcp6 as (local, remote, state, uid).
fn proc_tcp() -> Vec<(SocketAddr, SocketAddr, u8, u32)> {
    let mut rows = Vec::new();
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = fs::read_to_string(file) else { continue };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 8 {
                continue;
            }
            let (Some(l), Some(r), Ok(st), Ok(uid)) =
                (parse_proc_addr(f[1]), parse_proc_addr(f[2]), u8::from_str_radix(f[3], 16), f[7].parse())
            else {
                continue;
            };
            rows.push((l, r, st, uid));
        }
    }
    rows
}

pub fn listening() -> Vec<Listener> {
    proc_tcp().into_iter().filter(|r| r.2 == 0x0A).map(|(l, ..)| Listener { addr: l.ip(), port: l.port() }).collect()
}

/// The owner of the local TCP socket connected from `peer` to `local`, if /proc shows it.
pub fn tcp_peer_uid(local: SocketAddr, peer: SocketAddr) -> Option<u32> {
    proc_tcp().into_iter().find(|r| r.0 == peer && r.1 == local).map(|r| r.3)
}

pub fn is_loopback_or_any(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// A set of port ranges, written like `3000-3999,41xx,8080`, `all` or `none`.
#[derive(Clone, Debug)]
pub struct PortSpec(Vec<(u16, u16)>);

impl PortSpec {
    pub fn parse(s: &str) -> Result<PortSpec, String> {
        let s = s.trim();
        if s == "none" || s.is_empty() {
            return Ok(PortSpec(Vec::new()));
        }
        if s == "all" {
            return Ok(PortSpec(vec![(1, 65535)]));
        }
        let mut ranges = Vec::new();
        for part in s.split(',').map(str::trim) {
            let bad = || format!("bad port range {part:?}; use forms like 8080, 3000-3999 or 41xx");
            let range = if let Some((a, b)) = part.split_once('-') {
                (a.trim().parse().map_err(|_| bad())?, b.trim().parse().map_err(|_| bad())?)
            } else if part.contains(['x', 'X']) {
                if !part.chars().all(|c| c.is_ascii_digit() || c == 'x' || c == 'X') {
                    return Err(bad());
                }
                let lo = part.replace(['x', 'X'], "0").parse().map_err(|_| bad())?;
                let hi = part.replace(['x', 'X'], "9").parse().map_err(|_| bad())?;
                (lo, hi)
            } else {
                let p = part.parse().map_err(|_| bad())?;
                (p, p)
            };
            if range.0 == 0 || range.0 > range.1 {
                return Err(bad());
            }
            ranges.push(range);
        }
        Ok(PortSpec(ranges))
    }

    pub fn contains(&self, port: u16) -> bool {
        self.0.iter().any(|&(lo, hi)| (lo..=hi).contains(&port))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Percent-encodes a path for a file:// URL.
pub fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

pub fn data_dir() -> PathBuf {
    match std::env::var("XDG_DATA_HOME") {
        Ok(d) if d.starts_with('/') => PathBuf::from(d),
        _ => expand_home("~/.local/share", home().as_deref()),
    }
}

pub fn mkdtemp(base: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let template = base.join(format!("{prefix}XXXXXX"));
    let mut bytes = template.into_os_string().into_vec();
    bytes.push(0);
    let p = unsafe { libc::mkdtemp(bytes.as_mut_ptr() as *mut libc::c_char) };
    if p.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    bytes.pop();
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// A private directory for sockets and other session files.
pub fn runtime_base() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR")
        && d.starts_with('/')
        && fs::metadata(&d).is_ok_and(|m| m.is_dir())
        && unsafe { libc::access(std::ffi::CString::new(d.clone()).unwrap().as_ptr(), libc::W_OK) } == 0
    {
        return PathBuf::from(d);
    }
    std::env::temp_dir()
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    if unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } != 0 {
        return String::new();
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

pub fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Makes a target or browser name safe to use as a directory name.
pub fn slug(s: &str) -> String {
    let out: String =
        s.chars().map(|c| if c.is_ascii_alphanumeric() || "._@-".contains(c) { c } else { '-' }).collect();
    let out = out.trim_matches(['-', '.']).to_string();
    if out.is_empty() { "target".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("abc"), "abc");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn words() {
        assert_eq!(
            split_words(r#"docker compose exec -T 'my app' "x y" a\ b"#).unwrap(),
            ["docker", "compose", "exec", "-T", "my app", "x y", "a b"]
        );
        assert!(split_words("'oops").is_err());
    }

    #[test]
    fn natural() {
        let mut v = vec!["chromium-1200", "chromium-999", "chromium-1134"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["chromium-999", "chromium-1134", "chromium-1200"]);
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("chromium-*", "chromium-1200"));
        assert!(wildcard("chrome-linux*", "chrome-linux64"));
        assert!(!wildcard("chromium-*", "chromium_headless_shell-1"));
        assert!(wildcard("*-x", "a-x"));
        assert!(wildcard("a*b*c", "aXbYc"));
        assert!(!wildcard("a*b*c", "aXbY"));
    }

    #[test]
    fn ports() {
        let p = PortSpec::parse("41xx,51xx,8xxx,3000-3999").unwrap();
        assert!(p.contains(4173) && p.contains(5173) && p.contains(8080) && p.contains(3000));
        assert!(!p.contains(5432) && !p.contains(9222));
        assert!(PortSpec::parse("none").unwrap().is_empty());
        assert!(PortSpec::parse("0").is_err());
        assert!(PortSpec::parse("9-1").is_err());
    }

    #[test]
    fn proc_addrs() {
        assert_eq!(parse_proc_addr("0100007F:1F90").unwrap(), "127.0.0.1:8080".parse().unwrap());
        assert_eq!(parse_proc_addr("00000000000000000000000001000000:0050").unwrap(), "[::1]:80".parse().unwrap());
    }

    #[test]
    fn b64() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn urls() {
        assert_eq!(file_url("/tmp/a b/x.html"), "file:///tmp/a%20b/x.html");
    }
}
