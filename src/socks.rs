//! A SOCKS5 server on this machine's loopback whose connections are made from the target.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::host::Conn;
use crate::mux::{Mux, Sock};
use crate::proto::Open;
use crate::util;

/// Starts the server and returns its port.
pub fn serve(conn: Arc<Conn>, verbose: bool) -> io::Result<u16> {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = l.local_addr()?.port();
    let me = util::uid();
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            // Other users on this machine could otherwise use the server to reach into the target.
            if let (Ok(local), Ok(peer)) = (s.local_addr(), s.peer_addr())
                && let Some(uid) = util::tcp_peer_uid(local, peer)
                && uid != me
            {
                if verbose {
                    eprintln!("rembro: refused a SOCKS connection from uid {uid}");
                }
                continue;
            }
            let mux = conn.mux();
            thread::spawn(move || {
                if let Err(e) = handle(&mux, s, verbose)
                    && verbose
                {
                    eprintln!("rembro: SOCKS: {e}");
                }
            });
        }
    });
    Ok(port)
}

fn reply_code(err: &str) -> u8 {
    match err.split(':').next().unwrap_or("") {
        "refused" => 5,
        "unreachable" | "dns" => 4,
        "timeout" => 6,
        _ => 1,
    }
}

fn handle(mux: &Arc<Mux>, mut s: TcpStream, verbose: bool) -> io::Result<()> {
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr)?;
    if hdr[0] != 5 {
        return Ok(());
    }
    let mut methods = vec![0u8; hdr[1] as usize];
    s.read_exact(&mut methods)?;
    if !methods.contains(&0) {
        return s.write_all(&[5, 0xff]);
    }
    s.write_all(&[5, 0])?;
    let mut req = [0u8; 4];
    s.read_exact(&mut req)?;
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a)?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n)?;
            let mut name = vec![0u8; n[0] as usize];
            s.read_exact(&mut name)?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a)?;
            Ipv6Addr::from(a).to_string()
        }
        _ => return s.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]),
    };
    let mut port = [0u8; 2];
    s.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);
    if req[1] != 1 {
        return s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]);
    }
    s.set_read_timeout(None)?;
    let result = mux.open(&Open::Tcp { host: host.clone(), port }, Sock::Tcp(s), |sock, result| {
        let code = match result {
            Ok(()) => 0,
            Err(e) => reply_code(e),
        };
        let _ = sock.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]);
    });
    if let Err(e) = result
        && verbose
    {
        eprintln!("rembro: {host}:{port}: {e}");
    }
    Ok(())
}
