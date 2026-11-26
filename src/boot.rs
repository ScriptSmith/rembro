//! Starting the agent in a target, uploading it first when the target lacks this build.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use crate::target::Target;
use crate::{sys, util};

pub const LOCAL_NEEDS_LINUX: &str =
    "the local target works only on Linux, since rembro's agent runs on Linux; use a container or VM";

pub struct AgentBin {
    bytes: Vec<u8>,
    hash: String,
    machine: Option<u16>,
    dynamic: bool,
    /// Given with --agent, so never swapped for a build for another architecture.
    explicit: bool,
}

impl AgentBin {
    /// The binary to run as the agent: `path`, or this executable. On macOS and Windows it
    /// is the Linux build for this machine's architecture, which its containers and VMs
    /// usually share.
    pub fn load(path: Option<&Path>, verbose: bool) -> Result<AgentBin, String> {
        match path {
            Some(p) => AgentBin::read(p, true),
            None if cfg!(target_os = "linux") => {
                let exe = std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?;
                AgentBin::read(&exe, false)
            }
            None => AgentBin::for_arch(std::env::consts::ARCH, verbose),
        }
    }

    fn read(path: &Path, explicit: bool) -> Result<AgentBin, String> {
        let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let hash = format!("{:016x}", util::fnv64(&bytes));
        let (machine, dynamic) = elf_info(&bytes);
        Ok(AgentBin { bytes, hash, machine, dynamic, explicit })
    }

    /// The Linux build for an architecture: beside this executable, cached, or downloaded.
    fn for_arch(arch: &str, verbose: bool) -> Result<AgentBin, String> {
        let path = crate::fetch::tool("rembro", arch, verbose)?;
        let bin = AgentBin::read(&path, false)?;
        if bin.machine != machine_for(arch) {
            return Err(format!("{} is not built for {arch}", path.display()));
        }
        Ok(bin)
    }
}

/// The ELF machine type, and whether the binary needs a dynamic loader.
fn elf_info(b: &[u8]) -> (Option<u16>, bool) {
    if b.len() < 64 || &b[..4] != b"\x7fELF" || b[5] != 1 {
        return (None, false);
    }
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let machine = u16_at(18);
    let dynamic = match b[4] {
        2 => {
            let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap()) as usize;
            let (phoff, phentsize, phnum) = (u64_at(32), u16_at(54) as usize, u16_at(56) as usize);
            (0..phnum).any(|i| {
                let o = phoff + i * phentsize;
                o + 4 <= b.len() && u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) == 3
            })
        }
        _ => false,
    };
    (Some(machine), dynamic)
}

/// The ELF machine type for a `uname -m` name.
fn machine_for(uname: &str) -> Option<u16> {
    Some(match uname {
        "x86_64" | "amd64" => 62,
        "aarch64" | "arm64" => 183,
        "i386" | "i486" | "i586" | "i686" => 3,
        "riscv64" => 243,
        "ppc64le" => 21,
        "s390x" => 22,
        u if u.starts_with("arm") => 40,
        _ => return None,
    })
}

/// A POSIX sh script that runs a cached agent, or receives one on stdin first.
fn script(hash: &str, size: usize) -> String {
    format!(
        r#"h={hash}; n={size}; u=$(id -u)
set -- "${{XDG_CACHE_HOME:-${{HOME:-/nonexistent}}/.cache}}/rembro" "${{TMPDIR:-/tmp}}/rembro-$u" "/dev/shm/rembro-$u"
for d do
  f=$d/agent-$h
  if [ -f "$f" ] && [ "$(wc -c <"$f")" -eq "$n" ] && "$f" agent --check 2>/dev/null; then
    echo rembro-ok; exec "$f" agent
  fi
done
echo "rembro-need $(uname -m)"
t=
for d do
  if mkdir -p "$d" 2>/dev/null && chmod 700 "$d" 2>/dev/null && : >"$d/.agent-$h.$$" 2>/dev/null; then
    t=$d/.agent-$h.$$; break
  fi
done
if [ -z "$t" ]; then echo "rembro-error no writable directory for the agent in $*"; exit 1; fi
head -c "$n" >"$t"
if [ "$(wc -c <"$t")" -ne "$n" ]; then rm -f "$t"; echo "rembro-error the upload was cut short"; exit 1; fi
err=
for d do
  f=$d/agent-$h
  mkdir -p "$d" 2>/dev/null || continue
  if cp "$t" "$f.$$" 2>/dev/null && chmod 700 "$f.$$" && mv -f "$f.$$" "$f"; then
    if e=$("$f" agent --check 2>&1); then
      rm -f "$t"
      for o in "$d"/agent-????????????????; do [ "$o" = "$f" ] || rm -f "$o"; done
      echo rembro-ok; exec "$f" agent
    fi
    [ -n "$err" ] || err=${{e:-it exited with an error}}
    rm -f "$f"
  else
    rm -f "$f.$$"
  fi
done
rm -f "$t"
printf 'rembro-error the agent does not run in the target: %s\n' "$(printf '%s' "$err" | tr '\n' ' ')"
exit 1
"#
    )
}

