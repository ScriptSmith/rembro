use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::browser::Family;
use crate::target::Target;

const AFTER_HELP: &str = "\
Targets:
  local                            this machine, on Linux, and the default
  ssh:[USER@]HOST[:PORT]           a host or VM reached with ssh; your ssh config applies
  docker:CONTAINER                 a running Docker container (also podman:, nerdctl:)
  apple-container:CONTAINER        a container run by Apple's container tool on macOS
  sbx:SANDBOX                      a Docker sbx sandbox
  k8s:[NAMESPACE/]POD[/CONTAINER]  a Kubernetes pod in kubectl's current context
  incus:INSTANCE, lxc:INSTANCE     an Incus or LXD instance
  wsl:DISTRO                       a WSL distribution on Windows
  exec:COMMAND                     any command that runs its arguments in the target,
                                   such as exec:'docker compose exec -T web'
  TARGET,TARGET...                 a target inside the one before it, outermost first,
                                   such as ssh:bastion,ssh:box,docker:web

Targets run Linux, and need only sh. rembro copies its Linux agent there (to
~/.cache/rembro, or /tmp) the first time, and talks to it over the command's stdin
and stdout.

With no target, rembro uses local. When the first argument is not a target but starts
with http://, https://, localhost:, 127. or [, it is the first URL and the target is
local.

Examples:
  rembro                                open this machine's dev servers, on Linux
  rembro sbx:myapp                      open the sandbox's dev servers in a browser here
  rembro docker:web http://localhost:3000 -b firefox
  rembro k8s:shop/api-7d9f http://orders.shop.svc:8080
  rembro ssh:bastion,ssh:box,docker:web a container on a host behind a bastion
  rembro --remote ssh:devbox            run Chromium on devbox and show its window here
  rembro --debug sbx:myapp              let an agent in the sandbox drive the browser
  rembro --debug --headless sbx:myapp   the same, with no browser window
  rembro --debug localhost:5173         let a program on this machine drive the browser
  rembro --profile work ssh:bastion     keep this browser's logins for next time
  rembro --fork work ssh:bastion        start from a copy of them, deleted at exit
  rembro --fork system ssh:bastion      start from a copy of your own browser profile
  rembro sbx:myapp -- --incognito       pass arguments to the browser
  rembro run ssh:devbox gimp            run any program there, with its window here
  rembro install docker:web firefox     install a browser in the target for --remote
  rembro profiles --prune 30            delete profiles no session has used for 30 days";

#[derive(Parser, Debug)]
#[command(
    name = "rembro",
    version,
    about = "Open a browser that uses the network of an SSH host, container, sandbox or pod",
    long_about = "Open a browser that uses the network of an SSH host, container, sandbox or pod.\n\n\
        By default the browser runs on this machine and all of its traffic, DNS included, \
        goes through the target, so localhost, service names and private addresses resolve \
        and connect as they would there. With --remote, the browser runs in the target \
        and its window is shown here.",
    override_usage = "rembro [OPTIONS] [TARGET] [URL]... [-- <BROWSER_ARGS>...]\n       rembro [OPTIONS] <COMMAND>",
    after_help = AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub browse: Browse,

    #[command(flatten)]
    pub common: Common,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a program in the target, with its windows and sound shown here
    Run(Run),
    /// Install a browser in the target, for --remote
    Install(Install),
    /// List the browser profiles that rembro keeps on this machine and the browsers' own, or
    /// delete old ones that rembro keeps
    Profiles(Profiles),
}

#[derive(Args, Debug)]
pub struct Browse {
    /// Where the browser's network lives (see Targets below). Without one, or with a URL in its
    /// place, the target is local
    pub target: Option<String>,

    /// Pages to open. With none, opens the HTTP servers listening on the target's loopback
    #[arg(value_name = "URL")]
    pub urls: Vec<String>,

    /// Arguments for the browser, after --
    #[arg(last = true, value_name = "BROWSER_ARGS")]
    pub browser_args: Vec<String>,

    /// Browser to use: auto, chromium, chrome, brave, edge, vivaldi, firefox, librewolf,
    /// zen, webkit, epiphany, minibrowser, playwright-chromium, playwright-firefox,
    /// playwright-webkit, or a command or path
    #[arg(short, long, env = "REMBRO_BROWSER", default_value = "auto", value_name = "BROWSER")]
    pub browser: String,

    /// Kind of browser, when --browser names one rembro does not know
    #[arg(long, value_enum)]
    pub family: Option<Family>,

