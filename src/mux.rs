//! Carries calls, events and any number of byte streams over one pair of pipes.
//!
//! Every frame is a 9-byte header (kind, id and payload length, big-endian) and a payload.
//! Each side numbers the channels and calls it starts, rembro with even ids and the agent
//! with odd ones. Channels have per-direction send windows, so one slow reader cannot
//! stall the pipe for the others.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const CALL: u8 = 1;
const REPLY: u8 = 2;
const EVENT: u8 = 3;
const OPEN: u8 = 4;
const OPEN_OK: u8 = 5;
const OPEN_ERR: u8 = 6;
const DATA: u8 = 7;
const EOF: u8 = 8;
const CLOSE: u8 = 9;
const WINDOW: u8 = 10;
const PING: u8 = 11;

/// Each side pings this often, and gives up on a peer it has not heard from for TIMEOUT.
/// Some transports (older kubectl, for one) never pass on the end of their input.
const PING_EVERY: Duration = Duration::from_secs(15);
const TIMEOUT: Duration = Duration::from_secs(60);

const MAX_PAYLOAD: usize = 1 << 20;
const WINDOW_SIZE: u32 = 1 << 20;
const CHUNK: usize = 32 << 10;

pub enum Sock {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Sock {
    /// Two connected sockets: a Unix socket pair, or on Windows a TCP connection over loopback.
    #[cfg(unix)]
    pub fn pair() -> io::Result<(Sock, Sock)> {
        let (a, b) = UnixStream::pair()?;
        Ok((Sock::Unix(a), Sock::Unix(b)))
    }

    #[cfg(not(unix))]
    pub fn pair() -> io::Result<(Sock, Sock)> {
        let l = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let a = TcpStream::connect(l.local_addr()?)?;
        let ours = a.local_addr()?;
        // Another program could connect first; skip anything that is not our own end.
        loop {
            let (b, peer) = l.accept()?;
            if peer == ours {
                return Ok((Sock::Tcp(a), Sock::Tcp(b)));
            }
        }
    }

    pub fn try_clone(&self) -> io::Result<Sock> {
        Ok(match self {
            Sock::Tcp(s) => Sock::Tcp(s.try_clone()?),
            #[cfg(unix)]
            Sock::Unix(s) => Sock::Unix(s.try_clone()?),
        })
    }

    pub fn set_timeout(&self, t: Option<std::time::Duration>) {
        let _ = match self {
            Sock::Tcp(s) => s.set_read_timeout(t).and(s.set_write_timeout(t)),
            #[cfg(unix)]
            Sock::Unix(s) => s.set_read_timeout(t).and(s.set_write_timeout(t)),
        };
    }

    pub fn shutdown(&self, how: Shutdown) {
        let _ = match self {
            Sock::Tcp(s) => s.shutdown(how),
            #[cfg(unix)]
            Sock::Unix(s) => s.shutdown(how),
        };
    }
}

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Sock::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Sock::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Sock::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Sock::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What a side does with the requests the other side sends it.
pub trait Handler: Send + Sync {
    fn call(&self, mux: &Arc<Mux>, call: Value) -> Result<Value, String>;
    fn open(&self, mux: &Arc<Mux>, target: Value) -> Result<Sock, String>;
    fn event(&self, event: Value);
    fn closed(&self);
}

enum Inbound {
    Data(Vec<u8>),
    Eof,
    Close,
}

struct Chan {
    /// Bytes we may still send, and whether the channel has closed.
    credit: Mutex<(u32, bool)>,
    cv: Condvar,
    inbox: Sender<Inbound>,
    /// Bytes received but not yet written out, which the peer must keep within the window.
    unacked: AtomicU32,
    /// Whether we sent EOF, and whether we received it.
    eofs: Mutex<(bool, bool)>,
    sock: Sock,
}

impl Chan {
    fn take(&self, want: usize) -> usize {
        let mut g = self.credit.lock().unwrap();
        while g.0 == 0 && !g.1 {
            g = self.cv.wait(g).unwrap();
        }
        if g.1 {
            return 0;
        }
        let n = want.min(g.0 as usize);
        g.0 -= n as u32;
        n
    }

