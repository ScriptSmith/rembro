//! Where the browser's network lives, and how to run a command there.

use std::process::Command;

use crate::util::{sh_quote, slug, split_words};

pub const FORMS: &str = "local, ssh:[USER@]HOST[:PORT], docker:CONTAINER, podman:CONTAINER, nerdctl:CONTAINER, \
apple-container:CONTAINER, sbx:SANDBOX, k8s:[NAMESPACE/]POD[/CONTAINER], incus:INSTANCE, lxc:INSTANCE, wsl:DISTRO or \
exec:COMMAND";

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Local,
    Ssh { dest: String, port: Option<u16> },
    Container { engine: &'static str, name: String },
    AppleContainer { name: String },
    Sbx { name: String },
    K8s { namespace: Option<String>, pod: String, container: Option<String> },
    Incus { cli: &'static str, name: String },
    Wsl { distro: String },
    Exec { argv: Vec<String> },
}

impl Target {
    pub fn parse(s: &str) -> Result<Target, String> {
        if s == "local" || s == "localhost" {
            return Ok(Target::Local);
        }
        let Some((scheme, rest)) = s.split_once(':') else {
            return Err(format!("unknown target {s:?}; use {FORMS}"));
        };
        let rest = rest.strip_prefix("//").unwrap_or(rest);
        let need = |what: &str| -> Result<String, String> {
            if rest.is_empty() { Err(format!("{scheme}: needs {what}")) } else { Ok(rest.to_string()) }
        };
        Ok(match scheme {
            "ssh" => {
                let rest = need("a host")?;
                let rest = rest.trim_end_matches('/');
                match rest.rsplit_once(':') {
                    Some((dest, port)) if !dest.ends_with(']') || dest.contains('[') => {
                        let port = port.parse().map_err(|_| format!("bad ssh port in {s:?}"))?;
                        Target::Ssh { dest: dest.trim_matches(['[', ']']).to_string(), port: Some(port) }
                    }
                    _ => Target::Ssh { dest: rest.to_string(), port: None },
                }
            }
            "docker" | "podman" | "nerdctl" => {
                let engine = match scheme {
                    "docker" => "docker",
                    "podman" => "podman",
                    _ => "nerdctl",
                };
                Target::Container { engine, name: need("a container name")? }
            }
            "apple-container" => Target::AppleContainer { name: need("a container name")? },
            "sbx" => Target::Sbx { name: need("a sandbox name")? },
            "k8s" | "kube" | "kubectl" => {
                let rest = need("a pod name")?;
                let parts: Vec<&str> = rest.split('/').collect();
                let (namespace, pod, container) = match parts.as_slice() {
                    [pod] => (None, *pod, None),
                    [ns, pod] => (Some(*ns), *pod, None),
                    [ns, pod, c] => (Some(*ns), *pod, Some(*c)),
                    _ => return Err(format!("expected k8s:[NAMESPACE/]POD[/CONTAINER], not {s:?}")),
                };
                if pod.is_empty() || namespace == Some("") || container == Some("") {
                    return Err(format!("expected k8s:[NAMESPACE/]POD[/CONTAINER], not {s:?}"));
                }
                Target::K8s {
                    namespace: namespace.map(String::from),
                    pod: pod.to_string(),
                    container: container.map(String::from),
                }
            }
            "incus" => Target::Incus { cli: "incus", name: need("an instance name")? },
            "lxc" => Target::Incus { cli: "lxc", name: need("an instance name")? },
            "wsl" => Target::Wsl { distro: need("a distribution name, as wsl --list shows it")? },
            "exec" => {
                let argv = split_words(rest).map_err(|e| format!("exec: {e}"))?;
                if argv.is_empty() {
                    return Err("exec: needs a command, such as exec:'docker compose exec -T web'".into());
                }
                Target::Exec { argv }
            }
            _ => return Err(format!("unknown target type {scheme:?}; use {FORMS}")),
        })
    }

    /// A name for the target's directory of browser profiles.
    pub fn slug(&self) -> String {
        match self {
            Target::Local => "local".into(),
            Target::Ssh { dest, port } => {
                slug(&format!("ssh-{dest}{}", port.map(|p| format!("-{p}")).unwrap_or_default()))
            }
            Target::Container { engine, name } => slug(&format!("{engine}-{name}")),
            Target::AppleContainer { name } => slug(&format!("apple-container-{name}")),
            Target::Sbx { name } => slug(&format!("sbx-{name}")),
            Target::K8s { namespace, pod, container } => slug(&format!(
                "k8s-{}{pod}{}",
                namespace.as_ref().map(|n| format!("{n}-")).unwrap_or_default(),
                container.as_ref().map(|c| format!("-{c}")).unwrap_or_default()
            )),
            Target::Incus { cli, name } => slug(&format!("{cli}-{name}")),
            Target::Wsl { distro } => slug(&format!("wsl-{distro}")),
            Target::Exec { argv } => slug(&format!("exec-{}", argv.join("-"))),
        }
    }

    /// A command that runs `argv` in the target with stdin and stdout passed through.
    pub fn command(&self, argv: &[String]) -> Command {
        let mut cmd;
        match self {
            Target::Local => {
                cmd = Command::new(&argv[0]);
                cmd.args(&argv[1..]);
            }
            Target::Ssh { dest, port } => {
                cmd = Command::new("ssh");
                cmd.args(["-T", "-e", "none", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=4"]);
                if let Some(p) = port {
                    cmd.arg("-p").arg(p.to_string());
                }
                let line: Vec<String> = argv.iter().map(|a| sh_quote(a)).collect();
                cmd.arg("--").arg(dest).arg(line.join(" "));
            }
            Target::Container { engine, name } => {
                cmd = Command::new(engine);
                cmd.args(["exec", "-i", name]).args(argv);
            }
            Target::AppleContainer { name } => {
                cmd = Command::new("container");
                cmd.args(["exec", "-i", name]).args(argv);
            }
            Target::Sbx { name } => {
                cmd = Command::new("sbx");
                cmd.args(["exec", "-i", name]).args(argv);
            }
            Target::K8s { namespace, pod, container } => {
                cmd = Command::new("kubectl");
                cmd.arg("exec").arg("-i");
                if let Some(ns) = namespace {
                    cmd.arg("-n").arg(ns);
                }
                cmd.arg(pod);
                if let Some(c) = container {
                    cmd.arg("-c").arg(c);
                }
                cmd.arg("--").args(argv);
            }
            Target::Incus { cli, name } => {
                cmd = Command::new(cli);
                cmd.args(["exec", "--mode=non-interactive", name, "--"]).args(argv);
            }
            Target::Wsl { distro } => {
                cmd = Command::new("wsl.exe");
                cmd.args(["--distribution", distro, "--exec"]).args(argv);
            }
            Target::Exec { argv: prefix } => {
                cmd = Command::new(&prefix[0]);
                cmd.args(&prefix[1..]).args(argv);
            }
        }
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(Target::parse("local").unwrap(), Target::Local);
        assert_eq!(Target::parse("ssh:me@box").unwrap(), Target::Ssh { dest: "me@box".into(), port: None });
        assert_eq!(
            Target::parse("ssh://me@box:2222").unwrap(),
            Target::Ssh { dest: "me@box".into(), port: Some(2222) }
        );
        assert_eq!(Target::parse("sbx:qut").unwrap(), Target::Sbx { name: "qut".into() });
        assert_eq!(Target::parse("apple-container:web").unwrap(), Target::AppleContainer { name: "web".into() });
        assert!(Target::parse("container:web").is_err());
        assert_eq!(Target::parse("wsl:Ubuntu-24.04").unwrap(), Target::Wsl { distro: "Ubuntu-24.04".into() });
        assert_eq!(
            Target::parse("k8s:shop/api/app").unwrap(),
            Target::K8s { namespace: Some("shop".into()), pod: "api".into(), container: Some("app".into()) }
        );
        assert_eq!(
            Target::parse("exec:docker compose exec -T web").unwrap(),
            Target::Exec { argv: vec!["docker".into(), "compose".into(), "exec".into(), "-T".into(), "web".into()] }
        );
        assert!(Target::parse("qut").is_err());
        assert!(Target::parse("docker:").is_err());
        assert!(Target::parse("wsl:").is_err());
        assert!(Target::parse("k8s:a/b/c/d").is_err());
    }

    #[test]
    fn slugs() {
        assert_eq!(Target::parse("sbx:qut").unwrap().slug(), "sbx-qut");
        assert_eq!(Target::parse("ssh:me@box:22").unwrap().slug(), "ssh-me@box-22");
        assert_eq!(Target::parse("k8s:shop/api").unwrap().slug(), "k8s-shop-api");
        assert_eq!(Target::parse("apple-container:web").unwrap().slug(), "apple-container-web");
    }
}
