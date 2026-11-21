//! The side of rembro that runs in the target, started as `rembro agent`.
//!
//! It speaks the mux protocol on the stdin and stdout it was given and exits, taking the
//! processes it started with it, when rembro goes away.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::mux::{Handler, Mux, Sock};
use crate::proto::{Addr, Call, Event, Info, Open, PROTOCOL};
use crate::util;

pub fn self_hash() -> io::Result<String> {
    Ok(format!("{:016x}", util::fnv64(&fs::read("/proc/self/exe")?)))
}

pub fn main(args: &[String]) -> ! {
    if args.iter().any(|a| a == "--check") {
        std::process::exit(0);
    }
    let hash = match self_hash() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("rembro agent: cannot read its own executable: {e}");
            std::process::exit(1);
        }
    };
    // Keep the protocol's pipes for the mux, and point fds 0 and 1 elsewhere so that
    // nothing else can read or write them by accident.
    let (input, mut output) = unsafe {
        let i = libc::dup(0);
        let o = libc::dup(1);
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        libc::dup2(null, 0);
        libc::dup2(2, 1);
        libc::fcntl(i, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(o, libc::F_SETFD, libc::FD_CLOEXEC);
        (File::from_raw_fd(i), File::from_raw_fd(o))
    };
    if writeln!(output, "rembro-agent {hash}").and_then(|_| output.flush()).is_err() {
        std::process::exit(1);
    }

    let (closed_tx, closed_rx) = mpsc::channel();
    let agent = Arc::new(Agent {
        procs: Mutex::new(HashMap::new()),
        groups: Mutex::new(Vec::new()),
        dirs: Mutex::new(Vec::new()),
        files: Mutex::new(Vec::new()),
        shim: Mutex::new(None),
        next_proc: AtomicU32::new(1),
        target_proxy: AtomicBool::new(false),
        proxy: ProxyEnv::from_env(),
        spawner: spawner(),
        closed: Mutex::new(Some(closed_tx)),
    });
    let _mux = Mux::start(input, output, Arc::new(agent.clone()), true);
    let _ = closed_rx.recv();
    agent.cleanup();
    std::process::exit(0);
}

struct Agent {
    /// Running processes by id, as process group ids.
    procs: Mutex<HashMap<u32, i32>>,
    /// Every process group started, to signal at exit even after their leaders exit.
    groups: Mutex<Vec<i32>>,
    dirs: Mutex<Vec<PathBuf>>,
    /// Files to delete at exit, such as the URL socket.
    files: Mutex<Vec<PathBuf>>,
    /// The directory with the BROWSER and xdg-open commands, once `Call::Shim` set them up.
    shim: Mutex<Option<PathBuf>>,
    next_proc: AtomicU32,
    target_proxy: AtomicBool,
    proxy: ProxyEnv,
    spawner: Sender<(Command, Sender<io::Result<std::process::Child>>)>,
    closed: Mutex<Option<Sender<()>>>,
}

/// Starts processes from one thread that lives as long as the agent, because
/// PR_SET_PDEATHSIG fires when the thread that forked a child exits.
fn spawner() -> Sender<(Command, Sender<io::Result<std::process::Child>>)> {
    let (tx, rx) = mpsc::channel::<(Command, Sender<io::Result<std::process::Child>>)>();
    thread::spawn(move || {
        for (mut cmd, reply) in rx {
            let _ = reply.send(cmd.spawn());
        }
    });
    tx
}