    /// Run the browser in the target and show its window here
    #[arg(short, long)]
    pub remote: bool,

    /// Send only localhost, 127.0.0.0/8 and ::1 through the target; connect to everything else from here
    #[arg(long, conflicts_with = "remote")]
    pub loopback_only: bool,

    /// Make the target's connections through the HTTP proxy its environment names
    /// (https_proxy, http_proxy, no_proxy), as programs there do
    #[arg(long, conflicts_with = "remote")]
    pub target_proxy: bool,

    /// Trust the CA certificates added to the target's trust store, in this profile only.
    /// This lets the target, or a proxy it uses, see this profile's HTTPS traffic
    #[arg(long, conflicts_with = "remote")]
    pub trust_target_ca: bool,

    /// Keep the browser's cookies and other data in a profile with this name, or in the
    /// directory at PATH. `system` is the browser's own default profile, and `system:NAME`
    /// its profile called NAME. Without it (or REMBRO_PROFILE), each session gets a new
    /// profile that is deleted when the browser exits
    #[arg(long, value_name = "NAME|PATH", conflicts_with_all = ["fork", "temp_profile"])]
    pub profile: Option<String>,

    /// Start from a copy of this profile, deleted when the browser exits. NAME and PATH are
    /// as for --profile, so `--fork system` starts from the browser's own default profile
    #[arg(long, value_name = "NAME|PATH", conflicts_with = "temp_profile")]
    pub fork: Option<String>,

    /// Use a new profile that is deleted when the browser exits, even when REMBRO_PROFILE is set
    #[arg(long)]
    pub temp_profile: bool,

    /// Use the profile even where rembro would refuse: when a browser that rembro did not
    /// start has it open (the pages then load from that browser's network), or when it is a
    /// Firefox profile that rembro did not create (rembro puts its settings back afterwards,
    /// as it does for `--profile system` without this flag)
    #[arg(long)]
    pub force_profile: bool,

    /// Let programs in the target drive the browser: expose its debugging endpoint
    /// (CDP for Chromium, WebDriver BiDi for Firefox) on the target's 127.0.0.1:PORT.
    /// The default port is 9222, or a free one if that is taken. REMBRO_PROFILE does not
    /// apply, so the browser gets a new profile unless --profile or --fork is given
    #[arg(long, value_name = "PORT", num_args = 0..=1, require_equals = true, default_missing_value = "auto")]
    pub debug: Option<String>,

    /// Run the browser without a window, for a program that drives it with --debug.
    /// Chromium and Firefox only
    #[arg(long, requires = "debug")]
    pub headless: bool,

    /// Ports to look for HTTP servers on: ranges like 3000-3999, patterns like 41xx,
    /// `all` or `none`
    #[arg(long, env = "REMBRO_PORTS", default_value = "1-32767", value_name = "PORTS")]
    pub ports: String,

    /// Keep opening tabs for HTTP servers that start while the browser is open.
    /// This is the default when no URL is given
    #[arg(long, conflicts_with = "no_watch")]
    pub watch: bool,

    /// Do not open tabs for servers that start later
    #[arg(long)]
    pub no_watch: bool,

    #[command(flatten)]
    pub gui: GuiOpts,
}

#[derive(Args, Debug)]
pub struct Run {
    /// Where to run the program (see `rembro --help`)
    pub target: String,

    /// The program and its arguments
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, value_name = "COMMAND")]
    pub command: Vec<String>,

    #[command(flatten)]
    pub gui: GuiOpts,
}

#[derive(Args, Debug)]
pub struct Profiles {
    /// Delete the profiles that no session has used for DAYS days, and the temporary ones
    /// that sessions which were killed left behind. Profiles that rembro did not create stay
    #[arg(long, value_name = "DAYS")]
    pub prune: Option<u64>,
}

#[derive(Args, Debug)]
pub struct Install {
    /// Where to install it (see `rembro --help`)
    pub target: String,

    /// chromium, firefox or webkit
    #[arg(default_value = "chromium")]
    pub browser: String,
}

#[derive(Args, Debug, Clone)]
pub struct GuiOpts {
    /// How windows from the target are shown here
    #[arg(long, value_enum, default_value = "auto")]
    pub display: DisplayArg,

    /// Over X11, give the target full access to this display: it can then read other
    /// windows and send them input. By default it gets an untrusted connection, as with ssh -X
    #[arg(long)]
    pub x11_trusted: bool,