/// A started agent's process and pipes, past the handshake.
pub struct Started {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
}

/// The agent for the `local` target: this executable, which on Linux is its own agent.
#[cfg(target_os = "linux")]
fn local_agent() -> Result<(std::process::Command, String), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?;
    let hash = crate::agent::self_hash().map_err(|e| format!("cannot read this executable: {e}"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("agent");
    // Ctrl-C then reaches only rembro, which ends the session. The commands that reach other
    // targets stay in rembro's group, since they may ask for a password on the terminal.
    sys::own_group(&mut cmd);
    Ok((cmd, hash))
}

#[cfg(not(target_os = "linux"))]
fn local_agent() -> Result<(std::process::Command, String), String> {
    Err(LOCAL_NEEDS_LINUX.into())
}

pub fn start(target: &Target, bin: &AgentBin, verbose: bool) -> Result<Started, String> {
    let (mut cmd, expect) = if *target == Target::Local {
        local_agent()?
    } else {
        let argv = ["sh".to_string(), "-c".to_string(), script(&bin.hash, bin.bytes.len())];
        (target.command(&argv), bin.hash.clone())
    };
    if verbose {
        eprintln!("rembro: running {:?}", cmd.get_program());
    }
    sys::unblock_signals(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", cmd.get_program().to_string_lossy()))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut uploaded = false;
    let mut line = String::new();
    let fail = |child: &mut Child, msg: String| -> String {
        let _ = child.kill();
        let _ = child.wait();
        msg
    };
    loop {
        line.clear();
        match stdout.read_line(&mut line) {
            Ok(0) | Err(_) => {
                let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
                return Err(format!("could not start the agent in the target ({status})"));
            }
            Ok(_) => {}
        }
        let l = line.trim_end();
        if let Some(arch) = l.strip_prefix("rembro-need ") {
            if let (Some(want), Some(have)) = (machine_for(arch), bin.machine)
                && want != have
            {
                if bin.explicit {
                    return Err(fail(
                        &mut child,
                        format!("the target is {arch}, but --agent is built for another architecture"),
                    ));
                }
                fail(&mut child, String::new());
                let other = AgentBin::for_arch(arch, verbose).map_err(|e| {
                    format!("the target is {arch}, so it needs an agent built for {arch}: {e}. Pass one with --agent")
                })?;
                if verbose {
                    eprintln!("rembro: the target is {arch}; using the {arch} agent");
                }
                return start(target, &other, verbose);
            }
            if verbose {
                eprintln!("rembro: uploading the agent ({} KiB)", bin.bytes.len() / 1024);
            }
            stdin
                .write_all(&bin.bytes)
                .and_then(|_| stdin.flush())
                .map_err(|e| fail(&mut child, format!("uploading the agent failed: {e}")))?;
            uploaded = true;
        } else if let Some(msg) = l.strip_prefix("rembro-error ") {
            let mut msg = msg.to_string();
            if uploaded && bin.dynamic {
                msg.push_str(
                    "; this rembro is dynamically linked, so it may need a newer C library than the target has. \
                     A static build (x86_64-unknown-linux-musl) runs anywhere; pass it with --agent",
                );
            }
            return Err(fail(&mut child, msg));
        } else if let Some(hash) = l.strip_prefix("rembro-agent ") {
            if hash != expect {
                return Err(fail(&mut child, format!("the agent in the target is a different build ({hash})")));
            }
            return Ok(Started { child, stdin, stdout });
        } else if l != "rembro-ok" && verbose {
            eprintln!("rembro: target says: {l}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machines() {
        assert_eq!(machine_for("x86_64"), Some(62));
        assert_eq!(machine_for("armv7l"), Some(40));
        assert_eq!(machine_for("mips"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn this_binary() {
        let bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
        let (machine, _) = elf_info(&bytes);
        assert_eq!(machine, machine_for(std::env::consts::ARCH));
    }
}