    fn grant(&self, n: u32) {
        let mut g = self.credit.lock().unwrap();
        g.0 = g.0.saturating_add(n);
        self.cv.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.credit.lock().unwrap().1
    }

    fn close(&self) {
        self.credit.lock().unwrap().1 = true;
        self.cv.notify_all();
        let _ = self.inbox.send(Inbound::Close);
        self.sock.shutdown(Shutdown::Both);
    }

    /// Records an EOF in one direction and reports whether both directions are now done.
    fn eof(&self, sent: bool) -> bool {
        let mut g = self.eofs.lock().unwrap();
        if sent {
            g.0 = true;
        } else {
            g.1 = true;
        }
        g.0 && g.1
    }
}

pub struct Mux {
    out: Mutex<Option<Sender<Vec<u8>>>>,
    handler: Arc<dyn Handler>,
    chans: Mutex<HashMap<u32, Arc<Chan>>>,
    calls: Mutex<HashMap<u32, Sender<Result<Value, String>>>>,
    opens: Mutex<HashMap<u32, Sender<Result<(), String>>>>,
    next: AtomicU32,
    closed: AtomicBool,
    heard: Mutex<Instant>,
}

impl Mux {
    /// Starts the mux over `reader` and `writer`. `agent` picks the odd half of the id space.
    pub fn start(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        handler: Arc<dyn Handler>,
        agent: bool,
    ) -> Arc<Mux> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let mux = Arc::new(Mux {
            out: Mutex::new(Some(tx)),
            handler,
            chans: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
            opens: Mutex::new(HashMap::new()),
            next: AtomicU32::new(if agent { 1 } else { 2 }),
            closed: AtomicBool::new(false),
            heard: Mutex::new(Instant::now()),
        });
        thread::spawn(move || write_frames(writer, rx));
        let m = mux.clone();
        thread::spawn(move || {
            let _ = m.demux(reader);
            m.end();
        });
        let m = mux.clone();
        thread::spawn(move || {
            while !m.is_closed() {
                thread::sleep(PING_EVERY);
                m.send(PING, 0, &[]);
                if m.heard.lock().unwrap().elapsed() > TIMEOUT {
                    m.end();
                }
            }
        });
        mux
    }

