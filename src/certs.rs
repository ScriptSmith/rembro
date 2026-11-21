//! Makes one browser profile trust extra CA certificates, such as the CA of a target's TLS-intercepting proxy or a
//! company's private CA, without changing the user's own trust: not ~/.pki/nssdb, not /etc and not other profiles.
//!
//! Firefox: the profile's pkcs11.txt loads p11-kit's trust module with `paths=` set to a PEM bundle in the profile,
//! and NSS takes the bundle's certificates as trust anchors. Where p11-kit-trust.so is missing, the certificates go
//! into the profile's cert9.db with NSS's certutil instead.
//!
//! Chromium: the certificates go into the profile's `ServerCertificate` database, where chrome://certificate-manager
//! keeps the certificates that the user installs ("Installed by you"). Chromium reads it for that profile only. The
//! database is SQLite, so this module carries a small SQLite reader and writer.
//!
//! Call these functions only while the browser does not have the profile open.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::fetch::{hex, sha256};
use crate::util::base64;

const FIREFOX_BUNDLE: &str = "rembro-ca.pem";
const FIREFOX_MODULE: &str = "rembro CA certificates";
/// Present while certificates that rembro added with certutil may be in the profile's cert9.db.
const CERTUTIL_MARKER: &str = "rembro-certutil";
const CERTUTIL_PREFIX: &str = "rembro-";

const CHROMIUM_DB: &str = "Default/ServerCertificate";
/// The SHA-256 hashes (hex) of the rows that rembro put in the ServerCertificate database.
const CHROMIUM_STATE: &str = "rembro-certificates";
/// A serialized CertificateMetadata with trust CERTIFICATE_TRUST_TYPE_TRUSTED, as chrome://certificate-manager
/// writes it for a certificate imported under "Trusted Certificates".
const CHROMIUM_TRUSTED: &[u8] = &[0x0a, 0x02, 0x08, 0x03];
const CHROMIUM_META_SQL: &str = "CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR)";
const CHROMIUM_CERTS_SQL: &str = "CREATE TABLE certificates(sha256hash_hex TEXT PRIMARY KEY,der_cert BLOB NOT NULL,\
trust_settings BLOB NOT NULL)";

/// Decodes the certificates in PEM text: `CERTIFICATE`, `X509 CERTIFICATE` and OpenSSL's `TRUSTED CERTIFICATE`
/// blocks. Other blocks, such as keys, are skipped, and so is any text between blocks.
pub fn parse_pem(text: &str) -> Vec<Vec<u8>> {
    let mut certs = Vec::new();
    let mut block: Option<(&str, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(label) = line.strip_prefix("-----BEGIN ").and_then(|l| l.strip_suffix("-----")) {
            block = Some((label, String::new()));
        } else if let Some(label) = line.strip_prefix("-----END ").and_then(|l| l.strip_suffix("-----")) {
            if let Some((begin, body)) = block.take()
                && begin == label
                && matches!(label, "CERTIFICATE" | "X509 CERTIFICATE" | "TRUSTED CERTIFICATE")
                && let Some(der) = base64_decode(&body)
                && let Some(len) = der_len(&der)
                // OpenSSL puts its trust settings after a trusted certificate; browsers have no use for them.
                && (len == der.len() || label == "TRUSTED CERTIFICATE")
            {
                certs.push(der[..len].to_vec());
            }
        } else if let Some((_, body)) = &mut block
            && !line.contains(':')
        {
            body.push_str(line);
        }
    }
    certs
}

/// Encodes a DER certificate as PEM.
pub fn to_pem(der: &[u8]) -> String {
    let b64 = base64(der);
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// The length of the DER SEQUENCE at the start of `der`, header included.
fn der_len(der: &[u8]) -> Option<usize> {
    if der.len() < 2 || der[0] != 0x30 {
        return None;
    }
    let (header, len) = match der[1] {
        n if n < 0x80 => (2, n as usize),
        n @ 0x81..=0x84 => {
            let k = (n & 0x7f) as usize;
            (2 + k, der.get(2..2 + k)?.iter().fold(0usize, |a, &b| a << 8 | b as usize))
        }
        _ => return None,
    };
    let total = header.checked_add(len)?;
    (total <= der.len()).then_some(total)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\t' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn unique(certs: &[Vec<u8>]) -> Vec<&[u8]> {
    let mut seen = BTreeSet::new();
    certs.iter().map(Vec::as_slice).filter(|c| seen.insert(*c)).collect()
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = sibling(path, ".rembro-new");
    let write = || -> io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot write {}: {e}", path.display())
    })
}

fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(format!("cannot remove {}: {e}", path.display())),
        _ => Ok(()),
    }
}

/// Whether rembro added certificates to this profile on an earlier launch.
pub fn has_rembro_trust(profile: &Path) -> bool {
    [CHROMIUM_STATE, FIREFOX_BUNDLE, CERTUTIL_MARKER].iter().any(|f| profile.join(f).exists())
}