impl Agent {
    fn cleanup(&self) {
        let groups = self.groups.lock().unwrap().clone();
        for &g in &groups {
            unsafe { libc::kill(-g, libc::SIGTERM) };
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !self.procs.lock().unwrap().is_empty() {
            thread::sleep(Duration::from_millis(50));
        }
        for &g in &groups {
            unsafe { libc::kill(-g, libc::SIGKILL) };
        }
        for d in self.dirs.lock().unwrap().iter() {
            let _ = fs::remove_dir_all(d);
        }
        for f in self.files.lock().unwrap().iter() {
            let _ = fs::remove_file(f);
        }
    }

    fn home(&self) -> Option<String> {
        util::home()
    }

    fn path(&self, p: &str) -> Result<PathBuf, String> {
        let path = util::expand_home(p, self.home().as_deref());
        if !path.is_absolute() {
            return Err(format!("{p} is not an absolute path"));
        }
        Ok(path)
    }

    fn info(&self) -> Info {
        let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
        let arch = if unsafe { libc::uname(&mut uts) } == 0 {
            unsafe { std::ffi::CStr::from_ptr(uts.machine.as_ptr()) }.to_string_lossy().into_owned()
        } else {
            String::new()
        };
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok().filter(|d| {
            d.starts_with('/')
                && fs::metadata(d).is_ok_and(|m| m.is_dir())
                && unsafe { libc::access(std::ffi::CString::new(d.as_str()).unwrap().as_ptr(), libc::W_OK) } == 0
        });
        let shm_bytes = std::ffi::CString::new("/dev/shm").ok().and_then(|p| {
            let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
            (unsafe { libc::statvfs(p.as_ptr(), &mut st) } == 0).then(|| st.f_blocks as u64 * st.f_frsize as u64)
        });
        Info {
            arch,
            uid: util::uid(),
            home: self.home(),
            runtime_dir,
            hostname: util::hostname(),
            shm_bytes,
            proxy: self.proxy.https.as_ref().or(self.proxy.http.as_ref()).map(|p| format!("{}:{}", p.host, p.port)),
            protocol: PROTOCOL,
            agent_dir: agent_dir().to_string_lossy().into_owned(),
            os: os_ids(),
        }
    }

    fn handle(self: &Arc<Self>, mux: &Arc<Mux>, call: Call) -> Result<Value, String> {
        Ok(match call {
            Call::Info => json!(self.info()),
            Call::Configure { target_proxy } => {
                self.target_proxy.store(target_proxy, Ordering::Relaxed);
                Value::Null
            }
            Call::Find { groups } => {
                let home = self.home();
                for (i, group) in groups.iter().enumerate() {
                    for cand in group {
                        if let Some(p) = util::find_executable(cand, home.as_deref()) {
                            return Ok(json!([i, p.to_string_lossy()]));
                        }
                    }
                }
                Value::Null
            }
            Call::Mkdtemp => {
                let dir = util::mkdtemp(&self.runtime_base(), "rembro-").map_err(|e| format!("mkdtemp: {e}"))?;
                self.dirs.lock().unwrap().push(dir.clone());
                json!(dir.to_string_lossy())
            }
            Call::Mkdir { path } => {
                let path = self.path(&path)?;
                fs::create_dir_all(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
                json!(path.to_string_lossy())
            }
            Call::ReadFile { path } => match fs::read(self.path(&path)?) {
                Ok(data) => json!(data),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Value::Null,
                Err(e) => return Err(format!("cannot read {path}: {e}")),
            },
            Call::WriteFile { path, data, mode } => {
                let path = self.path(&path)?;
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let mut f = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(mode)
                    .open(&path)
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                f.write_all(&data).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                json!(path.to_string_lossy())
            }
            Call::Listen { id, addr } => json!(self.listen(mux, id, addr)?),
            Call::Listening => {
                let mut ports: Vec<u16> = util::listening()
                    .into_iter()
                    .filter(|l| util::is_loopback_or_any(&l.addr))
                    .map(|l| l.port)
                    .collect();
                ports.sort_unstable();
                ports.dedup();
                json!(ports)
            }
            Call::ProbeHttp { ports } => json!(probe_http(&ports)),
            Call::PortFree { port } => json!(TcpListener::bind(("127.0.0.1", port)).is_ok()),
            Call::FreePort => {
                let l = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
                json!(l.local_addr().map_err(|e| e.to_string())?.port())
            }
            Call::Spawn { argv, mut env, unset, shim } => {
                if shim && let Some(bin) = self.shim.lock().unwrap().clone() {
                    let path = env
                        .iter()
                        .find(|(k, _)| k == "PATH")
                        .map(|(_, v)| v.clone())
                        .or_else(|| std::env::var("PATH").ok())
                        .unwrap_or_default();
                    env.retain(|(k, _)| k != "PATH" && k != "BROWSER");
                    env.push(("PATH".into(), format!("{}:{path}", bin.display())));
                    env.push(("BROWSER".into(), bin.join("rembro-open").to_string_lossy().into_owned()));
                }
                json!(self.spawn(mux, argv, env, unset)?)
            }
            Call::Kill { proc, signal } => {
                if let Some(&pg) = self.procs.lock().unwrap().get(&proc) {
                    unsafe { libc::kill(-pg, signal) };
                }
                Value::Null
            }
            Call::Stat { path } => json!(fs::metadata(self.path(&path)?).ok().map(|m| m.len())),
            Call::Shim => json!(self.shim(mux)?.to_string_lossy()),
            Call::ExtraCas => json!(extra_cas()),
            Call::Capture { argv, timeout_ms } => json!(capture(&argv, Duration::from_millis(timeout_ms))?),
        })
    }

    /// Writes the BROWSER and xdg-open commands, and listens for the URLs they send.
    fn shim(&self, mux: &Arc<Mux>) -> Result<PathBuf, String> {
        let mut shim = self.shim.lock().unwrap();
        if let Some(bin) = &*shim {
            return Ok(bin.clone());
        }
        let exe = fs::read_link("/proc/self/exe").map_err(|e| format!("cannot find the agent: {e}"))?;
        let bin = agent_dir().join("bin");
        fs::create_dir_all(&bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
        for (name, fallback) in [("rembro-open", ""), ("xdg-open", "--fallback xdg-open ")] {
            let script = format!("#!/bin/sh\nexec {} open {fallback}\"$@\"\n", util::sh_quote(&exe.to_string_lossy()));
            let path = bin.join(name);
            let tmp = bin.join(format!(".{name}.{}", std::process::id()));
            fs::write(&tmp, script)
                .and_then(|_| fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)))
                .and_then(|_| fs::rename(&tmp, &path))
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        let dir = socket_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref())
            .ok_or("no private directory for the URL socket")?;
        let path = dir.join(format!("open-{}.sock", std::process::id()));
        let _ = fs::remove_file(&path);
        let l = UnixListener::bind(&path).map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        self.files.lock().unwrap().push(path);
        let mux = mux.clone();
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mux = mux.clone();
                thread::spawn(move || {
                    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut line = Vec::new();
                    let _ = io::BufReader::new(&s).take(8193).read_until(b'\n', &mut line);
                    let url = String::from_utf8_lossy(&line).trim().to_string();
                    let reply = if is_web_url(&url) {
                        mux.event(&Event::Open { url });
                        "ok\n"
                    } else {
                        "only http and https URLs can be opened\n"
                    };
                    let _ = (&s).write_all(reply.as_bytes());
                });
            }
        });
        *shim = Some(bin.clone());
        Ok(bin)
    }

    fn runtime_base(&self) -> PathBuf {
        match self.info().runtime_dir {
            Some(d) => PathBuf::from(d),
            None => std::env::temp_dir(),
        }
    }

    fn listen(self: &Arc<Self>, mux: &Arc<Mux>, id: u32, addr: Addr) -> Result<u16, String> {
        match addr {
            Addr::Tcp { host, port } => {
                let l = TcpListener::bind((host.as_str(), port))
                    .map_err(|e| format!("cannot listen on {host}:{port}: {e}"))?;
                let port = l.local_addr().map_err(|e| e.to_string())?.port();
                let mux = mux.clone();
                thread::spawn(move || {
                    for s in l.incoming().flatten() {
                        let _ = s.set_nodelay(true);
                        let mux = mux.clone();
                        thread::spawn(move || {
                            let _ = mux.open(&Open::Forward { id }, Sock::Tcp(s), |_, _| ());
                        });
                    }
                });
                Ok(port)
            }
            Addr::Unix { path } => {
                let path = self.path(&path)?;
                if fs::symlink_metadata(&path).is_ok_and(|m| {
                    use std::os::unix::fs::FileTypeExt;
                    m.file_type().is_socket()
                }) {
                    let _ = fs::remove_file(&path);
                }
                let l = UnixListener::bind(&path).map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
                let mux = mux.clone();
                thread::spawn(move || {
                    for s in l.incoming().flatten() {
                        let mux = mux.clone();
                        thread::spawn(move || {
                            let _ = mux.open(&Open::Forward { id }, Sock::Unix(s), |_, _| ());
                        });
                    }
                });
                Ok(0)
            }
        }
    }

    fn spawn(
        self: &Arc<Self>,
        mux: &Arc<Mux>,
        argv: Vec<String>,
        env: Vec<(String, String)>,
        unset: Vec<String>,
    ) -> Result<u32, String> {
        if argv.is_empty() {
            return Err("nothing to run".into());
        }
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
        for k in &unset {
            cmd.env_remove(k);
        }
        cmd.envs(env);
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        util::unblock_signals(&mut cmd);
        let (tx, rx) = mpsc::channel();
        self.spawner.send((cmd, tx)).map_err(|_| "the agent is shutting down".to_string())?;
        let mut child = rx
            .recv()
            .map_err(|_| "the agent is shutting down".to_string())?
            .map_err(|e| format!("{}: {e}", argv[0]))?;
        let id = self.next_proc.fetch_add(1, Ordering::Relaxed);
        let pid = child.id() as i32;
        self.procs.lock().unwrap().insert(id, pid);
        self.groups.lock().unwrap().push(pid);

        let mut readers = Vec::new();
        for (stream, pipe) in [
            (1, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
            (2, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        ] {
            let Some(pipe) = pipe else { continue };
            let mux = mux.clone();
            readers.push(thread::spawn(move || {
                let mut r = io::BufReader::new(pipe);
                let mut line = Vec::new();
                while matches!(r.read_until(b'\n', &mut line), Ok(n) if n > 0) {
                    mux.event(&Event::Output { proc: id, stream, text: String::from_utf8_lossy(&line).into_owned() });
                    line.clear();
                }
            }));
        }
        let (me, mux) = (self.clone(), mux.clone());
        thread::spawn(move || {
            let status = child.wait();
            for r in readers {
                let _ = r.join();
            }
            me.procs.lock().unwrap().remove(&id);
            let (code, signal) = match status {
                Ok(s) => (s.code(), s.signal()),
                Err(_) => (None, None),
            };
            mux.event(&Event::Exited { proc: id, code, signal });
        });
        Ok(id)
    }

    fn connect(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        if self.target_proxy.load(Ordering::Relaxed)
            && let Some(proxy) = self.proxy.for_host(host, port)
        {
            return connect_via(proxy, host, port);
        }
        connect_direct(host, port)
    }
}

// Implemented on the Arc because calls start threads that need their own reference.
impl Handler for Arc<Agent> {
    fn call(&self, mux: &Arc<Mux>, call: Value) -> Result<Value, String> {
        let call: Call = serde_json::from_value(call).map_err(|e| format!("unknown request: {e}"))?;
        self.handle(mux, call)
    }

    fn open(&self, _mux: &Arc<Mux>, target: Value) -> Result<Sock, String> {
        match serde_json::from_value::<Open>(target).map_err(|e| format!("other: {e}"))? {
            Open::Tcp { host, port } => self.connect(&host, port).map(Sock::Tcp),
            Open::Forward { .. } => Err("other: the agent does not accept forwards".into()),
            Open::Store { path, size, mode } => {
                let path = self.path(&path).map_err(|e| format!("other: {e}"))?;
                let (a, b) = UnixStream::pair().map_err(|e| format!("other: {e}"))?;
                thread::spawn(move || store(b, &path, size, mode));
                Ok(Sock::Unix(a))
            }
        }
    }

    fn event(&self, _event: Value) {}

    fn closed(&self) {
        if let Some(tx) = self.closed.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

/// Runs a command for at most `timeout`, returning its exit code and up to 64 KiB of stdout.
fn capture(argv: &[String], timeout: Duration) -> Result<(Option<i32>, String), String> {
    let Some((prog, args)) = argv.split_first() else { return Err("nothing to run".into()) };
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    util::unblock_signals(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    let out = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut data = Vec::new();
        let _ = out.take(64 << 10).read_to_end(&mut data);
        data
    });
    let deadline = Instant::now() + timeout;
    let code = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status.code();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ok((code, String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned()))
}

/// The directory the agent binary is in.
fn agent_dir() -> PathBuf {
    fs::read_link("/proc/self/exe")
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir)
}

fn os_ids() -> Vec<String> {
    let text = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut ids = Vec::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ID=").or_else(|| line.strip_prefix("ID_LIKE=")) {
            ids.extend(v.trim_matches(['"', '\'']).split_whitespace().map(String::from));
        }
    }
    ids
}

