//! Untrusted MIT-MAGIC-COOKIE-1 cookies from the X SECURITY extension, made the way
//! `xauth generate DISPLAY . untrusted` makes them.
//!
//! A client that connects with an untrusted cookie can read other clients' window properties, but
//! cannot capture their windows, change their properties, send them events, change server settings,
//! or use any extension except BIG-REQUESTS and XC-MISC (so no MIT-SHM, RENDER, XKEYBOARD,
//! XInputExtension, XTEST, RANDR or GLX). It can still own selections, but a trusted client that
//! asks for one gets nothing, so copying out of it fails while pasting into it works. The X server
//! disconnects every client using an authorization when it is revoked or expires.

use std::io::{self, Read, Write};

/// Seconds an authorization may go unused before the X server deletes it. The clock runs only while
/// no client is connected with the cookie: from generation until the first client connects, and from
/// when the last one disconnects. It stops whenever a client is connected, so a browser session that
/// holds its connection lasts as long as it likes. 0 means the authorization never expires.
pub const DEFAULT_TIMEOUT: u32 = 300;

const AUTH_NAME: &[u8] = b"MIT-MAGIC-COOKIE-1";
const GET_INPUT_FOCUS: u8 = 43;
const QUERY_EXTENSION: u8 = 98;
const SECURITY_QUERY_VERSION: u8 = 0;
const SECURITY_GENERATE_AUTHORIZATION: u8 = 1;
const SECURITY_REVOKE_AUTHORIZATION: u8 = 2;
const ATTR_TIMEOUT: u32 = 1 << 0;
const ATTR_TRUST_LEVEL: u32 = 1 << 1;
const UNTRUSTED: u32 = 1;
const GENERIC_EVENT: u8 = 35;

/// Connects with `cookie` (empty for none) and asks for a new untrusted cookie that expires after
/// `timeout_secs` unused. Returns the authorization's id, for `revoke`, and the cookie. The
/// authorization belongs to the server, so it outlives `stream`. Set a read timeout on `stream`
/// first, as nothing here gives up on a silent server.
pub fn generate_untrusted<S: Read + Write>(
    stream: &mut S,
    cookie: &[u8],
    timeout_secs: u32,
) -> Result<(u32, Vec<u8>), String> {
    generate_with(stream, Order::NATIVE, cookie, timeout_secs)
}

/// Connects with `cookie` and revokes an authorization from `generate_untrusted`. The X server
/// disconnects any client still using it.
pub fn revoke<S: Read + Write>(stream: &mut S, cookie: &[u8], auth_id: u32) -> Result<(), String> {
    revoke_with(stream, Order::NATIVE, cookie, auth_id)
}

fn generate_with<S: Read + Write>(
    stream: &mut S,
    order: Order,
    cookie: &[u8],
    timeout_secs: u32,
) -> Result<(u32, Vec<u8>), String> {
    let mut c = Conn::open(stream, order, cookie)?;
    let ext = c.security()?;
    let seq = c.send(&generate_request(order, ext.opcode, timeout_secs))?;
    let reply = c.reply(seq, Some(&ext))?;
    parse_generate(order, &reply)
}

fn revoke_with<S: Read + Write>(stream: &mut S, order: Order, cookie: &[u8], auth_id: u32) -> Result<(), String> {
    let mut c = Conn::open(stream, order, cookie)?;
    let ext = c.security()?;
    c.send(&request(order, ext.opcode, SECURITY_REVOKE_AUTHORIZATION, &order.b32(auth_id)))?;
    // RevokeAuthorization has no reply; an error for it arrives before the reply to this.
    let seq = c.send(&request(order, GET_INPUT_FOCUS, 0, &[]))?;
    c.reply(seq, Some(&ext)).map(|_| ())
}

/// The byte order chosen for a connection. The server answers in it, and servers may refuse clients
/// whose order differs from their own (mutter's Xwayland does), so connections use the native one.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Order {
    Little,
    Big,
}

impl Order {
    const NATIVE: Order = if cfg!(target_endian = "little") { Order::Little } else { Order::Big };

    fn u16(self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self == Order::Little { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) }
    }

    fn u32(self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self == Order::Little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
    }

    fn b16(self, v: u16) -> [u8; 2] {
        if self == Order::Little { v.to_le_bytes() } else { v.to_be_bytes() }
    }

    fn b32(self, v: u32) -> [u8; 4] {
        if self == Order::Little { v.to_le_bytes() } else { v.to_be_bytes() }
    }
}

