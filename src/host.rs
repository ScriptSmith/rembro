//! rembro's side of a session: the agent connection, forwards, signals and shared steps.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{SocketAddr, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::boot::{self, AgentBin};
use crate::browser::Family;
use crate::cli::{Cli, Command, Common};
use crate::mux::{Handler, Mux, Sock};
use crate::proto::{Addr, Call, Event, Info, Open, PROTOCOL};
use crate::target::Target;
use crate::util::PortSpec;
use crate::{attach, install, local, remote, run};
use crate::{sys, util, x11};

pub const DEBUG_ID: u32 = 1;
pub const WAYLAND_ID: u32 = 2;
pub const X11_ID: u32 = 3;
pub const PULSE_ID: u32 = 4;

pub enum HostEvent {
    Agent(Event),
    AgentClosed,
    Signal(i32),
    BrowserExited(Option<i32>, Option<i32>),
    /// The port watcher found a new HTTP server.
    NewServer(String),
    Reconnected(Box<Link>),
}

/// Something on this machine that connections accepted in the target are joined to.
pub enum Endpoint {
    Tcp(SocketAddr),
    Unix(PathBuf),
    /// The X server, for clients that present this cookie.
    X11(x11::Display, Option<Vec<u8>>),
    Pending(&'static str),
}

pub struct HostHandler {
    forwards: Mutex<HashMap<u32, Endpoint>>,
    events: Sender<HostEvent>,
}

impl HostHandler {
    pub fn set_forward(&self, id: u32, endpoint: Endpoint) {
        self.forwards.lock().unwrap().insert(id, endpoint);
    }
}

impl Handler for HostHandler {
    fn call(&self, _mux: &Arc<Mux>, _call: Value) -> Result<Value, String> {
        Err("rembro does not take requests from the agent".into())
    }

    // The agent can reach only the endpoints this session registered.
    fn open(&self, _mux: &Arc<Mux>, target: Value) -> Result<Sock, String> {
        let Ok(Open::Forward { id }) = serde_json::from_value::<Open>(target) else {
            return Err("other: not permitted".into());
        };
        let forwards = self.forwards.lock().unwrap();
        match forwards.get(&id) {
            Some(Endpoint::Tcp(a)) => TcpStream::connect(a)
                .map(|s| {
                    let _ = s.set_nodelay(true);
                    Sock::Tcp(s)
                })
                .map_err(|e| format!("refused: {e}")),
            #[cfg(unix)]
            Some(Endpoint::Unix(p)) => UnixStream::connect(p).map(Sock::Unix).map_err(|e| format!("refused: {e}")),
            #[cfg(not(unix))]
            Some(Endpoint::Unix(p)) => Err(format!("refused: cannot connect to {} here", p.display())),
            Some(Endpoint::X11(d, cookie)) => d.gate(cookie.clone()),
            Some(Endpoint::Pending(why)) => Err(format!("refused: {why}")),
            None => Err("other: not permitted".into()),
        }
    }

    fn event(&self, event: Value) {
        match serde_json::from_value(event) {
            // The agent checks URLs too, but a compromised target could send anything, and a
            // "URL" such as --renderer-cmd-prefix=... would reach the browser as an option.
            Ok(Event::Open { url }) if !util::is_web_url(&url) => {
                eprintln!("rembro: ignored a request from the target to open something that is not an http(s) URL");
            }
            Ok(e) => {
                let _ = self.events.send(HostEvent::Agent(e));
            }
            Err(_) => {}
        }
    }