/// Receives a file over a channel: exactly `size` bytes, then answers "ok" or an error.
fn store(mut s: UnixStream, path: &Path, size: u64, mode: u32) {
    let result = (|| -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_file_name(format!(
            ".{}.{}",
            path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
            std::process::id()
        ));
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&tmp)?;
        let n = io::copy(&mut (&mut s).take(size), &mut f)?;
        if n != size {
            let _ = fs::remove_file(&tmp);
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("got {n} of {size} bytes")));
        }
        f.sync_all()?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        fs::rename(&tmp, path)
    })();
    let reply = match result {
        Ok(()) => "ok\n".to_string(),
        Err(e) => format!("cannot write {}: {e}\n", path.display()),
    };
    let _ = s.write_all(reply.as_bytes());
    let _ = s.shutdown(std::net::Shutdown::Both);
}

/// CA certificates an administrator added to the target's trust store.
fn extra_cas() -> Vec<(String, String)> {
    let mut files = Vec::new();
    fn walk(dir: &Path, depth: u32, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 4 {
                walk(&p, depth + 1, files);
            } else if p.is_file() {
                files.push(p);
            }
        }
    }
    for dir in [
        "/usr/local/share/ca-certificates",
        "/etc/pki/ca-trust/source/anchors",
        "/etc/ca-certificates/trust-source/anchors",
        "/etc/pki/trust/anchors",
    ] {
        walk(Path::new(dir), 0, &mut files);
    }
    // NODE_EXTRA_CA_CERTS sometimes names the whole system bundle, which holds every public root.
    const SYSTEM_BUNDLES: &[&str] = &[
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/ssl/cert.pem",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/pki/tls/cert.pem",
        "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
        "/etc/ssl/ca-bundle.pem",
    ];
    if let Ok(f) = std::env::var("NODE_EXTRA_CA_CERTS") {
        let real = fs::canonicalize(&f).unwrap_or_else(|_| PathBuf::from(&f));
        if !SYSTEM_BUNDLES.iter().any(|b| fs::canonicalize(b).is_ok_and(|b| b == real)) {
            files.push(PathBuf::from(f));
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else { continue };
        if text.len() < 1 << 20 && text.contains("-----BEGIN CERTIFICATE-----") && !out.iter().any(|(_, t)| *t == text)
        {
            out.push((f.to_string_lossy().into_owned(), text));
        }
    }
    out
}