fn pad(b: &mut Vec<u8>) {
    b.resize(b.len().next_multiple_of(4), 0);
}

fn setup_request(o: Order, cookie: &[u8]) -> Vec<u8> {
    let name = if cookie.is_empty() { &b""[..] } else { AUTH_NAME };
    let mut r = vec![if o == Order::Little { b'l' } else { b'B' }, 0];
    r.extend(o.b16(11));
    r.extend(o.b16(0));
    r.extend(o.b16(name.len() as u16));
    r.extend(o.b16(cookie.len() as u16));
    r.extend([0, 0]);
    r.extend_from_slice(name);
    pad(&mut r);
    r.extend_from_slice(cookie);
    pad(&mut r);
    r
}

/// A request: the opcode, a byte of data (the minor opcode for extensions), its length in 4-byte
/// units, then `body`, padded.
fn request(o: Order, opcode: u8, data: u8, body: &[u8]) -> Vec<u8> {
    let mut r = vec![opcode, data, 0, 0];
    r.extend_from_slice(body);
    pad(&mut r);
    let words = o.b16((r.len() / 4) as u16);
    r[2..4].copy_from_slice(&words);
    r
}

fn query_extension(o: Order, name: &[u8]) -> Vec<u8> {
    let mut body = o.b16(name.len() as u16).to_vec();
    body.extend([0, 0]);
    body.extend_from_slice(name);
    request(o, QUERY_EXTENSION, 0, &body)
}

fn generate_request(o: Order, opcode: u8, timeout_secs: u32) -> Vec<u8> {
    let mut body = o.b16(AUTH_NAME.len() as u16).to_vec();
    body.extend(o.b16(0));
    body.extend(o.b32(ATTR_TIMEOUT | ATTR_TRUST_LEVEL));
    body.extend_from_slice(AUTH_NAME);
    pad(&mut body);
    body.extend(o.b32(timeout_secs));
    body.extend(o.b32(UNTRUSTED));
    request(o, opcode, SECURITY_GENERATE_AUTHORIZATION, &body)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim_end_matches(['\0', '\n', ' ']).to_string()
}

/// The answer to the connection setup request: the 8-byte header and the additional data after it.
fn parse_setup(o: Order, head: &[u8; 8], body: &[u8]) -> Result<(), String> {
    match head[0] {
        1 => Ok(()),
        0 => {
            let n = (head[1] as usize).min(body.len());
            Err(format!("the X server refused the connection: {}", text(&body[..n])))
        }
        2 => Err(format!("the X server wants further authentication: {}", text(body))),
        b => Err(format!(
            "unexpected reply {b} to X connection setup (protocol {}.{})",
            o.u16(&head[2..]),
            o.u16(&head[4..])
        )),
    }
}

#[derive(Debug, PartialEq)]
struct Ext {
    opcode: u8,
    first_error: u8,
}

fn parse_query_extension(p: &[u8]) -> Option<Ext> {
    (p[8] != 0).then(|| Ext { opcode: p[9], first_error: p[11] })
}

fn parse_generate(o: Order, p: &[u8]) -> Result<(u32, Vec<u8>), String> {
    let id = o.u32(&p[8..]);
    let n = o.u16(&p[12..]) as usize;
    match p.get(32..32 + n) {
        Some(data) if n > 0 => Ok((id, data.to_vec())),
        _ => Err(format!("bad SecurityGenerateAuthorization reply with {n} bytes of cookie")),
    }
}

#[derive(Debug, PartialEq)]
enum Packet {
    Error { code: u8, seq: u16, value: u32, major: u8, minor: u16 },
    Reply { seq: u16, extra: usize },
    Event { extra: usize },
}

/// What the 32 bytes at the start of something from the server are, and how many bytes follow.
fn packet(o: Order, p: &[u8; 32]) -> Packet {
    match p[0] {
        0 => Packet::Error {
            code: p[1],
            seq: o.u16(&p[2..]),
            value: o.u32(&p[4..]),
            minor: o.u16(&p[8..]),
            major: p[10],
        },
        1 => Packet::Reply { seq: o.u16(&p[2..]), extra: o.u32(&p[4..]) as usize * 4 },
        t if t & 0x7f == GENERIC_EVENT => Packet::Event { extra: o.u32(&p[4..]) as usize * 4 },
        _ => Packet::Event { extra: 0 },
    }
}