/// Finds p11-kit's trust module, which NSS can load to read anchors from PEM files. Only
/// Linux distributions build Firefox's NSS to go with it.
pub fn find_p11kit_trust() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    [
        "/usr/lib64/pkcs11/p11-kit-trust.so".to_string(),
        format!("/usr/lib/{}-linux-gnu/pkcs11/p11-kit-trust.so", std::env::consts::ARCH),
        "/usr/lib/pkcs11/p11-kit-trust.so".to_string(),
        "/usr/local/lib/pkcs11/p11-kit-trust.so".to_string(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Makes the Firefox profile in `profile` trust `certs` (DER), and stops it trusting certificates that rembro added
/// on earlier launches. An empty `certs` removes what rembro added. An error leaves the profile usable, so the
/// caller can print it as a warning and start the browser.
pub fn apply_firefox(profile: &Path, certs: &[Vec<u8>]) -> Result<(), String> {
    // Windows has a certutil of its own, which is not NSS's, and no p11-kit.
    if cfg!(windows) && !certs.is_empty() {
        return Err("on Windows, rembro can add CA certificates only to Chromium profiles".into());
    }
    apply_firefox_with(profile, certs, find_p11kit_trust().as_deref(), "certutil")
}

fn apply_firefox_with(profile: &Path, certs: &[Vec<u8>], module: Option<&Path>, certutil: &str) -> Result<(), String> {
    let certs = unique(certs);
    let bundle = profile.join(FIREFOX_BUNDLE);
    let marker = profile.join(CERTUTIL_MARKER);
    // The path goes in single quotes, so it must not hold characters that would end or escape them.
    let quotable = bundle.to_str().filter(|b| !b.contains(['\'', '"', '\\', '\n']));
    if let (Some(module), Some(bundle_str)) = (module, quotable) {
        if certs.is_empty() {
            set_pkcs11_module(profile, None)?;
            remove(&bundle)?;
        } else {
            let pem: String = certs.iter().map(|c| to_pem(c)).collect();
            write_atomic(&bundle, pem.as_bytes())?;
            let stanza = format!(
                "library={}\nname={FIREFOX_MODULE}\nparameters=paths='{bundle_str}'\nNSS=trustOrder=100\n",
                module.display()
            );
            set_pkcs11_module(profile, Some(&stanza))?;
        }
        if marker.exists() {
            certutil_sync(profile, &[], certutil)
                .map_err(|e| format!("cannot remove the certificates that rembro added with certutil: {e}"))?;
            remove(&marker)?;
        }
        return Ok(());
    }
    set_pkcs11_module(profile, None)?;
    remove(&bundle)?;
    if certs.is_empty() && !marker.exists() {
        return Ok(());
    }
    certutil_sync(profile, &certs, certutil).map_err(|e| match module {
        None => format!("p11-kit's trust module (p11-kit-trust.so) is not installed, and {e}"),
        Some(_) => e,
    })?;
    if certs.is_empty() { remove(&marker) } else { write_atomic(&marker, b"") }
}

/// Puts `stanza` in the profile's pkcs11.txt as rembro's module, or takes rembro's module out.
fn set_pkcs11_module(profile: &Path, stanza: Option<&str>) -> Result<(), String> {
    let path = profile.join("pkcs11.txt");
    let old = match fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let new = edit_pkcs11(old.as_deref().unwrap_or(""), stanza);
    match old {
        Some(old) if old == new => Ok(()),
        // NSS writes a new pkcs11.txt with its own module when it finds none.
        Some(_) if new.is_empty() => remove(&path),
        None if new.is_empty() => Ok(()),
        _ => write_atomic(&path, new.as_bytes()),
    }
}

/// Replaces rembro's module in the text of an NSS pkcs11.txt, where each module is a group of `key=value` lines
/// followed by a blank line, and keeps the other modules as they are.
fn edit_pkcs11(text: &str, stanza: Option<&str>) -> String {
    let ours = format!("name={FIREFOX_MODULE}");
    let mut out = String::new();
    let mut group: Vec<&str> = Vec::new();
    for line in text.lines().chain([""]) {
        if !line.trim().is_empty() {
            group.push(line);
            continue;
        }
        if !group.is_empty() && !group.iter().any(|l| l.trim_end() == ours) {
            for l in &group {
                out.push_str(l);
                out.push('\n');
            }
            out.push('\n');
        }
        group.clear();
    }
    if let Some(s) = stanza {
        out.push_str(s);
        out.push('\n');
    }
    out
}

fn run(cmd: &str, args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => format!(
                "{cmd} is not installed (it comes with NSS's tools: nss on Arch and Homebrew, libnss3-tools on \
                 Debian and Ubuntu, nss-tools on Fedora)"
            ),
            _ => format!("cannot run {cmd}: {e}"),
        })?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A failed write shows up as certutil's own error below.
        let _ = pipe.write_all(data);
    }
    let out = child.wait_with_output().map_err(|e| format!("{cmd}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{cmd} {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn nickname(der: &[u8]) -> String {
    format!("{CERTUTIL_PREFIX}{}", &hex(&sha256(der))[..16])
}

/// Makes the certificates that rembro keeps in the profile's cert9.db exactly `certs`.
fn certutil_sync(profile: &Path, certs: &[&[u8]], certutil: &str) -> Result<(), String> {
    let db = format!("sql:{}", profile.display());
    if !profile.join("cert9.db").exists() {
        if certs.is_empty() {
            return Ok(());
        }
        run(certutil, &["-N", "-d", &db, "--empty-password"], None)?;
    }
    let listing = run(certutil, &["-L", "-d", &db], None)?;
    let present: BTreeSet<&str> = listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| n.starts_with(CERTUTIL_PREFIX))
        .collect();
    let wanted: BTreeMap<String, &[u8]> = certs.iter().map(|c| (nickname(c), *c)).collect();
    // Deleting first lets a renewed CA with the same subject take the place of the old one.
    for nick in present.iter().filter(|n| !wanted.contains_key(**n)) {
        run(certutil, &["-D", "-d", &db, "-n", nick], None)?;
    }
    for (nick, der) in wanted.iter().filter(|(n, _)| !present.contains(n.as_str())) {
        run(certutil, &["-A", "-d", &db, "-n", nick, "-t", "C,,"], Some(der))?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
struct CertRow {
    hash: String,
    der: Vec<u8>,
    trust: Vec<u8>,
}

/// Makes the Chromium profile `Default` in `user_data_dir` trust `certs` (DER), and stops it trusting certificates
/// that rembro added on earlier launches. Certificates that the user installed in chrome://certificate-manager stay.
/// An empty `certs` removes what rembro added. An error leaves the profile usable, so the caller can print it as a
/// warning and start the browser.
pub fn apply_chromium(user_data_dir: &Path, certs: &[Vec<u8>]) -> Result<(), String> {
    let certs = unique(certs);
    let db_path = user_data_dir.join(CHROMIUM_DB);
    let state_path = user_data_dir.join(CHROMIUM_STATE);
    let added_before: BTreeSet<String> =
        fs::read_to_string(&state_path).unwrap_or_default().lines().map(str::to_owned).collect();
    let existing = match fs::read(&db_path) {
        Ok(data) => {
            for suffix in ["-journal", "-wal"] {
                if fs::metadata(sibling(&db_path, suffix)).is_ok_and(|m| m.len() > 0) {
                    return Err(format!(
                        "{} has an unfinished transaction; open and close this profile in the browser once",
                        db_path.display()
                    ));
                }
            }
            Some(read_server_certs(&data).map_err(|e| format!("cannot read {}: {e}", db_path.display()))?)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", db_path.display())),
    };
    if existing.is_none() && certs.is_empty() {
        return remove(&state_path);
    }
    let existing = existing.unwrap_or_default();
    let mut rows: Vec<CertRow> = existing.iter().filter(|r| !added_before.contains(&r.hash)).cloned().collect();
    let mut added = BTreeSet::new();
    for der in certs {
        let hash = hex(&sha256(der));
        // The user installed this one; leave its trust as they set it.
        if rows.iter().any(|r| r.hash == hash) {
            continue;
        }
        rows.push(CertRow { hash: hash.clone(), der: der.to_vec(), trust: CHROMIUM_TRUSTED.to_vec() });
        added.insert(hash);
    }
    let unchanged = db_path.exists() && rows.len() == existing.len() && rows.iter().all(|r| existing.contains(r));
    if !unchanged {
        if let Some(dir) = db_path.parent() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        write_atomic(&db_path, &server_cert_db(&rows)?)?;
    }
    if added == added_before {
        Ok(())
    } else if added.is_empty() {
        remove(&state_path)
    } else {
        write_atomic(&state_path, added.iter().map(|h| format!("{h}\n")).collect::<String>().as_bytes())
    }
}

fn read_server_certs(data: &[u8]) -> Result<Vec<CertRow>, String> {
    use sqlite::Value::{Blob, Int, Text};
    let db = sqlite::Db::open(data)?;
    let (_, meta) = db.table("meta")?.ok_or("it has no meta table")?;
    let version = meta.iter().find(|r| r.first() == Some(&Text("version".into()))).and_then(|r| r.get(1));
    // Rewriting a newer version would downgrade it.
    if !matches!(version, Some(Text(v)) if v == "1") && version != Some(&Int(1)) {
        return Err(format!("its version is {version:?}, and rembro only knows version 1"));
    }
    let (sql, rows) = db.table("certificates")?.ok_or("it has no certificates table")?;
    if sql != CHROMIUM_CERTS_SQL {
        return Err(format!("its certificates table is not the one rembro knows: {sql}"));
    }
    rows.into_iter()
        .map(|r| match r.as_slice() {
            [Text(hash), Blob(der), Blob(trust)] => {
                Ok(CertRow { hash: hash.clone(), der: der.clone(), trust: trust.clone() })
            }
            _ => Err(format!("unexpected row {r:?}")),
        })
        .collect()
}

fn server_cert_db(rows: &[CertRow]) -> Result<Vec<u8>, String> {
    use sqlite::Value::{Blob, Text};
    let meta = [("version", "1"), ("last_compatible_version", "1")];
    sqlite::write(
        4096,
        &[
            sqlite::Table {
                name: "meta",
                sql: CHROMIUM_META_SQL,
                index_on: Some(0),
                rows: meta.iter().map(|(k, v)| vec![Text(k.to_string()), Text(v.to_string())]).collect(),
            },
            sqlite::Table {
                name: "certificates",
                sql: CHROMIUM_CERTS_SQL,
                index_on: Some(0),
                rows: rows
                    .iter()
                    .map(|r| vec![Text(r.hash.clone()), Blob(r.der.clone()), Blob(r.trust.clone())])
                    .collect(),
            },
        ],
    )
}

/// Just enough of the SQLite file format (https://www.sqlite.org/fileformat2.html) to read the rows of a table and
/// to write a new database of rowid tables, each with at most one single-column unique index.
mod sqlite {
    use std::cmp::Ordering;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Value {
        Null,
        Int(i64),
        Real(f64),
        Text(String),
        Blob(Vec<u8>),
    }

    pub type Rows = Vec<Vec<Value>>;

    pub struct Table<'a> {
        pub name: &'a str,
        pub sql: &'a str,
        /// The column of an automatic index, as `PRIMARY KEY` or `UNIQUE` on a column other than INTEGER creates.
        pub index_on: Option<usize>,
        pub rows: Rows,
    }

    const TABLE_LEAF: u8 = 0x0d;
    const TABLE_INTERIOR: u8 = 0x05;
    const INDEX_LEAF: u8 = 0x0a;
    const INDEX_INTERIOR: u8 = 0x02;

    pub(super) fn put_varint(out: &mut Vec<u8>, v: u64) {
        if v >> 56 != 0 {
            let mut buf = [0u8; 9];
            buf[8] = v as u8;
            let mut x = v >> 8;
            for b in buf[..8].iter_mut().rev() {
                *b = (x as u8 & 0x7f) | 0x80;
                x >>= 7;
            }
            out.extend_from_slice(&buf);
            return;
        }
        let mut buf = Vec::with_capacity(9);
        let mut x = v;
        loop {
            buf.push((x as u8 & 0x7f) | 0x80);
            x >>= 7;
            if x == 0 {
                break;
            }
        }
        buf[0] &= 0x7f;
        out.extend(buf.iter().rev());
    }

    pub(super) fn get_varint(b: &[u8]) -> Result<(u64, usize), String> {
        let mut v = 0u64;
        for i in 0..9 {
            let byte = *b.get(i).ok_or("truncated varint")?;
            if i == 8 {
                return Ok((v << 8 | byte as u64, 9));
            }
            v = v << 7 | (byte & 0x7f) as u64;
            if byte & 0x80 == 0 {
                return Ok((v, i + 1));
            }
        }
        unreachable!()
    }

    fn varint_len(v: u64) -> usize {
        let mut out = Vec::new();
        put_varint(&mut out, v);
        out.len()
    }

    pub fn record(values: &[Value]) -> Vec<u8> {
        let mut types = Vec::new();
        let mut body = Vec::new();
        for v in values {
            match v {
                Value::Null => types.push(0),
                Value::Int(0) => types.push(8),
                Value::Int(1) => types.push(9),
                Value::Int(i) => {
                    let (t, n) = match *i {
                        -0x80..=0x7f => (1, 1),
                        -0x8000..=0x7fff => (2, 2),
                        -0x80_0000..=0x7f_ffff => (3, 3),
                        -0x8000_0000..=0x7fff_ffff => (4, 4),
                        -0x8000_0000_0000..=0x7fff_ffff_ffff => (5, 6),
                        _ => (6, 8),
                    };
                    types.push(t);
                    body.extend_from_slice(&i.to_be_bytes()[8 - n..]);
                }
                Value::Real(f) => {
                    types.push(7);
                    body.extend_from_slice(&f.to_bits().to_be_bytes());
                }
                Value::Text(s) => {
                    types.push(13 + 2 * s.len() as u64);
                    body.extend_from_slice(s.as_bytes());
                }
                Value::Blob(b) => {
                    types.push(12 + 2 * b.len() as u64);
                    body.extend_from_slice(b);
                }
            }
        }
        let types_len: usize = types.iter().map(|t| varint_len(*t)).sum();
        let mut header_len = types_len + 1;
        while types_len + varint_len(header_len as u64) != header_len {
            header_len = types_len + varint_len(header_len as u64);
        }
        let mut out = Vec::with_capacity(header_len + body.len());
        put_varint(&mut out, header_len as u64);
        for t in types {
            put_varint(&mut out, t);
        }
        out.extend(body);
        out
    }

    pub fn parse_record(payload: &[u8]) -> Result<Vec<Value>, String> {
        let (header_len, mut n) = get_varint(payload)?;
        let header_len = usize::try_from(header_len).map_err(|e| e.to_string())?;
        let header = payload.get(..header_len).ok_or("record header overruns the record")?;
        let mut pos = header_len;
        let mut values = Vec::new();
        while n < header_len {
            let (t, k) = get_varint(&header[n..])?;
            n += k;
            let len = match t {
                0 | 8 | 9 => 0,
                1..=4 => t as usize,
                5 => 6,
                6 | 7 => 8,
                t if t >= 12 => ((t - 12) / 2) as usize,
                _ => return Err(format!("unknown serial type {t}")),
            };
            let bytes = payload.get(pos..pos.checked_add(len).ok_or("bad length")?).ok_or("record overruns")?;
            pos += len;
            values.push(match t {
                0 => Value::Null,
                8 => Value::Int(0),
                9 => Value::Int(1),
                7 => Value::Real(f64::from_bits(u64::from_be_bytes(bytes.try_into().unwrap()))),
                1..=6 => {
                    let mut buf = [if bytes[0] & 0x80 != 0 { 0xff } else { 0 }; 8];
                    buf[8 - len..].copy_from_slice(bytes);
                    Value::Int(i64::from_be_bytes(buf))
                }
                t if t % 2 == 0 => Value::Blob(bytes.to_vec()),
                _ => Value::Text(String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?),
            });
        }
        Ok(values)
    }

    fn compare(a: &Value, b: &Value) -> Ordering {
        fn rank(v: &Value) -> u8 {
            match v {
                Value::Null => 0,
                Value::Int(_) | Value::Real(_) => 1,
                Value::Text(_) => 2,
                Value::Blob(_) => 3,
            }
        }
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x.cmp(y),
            (Value::Text(x), Value::Text(y)) => x.as_bytes().cmp(y.as_bytes()),
            (Value::Blob(x), Value::Blob(y)) => x.cmp(y),
            _ => rank(a).cmp(&rank(b)),
        }
    }

    /// The largest payload a cell keeps on its own page, and the least it keeps when the rest overflows.
    fn local_limits(usable: usize, table_leaf: bool) -> (usize, usize) {
        let max = if table_leaf { usable - 35 } else { (usable - 12) * 64 / 255 - 23 };
        (max, (usable - 12) * 32 / 255 - 23)
    }

    fn local_len(usable: usize, payload: usize, (max, min): (usize, usize)) -> usize {
        if payload <= max {
            return payload;
        }
        let k = min + (payload - min) % (usable - 4);
        if k <= max { k } else { min }
    }

    struct Writer {
        size: usize,
        pages: Vec<Vec<u8>>,
    }

    impl Writer {
        fn alloc(&mut self) -> u32 {
            self.pages.push(vec![0; self.size]);
            self.pages.len() as u32
        }

        /// The part of a payload that stays in its cell, followed by the first overflow page when there is one.
        fn local_part(&mut self, payload: &[u8], table_leaf: bool) -> Vec<u8> {
            let local = local_len(self.size, payload.len(), local_limits(self.size, table_leaf));
            let mut out = payload[..local].to_vec();
            if local < payload.len() {
                let chunks: Vec<&[u8]> = payload[local..].chunks(self.size - 4).collect();
                out.extend_from_slice(&(self.pages.len() as u32 + 1).to_be_bytes());
                for (i, chunk) in chunks.iter().enumerate() {
                    let no = self.alloc();
                    let next = if i + 1 < chunks.len() { no + 1 } else { 0 };
                    let page = &mut self.pages[no as usize - 1];
                    page[..4].copy_from_slice(&next.to_be_bytes());
                    page[4..4 + chunk.len()].copy_from_slice(chunk);
                }
            }
            out
        }

        fn write_page(&mut self, no: u32, kind: u8, cells: &[Vec<u8>], right: Option<u32>) {
            let h = if no == 1 { 100 } else { 0 };
            let header = if right.is_some() { 12 } else { 8 };
            let page = &mut self.pages[no as usize - 1];
            page[h] = kind;
            page[h + 3..h + 5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
            let mut content = page.len();
            for (i, cell) in cells.iter().enumerate() {
                content -= cell.len();
                page[content..content + cell.len()].copy_from_slice(cell);
                let ptr = h + header + 2 * i;
                page[ptr..ptr + 2].copy_from_slice(&(content as u16).to_be_bytes());
            }
            page[h + 5..h + 7].copy_from_slice(&((content % 65536) as u16).to_be_bytes());
            if let Some(r) = right {
                page[h + 8..h + 12].copy_from_slice(&r.to_be_bytes());
            }
        }

        fn new_page(&mut self, kind: u8, cells: &[Vec<u8>], right: Option<u32>) -> u32 {
            let no = self.alloc();
            self.write_page(no, kind, cells, right);
            no
        }

        /// Splits cells into runs that fit on one page each, in order.
        fn runs(&self, cells: &[Vec<u8>], header: usize) -> Vec<std::ops::Range<usize>> {
            let mut runs = Vec::new();
            let (mut start, mut used) = (0, 0);
            for (i, c) in cells.iter().enumerate() {
                if i > start && used + c.len() + 2 > self.size - header {
                    runs.push(start..i);
                    (start, used) = (i, 0);
                }
                used += c.len() + 2;
            }
            runs.push(start..cells.len());
            runs
        }

        /// Groups the children of a level of interior pages: each group's children but the last become cells, and
        /// its last child becomes the page's right-most pointer. Every group has at least two children.
        fn groups(&self, cells: &[Vec<u8>]) -> Result<Vec<std::ops::RangeInclusive<usize>>, String> {
            let n = cells.len();
            let mut groups = Vec::new();
            let mut start = 0;
            while start < n {
                let mut end = start + 1;
                let mut used = cells[start].len() + 2;
                while end + 1 < n && used + cells[end].len() + 2 <= self.size - 12 {
                    used += cells[end].len() + 2;
                    end += 1;
                }
                if end + 2 == n {
                    // One child would be left over for a page of its own; leave two instead.
                    if end - start < 2 {
                        return Err("page too small".into());
                    }
                    end -= 1;
                }
                groups.push(start..=end.min(n - 1));
                start = end + 1;
            }
            Ok(groups)
        }

        fn table(&mut self, rows: &[Vec<u8>]) -> Result<u32, String> {
            let cells: Vec<Vec<u8>> = rows
                .iter()
                .enumerate()
                .map(|(i, rec)| {
                    let mut c = Vec::new();
                    put_varint(&mut c, rec.len() as u64);
                    put_varint(&mut c, i as u64 + 1);
                    c.extend(self.local_part(rec, true));
                    c
                })
                .collect();
            let mut level: Vec<(u32, u64)> = Vec::new();
            for run in self.runs(&cells, 8) {
                level.push((self.new_page(TABLE_LEAF, &cells[run.clone()], None), run.end as u64));
            }
            while level.len() > 1 {
                let cells: Vec<Vec<u8>> = level
                    .iter()
                    .map(|(page, key)| {
                        let mut c = page.to_be_bytes().to_vec();
                        put_varint(&mut c, *key);
                        c
                    })
                    .collect();
                let mut next = Vec::new();
                for g in self.groups(&cells)? {
                    let (s, e) = (*g.start(), *g.end());
                    next.push((self.new_page(TABLE_INTERIOR, &cells[s..e], Some(level[e].0)), level[e].1));
                }
                level = next;
            }
            Ok(level[0].0)
        }

        fn index(&mut self, keys: &[Vec<u8>]) -> Result<u32, String> {
            let entries: Vec<Vec<u8>> = keys
                .iter()
                .map(|rec| {
                    let mut c = Vec::new();
                    put_varint(&mut c, rec.len() as u64);
                    c.extend(self.local_part(rec, false));
                    c
                })
                .collect();
            // Unlike a table, an index keeps each entry once: the entries between two leaves go up a level.
            let mut leaves: Vec<std::ops::Range<usize>> = Vec::new();
            let mut start = 0;
            while start < entries.len() {
                let run = self.runs(&entries[start..], 8)[0].clone();
                let mut end = start + run.end;
                if end + 1 == entries.len() {
                    // The separator would be the last entry, leaving an empty leaf after it.
                    if end - start < 2 {
                        return Err("page too small".into());
                    }
                    end -= 1;
                }
                leaves.push(start..end);
                start = end + 1;
            }
            if leaves.is_empty() {
                return Ok(self.new_page(INDEX_LEAF, &[], None));
            }
            let mut children: Vec<u32> =
                leaves.iter().map(|r| self.new_page(INDEX_LEAF, &entries[r.clone()], None)).collect();
            let mut seps: Vec<Vec<u8>> = leaves[..leaves.len() - 1].iter().map(|r| entries[r.end].clone()).collect();
            while children.len() > 1 {
                let cells: Vec<Vec<u8>> = children
                    .iter()
                    .zip(seps.iter().chain([&Vec::new()]))
                    .map(|(page, sep)| [&page.to_be_bytes()[..], sep].concat())
                    .collect();
                let (mut next, mut next_seps) = (Vec::new(), Vec::new());
                for g in self.groups(&cells)? {
                    let (s, e) = (*g.start(), *g.end());
                    next.push(self.new_page(INDEX_INTERIOR, &cells[s..e], Some(children[e])));
                    if e + 1 < children.len() {
                        next_seps.push(seps[e].clone());
                    }
                }
                (children, seps) = (next, next_seps);
            }
            Ok(children[0])
        }
    }

    /// Writes a database that holds `tables`, with pages of `page_size` bytes.
    pub fn write(page_size: usize, tables: &[Table]) -> Result<Vec<u8>, String> {
        if !page_size.is_power_of_two() || !(512..=65536).contains(&page_size) {
            return Err(format!("bad page size {page_size}"));
        }
        let mut w = Writer { size: page_size, pages: vec![vec![0; page_size]] };
        let mut schema = Vec::new();
        for t in tables {
            let root = w.table(&t.rows.iter().map(|r| record(r)).collect::<Vec<_>>())?;
            schema.push(vec![
                Value::Text("table".into()),
                Value::Text(t.name.into()),
                Value::Text(t.name.into()),
                Value::Int(root as i64),
                Value::Text(t.sql.into()),
            ]);
            if let Some(col) = t.index_on {
                let mut keys: Vec<(&Value, i64)> =
                    t.rows.iter().enumerate().map(|(i, r)| (&r[col], i as i64 + 1)).collect();
                keys.sort_by(|a, b| compare(a.0, b.0).then(a.1.cmp(&b.1)));
                let records: Vec<Vec<u8>> =
                    keys.iter().map(|(k, id)| record(&[(*k).clone(), Value::Int(*id)])).collect();
                let root = w.index(&records)?;
                schema.push(vec![
                    Value::Text("index".into()),
                    Value::Text(format!("sqlite_autoindex_{}_1", t.name)),
                    Value::Text(t.name.into()),
                    Value::Int(root as i64),
                    Value::Null,
                ]);
            }
        }
        let cells: Vec<Vec<u8>> = schema
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let rec = record(r);
                let mut c = Vec::new();
                put_varint(&mut c, rec.len() as u64);
                put_varint(&mut c, i as u64 + 1);
                c.extend(w.local_part(&rec, true));
                c
            })
            .collect();
        if 100 + 8 + cells.iter().map(|c| c.len() + 2).sum::<usize>() > page_size {
            return Err("the schema does not fit on the first page".into());
        }
        w.write_page(1, TABLE_LEAF, &cells, None);
        let count = w.pages.len() as u32;
        let h = &mut w.pages[0];
        h[..16].copy_from_slice(b"SQLite format 3\0");
        h[16..18].copy_from_slice(&((page_size % 65536) as u16).max(1).to_be_bytes());
        h[18..24].copy_from_slice(&[1, 1, 0, 64, 32, 32]);
        h[24..28].copy_from_slice(&1u32.to_be_bytes()); // change counter
        h[28..32].copy_from_slice(&count.to_be_bytes());
        h[40..44].copy_from_slice(&1u32.to_be_bytes()); // schema cookie
        h[44..48].copy_from_slice(&4u32.to_be_bytes()); // schema format
        h[56..60].copy_from_slice(&1u32.to_be_bytes()); // UTF-8
        h[92..96].copy_from_slice(&1u32.to_be_bytes()); // the change counter that the page count is valid for
        h[96..100].copy_from_slice(&3046000u32.to_be_bytes());
        Ok(w.pages.concat())
    }

    pub struct Db<'a> {
        data: &'a [u8],
        size: usize,
        usable: usize,
    }

    fn be16(b: &[u8], at: usize) -> Result<usize, String> {
        b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]) as usize).ok_or("truncated page".into())
    }

    fn be32(b: &[u8], at: usize) -> Result<u32, String> {
        b.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]])).ok_or("truncated page".into())
    }

    impl<'a> Db<'a> {
        pub fn open(data: &'a [u8]) -> Result<Db<'a>, String> {
            if data.len() < 100 || &data[..16] != b"SQLite format 3\0" {
                return Err("not a SQLite database".into());
            }
            let size = match be16(data, 16)? {
                1 => 65536,
                n => n,
            };
            if !size.is_power_of_two() || size < 512 || !data.len().is_multiple_of(size) {
                return Err("bad page size".into());
            }
            if be32(data, 56)? != 1 {
                return Err("the text encoding is not UTF-8".into());
            }
            Ok(Db { data, size, usable: size - data[20] as usize })
        }

        fn page(&self, no: u32) -> Result<&'a [u8], String> {
            let start = (no as usize).checked_sub(1).ok_or("page 0")? * self.size;
            self.data.get(start..start + self.size).ok_or(format!("page {no} is past the end"))
        }

        /// The `CREATE TABLE` statement and the rows of a table, in rowid order.
        pub fn table(&self, name: &str) -> Result<Option<(String, Rows)>, String> {
            for row in self.rows(1)? {
                if let [Value::Text(kind), Value::Text(n), _, Value::Int(root), Value::Text(sql)] = row.as_slice()
                    && kind == "table"
                    && n == name
                {
                    let root = u32::try_from(*root).map_err(|e| e.to_string())?;
                    return Ok(Some((sql.clone(), self.rows(root)?)));
                }
            }
            Ok(None)
        }

        fn rows(&self, root: u32) -> Result<Rows, String> {
            let mut out = Vec::new();
            self.walk(root, &mut out, 0)?;
            Ok(out)
        }

        fn walk(&self, no: u32, out: &mut Rows, depth: usize) -> Result<(), String> {
            if depth > 20 {
                return Err("the b-tree is too deep".into());
            }
            let page = self.page(no)?;
            let h = if no == 1 { 100 } else { 0 };
            let cells = be16(page, h + 3)?;
            match page[h] {
                TABLE_LEAF => {
                    for i in 0..cells {
                        let at = be16(page, h + 8 + 2 * i)?;
                        let (len, a) = get_varint(page.get(at..).ok_or("bad cell pointer")?)?;
                        let (_rowid, b) = get_varint(page.get(at + a..).ok_or("bad cell")?)?;
                        out.push(parse_record(&self.payload(page, at + a + b, len)?)?);
                    }
                }
                TABLE_INTERIOR => {
                    for i in 0..cells {
                        self.walk(be32(page, be16(page, h + 12 + 2 * i)?)?, out, depth + 1)?;
                    }
                    self.walk(be32(page, h + 8)?, out, depth + 1)?;
                }
                kind => return Err(format!("page {no} has type {kind}, not a table page")),
            }
            Ok(())
        }

        fn payload(&self, page: &[u8], at: usize, len: u64) -> Result<Vec<u8>, String> {
            let len = usize::try_from(len).ok().filter(|l| *l <= self.data.len()).ok_or("bad payload length")?;
            let local = local_len(self.usable, len, local_limits(self.usable, true));
            let mut out = page.get(at..at + local).ok_or("cell overruns its page")?.to_vec();
            if local < len {
                let mut next = be32(page, at + local)?;
                while out.len() < len {
                    if next == 0 || out.len() > self.data.len() {
                        return Err("broken overflow chain".into());
                    }
                    let page = self.page(next)?;
                    let take = (len - out.len()).min(self.usable - 4);
                    out.extend_from_slice(&page[4..4 + take]);
                    next = be32(page, 0)?;
                }
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "-----BEGIN CERTIFICATE-----
MIIBgDCCASagAwIBAgIUOOXnqu0QgNWCNO41qsdvqoNvrogwCgYIKoZIzj0EAwIw
HTEbMBkGA1UEAwwScmVtYnJvIGZpeHR1cmUgb25lMCAXDTI2MTEyMTAzNDIyNFoY
DzIxMjYxMDI4MDM0MjI0WjAdMRswGQYDVQQDDBJyZW1icm8gZml4dHVyZSBvbmUw
WTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAATEwXxJoyNaDbwuHxe6G9311eMEyOzM
Ln+9hxzBS7t3QvKIJoiowOGpoYQBYpMSVMZaLxbgH/T/KA++K+biUsFUo0IwQDAd
BgNVHQ4EFgQUOJHZFHXqMOTOcgSeFzdXhpWGFTcwDwYDVR0TAQH/BAUwAwEB/zAO
BgNVHQ8BAf8EBAMCAgQwCgYIKoZIzj0EAwIDSAAwRQIgTXyqQOfQ1dLfoc4Pznl9
d5WJgi75hPEOPxqMRQAe+0sCIQCA1glrWFJn1l0BYFb81DzO6ZZXtjMPqyieGj1x
124rfA==
-----END CERTIFICATE-----
";
    const TWO: &str = "-----BEGIN CERTIFICATE-----
MIIBfzCCASagAwIBAgIUPCvecfTa+3H95/U3Ee4Um88tpoEwCgYIKoZIzj0EAwIw
HTEbMBkGA1UEAwwScmVtYnJvIGZpeHR1cmUgdHdvMCAXDTI2MTEyMTAzNDIyNFoY
DzIxMjYxMDI4MDM0MjI0WjAdMRswGQYDVQQDDBJyZW1icm8gZml4dHVyZSB0d28w
WTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAATAelbV+jeleQcbDVSQgwx05mLRQyOV
FfRechspYRV7g8qUQ40eM29ZjB3508wBVuivpRFFxdyr9c8Bp7tOgGWRo0IwQDAd
BgNVHQ4EFgQUIFOGG5nMZaXUtvprZUv9D+Ie8XMwDwYDVR0TAQH/BAUwAwEB/zAO
BgNVHQ8BAf8EBAMCAgQwCgYIKoZIzj0EAwIDRwAwRAIgQFgKswF0888dfHapfMiR
6Mdos6foKHn7AfDutPChRRoCIFQs62tGNMdcPMBnFmcMR7NWf33nNkdNLqCdkipu
72lK
-----END CERTIFICATE-----
";
    const ONE_SHA256: &str = "526c41e476068436eb6c61856c092a8f9ae6b7cfde7156e10afdda65451b04f2";
    const TWO_SHA256: &str = "9b57e9109bf733a65fdd7dfa52987cb44d4d8eb0d5d856f25914758396c42afa";

    const NSS_PKCS11: &str = "library=\nname=NSS Internal PKCS #11 Module\nparameters=configdir='sql:/p' \
certPrefix='' keyPrefix='' secmod='secmod.db' flags=optimizeSpace \nNSS=Flags=internal,critical trustOrder=75 \
cipherOrder=100 slotParams=(1={slotFlags=[ECC,RSA] askpw=any timeout=30})\n\n";

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Dir {
            let p = std::env::temp_dir().join(format!("rembro-certs-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn certs() -> (Vec<u8>, Vec<u8>) {
        (parse_pem(ONE).remove(0), parse_pem(TWO).remove(0))
    }

    fn have(cmd: &str) -> bool {
        // Windows's own certutil is not NSS's.
        if cfg!(windows) && cmd == "certutil" {
            return false;
        }
        Command::new(cmd).arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
    }

    #[test]
    fn sha256_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrs\
                  mnopqrstnopqrstu",
                "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(hex(&sha256(input)), *want);
        }
        assert_eq!(
            hex(&sha256(&vec![b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        for (len, want) in [
            (55, "d5e285683cd4efc02d021a5c62014694958901005d6f71e89e0989fac77e4072"),
            (56, "04c26261370ee7541549d16dee320c723e3fd14671e66a099afe0a377c16888e"),
            (63, "75220b47218278e656f2013bb8f0c455a25eaf01e86c64924e9d48d89776d6f2"),
            (64, "7ce100971f64e7001e8fe5a51973ecdfe1ced42befe7ee8d5fd6219506b5393c"),
            (65, "9537c5fdf120482f7d58d25e9ed583f52c02b4e304ea814db1633ad565aed7e9"),
        ] {
            assert_eq!(hex(&sha256(&vec![b'x'; len])), want, "{len} bytes");
        }
    }

    #[test]
    fn base64_round_trip() {
        for n in 0..70 {
            let data: Vec<u8> = (0..n as u8).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(base64_decode(&base64(&data)).unwrap(), data);
        }
        assert!(base64_decode("not*base64").is_none());
    }

    #[test]
    fn pem() {
        let (one, two) = certs();
        assert_eq!(hex(&sha256(&one)), ONE_SHA256);
        assert_eq!(hex(&sha256(&two)), TWO_SHA256);
        assert_eq!(parse_pem(&to_pem(&one)), vec![one.clone()]);

        let trusted = to_pem(&[one.as_slice(), &[0x30, 0x03, 0x06, 0x01, 0x00]].concat())
            .replace("CERTIFICATE", "TRUSTED CERTIFICATE");
        let key = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg\n-----END PRIVATE KEY-----\n";
        let broken = "-----BEGIN CERTIFICATE-----\nMIIB*AAA\n-----END CERTIFICATE-----\n";
        let unterminated = "-----BEGIN CERTIFICATE-----\nMIIBgDCC\n-----END X509 CRL-----\n";
        let bundle =
            format!("subject=CN=one\n{ONE}{key}{broken}{unterminated}# two\n{TWO}{trusted}").replace('\n', "\r\n");
        assert_eq!(parse_pem(&bundle), vec![one.clone(), two, one]);
    }

    #[test]
    fn pkcs11_text() {
        let stanza = "library=/usr/lib/pkcs11/p11-kit-trust.so\nname=rembro CA certificates\nparameters=paths='/a'\n";
        let added = edit_pkcs11(NSS_PKCS11, Some(stanza));
        assert_eq!(added, format!("{NSS_PKCS11}{stanza}\n"));
        let moved = edit_pkcs11(&added, Some(&stanza.replace("/a", "/b")));
        assert_eq!(moved, format!("{NSS_PKCS11}{}\n", stanza.replace("/a", "/b")));
        assert_eq!(edit_pkcs11(&moved, None), NSS_PKCS11);
        assert_eq!(edit_pkcs11("", None), "");
    }

    #[test]
    // NSS's module parameters cannot quote a Windows path's backslashes.
    #[cfg(unix)]
    fn firefox_p11kit() {
        let d = Dir::new("ff-p11kit");
        let (one, two) = certs();
        let module = Path::new("/usr/lib/pkcs11/p11-kit-trust.so");
        fs::write(d.0.join("pkcs11.txt"), NSS_PKCS11).unwrap();
        apply_firefox_with(&d.0, &[one.clone(), two.clone(), one.clone()], Some(module), "/nonexistent").unwrap();
        let bundle = fs::read_to_string(d.0.join(FIREFOX_BUNDLE)).unwrap();
        assert_eq!(parse_pem(&bundle), vec![one.clone(), two.clone()]);
        let text = fs::read_to_string(d.0.join("pkcs11.txt")).unwrap();
        assert!(text.starts_with(NSS_PKCS11));
        assert!(text.contains(&format!("parameters=paths='{}'\n", d.0.join(FIREFOX_BUNDLE).display())));

        apply_firefox_with(&d.0, std::slice::from_ref(&two), Some(module), "/nonexistent").unwrap();
        assert_eq!(fs::read_to_string(d.0.join("pkcs11.txt")).unwrap(), text);
        assert_eq!(parse_pem(&fs::read_to_string(d.0.join(FIREFOX_BUNDLE)).unwrap()), vec![two]);

        apply_firefox_with(&d.0, &[], Some(module), "/nonexistent").unwrap();
        assert_eq!(fs::read_to_string(d.0.join("pkcs11.txt")).unwrap(), NSS_PKCS11);
        assert!(!d.0.join(FIREFOX_BUNDLE).exists());

        // A fresh profile gets a pkcs11.txt with only rembro's module, and loses it again.
        let fresh = Dir::new("ff-fresh");
        apply_firefox_with(&fresh.0, &[one], Some(module), "/nonexistent").unwrap();
        assert!(fs::read_to_string(fresh.0.join("pkcs11.txt")).unwrap().starts_with("library=/usr/lib/pkcs11/"));
        apply_firefox_with(&fresh.0, &[], Some(module), "/nonexistent").unwrap();
        assert!(fs::read_dir(&fresh.0).unwrap().next().is_none());
    }

    #[test]
    fn firefox_without_tools() {
        let d = Dir::new("ff-none");
        let (one, _) = certs();
        let err = apply_firefox_with(&d.0, &[one], None, "/nonexistent/certutil").unwrap_err();
        assert!(err.contains("p11-kit-trust.so") && err.contains("not installed"), "{err}");
        apply_firefox_with(&d.0, &[], None, "/nonexistent/certutil").unwrap();
    }

    #[test]
    fn firefox_certutil() {
        if !have("certutil") {
            eprintln!("skipped: certutil is not installed");
            return;
        }
        let d = Dir::new("ff-certutil");
        let (one, two) = certs();
        let listing = |d: &Dir| -> Vec<String> {
            let out = run("certutil", &["-L", "-d", &format!("sql:{}", d.0.display())], None).unwrap();
            out.lines()
                .filter(|l| l.starts_with(CERTUTIL_PREFIX))
                .map(|l| l.split_whitespace().next().unwrap().into())
                .collect()
        };
        apply_firefox_with(&d.0, &[one.clone(), two.clone()], None, "certutil").unwrap();
        let both = listing(&d);
        assert_eq!(both.len(), 2);
        assert!(d.0.join(CERTUTIL_MARKER).exists());
        apply_firefox_with(&d.0, &[two.clone(), one.clone()], None, "certutil").unwrap();
        assert_eq!(listing(&d), both);
        apply_firefox_with(&d.0, std::slice::from_ref(&two), None, "certutil").unwrap();
        assert_eq!(listing(&d), vec![nickname(&two)]);

        // Once p11-kit is there, the certificates move out of cert9.db.
        apply_firefox_with(&d.0, &[two], Some(Path::new("/usr/lib/pkcs11/p11-kit-trust.so")), "certutil").unwrap();
        assert!(listing(&d).is_empty());
        assert!(!d.0.join(CERTUTIL_MARKER).exists());
        assert!(d.0.join(FIREFOX_BUNDLE).exists());
    }

    #[test]
    fn varints_and_records() {
        for v in [0u64, 1, 127, 128, 240, 2287, 16383, 16384, 1 << 35, (1 << 56) - 1, 1 << 56, u64::MAX] {
            let mut b = Vec::new();
            sqlite::put_varint(&mut b, v);
            assert_eq!(sqlite::get_varint(&b).unwrap(), (v, b.len()), "{v}");
        }
        use sqlite::Value::*;
        let values = vec![
            Null,
            Int(0),
            Int(1),
            Int(-1),
            Int(300),
            Int(-70000),
            Int(1 << 40),
            Int(i64::MIN),
            Real(1.5),
            Text("héllo".into()),
            Blob(vec![0; 200]),
        ];
        assert_eq!(sqlite::parse_record(&sqlite::record(&values)).unwrap(), values);
    }

    #[test]
    fn sqlite_round_trip() {
        use sqlite::Value::*;
        // Small pages, so the trees get interior pages and the big rows overflow.
        for (page_size, n) in [(512, 300), (1024, 3), (4096, 200), (65536, 50)] {
            let rows: sqlite::Rows = (0..n)
                .map(|i| vec![Text(hex(&sha256(&[i as u8, (i >> 8) as u8]))), Blob(vec![i as u8; i * 37 % 5000])])
                .collect();
            let tables = [
                sqlite::Table { name: "meta", sql: CHROMIUM_META_SQL, index_on: Some(0), rows: vec![] },
                sqlite::Table { name: "t", sql: "CREATE TABLE t(k TEXT PRIMARY KEY, v BLOB)", index_on: Some(0), rows },
            ];
            let data = sqlite::write(page_size, &tables).unwrap();
            let db = sqlite::Db::open(&data).unwrap();
            let (sql, read) = db.table("t").unwrap().unwrap();
            assert_eq!(sql, tables[1].sql);
            assert_eq!(read, tables[1].rows);
            assert_eq!(db.table("meta").unwrap().unwrap().1, sqlite::Rows::new());
            assert!(db.table("nope").unwrap().is_none());

            if have("sqlite3") {
                let d = Dir::new(&format!("sqlite-{page_size}"));
                let path = d.0.join("db");
                fs::write(&path, &data).unwrap();
                let out = Command::new("sqlite3")
                    .args(["-init", "/dev/null", "-batch", "-list", "-noheader"])
                    .arg(&path)
                    .arg(
                        "PRAGMA integrity_check; SELECT count(*), sum(length(v)) FROM t; \
                          SELECT count(*) FROM t WHERE k = (SELECT max(k) FROM t);",
                    )
                    .output()
                    .unwrap();
                let want_len: usize = (0..n).map(|i| i * 37 % 5000).sum();
                let want = format!("ok\n{n}|{want_len}\n1\n");
                assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{}", String::from_utf8_lossy(&out.stderr));
            }
        }
    }

    #[test]
    fn chromium() {
        let d = Dir::new("chromium");
        let (one, two) = certs();
        let db = d.0.join(CHROMIUM_DB);
        let rows = || read_server_certs(&fs::read(&db).unwrap()).unwrap();

        apply_chromium(&d.0, &[]).unwrap();
        assert!(!db.exists());

        apply_chromium(&d.0, &[one.clone(), two.clone(), one.clone()]).unwrap();
        let first = rows();
        assert_eq!(first.iter().map(|r| r.hash.as_str()).collect::<Vec<_>>(), [ONE_SHA256, TWO_SHA256]);
        assert!(first.iter().all(|r| r.trust == CHROMIUM_TRUSTED));
        assert_eq!(first[0].der, one);

        let before = fs::metadata(&db).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        apply_chromium(&d.0, &[two.clone(), one.clone()]).unwrap();
        assert_eq!(fs::metadata(&db).unwrap().modified().unwrap(), before, "rewrote an unchanged database");

        apply_chromium(&d.0, std::slice::from_ref(&two)).unwrap();
        assert_eq!(rows().iter().map(|r| r.hash.as_str()).collect::<Vec<_>>(), [TWO_SHA256]);

        // A certificate that the user installed stays, with the trust they gave it, and rembro does not claim it.
        let user = CertRow { hash: ONE_SHA256.into(), der: one.clone(), trust: vec![0x0a, 0x02, 0x08, 0x01] };
        let mut all = rows();
        all.insert(0, user.clone());
        fs::write(&db, server_cert_db(&all).unwrap()).unwrap();
        apply_chromium(&d.0, std::slice::from_ref(&one)).unwrap();
        assert_eq!(rows(), vec![user.clone()]);
        apply_chromium(&d.0, &[]).unwrap();
        assert_eq!(rows(), vec![user]);
        assert!(!d.0.join(CHROMIUM_STATE).exists());

        fs::write(sibling(&db, "-journal"), b"x").unwrap();
        assert!(apply_chromium(&d.0, &[two]).unwrap_err().contains("unfinished"));
    }
}