/// Whether `url` is an http or https URL with no spaces or control characters.
pub fn is_web_url(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && url.len() <= 8192
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A private per-user directory for the URL sockets: $XDG_RUNTIME_DIR/rembro, or
/// /tmp/rembro-UID. Returns None if the directory is not ours alone.
fn socket_dir(runtime: Option<&str>) -> Option<PathBuf> {
    let dir = match runtime {
        Some(r) if r.starts_with('/') && Path::new(r).is_dir() => Path::new(r).join("rembro"),
        _ => PathBuf::from(format!("/tmp/rembro-{}", util::uid())),
    };
    let _ = fs::create_dir(&dir);
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    let m = fs::symlink_metadata(&dir).ok()?;
    use std::os::unix::fs::MetadataExt;
    (m.is_dir() && m.uid() == util::uid() && m.mode() & 0o077 == 0).then_some(dir)
}

/// `rembro open [--fallback CMD] URL`, run by the BROWSER and xdg-open commands in the
/// target: hands the URL to the newest rembro session, or runs the real CMD.
pub fn open_main(args: &[String]) -> ! {
    let (fallback, args) = match args {
        [flag, cmd, rest @ ..] if flag == "--fallback" => (Some(cmd.as_str()), rest),
        _ => (None, args),
    };
    if let [url] = args
        && is_web_url(url)
    {
        match send_url(url) {
            Ok(()) => std::process::exit(0),
            Err(e) if fallback.is_none() => {
                eprintln!("rembro-open: {e}");
                std::process::exit(1);
            }
            Err(_) => {}
        }
    }
    if let Some(cmd) = fallback {
        // Run the real command: the next one in PATH that is not in our own directory.
        let own = agent_dir().join("bin");
        let path = std::env::var_os("PATH").unwrap_or_default();
        if let Some(real) =
            std::env::split_paths(&path).filter(|d| *d != own).map(|d| d.join(cmd)).find(|p| util::is_executable(p))
        {
            let err = Command::new(&real).args(args).exec();
            eprintln!("rembro-open: {}: {err}", real.display());
            std::process::exit(1);
        }
        eprintln!("rembro-open: no rembro session to open this in, and no other {cmd} in PATH");
    } else {
        eprintln!("usage: rembro-open http(s)://URL");
    }
    std::process::exit(1);
}

fn send_url(url: &str) -> Result<(), String> {
    let mut dirs = Vec::new();
    if let Some(d) = socket_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref()) {
        dirs.push(d);
    }
    if let Some(d) = socket_dir(None)
        && !dirs.contains(&d)
    {
        dirs.push(d);
    }
    let mut socks: Vec<(std::time::SystemTime, PathBuf)> = dirs
        .iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("open-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    socks.sort_by_key(|s| std::cmp::Reverse(s.0));
    for (_, path) in socks {
        let mut s = match UnixStream::connect(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                let _ = fs::remove_file(&path);
                continue;
            }
            Err(_) => continue,
        };
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        s.write_all(format!("{url}\n").as_bytes()).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        let _ = s.read_to_string(&mut reply);
        return if reply.trim() == "ok" { Ok(()) } else { Err(reply.trim().to_string()) };
    }
    Err("no rembro session is running for this user".into())
}