fn describe_error(code: u8, value: u32, major: u8, minor: u16, ext: Option<&Ext>) -> String {
    const CORE: [&str; 17] = [
        "BadRequest",
        "BadValue",
        "BadWindow",
        "BadPixmap",
        "BadAtom",
        "BadCursor",
        "BadFont",
        "BadMatch",
        "BadDrawable",
        "BadAccess",
        "BadAlloc",
        "BadColor",
        "BadGC",
        "BadIDChoice",
        "BadName",
        "BadLength",
        "BadImplementation",
    ];
    let error = match (code, ext) {
        (1..=17, _) => CORE[code as usize - 1].to_string(),
        (c, Some(e)) if c == e.first_error => "BadAuthorization".to_string(),
        (c, Some(e)) if c == e.first_error.wrapping_add(1) => "BadAuthorizationProtocol".to_string(),
        (c, _) => format!("error {c}"),
    };
    let req = match (major, ext) {
        (GET_INPUT_FOCUS, _) => "GetInputFocus".to_string(),
        (QUERY_EXTENSION, _) => "QueryExtension".to_string(),
        (m, Some(e)) if m == e.opcode => match minor as u8 {
            SECURITY_QUERY_VERSION => "SecurityQueryVersion".to_string(),
            SECURITY_GENERATE_AUTHORIZATION => "SecurityGenerateAuthorization".to_string(),
            SECURITY_REVOKE_AUTHORIZATION => "SecurityRevokeAuthorization".to_string(),
            n => format!("SECURITY request {n}"),
        },
        (m, _) => format!("request {m}"),
    };
    format!("X server: {error} (value {value:#x}) in {req}")
}

fn io_error(e: io::Error) -> String {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => "the X server closed the connection".to_string(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "timed out waiting for the X server".to_string(),
        _ => format!("X server: {e}"),
    }
}

struct Conn<'a, S> {
    s: &'a mut S,
    order: Order,
    seq: u16,
}

impl<'a, S: Read + Write> Conn<'a, S> {
    fn open(s: &'a mut S, order: Order, cookie: &[u8]) -> Result<Self, String> {
        s.write_all(&setup_request(order, cookie)).and_then(|_| s.flush()).map_err(io_error)?;
        let mut head = [0u8; 8];
        s.read_exact(&mut head).map_err(io_error)?;
        let mut body = vec![0; order.u16(&head[6..]) as usize * 4];
        s.read_exact(&mut body).map_err(io_error)?;
        parse_setup(order, &head, &body)?;
        Ok(Conn { s, order, seq: 0 })
    }

    fn send(&mut self, req: &[u8]) -> Result<u16, String> {
        self.s.write_all(req).and_then(|_| self.s.flush()).map_err(io_error)?;
        self.seq = self.seq.wrapping_add(1);
        Ok(self.seq)
    }

    /// Reads up to the reply to request `seq` and returns it whole, skipping events. Any error fails.
    fn reply(&mut self, seq: u16, ext: Option<&Ext>) -> Result<Vec<u8>, String> {
        loop {
            let mut p = vec![0u8; 32];
            self.s.read_exact(&mut p).map_err(io_error)?;
            let extra = match packet(self.order, p[..32].try_into().unwrap()) {
                Packet::Error { code, value, major, minor, .. } => {
                    return Err(describe_error(code, value, major, minor, ext));
                }
                Packet::Reply { extra, .. } | Packet::Event { extra } => extra,
            };
            p.resize(32 + extra, 0);
            self.s.read_exact(&mut p[32..]).map_err(io_error)?;
            if p[0] == 1 && self.order.u16(&p[2..]) == seq {
                return Ok(p);
            }
        }
    }

