//! Messages exchanged between rembro and its agent in the target.

use serde::{Deserialize, Serialize};

/// Bumped whenever a message changes shape, so mismatched builds fail clearly.
pub const PROTOCOL: u32 = 3;

/// A request from rembro to the agent. Each one gets exactly one reply.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Call {
    Info,
    Configure {
        target_proxy: bool,
    },
    /// Returns the index of the first group with an executable candidate, and its path.
    Find {
        groups: Vec<Vec<String>>,
    },
    Mkdtemp,
    Mkdir {
        path: String,
    },
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        data: Vec<u8>,
        mode: u32,
    },
    /// Listens in the target. Each accepted connection is opened back to rembro as `Open::Forward { id }`.
    Listen {
        id: u32,
        addr: Addr,
    },
    Listening,
    ProbeHttp {
        ports: Vec<u16>,
    },
    PortFree {
        port: u16,
    },
    FreePort,
    /// Starts a process. With `shim`, its BROWSER and xdg-open hand URLs back to rembro.
    Spawn {
        argv: Vec<String>,
        env: Vec<(String, String)>,
        unset: Vec<String>,
        #[serde(default)]
        shim: bool,
    },
    Kill {
        proc: u32,
        signal: i32,
    },
    /// Size of a file, if it exists.
    Stat {
        path: String,
    },
    /// Listens for URLs from the target's BROWSER and xdg-open, and returns the directory
    /// that holds those commands.
    Shim,
    /// The CA certificates added to the target's trust store, as (source, PEM text).
    ExtraCas,
    /// Runs a short command and returns its exit code and stdout.
    Capture {
        argv: Vec<String>,
        timeout_ms: u64,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Addr {
    Tcp { host: String, port: u16 },
    Unix { path: String },
}

/// What the opening side wants the other end of a new channel connected to.
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Open {
    /// Opened by rembro: a TCP connection made from the target.
    Tcp { host: String, port: u16 },
    /// Opened by the agent: a connection accepted on a `Call::Listen` listener.
    Forward { id: u32 },
    /// Opened by rembro: the agent writes the next `size` bytes to `path`, then answers
    /// "ok" or an error message and closes.
    Store { path: String, size: u64, mode: u32 },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// A line of a process's output; `stream` is 1 for stdout and 2 for stderr.
    Output {
        proc: u32,
        stream: u8,
        text: String,
    },
    Exited {
        proc: u32,
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// A program in the target asked to open this http or https URL.
    Open {
        url: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Info {
    pub arch: String,
    pub uid: u32,
    pub home: Option<String>,
    pub runtime_dir: Option<String>,
    pub hostname: String,
    pub shm_bytes: Option<u64>,
    /// The HTTP proxy the target's environment names, if any.
    pub proxy: Option<String>,
    pub protocol: u32,
    /// The directory the agent runs from, where rembro keeps other uploaded tools.
    pub agent_dir: String,
    /// ID and ID_LIKE from /etc/os-release.
    pub os: Vec<String>,
}
