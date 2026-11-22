//! Where the browser's network lives, and how to run a command there.

use std::process::Command;

use crate::util::{sh_quote, slug, split_words};

pub const FORMS: &str = "local, ssh:[USER@]HOST[:PORT], docker:CONTAINER, podman:CONTAINER, nerdctl:CONTAINER, \
apple-container:CONTAINER, sbx:SANDBOX, k8s:[NAMESPACE/]POD[/CONTAINER], incus:INSTANCE, lxc:INSTANCE, wsl:DISTRO, \
exec:COMMAND, or several of these joined with commas, such as ssh:bastion,ssh:box,docker:web";

const SCHEMES: &str = "ssh docker podman nerdctl apple-container sbx k8s kube kubectl incus lxc wsl exec";

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
    Chain(Vec<Target>),
}

impl Target {
    pub fn parse(s: &str) -> Result<Target, String> {
        let hops = split_hops(s);
        if hops.len() == 1 {
            return Target::parse_hop(s);
        }
        let hops = hops.into_iter().map(Target::parse_hop).collect::<Result<Vec<_>, _>>()?;
        if hops.contains(&Target::Local) {
            return Err(format!("local can't be one of the targets in {s:?}"));
        }
        Ok(Target::Chain(hops))
    }

    fn parse_hop(s: &str) -> Result<Target, String> {
        if s == "local" || s == "localhost" {
            return Ok(Target::Local);
        }
        let Some((scheme, rest)) = s.split_once(':') else {
            return Err(format!("unknown target {s:?}; use {FORMS}"));
        };
        let rest = rest.strip_prefix("//").unwrap_or(rest);
        if scheme != "exec" && rest.contains(',') {
            return Err(format!("unknown target type after a comma in {s:?}; use {FORMS}"));
        }
        let need = |what: &str| -> Result<String, String> {
            if rest.is_empty() { Err(format!("{scheme}: needs {what}")) } else { Ok(rest.to_string()) }
        };
        Ok(match scheme {
            "ssh" => {
                let rest = need("a host")?;
                let rest = rest.trim_end_matches('/');
                match rest.rsplit_once(':') {
                    _ if rest.ends_with(']') => Target::Ssh { dest: rest.replace(['[', ']'], ""), port: None },
                    Some((dest, port)) if !dest.ends_with(']') || dest.contains('[') => {
                        let port = port.parse().map_err(|_| format!("bad ssh port in {s:?}"))?;
                        Target::Ssh { dest: dest.replace(['[', ']'], ""), port: Some(port) }
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
                let program = argv[0].rsplit(['/', '\\']).next().unwrap_or_default();
                if program == "ssh" || program.eq_ignore_ascii_case("ssh.exe") {
                    return Err("exec: can't run ssh, which joins its arguments into one line for the remote shell. \
                                Use ssh:HOST, or a chain such as ssh:HOST,docker:CONTAINER, with ssh options in your \
                                ssh config"
                        .into());
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
            Target::Chain(hops) => hops.iter().map(Target::slug).collect::<Vec<_>>().join("_"),
        }
    }

    /// A command that runs `argv` in the target with stdin and stdout passed through.
    pub fn command(&self, argv: &[String]) -> Command {
        let line = self.command_line(argv);
        let mut cmd = Command::new(&line[0]);
        cmd.args(&line[1..]);
        cmd
    }

    fn command_line(&self, argv: &[String]) -> Vec<String> {
        let words = |prefix: &[&str]| -> Vec<String> {
            prefix.iter().map(|w| w.to_string()).chain(argv.iter().cloned()).collect()
        };
        match self {
            Target::Local => argv.to_vec(),
            Target::Ssh { dest, port } => ssh_line(&[], dest, *port, argv),
            Target::Container { engine, name } => words(&[engine, "exec", "-i", name]),
            Target::AppleContainer { name } => words(&["container", "exec", "-i", name]),
            Target::Sbx { name } => words(&["sbx", "exec", "-i", name]),
            Target::K8s { namespace, pod, container } => {
                let mut line = vec!["kubectl".to_string(), "exec".into(), "-i".into()];
                if let Some(ns) = namespace {
                    line.extend(["-n".into(), ns.clone()]);
                }
                line.push(pod.clone());
                if let Some(c) = container {
                    line.extend(["-c".into(), c.clone()]);
                }
                line.push("--".into());
                line.extend(argv.iter().cloned());
                line
            }
            Target::Incus { cli, name } => words(&[cli, "exec", "--mode=non-interactive", name, "--"]),
            Target::Wsl { distro } => words(&["wsl.exe", "--distribution", distro, "--exec"]),
            Target::Exec { argv: prefix } => prefix.iter().chain(argv).cloned().collect(),
            Target::Chain(hops) => {
                let mut line = argv.to_vec();
                let mut rest = &hops[..];
                while let Some((last, outer)) = rest.split_last() {
                    let mut first = outer.len();
                    if let Target::Ssh { dest, port } = last {
                        while first > 0 && matches!(outer[first - 1], Target::Ssh { .. }) {
                            first -= 1;
                        }
                        line = ssh_line(&outer[first..], dest, *port, &line);
                    } else {
                        line = last.command_line(&line);
                    }
                    rest = &outer[..first];
                }
                line
            }
        }
    }
}

/// Splits a chain of targets at each comma that a target type follows.
fn split_hops(s: &str) -> Vec<&str> {
    let starts_hop = |t: &str| match t.find([':', ',']) {
        Some(i) if t[i..].starts_with(':') => SCHEMES.split(' ').any(|scheme| scheme == &t[..i]),
        i => matches!(&t[..i.unwrap_or(t.len())], "local" | "localhost"),
    };
    let mut hops = Vec::new();
    let mut start = 0;
    for (i, _) in s.match_indices(',') {
        if starts_hop(&s[i + 1..]) {
            hops.push(&s[start..i]);
            start = i + 1;
        }
    }
    hops.push(&s[start..]);
    hops
}

/// ssh to `dest` through `jumps`, which are ssh targets, running `argv` there.
fn ssh_line(jumps: &[Target], dest: &str, port: Option<u16>, argv: &[String]) -> Vec<String> {
    let mut line: Vec<String> =
        ["ssh", "-T", "-e", "none", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=4"]
            .map(String::from)
            .into();
    let jumps: Vec<String> = jumps
        .iter()
        .filter_map(|t| match t {
            Target::Ssh { dest, port } => Some(jump(dest, *port)),
            _ => None,
        })
        .collect();
    if !jumps.is_empty() {
        line.extend(["-J".into(), jumps.join(",")]);
    }
    if let Some(p) = port {
        line.extend(["-p".into(), p.to_string()]);
    }
    let quoted: Vec<String> = argv.iter().map(|a| sh_quote(a)).collect();
    line.extend(["--".into(), dest.to_string(), quoted.join(" ")]);
    line
}

/// An ssh destination as `ssh -J` takes it, with brackets around an IPv6 address.
fn jump(dest: &str, port: Option<u16>) -> String {
    let (user, host) = match dest.rsplit_once('@') {
        Some((user, host)) => (format!("{user}@"), host),
        None => (String::new(), dest),
    };
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    match port {
        Some(p) => format!("{user}{host}:{p}"),
        None => format!("{user}{host}"),
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
        assert_eq!(Target::parse("ssh:me@[::1]:22").unwrap(), Target::Ssh { dest: "me@::1".into(), port: Some(22) });
        assert_eq!(Target::parse("ssh:[fe80::1]").unwrap(), Target::Ssh { dest: "fe80::1".into(), port: None });
        assert!(Target::parse("exec:ssh box").is_err());
        assert!(Target::parse("exec:/usr/bin/ssh -p 22 box").is_err());
    }

    #[test]
    fn chains() {
        assert_eq!(
            Target::parse("ssh:bastion,ssh://me@box:2222,sbx:dev,docker:web").unwrap(),
            Target::Chain(vec![
                Target::Ssh { dest: "bastion".into(), port: None },
                Target::Ssh { dest: "me@box".into(), port: Some(2222) },
                Target::Sbx { name: "dev".into() },
                Target::Container { engine: "docker", name: "web".into() },
            ])
        );
        assert_eq!(
            Target::parse("ssh:box,exec:tool --hosts a,b").unwrap(),
            Target::Chain(vec![
                Target::Ssh { dest: "box".into(), port: None },
                Target::Exec { argv: vec!["tool".into(), "--hosts".into(), "a,b".into()] },
            ])
        );
        assert!(Target::parse("local,docker:web").is_err());
        assert!(Target::parse("ssh:box,localhost").is_err());
        assert!(Target::parse("ssh:box,nope:web").is_err());
        assert!(Target::parse("ssh:box,exec:ssh inner").is_err());
    }

    fn line(target: &str) -> Vec<String> {
        let argv = ["sh".to_string(), "-c".into(), "echo 'hi'".into()];
        Target::parse(target).unwrap().command_line(&argv)
    }

    const SSH: [&str; 8] = ["ssh", "-T", "-e", "none", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=4"];

    #[test]
    fn command_lines() {
        let script = r"sh -c 'echo '\''hi'\'''";
        assert_eq!(line("ssh:box"), [&SSH[..], &["--", "box", script]].concat());
        assert_eq!(line("docker:web"), ["docker", "exec", "-i", "web", "sh", "-c", "echo 'hi'"]);
        let sbx = format!("sbx exec -i dev {script}");
        assert_eq!(
            line("ssh:bastion,ssh:[::1]:2222,ssh:me@box:22,sbx:dev"),
            [&SSH[..], &["-J", "bastion,[::1]:2222", "-p", "22", "--", "me@box", &sbx]].concat()
        );
        assert_eq!(
            line("docker:jump,ssh:a,ssh:b"),
            [&["docker", "exec", "-i", "jump"], &SSH[..], &["-J", "a", "--", "b", script]].concat()
        );
        let inner: Vec<String> = [&SSH[..], &["--", "b", script]].concat().iter().map(|w| sh_quote(w)).collect();
        let docker = format!("docker exec -i x {}", inner.join(" "));
        assert_eq!(line("ssh:a,docker:x,ssh:b"), [&SSH[..], &["--", "a", &docker]].concat());
    }

    #[test]
    fn slugs() {
        assert_eq!(Target::parse("sbx:qut").unwrap().slug(), "sbx-qut");
        assert_eq!(Target::parse("ssh:me@box:22").unwrap().slug(), "ssh-me@box-22");
        assert_eq!(Target::parse("k8s:shop/api").unwrap().slug(), "k8s-shop-api");
        assert_eq!(Target::parse("apple-container:web").unwrap().slug(), "apple-container-web");
        assert_eq!(Target::parse("ssh:bastion,ssh:box,docker:web").unwrap().slug(), "ssh-bastion_ssh-box_docker-web");
    }
}