    fn end(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.teardown();
            self.handler.closed();
        }
    }

    fn next_id(&self) -> u32 {
        self.next.fetch_add(2, Ordering::Relaxed)
    }

    fn send(&self, kind: u8, id: u32, payload: &[u8]) {
        let mut frame = Vec::with_capacity(9 + payload.len());
        frame.push(kind);
        frame.extend_from_slice(&id.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        if let Some(tx) = self.out.lock().unwrap().as_ref() {
            let _ = tx.send(frame);
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Stops sending, which the other side sees as the end of its input.
    pub fn shutdown(&self) {
        self.out.lock().unwrap().take();
    }

    pub fn call<T: DeserializeOwned>(&self, call: &impl Serialize) -> Result<T, String> {
        let id = self.next_id();
        let (tx, rx) = mpsc::channel();
        self.calls.lock().unwrap().insert(id, tx);
        if self.is_closed() {
            self.calls.lock().unwrap().remove(&id);
            return Err("the connection to the agent closed".into());
        }
        self.send(CALL, id, &serde_json::to_vec(call).unwrap());
        let value = rx.recv().map_err(|_| "the connection to the agent closed".to_string())??;
        serde_json::from_value(value).map_err(|e| format!("unexpected reply from the agent: {e}"))
    }

    /// Only the agent sends events.
    #[cfg(target_os = "linux")]
    pub fn event(&self, event: &impl Serialize) {
        self.send(EVENT, 0, &serde_json::to_vec(event).unwrap());
    }

    /// Asks the other side to connect a new channel to `target`, and joins `sock` to it.
    /// `respond` runs before any data flows, with the outcome, so a protocol such as SOCKS
    /// can answer its client first.
    pub fn open(
        self: &Arc<Self>,
        target: &impl Serialize,
        mut sock: Sock,
        respond: impl FnOnce(&mut Sock, Result<(), &str>),
    ) -> Result<(), String> {
        let id = self.next_id();
        let (tx, rx) = mpsc::channel();
        self.opens.lock().unwrap().insert(id, tx);
        let (chan, inbox) = match self.insert(id, &sock) {
            Ok(c) => c,
            Err(e) => {
                self.opens.lock().unwrap().remove(&id);
                respond(&mut sock, Err(&e));
                return Err(e);
            }
        };
        if self.is_closed() {
            self.opens.lock().unwrap().remove(&id);
            self.chans.lock().unwrap().remove(&id);
            respond(&mut sock, Err("other: the connection to the agent closed"));
            return Err("the connection to the agent closed".into());
        }
        self.send(OPEN, id, &serde_json::to_vec(target).unwrap());
        let result = rx.recv().unwrap_or_else(|_| Err("the connection to the agent closed".into()));
        match result {
            Ok(()) => {
                respond(&mut sock, Ok(()));
                self.run(id, chan, inbox, sock);
                Ok(())
            }
            Err(e) => {
                self.chans.lock().unwrap().remove(&id);
                respond(&mut sock, Err(&e));
                Err(e)
            }
        }
    }

    fn insert(&self, id: u32, sock: &Sock) -> Result<(Arc<Chan>, Receiver<Inbound>), String> {
        let (tx, rx) = mpsc::channel();
        let chan = Arc::new(Chan {
            credit: Mutex::new((WINDOW_SIZE, false)),
            cv: Condvar::new(),
            inbox: tx,
            unacked: AtomicU32::new(0),
            eofs: Mutex::new((false, false)),
            sock: sock.try_clone().map_err(|e| format!("other: {e}"))?,
        });
        self.chans.lock().unwrap().insert(id, chan.clone());
        Ok((chan, rx))
    }

    fn chan(&self, id: u32) -> Option<Arc<Chan>> {
        self.chans.lock().unwrap().get(&id).cloned()
    }

    /// Starts the threads that copy between `sock` and the channel.
    fn run(self: &Arc<Self>, id: u32, chan: Arc<Chan>, inbox: Receiver<Inbound>, sock: Sock) {
        let mut w = match sock.try_clone() {
            Ok(w) => w,
            Err(_) => return self.close_chan(id, true),
        };
        let (mux, c) = (self.clone(), chan.clone());
        thread::spawn(move || {
            for msg in inbox {
                match msg {
                    Inbound::Data(d) => {
                        if w.write_all(&d).is_err() {
                            mux.close_chan(id, true);
                            break;
                        }
                        c.unacked.fetch_sub(d.len() as u32, Ordering::Relaxed);
                        mux.send(WINDOW, id, &(d.len() as u32).to_be_bytes());
                    }
                    Inbound::Eof => {
                        w.shutdown(Shutdown::Write);
                        if c.eof(false) {
                            mux.finish(id);
                        }
                    }
                    Inbound::Close => break,
                }
            }
        });
        let mux = self.clone();
        let mut r = sock;
        thread::spawn(move || {
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = match r.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return mux.close_chan(id, true),
                };
                if chan.is_closed() {
                    return;
                }
                if n == 0 {
                    mux.send(EOF, id, &[]);
                    if chan.eof(true) {
                        mux.finish(id);
                    }
                    return;
                }
                let mut off = 0;
                while off < n {
                    let k = chan.take(n - off);
                    if k == 0 {
                        return;
                    }
                    mux.send(DATA, id, &buf[off..off + k]);
                    off += k;
                }
            }
        });
    }

    /// Ends a channel whose two directions both reached EOF.
    fn finish(&self, id: u32) {
        if let Some(c) = self.chans.lock().unwrap().remove(&id) {
            c.close();
        }
    }

    fn close_chan(&self, id: u32, notify: bool) {
        let removed = self.chans.lock().unwrap().remove(&id);
        if let Some(c) = removed {
            if notify {
                self.send(CLOSE, id, &[]);
            }
            c.close();
        }
    }

    fn accept_open(self: &Arc<Self>, id: u32, payload: Vec<u8>) {
        let target = match serde_json::from_slice::<Value>(&payload) {
            Ok(t) => t,
            Err(e) => return self.send(OPEN_ERR, id, format!("other: {e}").as_bytes()),
        };
        match self.handler.open(self, target) {
            Ok(sock) => match self.insert(id, &sock) {
                Ok((chan, inbox)) => {
                    self.send(OPEN_OK, id, &[]);
                    self.run(id, chan, inbox, sock);
                }
                Err(e) => self.send(OPEN_ERR, id, e.as_bytes()),
            },
            Err(e) => self.send(OPEN_ERR, id, e.as_bytes()),
        }
    }

    fn demux(self: &Arc<Self>, mut r: impl Read) -> io::Result<()> {
        let mut hdr = [0u8; 9];
        loop {
            r.read_exact(&mut hdr)?;
            let kind = hdr[0];
            let id = u32::from_be_bytes(hdr[1..5].try_into().unwrap());
            let len = u32::from_be_bytes(hdr[5..9].try_into().unwrap()) as usize;
            if len > MAX_PAYLOAD {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
            }
            let mut payload = vec![0u8; len];
            r.read_exact(&mut payload)?;
            *self.heard.lock().unwrap() = Instant::now();
            match kind {
                CALL => {
                    let mux = self.clone();
                    thread::spawn(move || {
                        let result = serde_json::from_slice::<Value>(&payload)
                            .map_err(|e| e.to_string())
                            .and_then(|call| mux.handler.call(&mux, call));
                        let body = match result {
                            Ok(v) => json!({ "ok": v }),
                            Err(e) => json!({ "err": e }),
                        };
                        mux.send(REPLY, id, &serde_json::to_vec(&body).unwrap());
                    });
                }
                REPLY => {
                    if let Some(tx) = self.calls.lock().unwrap().remove(&id) {
                        let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                        let result = match v.get("ok") {
                            Some(ok) => Ok(ok.clone()),
                            None => Err(v.get("err").and_then(Value::as_str).unwrap_or("malformed reply").to_string()),
                        };
                        let _ = tx.send(result);
                    }
                }
                EVENT => {
                    if let Ok(v) = serde_json::from_slice(&payload) {
                        self.handler.event(v);
                    }
                }
                OPEN => {
                    let mux = self.clone();
                    thread::spawn(move || mux.accept_open(id, payload));
                }
                OPEN_OK | OPEN_ERR => {
                    if let Some(tx) = self.opens.lock().unwrap().remove(&id) {
                        let _ = tx.send(if kind == OPEN_OK {
                            Ok(())
                        } else {
                            Err(String::from_utf8_lossy(&payload).into_owned())
                        });
                    }
                }
                DATA => {
                    if let Some(c) = self.chan(id) {
                        let pending = c.unacked.fetch_add(len as u32, Ordering::Relaxed) as usize + len;
                        if pending > WINDOW_SIZE as usize {
                            self.close_chan(id, true);
                        } else {
                            let _ = c.inbox.send(Inbound::Data(payload));
                        }
                    }
                }
                EOF => {
                    if let Some(c) = self.chan(id) {
                        let _ = c.inbox.send(Inbound::Eof);
                    }
                }
                CLOSE => self.close_chan(id, false),
                WINDOW => {
                    if let (Some(c), Ok(n)) = (self.chan(id), <[u8; 4]>::try_from(payload.as_slice())) {
                        c.grant(u32::from_be_bytes(n));
                    }
                }
                PING => {}
                _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown frame kind")),
            }
        }
    }

    fn teardown(&self) {
        for (_, tx) in self.calls.lock().unwrap().drain() {
            let _ = tx.send(Err("the connection to the agent closed".into()));
        }
        for (_, tx) in self.opens.lock().unwrap().drain() {
            let _ = tx.send(Err("other: the connection to the agent closed".into()));
        }
        let chans: Vec<_> = self.chans.lock().unwrap().drain().map(|(_, c)| c).collect();
        for c in chans {
            c.close();
        }
        self.shutdown();
    }
}

fn write_frames(writer: impl Write, rx: Receiver<Vec<u8>>) {
    let mut w = io::BufWriter::with_capacity(64 << 10, writer);
    while let Ok(frame) = rx.recv() {
        if w.write_all(&frame).is_err() {
            return;
        }
        while let Ok(frame) = rx.try_recv() {
            if w.write_all(&frame).is_err() {
                return;
            }
        }
        if w.flush().is_err() {
            return;
        }
    }
}