    fn security(&mut self) -> Result<Ext, String> {
        let seq = self.send(&query_extension(self.order, b"SECURITY"))?;
        let ext = parse_query_extension(&self.reply(seq, None)?).ok_or("the X server has no SECURITY extension")?;
        let mut version = self.order.b16(1).to_vec();
        version.extend(self.order.b16(0));
        let seq = self.send(&request(self.order, ext.opcode, SECURITY_QUERY_VERSION, &version))?;
        let p = self.reply(seq, Some(&ext))?;
        let (major, minor) = (self.order.u16(&p[8..]), self.order.u16(&p[10..]));
        if major != 1 {
            return Err(format!("the X server's SECURITY extension is version {major}.{minor}, not 1.x"));
        }
        Ok(ext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Plays back what an X server would send and records what the client writes.
    struct Fake {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Fake {
        fn new(input: Vec<u8>) -> Fake {
            Fake { input: Cursor::new(input), output: Vec::new() }
        }
    }

    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Fake {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    const OPCODE: u8 = 130;
    const FIRST_ERROR: u8 = 150;

    fn accepted(o: Order) -> Vec<u8> {
        // A success header, then 3 words standing in for the setup reply's body.
        let mut b = vec![1, 0];
        b.extend(o.b16(11));
        b.extend(o.b16(0));
        b.extend(o.b16(3));
        b.extend([0xaa; 12]);
        b
    }

    fn reply(o: Order, seq: u16, fields: &[u8], extra: &[u8]) -> Vec<u8> {
        let mut b = vec![1, 0];
        b.extend(o.b16(seq));
        b.extend(o.b32((extra.len() / 4) as u32));
        b.extend_from_slice(fields);
        b.resize(32, 0);
        b.extend_from_slice(extra);
        b
    }

    fn error(o: Order, code: u8, seq: u16, major: u8, minor: u16) -> Vec<u8> {
        let mut b = vec![0, code];
        b.extend(o.b16(seq));
        b.extend(o.b32(0x1234));
        b.extend(o.b16(minor));
        b.push(major);
        b.resize(32, 0);
        b
    }

    fn security_present(o: Order) -> Vec<u8> {
        let mut b = reply(o, 1, &[1, OPCODE, 90, FIRST_ERROR], &[]);
        let mut version = o.b16(1).to_vec();
        version.extend(o.b16(0));
        b.extend(reply(o, 2, &version, &[]));
        b
    }

    #[test]
    fn setup_request_encoding() {
        let cookie = [7u8; 16];
        let le = setup_request(Order::Little, &cookie);
        assert_eq!(&le[..12], &[b'l', 0, 11, 0, 0, 0, 18, 0, 16, 0, 0, 0]);
        assert_eq!(&le[12..30], AUTH_NAME);
        assert_eq!(&le[30..32], &[0, 0]);
        assert_eq!(&le[32..], &cookie);
        let be = setup_request(Order::Big, &cookie);
        assert_eq!(&be[..12], &[b'B', 0, 0, 11, 0, 0, 0, 18, 0, 16, 0, 0]);
        assert_eq!(setup_request(Order::Little, &[]), vec![b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn request_encoding() {
        assert_eq!(query_extension(Order::Little, b"SECURITY"), [&[98, 0, 4, 0, 8, 0, 0, 0][..], b"SECURITY"].concat());
        assert_eq!(query_extension(Order::Big, b"SHAPE")[..8], [98, 0, 0, 4, 0, 5, 0, 0]);
        let g = generate_request(Order::Little, OPCODE, 300);
        assert_eq!(&g[..12], &[OPCODE, 1, 10, 0, 18, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(&g[12..30], AUTH_NAME);
        assert_eq!(&g[30..], &[0, 0, 0x2c, 1, 0, 0, 1, 0, 0, 0]);
        let g = generate_request(Order::Big, OPCODE, 300);
        assert_eq!(&g[..12], &[OPCODE, 1, 0, 10, 0, 18, 0, 0, 0, 0, 0, 3]);
        assert_eq!(&g[32..], &[0, 0, 1, 0x2c, 0, 0, 0, 1]);
        assert_eq!(
            request(Order::Little, OPCODE, SECURITY_REVOKE_AUTHORIZATION, &Order::Little.b32(0x0102_0304)),
            vec![OPCODE, 2, 2, 0, 4, 3, 2, 1]
        );
        assert_eq!(request(Order::Big, GET_INPUT_FOCUS, 0, &[]), vec![43, 0, 0, 1]);
    }

    #[test]
    fn setup_replies() {
        let o = Order::Little;
        assert_eq!(parse_setup(o, &[1, 0, 11, 0, 0, 0, 3, 0], &[0; 12]), Ok(()));
        let reason = b"Invalid MIT-MAGIC-COOKIE-1 key\0\0";
        assert_eq!(
            parse_setup(o, &[0, 30, 11, 0, 0, 0, 8, 0], reason),
            Err("the X server refused the connection: Invalid MIT-MAGIC-COOKIE-1 key".into())
        );
        assert_eq!(
            parse_setup(o, &[2, 0, 0, 0, 0, 0, 2, 0], b"more please\0"),
            Err("the X server wants further authentication: more please".into())
        );
    }

    #[test]
    fn packets() {
        let o = Order::Big;
        let p: [u8; 32] = reply(o, 9, &[], &[0; 8])[..32].try_into().unwrap();
        assert_eq!(packet(o, &p), Packet::Reply { seq: 9, extra: 8 });
        let p: [u8; 32] = error(o, 10, 4, OPCODE, 2).try_into().unwrap();
        assert_eq!(packet(o, &p), Packet::Error { code: 10, seq: 4, value: 0x1234, major: OPCODE, minor: 2 });
        let mut p = [0u8; 32];
        p[0] = 34;
        assert_eq!(packet(o, &p), Packet::Event { extra: 0 });
        p[0] = GENERIC_EVENT | 0x80;
        p[4..8].copy_from_slice(&o.b32(3));
        assert_eq!(packet(o, &p), Packet::Event { extra: 12 });
    }

    fn generate_session(o: Order) {
        let cookie = [0x5au8; 16];
        let mut input = accepted(o);
        input.extend(reply(o, 1, &[1, OPCODE, 90, FIRST_ERROR], &[]));
        // A MappingNotify, which goes to every client, before the next reply.
        let mut mapping = vec![0u8; 32];
        mapping[0] = 34;
        input.extend(mapping);
        let mut version = o.b16(1).to_vec();
        version.extend(o.b16(0));
        input.extend(reply(o, 2, &version, &[]));
        let mut generic = vec![GENERIC_EVENT, 0, 0, 0];
        generic.extend(o.b32(2));
        generic.resize(40, 0x77);
        input.extend(generic);
        let mut fields = o.b32(0x0040_0001).to_vec();
        fields.extend(o.b16(16));
        input.extend(reply(o, 3, &fields, &cookie));
        let mut x = Fake::new(input);
        assert_eq!(generate_with(&mut x, o, b"real-cookie-0123", 300), Ok((0x0040_0001, cookie.to_vec())));
        let mut version = o.b16(1).to_vec();
        version.extend(o.b16(0));
        let sent = [
            setup_request(o, b"real-cookie-0123"),
            query_extension(o, b"SECURITY"),
            request(o, OPCODE, SECURITY_QUERY_VERSION, &version),
            generate_request(o, OPCODE, 300),
        ]
        .concat();
        assert_eq!(x.output, sent);
    }

    #[test]
    fn generate_little_endian() {
        generate_session(Order::Little);
    }

    #[test]
    fn generate_big_endian() {
        generate_session(Order::Big);
    }

    #[test]
    fn generate_errors() {
        let o = Order::Little;
        let mut refused = vec![0, 30, 11, 0, 0, 0, 8, 0];
        refused.extend(b"Invalid MIT-MAGIC-COOKIE-1 key\0\0");
        let r = generate_with(&mut Fake::new(refused), o, b"bogus", 60);
        assert_eq!(r, Err("the X server refused the connection: Invalid MIT-MAGIC-COOKIE-1 key".into()));

        let mut missing = accepted(o);
        missing.extend(reply(o, 1, &[0, 0, 0, 0], &[]));
        let r = generate_with(&mut Fake::new(missing), o, b"", 60);
        assert_eq!(r, Err("the X server has no SECURITY extension".into()));

        let mut untrusted = accepted(o);
        untrusted.extend(security_present(o));
        untrusted.extend(error(o, 10, 3, OPCODE, 1));
        let r = generate_with(&mut Fake::new(untrusted), o, b"", 60);
        assert_eq!(r, Err("X server: BadAccess (value 0x1234) in SecurityGenerateAuthorization".into()));

        let mut cut = accepted(o);
        cut.extend(&reply(o, 1, &[1, OPCODE, 90, FIRST_ERROR], &[])[..20]);
        let r = generate_with(&mut Fake::new(cut), o, b"", 60);
        assert_eq!(r, Err("the X server closed the connection".into()));
    }

    #[test]
    fn revoking() {
        let o = Order::Little;
        let mut ok = accepted(o);
        ok.extend(security_present(o));
        ok.extend(reply(o, 4, &[0, 0, 0, 0], &[]));
        let mut x = Fake::new(ok);
        assert_eq!(revoke_with(&mut x, o, b"", 0x0040_0001), Ok(()));
        assert!(x.output.ends_with(&[OPCODE, 2, 2, 0, 1, 0, 0x40, 0, 43, 0, 1, 0]));

        let mut unknown = accepted(o);
        unknown.extend(security_present(o));
        unknown.extend(error(o, FIRST_ERROR, 3, OPCODE, 2));
        unknown.extend(reply(o, 4, &[0, 0, 0, 0], &[]));
        let r = revoke_with(&mut Fake::new(unknown), o, b"", 0x0040_0001);
        assert_eq!(r, Err("X server: BadAuthorization (value 0x1234) in SecurityRevokeAuthorization".into()));
    }
}