    fn closed(&self) {
        let _ = self.events.send(HostEvent::AgentClosed);
    }
}

/// One connection to an agent: its mux and the transport process carrying it.
pub struct Link {
    mux: Arc<Mux>,
    child: Child,
    info: Info,
}

impl Link {
    fn start(target: &Target, bin: &AgentBin, handler: &Arc<HostHandler>, verbose: bool) -> Result<Link, String> {
        let started = boot::start(target, bin, verbose)?;
        let mux = Mux::start(started.stdout, started.stdin, handler.clone(), false);
        let mut child = started.child;
        let info: Info = match mux.call(&Call::Info) {
            Ok(i) => i,
            Err(e) => {
                mux.shutdown();
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the agent did not answer: {e}"));
            }
        };
        if info.protocol != PROTOCOL {
            mux.shutdown();
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "the agent speaks protocol {} and this rembro speaks {PROTOCOL}; pass a matching build with --agent",
                info.protocol
            ));
        }
        Ok(Link { mux, child, info })
    }

    fn close(mut self) {
        self.mux.shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The current mux, which a reconnect replaces.
pub struct Conn(RwLock<Arc<Mux>>);

impl Conn {
    pub fn mux(&self) -> Arc<Mux> {
        self.0.read().unwrap().clone()
    }

    pub fn call<T: serde::de::DeserializeOwned>(&self, call: &Call) -> Result<T, String> {
        self.mux().call(call)
    }
}

pub struct Session {
    pub conn: Arc<Conn>,
    link: Mutex<Option<Link>>,
    pub handler: Arc<HostHandler>,
    pub events: Receiver<HostEvent>,
    pub sender: Sender<HostEvent>,
    pub info: Info,
    pub target: Target,
    /// The target as the user wrote it.
    pub label: String,
    bin: Arc<AgentBin>,
    pub verbose: bool,
    pub quiet: bool,
    /// Tells background threads (the port watcher, reconnects) to stop.
    pub stopping: Arc<AtomicBool>,
}

impl Session {
    pub fn forward(&self, id: u32, endpoint: Endpoint) {
        self.handler.set_forward(id, endpoint);
    }

    pub fn call<T: serde::de::DeserializeOwned>(&self, call: &Call) -> Result<T, String> {
        self.conn.call(call)
    }

    /// Starts connecting again in the background; a `HostEvent::Reconnected` follows.
    pub fn reconnect(&self) {
        let (target, bin, handler, tx, stop) =
            (self.target.clone(), self.bin.clone(), self.handler.clone(), self.sender.clone(), self.stopping.clone());
        let (label, verbose) = (self.label.clone(), self.verbose);
        thread::spawn(move || {
            let mut delay = Duration::from_secs(1);
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(delay);
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                match Link::start(&target, &bin, &handler, verbose) {
                    Ok(link) => {
                        if stop.load(Ordering::SeqCst) {
                            link.close();
                        } else {
                            let _ = tx.send(HostEvent::Reconnected(Box::new(link)));
                        }
                        return;
                    }
                    Err(e) => {
                        if verbose || delay >= Duration::from_secs(8) {
                            eprintln!("rembro: still cannot reach {label}: {e}");
                        }
                        delay = (delay * 2).min(Duration::from_secs(15));
                    }
                }
            }
        });
    }

    /// Swaps in a new connection from `reconnect`.
    pub fn install(&self, link: Box<Link>) {
        *self.conn.0.write().unwrap() = link.mux.clone();
        if let Some(old) = self.link.lock().unwrap().replace(*link) {
            thread::spawn(move || old.close());
        }
    }

    fn close(self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(link) = self.link.lock().unwrap().take() {
            link.close();
        }
    }
}

/// Set once the session is up; before that a signal ends rembro at once.
static READY: AtomicBool = AtomicBool::new(false);

fn connect(target_arg: &str, common: &Common) -> Result<Session, String> {
    let target = Target::parse(target_arg)?;
    if target == Target::Local && !cfg!(target_os = "linux") {
        return Err(boot::LOCAL_NEEDS_LINUX.into());
    }
    let (tx, rx) = mpsc::channel();
    // This must come before any other thread starts.
    let signals = Mutex::new(tx.clone());
    sys::on_signals(move |sig| {
        if !READY.load(Ordering::SeqCst) {
            std::process::exit(128 + sig);
        }
        let _ = signals.lock().unwrap().send(HostEvent::Signal(sig));
    });
    let bin = Arc::new(AgentBin::load(common.agent.as_deref(), common.verbose)?);
    let handler = Arc::new(HostHandler { forwards: Mutex::new(HashMap::new()), events: tx.clone() });
    let link = Link::start(&target, &bin, &handler, common.verbose)?;
    let info = link.info.clone();
    if common.verbose {
        eprintln!(
            "rembro: agent running on {} ({}, uid {}{})",
            info.hostname,
            info.arch,
            info.uid,
            info.proxy.as_ref().map(|p| format!(", proxy {p}")).unwrap_or_default()
        );
    }
    READY.store(true, Ordering::SeqCst);
    Ok(Session {
        conn: Arc::new(Conn(RwLock::new(link.mux.clone()))),
        link: Mutex::new(Some(link)),
        handler,
        events: rx,
        sender: tx,
        info,
        target,
        label: target_arg.to_string(),
        bin,
        verbose: common.verbose,
        quiet: common.quiet,
        stopping: Arc::new(AtomicBool::new(false)),
    })
}

pub fn run(cli: Cli) -> Result<i32, String> {
    match &cli.command {
        Some(Command::Run(r)) => {
            let s = connect(&r.target, &cli.common)?;
            let result = run::run(r, &s, &cli.common);
            s.close();
            result
        }
        Some(Command::Install(i)) => {
            let s = connect(&i.target, &cli.common)?;
            let result = install::run(i, &s);
            s.close();
            result
        }
        None => {
            let b = &cli.browse;
            let target = b.target.as_deref().unwrap_or_default();
            let ports = PortSpec::parse(&b.ports)?;
            b.debug_port()?;
            let s = connect(target, &cli.common)?;
            let result = if b.remote { remote::run(b, &s, &ports) } else { local::run(b, &s, &ports) };
            s.close();
            result
        }
    }
}

