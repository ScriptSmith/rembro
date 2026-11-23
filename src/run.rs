//! `rembro run`: any program in the target, with its windows and sound shown here.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::cli::{Common, Run};
use crate::host::{self, HostEvent, Limiter, Session};
use crate::proto::{Call, Event};
use crate::{profile, remote, sys};

pub fn run(r: &Run, s: &Session, common: &Common) -> Result<i32, String> {
    // Links from the program open in one browser here, with a profile of their own unless
    // REMBRO_PROFILE names one.
    let links = match std::env::var_os("REMBRO_PROFILE") {
        Some(p) if !p.is_empty() => None,
        _ => Some(profile::RunDir::create()?),
    };
    let links_profile = links.as_ref().map(profile::RunDir::profile);
    let dir: String = s.call(&Call::Mkdtemp)?;
    let gui = remote::setup_gui(s, &r.gui, &dir, false)?;
    s.call::<String>(&Call::Shim)?;
    let mut argv = gui.wrapper.clone();
    // waypipe exits 0 whatever its program does, so the program's status goes in a file.
    let status_file = format!("{dir}/status");
    if !gui.wrapper.is_empty() {
        argv.extend(["sh", "-c", "\"$@\"; s=$?; echo $s > \"$0\"; exit $s", &status_file].map(String::from));
    }
    argv.extend(r.command.iter().cloned());
    let proc: u32 = s.call(&Call::Spawn { argv, env: gui.env.clone(), unset: Vec::new(), shim: true })?;

    let mut limiter = Limiter::new();
    loop {
        match s.events.recv() {
            Ok(HostEvent::Agent(Event::Output { proc: p, stream, text })) if p == proc => {
                if !s.quiet {
                    host::relay_output(stream, &text);
                }
            }
            Ok(HostEvent::Agent(Event::Exited { proc: p, code, signal })) if p == proc => {
                if !gui.wrapper.is_empty()
                    && let Ok(Some(data)) = s.call::<Option<Vec<u8>>>(&Call::ReadFile { path: status_file })
                    && let Ok(code) = String::from_utf8_lossy(&data).trim().parse()
                {
                    return Ok(code);
                }
                return Ok(host::exit_code(code, signal));
            }
            Ok(HostEvent::Agent(Event::Open { url })) if limiter.allow() => {
                open(common, s, links_profile.as_deref(), &url)
            }
            Ok(HostEvent::Signal(sig)) => {
                host::wait_exit(
                    s,
                    |e| {
                        matches!(e, HostEvent::AgentClosed)
                            || matches!(e, HostEvent::Agent(Event::Exited { proc: p, .. }) if *p == proc)
                    },
                    |sig| {
                        let _ = s.call::<serde_json::Value>(&Call::Kill { proc, signal: sig });
                    },
                );
                return Ok(128 + sig);
            }
            Ok(HostEvent::AgentClosed) | Err(_) => {
                eprintln!("rembro: the connection to {} closed", s.label);
                return Ok(1);
            }
            Ok(_) => {}
        }
    }
}

/// Opens a URL from the program in a browser here that uses the target's network, by
/// starting `rembro TARGET URL`. Later URLs go to that browser's window, since naming the
/// profile keeps rembro from opening another one.
fn open(common: &Common, s: &Session, profile: Option<&Path>, url: &str) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = Command::new(exe);
    if let Some(agent) = &common.agent {
        cmd.arg("--agent").arg(agent);
    }
    cmd.args(["--no-watch", "--ports", "none"]);
    if let Some(p) = profile {
        cmd.arg("--profile").arg(p);
    }
    cmd.args([&s.label, url]).stdin(Stdio::null());
    if !s.verbose {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    sys::unblock_signals(&mut cmd);
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("rembro: could not open {url}: {e}"),
    }
}
