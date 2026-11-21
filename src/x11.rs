//! Reaching this machine's X server, and the cookie a forwarded client needs.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::mux::Sock;
use crate::{sys, util, x11sec};

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    /// A socket path, or a host for TCP. Empty means the local socket for `number`, or on
    /// Windows, TCP on this machine.
    host: String,
    pub number: u32,
}

impl Display {
    pub fn parse(s: &str) -> Option<Display> {
        let (host, rest) = s.rsplit_once(':')?;
        let (number, screen) = match rest.split_once('.') {
            Some((n, screen)) => (n, Some(screen)),
            None => (rest, None),
        };
        let number = number.parse().ok()?;
        // XQuartz on macOS names a socket by its whole path, such as
        // /private/tmp/com.apple.launchd.x/org.xquartz:0.
        let host = if host.starts_with('/') {
            &s[..s.len() - screen.map_or(0, |sc| sc.len() + 1)]
        } else if host == "unix" {
            ""
        } else {
            host
        };
        Some(Display { host: host.to_string(), number })
    }

    pub fn from_env() -> Option<Display> {
        std::env::var("DISPLAY").ok().and_then(|d| Display::parse(&d))
    }

    pub fn connect(&self) -> Result<Sock, String> {
        #[cfg(unix)]
        {
            if self.host.starts_with('/') {
                return UnixStream::connect(&self.host).map(Sock::Unix).map_err(|e| format!("other: X server: {e}"));
            }
            if self.host.is_empty() {
                let path = format!("/tmp/.X11-unix/X{}", self.number);
                match UnixStream::connect(&path) {
                    Ok(s) => return Ok(Sock::Unix(s)),
                    #[cfg(target_os = "linux")]
                    Err(e) => {
                        use std::os::linux::net::SocketAddrExt;
                        let addr = std::os::unix::net::SocketAddr::from_abstract_name(path.as_bytes())
                            .map_err(|e| format!("other: {e}"))?;
                        return UnixStream::connect_addr(&addr)
                            .map(Sock::Unix)
                            .map_err(|_| format!("other: X server: {e}"));
                    }
                    #[cfg(not(target_os = "linux"))]
                    Err(e) => return Err(format!("other: X server: {e}")),
                }
            }
        }
        let host = if self.host.is_empty() { "127.0.0.1" } else { self.host.as_str() };
        let port = 6000 + self.number as u16;
        TcpStream::connect((host, port)).map(Sock::Tcp).map_err(|e| format!("other: X server: {e}"))
    }

    fn control(&self) -> Result<Sock, String> {
        let s = self.connect().map_err(|e| e.trim_start_matches("other: ").to_string())?;
        s.set_timeout(Some(Duration::from_secs(5)));
        Ok(s)
    }

    /// Asks the X server for an untrusted cookie through the SECURITY extension, as ssh -X
    /// does, and returns the authorization's id and the cookie.
    pub fn untrusted(&self, cookie: Option<&[u8]>) -> Result<(u32, Vec<u8>), String> {
        x11sec::generate_untrusted(&mut self.control()?, cookie.unwrap_or_default(), x11sec::DEFAULT_TIMEOUT)
    }

    /// Revokes a cookie from `untrusted`; the X server disconnects its clients.
    pub fn revoke(&self, cookie: Option<&[u8]>, id: u32) {
        if let Ok(mut s) = self.control() {
            let _ = x11sec::revoke(&mut s, cookie.unwrap_or_default(), id);
        }
    }

    /// A connection for the mux that admits only X clients presenting `cookie`, so a program
    /// in the target cannot reach the X server without it, whatever host access the server allows.
    pub fn gate(&self, cookie: Option<Vec<u8>>) -> Result<Sock, String> {
        let (outer, inner) = Sock::pair().map_err(|e| format!("other: {e}"))?;
        let display = self.clone();
        thread::spawn(move || {
            let _ = admit(inner, &display, cookie.as_deref());
        });
        Ok(outer)
    }

    /// The MIT-MAGIC-COOKIE-1 for this display from the Xauthority file, if there is one.
    pub fn cookie(&self) -> Option<Vec<u8>> {
        let path = match std::env::var_os("XAUTHORITY") {
            Some(p) => PathBuf::from(p),
            None => util::expand_home("~/.Xauthority", util::home().as_deref()),
        };
        let data = fs::read(path).ok()?;
        let host = sys::hostname();
        let number = self.number.to_string();
        let mut best = None;
        for e in parse_xauthority(&data) {
            if e.name != b"MIT-MAGIC-COOKIE-1" || !(e.number.is_empty() || e.number == number.as_bytes()) {
                continue;
            }
            // FamilyLocal (256) entries name this host; FamilyWild (65535) matches any.
            let local = (e.family == 256 && e.address == host.as_bytes()) || e.family == 65535;
            if local || best.is_none() {
                best = Some(e.data.clone());
                if local {
                    break;
                }
            }
        }
        best
    }
}