/// URLs for the HTTP servers listening on the target's loopback, and the ports seen.
pub fn server_urls(s: &Session, spec: &PortSpec, exclude: &[u16]) -> Result<(Vec<String>, Vec<u16>), String> {
    if spec.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let listening: Vec<u16> = s.call(&Call::Listening)?;
    let ports: Vec<u16> = listening.iter().copied().filter(|p| spec.contains(*p) && !exclude.contains(p)).collect();
    let http: Vec<u16> = if ports.is_empty() { Vec::new() } else { s.call(&Call::ProbeHttp { ports })? };
    if http.is_empty() {
        eprintln!("rembro: no HTTP servers are listening on {}'s loopback", s.label);
    }
    Ok((http.into_iter().map(|p| format!("http://localhost:{p}")).collect(), listening))
}

/// Polls the target for new listening ports, and reports those that answer HTTP.
/// `seen` holds the ports that were listening at the start; None fetches them now.
pub fn watch_ports(s: &Session, spec: PortSpec, exclude: Vec<u16>, seen: Option<Vec<u16>>) {
    if spec.is_empty() {
        return;
    }
    let seen = match seen {
        Some(seen) => seen,
        None => s.call(&Call::Listening).unwrap_or_default(),
    };
    let (conn, tx, stop) = (s.conn.clone(), s.sender.clone(), s.stopping.clone());
    thread::spawn(move || {
        let mut seen: HashSet<u16> = seen.into_iter().collect();
        // New ports that did not answer HTTP yet, with when they were first seen.
        let mut pending: HashMap<u16, Instant> = HashMap::new();
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_secs(2));
            let Ok(listening) = conn.call::<Vec<u16>>(&Call::Listening) else { continue };
            for p in listening {
                if !seen.contains(&p) && spec.contains(p) && !exclude.contains(&p) {
                    pending.entry(p).or_insert_with(Instant::now);
                }
                seen.insert(p);
            }
            if pending.is_empty() {
                continue;
            }
            let ports: Vec<u16> = pending.keys().copied().collect();
            let Ok(http) = conn.call::<Vec<u16>>(&Call::ProbeHttp { ports }) else { continue };
            for p in http {
                pending.remove(&p);
                let _ = tx.send(HostEvent::NewServer(format!("http://localhost:{p}")));
            }
            // Give a server 20 seconds to start answering before giving up on it.
            pending.retain(|_, since| since.elapsed() < Duration::from_secs(20));
        }
    });
}

/// Limits how often programs in the target can open tabs.
pub struct Limiter(VecDeque<Instant>);

impl Limiter {
    pub fn new() -> Limiter {
        Limiter(VecDeque::new())
    }

    pub fn allow(&mut self) -> bool {
        let now = Instant::now();
        while self.0.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(10)) {
            self.0.pop_front();
        }
        if self.0.len() >= 10 {
            return false;
        }
        self.0.push_back(now);
        true
    }
}

/// Listens for debugging clients on the target's loopback and returns the port: `port`,
/// or with `fallback`, any free one if that is taken.
pub fn debug_listener(s: &Session, port: u16, fallback: bool) -> Result<u16, String> {
    let listen =
        |host: &str, port| s.call::<u16>(&Call::Listen { id: DEBUG_ID, addr: Addr::Tcp { host: host.into(), port } });
    let port = match listen("127.0.0.1", port) {
        Ok(p) => p,
        Err(_) if fallback => listen("127.0.0.1", 0)?,
        Err(e) => return Err(e),
    };
    // Clients that resolve localhost to ::1 first find it there too.
    let _ = listen("::1", port);
    Ok(port)
}

/// Prints how to connect from the target, and returns the page that says the same.
pub fn announce_debug(s: &Session, family: Family, port: u16) -> Option<String> {
    let (protocol, url) = crate::browser::debug_endpoint(family, port)?;
    let commands = attach::commands(family, &url);
    println!("Programs in {} can drive this browser over {protocol} at {url}", s.label);
    if let Some(first) = commands.first() {
        println!("  {first}");
    }
    Some(attach::page(&s.label, protocol, &url, &commands))
}

/// Asks the program to exit, and kills it if it has not within a few seconds.
pub fn wait_exit(s: &Session, mut exited: impl FnMut(&HostEvent) -> bool, term: impl Fn(i32)) {
    term(sys::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match s.events.recv_timeout(left) {
            Ok(e) if exited(&e) => return,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    term(sys::SIGKILL);
}

pub fn exit_code(code: Option<i32>, signal: Option<i32>) -> i32 {
    code.unwrap_or_else(|| 128 + signal.unwrap_or(0))
}

/// Prints a program's output line from the target to the matching stream here.
pub fn relay_output(stream: u8, text: &str) {
    use std::io::Write;
    if stream == 1 {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    } else {
        let _ = std::io::stderr().lock().write_all(text.as_bytes());
    }
}