/// Classifies a connection error with a prefix rembro maps to a SOCKS reply.
fn conn_error(e: &io::Error, what: &str) -> String {
    let kind = match e.kind() {
        io::ErrorKind::ConnectionRefused => "refused",
        io::ErrorKind::TimedOut => "timeout",
        io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => "unreachable",
        _ => "other",
    };
    format!("{kind}: {what}: {e}")
}

fn connect_direct(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|e| format!("dns: {host}: {e}"))?.collect();
    let mut last = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, Duration::from_secs(10)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => conn_error(&e, &format!("{host}:{port}")),
        None => format!("dns: {host}: no addresses"),
    })
}

fn probe_http(ports: &[u16]) -> Vec<u16> {
    let handles: Vec<_> = ports
        .iter()
        .map(|&port| {
            thread::spawn(move || {
                for ip in ["127.0.0.1", "::1"] {
                    let addr = (ip.parse::<IpAddr>().unwrap(), port).into();
                    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else { continue };
                    let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
                    let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
                    if s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\nUser-Agent: rembro\r\n\r\n").is_err() {
                        return false;
                    }
                    let mut head = [0u8; 5];
                    return s.read_exact(&mut head).is_ok() && &head == b"HTTP/";
                }
                false
            })
        })
        .collect();
    ports.iter().zip(handles).filter_map(|(&p, h)| h.join().unwrap_or(false).then_some(p)).collect()
}

