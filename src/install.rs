//! `rembro install`: puts a browser in the target for --remote.
//!
//! Where Node.js is available this uses Playwright, which publishes builds for x86_64 and
//! aarch64 and knows each distribution's library dependencies. Otherwise it uses the
//! distribution's own packages, except on Ubuntu, whose Chromium and Firefox packages are
//! snaps that do not run in containers.

use crate::cli::Install;
use crate::host::{self, HostEvent, Session};
use crate::proto::{Call, Event};

const SCRIPT: &str = r#"
b=$1
if [ "$(id -u)" = 0 ]; then sudo=
elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then sudo="sudo -n"
else sudo=none
fi
arch=$(uname -m)
ID= ID_LIKE=
[ -r /etc/os-release ] && . /etc/os-release
if command -v npx >/dev/null 2>&1 && { [ "$arch" = x86_64 ] || [ "$arch" = aarch64 ]; }; then
  deps=
  if [ "$sudo" != none ]; then deps=--with-deps; fi
  echo "+ npx -y playwright@latest install $deps $b"
  exec npx -y playwright@latest install $deps "$b"
fi
if [ "$b" = webkit ]; then
  echo "rembro: WebKit builds come from Playwright, which needs Node.js (npx) on x86_64 or aarch64" >&2
  exit 1
fi
pre= alt=
if command -v apk >/dev/null 2>&1; then
  cmd="apk add --no-cache"; pkg=$b
elif command -v apt-get >/dev/null 2>&1; then
  case " $ID $ID_LIKE " in *" ubuntu "*)
    echo "rembro: Ubuntu's chromium and firefox packages are snaps, which do not run here." >&2
    echo "rembro: install Node.js (apt-get install nodejs npm) and run rembro install again to use Playwright's builds." >&2
    exit 1;;
  esac
  pre="apt-get update"; cmd="apt-get install -y --no-install-recommends"
  if [ "$b" = chromium ]; then pkg=chromium; alt=chromium-browser; else pkg=firefox-esr; alt=firefox; fi
elif command -v dnf >/dev/null 2>&1; then
  cmd="dnf install -y"; pkg=$b
elif command -v pacman >/dev/null 2>&1; then
  cmd="pacman -S --noconfirm --needed"; pkg=$b
elif command -v zypper >/dev/null 2>&1; then
  cmd="zypper -n install"; if [ "$b" = chromium ]; then pkg=chromium; else pkg=MozillaFirefox; fi
else
  echo "rembro: found neither npx nor a package manager rembro knows (apk, apt-get, dnf, pacman, zypper)" >&2
  exit 1
fi
if [ "$sudo" = none ]; then
  echo "rembro: installing $b needs root. As root in the target, run:" >&2
  echo "  ${pre:+$pre && }$cmd $pkg" >&2
  exit 1
fi
if [ -n "$pre" ]; then echo "+ $pre"; $sudo $pre || exit 1; fi
echo "+ $cmd $pkg"
$sudo $cmd $pkg && exit 0
if [ -n "$alt" ]; then echo "+ $cmd $alt"; exec $sudo $cmd $alt; fi
exit 1
"#;

pub fn run(i: &Install, s: &Session) -> Result<i32, String> {
    if !["chromium", "firefox", "webkit"].contains(&i.browser.as_str()) {
        return Err(format!("rembro install takes chromium, firefox or webkit, not {:?}", i.browser));
    }
    let argv = ["sh", "-c", SCRIPT, "rembro-install", &i.browser].map(String::from).to_vec();
    let env = vec![("DEBIAN_FRONTEND".into(), "noninteractive".into())];
    let proc: u32 = s.call(&Call::Spawn { argv, env, unset: Vec::new(), shim: false })?;
    loop {
        match s.events.recv() {
            Ok(HostEvent::Agent(Event::Output { proc: p, stream, text })) if p == proc => {
                host::relay_output(stream, &text);
            }
            Ok(HostEvent::Agent(Event::Exited { proc: p, code, signal })) if p == proc => {
                return Ok(host::exit_code(code, signal));
            }
            Ok(HostEvent::Signal(sig)) => {
                let _ = s.call::<serde_json::Value>(&Call::Kill { proc, signal: libc::SIGTERM });
                return Ok(128 + sig);
            }
            Ok(HostEvent::AgentClosed) | Err(_) => return Err(format!("the connection to {} closed", s.label)),
            Ok(_) => {}
        }
    }
}