/// Reads an X client's connection setup, refuses it unless it carries `cookie`, and
/// otherwise joins it to the X server.
fn admit(mut client: Sock, display: &Display, cookie: Option<&[u8]>) -> io::Result<()> {
    client.set_timeout(Some(Duration::from_secs(30)));
    let mut head = [0u8; 12];
    client.read_exact(&mut head)?;
    let little = match head[0] {
        b'l' => true,
        b'B' => false,
        _ => return Ok(()),
    };
    let u16_at = |o: usize| {
        let b = [head[o], head[o + 1]];
        if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) }
    };
    let (name_len, data_len) = (u16_at(6) as usize, u16_at(8) as usize);
    let mut rest = vec![0u8; name_len.next_multiple_of(4) + data_len.next_multiple_of(4)];
    client.read_exact(&mut rest)?;
    let name = &rest[..name_len];
    let data = &rest[name_len.next_multiple_of(4)..][..data_len];
    if let Some(cookie) = cookie
        && (name != b"MIT-MAGIC-COOKIE-1" || data != cookie)
    {
        // A "Failed" setup reply in the client's byte order.
        let reason = b"rembro: this connection did not present the cookie rembro issued";
        let words = reason.len().div_ceil(4);
        let mut reply = vec![0u8, reason.len() as u8];
        for v in [11u16, 0, words as u16] {
            reply.extend(if little { v.to_le_bytes() } else { v.to_be_bytes() });
        }
        reply.extend_from_slice(reason);
        reply.resize(8 + words * 4, 0);
        return client.write_all(&reply);
    }
    client.set_timeout(None);
    let Ok(mut server) = display.connect() else { return Ok(()) };
    server.write_all(&head)?;
    server.write_all(&rest)?;
    let (mut server_r, mut client_w) = (server.try_clone()?, client.try_clone()?);
    let up = thread::spawn(move || {
        let _ = io::copy(&mut server_r, &mut client_w);
        client_w.shutdown(Shutdown::Write);
    });
    let _ = io::copy(&mut client, &mut server);
    server.shutdown(Shutdown::Write);
    let _ = up.join();
    Ok(())
}

struct Entry {
    family: u16,
    address: Vec<u8>,
    number: Vec<u8>,
    name: Vec<u8>,
    data: Vec<u8>,
}

fn parse_xauthority(mut b: &[u8]) -> Vec<Entry> {
    fn field(b: &mut &[u8]) -> Option<Vec<u8>> {
        if b.len() < 2 {
            return None;
        }
        let n = u16::from_be_bytes([b[0], b[1]]) as usize;
        if b.len() < 2 + n {
            return None;
        }
        let v = b[2..2 + n].to_vec();
        *b = &b[2 + n..];
        Some(v)
    }
    let mut out = Vec::new();
    while b.len() >= 2 {
        let family = u16::from_be_bytes([b[0], b[1]]);
        b = &b[2..];
        let (Some(address), Some(number), Some(name), Some(data)) =
            (field(&mut b), field(&mut b), field(&mut b), field(&mut b))
        else {
            break;
        };
        out.push(Entry { family, address, number, name, data });
    }
    out
}

/// An Xauthority file that gives `cookie` for display `number` on any host.
pub fn xauthority(number: u32, cookie: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&65535u16.to_be_bytes());
    for f in [&b""[..], number.to_string().as_bytes(), b"MIT-MAGIC-COOKIE-1", cookie] {
        out.extend_from_slice(&(f.len() as u16).to_be_bytes());
        out.extend_from_slice(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays() {
        assert_eq!(Display::parse(":0"), Some(Display { host: String::new(), number: 0 }));
        assert_eq!(Display::parse("localhost:10.0"), Some(Display { host: "localhost".into(), number: 10 }));
        assert_eq!(Display::parse("unix:1"), Some(Display { host: String::new(), number: 1 }));
        assert_eq!(Display::parse("nonsense"), None);
        let xquartz = "/private/tmp/com.apple.launchd.AbC/org.xquartz:0";
        assert_eq!(Display::parse(xquartz), Some(Display { host: xquartz.into(), number: 0 }));
        assert_eq!(Display::parse(&format!("{xquartz}.0")), Some(Display { host: xquartz.into(), number: 0 }));
    }

    #[test]
    fn roundtrip() {
        let file = xauthority(12, b"0123456789abcdef");
        let e = parse_xauthority(&file);
        assert_eq!(e.len(), 1);
        assert_eq!(
            (e[0].family, e[0].number.as_slice(), e[0].data.as_slice()),
            (65535, &b"12"[..], &b"0123456789abcdef"[..])
        );
    }
}
