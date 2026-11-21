//! Binaries for other architectures: rembro agents and waypipe, found next to this
//! executable, in the cache, or downloaded from this version's GitHub release.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::util;

const RELEASES: &str = "https://github.com/ScriptSmith/rembro/releases/download";

/// The release name for a `uname -m` architecture, if rembro publishes builds for it.
pub fn release_arch(uname: &str) -> Option<&'static str> {
    Some(match uname {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        "armv7l" | "armv7" | "armv8l" => "armv7",
        _ => return None,
    })
}

fn cache_dir() -> PathBuf {
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(d) if d.starts_with('/') => PathBuf::from(d),
        _ => util::expand_home("~/.cache", util::home().as_deref()),
    };
    base.join("rembro").join(env!("CARGO_PKG_VERSION"))
}

/// Finds `NAME-ARCH` (for example `waypipe-aarch64`) for the target's `uname -m`.
pub fn tool(name: &str, uname: &str, verbose: bool) -> Result<PathBuf, String> {
    let arch = release_arch(uname).ok_or_else(|| format!("rembro has no {name} build for {uname}"))?;
    let file = format!("{name}-{arch}");
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        let p = dir.join(&file);
        if p.is_file() {
            return Ok(p);
        }
    }
    let cached = cache_dir().join(&file);
    if cached.is_file() {
        return Ok(cached);
    }
    download(&file, &cached, verbose)?;
    Ok(cached)
}

fn download(file: &str, dest: &Path, verbose: bool) -> Result<(), String> {
    let base = std::env::var("REMBRO_RELEASES").unwrap_or_else(|_| RELEASES.into());
    let base = format!("{}/v{}", base.trim_end_matches('/'), env!("CARGO_PKG_VERSION"));
    let dir = dest.parent().unwrap();
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    eprintln!("rembro: downloading {file} from {base}");
    let sums = String::from_utf8(get(&format!("{base}/SHA256SUMS"), verbose)?).map_err(|_| "SHA256SUMS is not text")?;
    let want = sums
        .lines()
        .find_map(|l| {
            let (hash, name) = l.split_once(char::is_whitespace)?;
            (name.trim().trim_start_matches('*') == file).then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| format!("{file} is not in the release's SHA256SUMS"))?;
    let data = get(&format!("{base}/{file}"), verbose)?;
    let got = hex(&sha256(&data));
    if got != want {
        return Err(format!("{file} does not match its checksum (expected {want}, got {got})"));
    }
    let tmp = dir.join(format!(".{file}.{}", std::process::id()));
    fs::write(&tmp, &data)
        .and_then(|_| {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))
        })
        .and_then(|_| fs::rename(&tmp, dest))
        .map_err(|e| format!("cannot write {}: {e}", dest.display()))
}

/// Fetches a URL with curl or wget.
fn get(url: &str, verbose: bool) -> Result<Vec<u8>, String> {
    let attempts: [(&str, &[&str]); 2] = [("curl", &["-fsSL", "--retry", "2"]), ("wget", &["-qO-"])];
    for (tool, args) in attempts {
        if util::find_executable(tool, None).is_none() {
            continue;
        }
        if verbose {
            eprintln!("rembro: {tool} {url}");
        }
        let out = Command::new(tool)
            .args(args)
            .arg(url)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("{tool}: {e}"))?;
        return if out.status.success() { Ok(out.stdout) } else { Err(format!("could not download {url}")) };
    }
    Err("downloading needs curl or wget".into())
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(&vec![b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn arches() {
        assert_eq!(release_arch("armv7l"), Some("armv7"));
        assert_eq!(release_arch("arm64"), Some("aarch64"));
        assert_eq!(release_arch("armv6l"), None);
    }
}