struct ProxyUrl {
    host: String,
    port: u16,
    auth: Option<String>,
}

impl ProxyUrl {
    fn parse(s: &str) -> Option<ProxyUrl> {
        let s = s.trim();
        let rest = match s.split_once("://") {
            Some(("http", rest)) => rest,
            Some(_) => return None,
            None => s,
        };
        let rest = rest.split('/').next()?;
        let (auth, hostport) = match rest.rsplit_once('@') {
            Some((a, h)) => (Some(util::base64(percent_decode(a).as_bytes())), h),
            None => (None, rest),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(':') => (h, p.parse().ok()?),
            _ => (hostport, 80),
        };
        let host = host.trim_matches(['[', ']']).to_string();
        if host.is_empty() {
            return None;
        }
        Some(ProxyUrl { host, port, auth })
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The target's HTTP proxy settings, from the usual environment variables.
struct ProxyEnv {
    http: Option<ProxyUrl>,
    https: Option<ProxyUrl>,
    no_proxy: Vec<String>,
}

impl ProxyEnv {
    fn from_env() -> ProxyEnv {
        let var = |names: &[&str]| names.iter().find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()));
        let all = var(&["all_proxy", "ALL_PROXY"]);
        let http = var(&["http_proxy", "HTTP_PROXY"]).or(all.clone()).and_then(|v| ProxyUrl::parse(&v));
        let https = var(&["https_proxy", "HTTPS_PROXY"]).or(all).and_then(|v| ProxyUrl::parse(&v));
        let no_proxy = var(&["no_proxy", "NO_PROXY"])
            .unwrap_or_default()
            .split(',')
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        ProxyEnv { http, https, no_proxy }
    }