    /// Do not forward sound
    #[arg(long)]
    pub no_audio: bool,
}

#[derive(Args, Debug, Clone)]
pub struct Common {
    /// Agent binary to copy into the target, such as a static build for another architecture
    #[arg(long, value_name = "PATH", global = true)]
    pub agent: Option<PathBuf>,

    /// Hide the program's own output
    #[arg(short, long, conflicts_with = "verbose", global = true)]
    pub quiet: bool,

    /// Explain what rembro is doing
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DisplayArg {
    /// Wayland through waypipe if this session is Wayland, otherwise X11. With neither,
    /// the target's own display if it has one, such as WSLg's in WSL
    Auto,
    /// Wayland through waypipe
    Wayland,
    /// X11, forwarded to this machine's X server
    X11,
    /// No display (for `rembro run` with programs that have no windows)
    None,
}

/// The profile a session uses.
#[derive(Debug, PartialEq)]
pub enum ProfileArg {
    /// A new one, deleted when the session ends.
    Temp,
    /// A new one that starts as a copy of this one, and is deleted when the session ends.
    Fork(String),
    /// This one, kept between sessions.
    Kept(String),
}

impl Browse {
    /// The profile from --profile, --fork, --temp-profile or REMBRO_PROFILE. A browser that a
    /// program drives with --debug never gets REMBRO_PROFILE's cookies.
    pub fn profile_arg(&self) -> ProfileArg {
        self.profile_from(std::env::var("REMBRO_PROFILE").ok())
    }

    fn profile_from(&self, env: Option<String>) -> ProfileArg {
        if let Some(p) = &self.profile {
            return ProfileArg::Kept(p.clone());
        }
        if let Some(p) = &self.fork {
            return ProfileArg::Fork(p.clone());
        }
        match env {
            Some(p) if !p.is_empty() && !self.temp_profile && self.debug.is_none() => ProfileArg::Kept(p),
            _ => ProfileArg::Temp,
        }
    }

    /// The requested debugging port: None for no debugging, Some(None) for the default.
    pub fn debug_port(&self) -> Result<Option<Option<u16>>, String> {
        match self.debug.as_deref() {
            None => Ok(None),
            Some("auto") => Ok(Some(None)),
            Some(p) => match p.parse::<u16>() {
                Ok(n) if n > 0 => Ok(Some(Some(n))),
                _ => Err(format!("--debug={p}: expected a port from 1 to 65535")),
            },
        }
    }

    pub fn watching(&self) -> bool {
        self.watch || (self.urls.is_empty() && !self.no_watch)
    }

    /// The target, which is local when none is given. A URL given in its place becomes the
    /// first URL, and URLs such as localhost:3000 get http:// in front, which Chromium needs.
    pub fn take_target(&mut self) -> Result<String, String> {
        self.target_on(cfg!(target_os = "linux"))
    }

    fn target_on(&mut self, linux: bool) -> Result<String, String> {
        let target = match self.target.take() {
            Some(t) => match Target::parse(&t) {
                Ok(_) => Some(t),
                Err(e) if !looks_like_url(&t) => return Err(e),
                Err(_) => {
                    self.urls.insert(0, t);
                    None
                }
            },
            None => None,
        };
        for url in &mut self.urls {
            if bare_loopback(url) {
                url.insert_str(0, "http://");
            }
        }
        match target {
            Some(t) => Ok(t),
            None if linux => Ok("local".into()),
            None => Err(NEEDS_TARGET.into()),
        }
    }
}

const NEEDS_TARGET: &str = "a target is needed on macOS and Windows, such as ssh:HOST or docker:CONTAINER \
    (see rembro --help); without one, rembro uses local, which works only on Linux because rembro's agent is a \
    Linux program";

/// Whether an argument that is not a target is a URL for the local target. Only web URLs and
/// loopback addresses count, so a mistyped target such as dokcer://web stays an error.
fn looks_like_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || bare_loopback(s)
}

