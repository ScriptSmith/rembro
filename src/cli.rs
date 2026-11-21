use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::browser::Family;

const AFTER_HELP: &str = "\
Targets:
  local                            this machine
  ssh:[USER@]HOST[:PORT]           a host reached with ssh; your ssh config applies
  docker:CONTAINER                 a running Docker container (also podman:, nerdctl:)
  sbx:SANDBOX                      a Docker sbx sandbox
  k8s:[NAMESPACE/]POD[/CONTAINER]  a Kubernetes pod in kubectl's current context
  incus:INSTANCE, lxc:INSTANCE     an Incus or LXD instance
  exec:COMMAND                     any command that runs its arguments in the target,
                                   such as exec:'docker compose exec -T web'

The target needs only sh. rembro copies itself there (to ~/.cache/rembro, or /tmp)
the first time, and talks to that copy over the command's stdin and stdout.

Examples:
  rembro sbx:qut                        open the sandbox's dev servers in a browser here
  rembro docker:web http://localhost:3000 -b firefox
  rembro k8s:shop/api-7d9f http://orders.shop.svc:8080
  rembro --remote ssh:devbox            run Chromium on devbox and show its window here
  rembro --debug sbx:qut                let an agent in the sandbox drive the browser
  rembro sbx:qut -- --incognito         pass arguments to the browser
  rembro run ssh:rpi gnome-calculator   run any program there, with its window here
  rembro install docker:web firefox     install a browser in the target for --remote";

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
    after_help = AFTER_HELP,
    subcommand_negates_reqs = true
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
}

#[derive(Args, Debug)]
pub struct Browse {
    /// Where the browser's network lives (see Targets below)
    #[arg(required = true)]
    pub target: Option<String>,

    /// Pages to open. With none, opens the HTTP servers listening on the target's loopback
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

    /// Profile name, or a path to a profile directory
    #[arg(long, value_name = "NAME|PATH")]
    pub profile: Option<String>,

    /// Use a new profile that is deleted when the browser exits
    #[arg(long, conflicts_with = "profile")]
    pub temp_profile: bool,

    /// Use the profile even where rembro would refuse: when a browser that rembro did not
    /// start has it open (the pages then load from that browser's network), or when it is a
    /// Firefox profile that rembro did not create (rembro puts its settings back afterwards)
    #[arg(long)]
    pub force_profile: bool,

    /// Let programs in the target drive the browser: expose its debugging endpoint
    /// (CDP for Chromium, WebDriver BiDi for Firefox) on the target's 127.0.0.1:PORT.
    /// The default port is 9222, or a free one if that is taken. Uses the "agent" profile
    /// unless --profile is given
    #[arg(long, value_name = "PORT", num_args = 0..=1, require_equals = true, default_missing_value = "auto")]
    pub debug: Option<String>,

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
    /// Wayland through waypipe if this session is Wayland, otherwise X11
    Auto,
    /// Wayland through waypipe
    Wayland,
    /// X11, forwarded to this machine's X server
    X11,
    /// No display (for `rembro run` with programs that have no windows)
    None,
}

impl Browse {
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
}