    fn for_host(&self, host: &str, port: u16) -> Option<&ProxyUrl> {
        let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
        if self.no_proxy.iter().any(|e| no_proxy_matches(e, &host)) {
            return None;
        }
        if port == 80 { self.http.as_ref() } else { self.https.as_ref() }
    }
}

fn no_proxy_matches(entry: &str, host: &str) -> bool {
    if entry == "*" {
        return true;
    }
    if let Some((net, bits)) = entry.split_once('/')
        && let (Ok(net), Ok(bits), Ok(ip)) = (net.parse::<IpAddr>(), bits.parse::<u32>(), host.parse::<IpAddr>())
    {
        return match (net, ip) {
            (IpAddr::V4(n), IpAddr::V4(i)) if bits <= 32 => {
                let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
                u32::from(n) & mask == u32::from(i) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(i)) if bits <= 128 => {
                let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
                u128::from(n) & mask == u128::from(i) & mask
            }
            _ => false,
        };
    }
    let entry = entry.trim_start_matches('*').trim_start_matches('.');
    let entry = match entry.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.contains(':') => h,
        _ => entry,
    };
    let entry = entry.trim_matches(['[', ']']);
    host == entry || host.ends_with(&format!(".{entry}"))
}

fn connect_via(proxy: &ProxyUrl, host: &str, port: u16) -> Result<TcpStream, String> {
    let mut s = connect_direct(&proxy.host, proxy.port)?;
    let authority = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = &proxy.auth {
        req.push_str(&format!("Proxy-Authorization: Basic {auth}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).map_err(|e| conn_error(&e, "proxy"))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
    // Read the response a byte at a time so nothing after the headers is consumed.
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 << 10 {
            return Err("other: the proxy sent an oversized response".into());
        }
        match s.read(&mut b) {
            Ok(1) => head.push(b[0]),
            Ok(_) => return Err("other: the proxy closed the connection".into()),
            Err(e) => return Err(conn_error(&e, "proxy")),
        }
    }
    let _ = s.set_read_timeout(None);
    let status = String::from_utf8_lossy(&head);
    let status = status.lines().next().unwrap_or("");
    match status.split_whitespace().nth(1) {
        Some("200") => Ok(s),
        Some("403") | Some("407") => Err(format!("refused: proxy answered {status}")),
        Some("502") | Some("503") | Some("504") => Err(format!("unreachable: proxy answered {status}")),
        _ => Err(format!("other: proxy answered {status}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_proxy() {
        assert!(no_proxy_matches("localhost", "localhost"));
        assert!(no_proxy_matches(".example.com", "a.example.com"));
        assert!(no_proxy_matches("example.com", "a.example.com"));
        assert!(!no_proxy_matches("example.com", "badexample.com"));
        assert!(no_proxy_matches("10.0.0.0/8", "10.1.2.3"));
        assert!(!no_proxy_matches("10.0.0.0/8", "11.1.2.3"));
        assert!(no_proxy_matches("::1", "::1"));
        assert!(no_proxy_matches("*", "anything"));
    }

    #[test]
    fn proxy_urls() {
        let p = ProxyUrl::parse("http://gateway.docker.internal:3128").unwrap();
        assert_eq!((p.host.as_str(), p.port, p.auth.is_none()), ("gateway.docker.internal", 3128, true));
        let p = ProxyUrl::parse("http://u%40x:p@proxy:8080/").unwrap();
        assert_eq!(p.auth.as_deref(), Some(util::base64(b"u@x:p").as_str()));
        assert!(ProxyUrl::parse("socks5://proxy:1080").is_none());
    }
}

#[cfg(test)]
mod url_tests {
    use super::is_web_url;

    #[test]
    fn web_urls() {
        assert!(is_web_url("https://example.org/a?b=c"));
        assert!(is_web_url("HTTP://localhost:3000"));
        assert!(!is_web_url("--renderer-cmd-prefix=sh -c id"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("https://a b"));
        assert!(!is_web_url("https://a\nb"));
    }
}