/// A loopback address with no scheme, such as localhost:3000. Chromium takes `localhost:` for
/// a scheme on its command line, and its debugging endpoint opens no URL without one.
fn bare_loopback(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    ["localhost:", "127.", "["].iter().any(|p| lower.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn profile(args: &[&str], env: Option<&str>) -> Result<ProfileArg, clap::Error> {
        let cli = Cli::try_parse_from(["rembro"].iter().chain(args).chain(&["sbx:qut"]))?;
        Ok(cli.browse.profile_from(env.map(String::from)))
    }

    #[test]
    fn profiles() {
        let kept = |p: &str| ProfileArg::Kept(p.into());
        assert_eq!(profile(&[], None).unwrap(), ProfileArg::Temp);
        assert_eq!(profile(&[], Some("")).unwrap(), ProfileArg::Temp);
        assert_eq!(profile(&[], Some("work")).unwrap(), kept("work"));
        assert_eq!(profile(&["--profile", "home"], Some("work")).unwrap(), kept("home"));
        assert_eq!(profile(&["--fork", "home"], Some("work")).unwrap(), ProfileArg::Fork("home".into()));
        assert_eq!(profile(&["--temp-profile"], Some("work")).unwrap(), ProfileArg::Temp);
        // A browser that a program drives doesn't get REMBRO_PROFILE's cookies unasked.
        assert_eq!(profile(&["--debug"], Some("work")).unwrap(), ProfileArg::Temp);
        assert_eq!(profile(&["--debug", "--profile", "work"], None).unwrap(), kept("work"));
        assert_eq!(profile(&[], Some("system:Work")).unwrap(), kept("system:Work"));
        assert_eq!(profile(&["--debug"], Some("system")).unwrap(), ProfileArg::Temp);
        assert!(profile(&["--profile", "a", "--fork", "b"], None).is_err());
        assert!(profile(&["--fork", "a", "--temp-profile"], None).is_err());
    }

    fn target(args: &[&str], linux: bool) -> Result<(String, Vec<String>), String> {
        let mut cli = Cli::try_parse_from(["rembro"].iter().chain(args)).map_err(|e| e.to_string())?;
        let target = cli.browse.target_on(linux)?;
        Ok((target, cli.browse.urls))
    }

    #[test]
    fn targets() {
        let ok = |t: &str, urls: &[&str]| Ok((t.to_string(), urls.iter().map(|u| u.to_string()).collect()));
        assert_eq!(target(&[], true), ok("local", &[]));
        assert_eq!(target(&["--debug", "--headless"], true), ok("local", &[]));
        assert_eq!(target(&["--", "--incognito"], true), ok("local", &[]));
        assert_eq!(target(&["sbx:myapp"], true), ok("sbx:myapp", &[]));
        assert_eq!(target(&["localhost"], true), ok("localhost", &[]));
        assert_eq!(target(&["ssh://me@box:2222"], true), ok("ssh://me@box:2222", &[]));
        assert_eq!(target(&["docker:web", "http://api:8000"], true), ok("docker:web", &["http://api:8000"]));
        assert_eq!(
            target(&["sbx:myapp", "localhost:3000", "api:8000"], true),
            ok("sbx:myapp", &["http://localhost:3000", "api:8000"])
        );
        // A URL in the target's place is the first URL, for the local target.
        assert_eq!(target(&["http://localhost:3000"], true), ok("local", &["http://localhost:3000"]));
        assert_eq!(
            target(&["localhost:5173", "HTTPS://example.com/a,ssh:b"], true),
            ok("local", &["http://localhost:5173", "HTTPS://example.com/a,ssh:b"])
        );
        assert_eq!(target(&["--debug", "127.0.0.1:8000"], true), ok("local", &["http://127.0.0.1:8000"]));
        assert_eq!(target(&["[::1]:3000", "--no-watch"], true), ok("local", &["http://[::1]:3000"]));
        // Anything else that is not a target stays an error, so a typo doesn't open a browser here.
        for typo in ["sbx-myapp", "dokcer://web", "example.com:8080", "docker:", "ssh:box:http", "3000"] {
            assert_eq!(target(&[typo], true), Err(Target::parse(typo).unwrap_err()), "{typo}");
        }
        // Elsewhere, local can't run, so a target has to be given.
        assert_eq!(target(&[], false), Err(NEEDS_TARGET.into()));
        assert_eq!(target(&["http://localhost:3000"], false), Err(NEEDS_TARGET.into()));
        assert_eq!(target(&["sbx:myapp"], false), ok("sbx:myapp", &[]));
        // rembro run and rembro install still need one.
        assert!(Cli::try_parse_from(["rembro", "run", "gimp"]).is_err());
        assert!(Cli::try_parse_from(["rembro", "install"]).is_err());
        assert!(Cli::try_parse_from(["rembro", "run", "ssh:box", "gimp"]).is_ok());
    }
}
