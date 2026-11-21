# rembro

rembro opens a web browser (or any application) that uses the network of an SSH host, a container, a Docker sbx sandbox, a Kubernetes pod or an Incus instance. In that browser, `localhost` is the target's loopback and host names resolve with the target's DNS, so `db` in a Compose project or `api.shop.svc` in a cluster work, and so do addresses that only the target can reach.

```
rembro sbx:qut                    # open the sandbox's dev servers here
rembro docker:web http://localhost:3000
rembro k8s:shop/api-7d9f http://orders.shop.svc:8080
rembro --remote ssh:devbox        # run the browser on devbox, show its window here
rembro --debug sbx:qut            # let an agent in the sandbox drive the browser
rembro run ssh:rpi gnome-calculator
rembro install docker:web firefox # put a browser in the container for --remote
```

The browser runs in one of two places:

- On this machine, by default. rembro points the browser at a SOCKS5 proxy on this machine's loopback, and the target makes every connection for it. You keep your own browser, fonts, GPU and window manager, with a separate profile for each target.
- In the target, with `--remote`. Its window comes to this desktop through [waypipe](https://gitlab.freedesktop.org/mstoeckl/waypipe) on Wayland or through a forwarded X11 connection, and its sound plays here.

`rembro run` starts any other graphical program in the target and shows its windows here in the same way.

rembro runs on Linux, and targets must be Linux too. It carries TCP only, so when the browser runs here, QUIC and WebRTC media don't go through the target.

## Install

Static builds for x86_64, aarch64 and armv7 Linux are on the [releases page](https://github.com/ScriptSmith/rembro/releases). Each archive holds `rembro`, the rembro builds for the other two architectures, and static waypipe builds for all three. rembro copies those into targets that need them, so keep them next to `rembro`:

```
mkdir -p ~/.local/lib/rembro ~/.local/bin
curl -L https://github.com/ScriptSmith/rembro/releases/latest/download/rembro-x86_64-linux.tar.gz |
  tar -xz -C ~/.local/lib/rembro --strip-components=1
ln -sf ~/.local/lib/rembro/rembro ~/.local/bin/rembro
```

With Cargo:

```
cargo install rembro --target x86_64-unknown-linux-musl
```

A Cargo install has no builds for other architectures or waypipe next to it. rembro downloads the ones it needs from the GitHub release of the same version, checks them against the release's `SHA256SUMS`, and keeps them in `~/.cache/rembro/VERSION`. A plain `cargo install rembro` also works, but it links against this machine's glibc. Since rembro copies its own binary into the target, that build fails in Alpine containers and on distributions with an older glibc. The musl build runs on any Linux.

## Targets

| Target | What it is |
|---|---|
| `local` | this machine |
| `ssh:[USER@]HOST[:PORT]` | a host reached with `ssh`, using your ssh config |
| `docker:NAME`, `podman:NAME`, `nerdctl:NAME` | a running container |
| `sbx:SANDBOX` | a Docker sbx sandbox |
| `k8s:[NAMESPACE/]POD[/CONTAINER]` | a pod in kubectl's current context |
| `incus:INSTANCE`, `lxc:INSTANCE` | an Incus or LXD instance |
| `exec:COMMAND` | any command that runs its arguments in the target, such as `exec:'docker compose exec -T web'` |

The target needs only `sh` and a writable directory. On first contact rembro copies itself there over the exec command's stdin, and later sessions start the cached copy.

## Getting started

Start a dev server in the target and run `rembro TARGET`. With no URL, rembro opens a tab for each HTTP server listening on the target's loopback, and more tabs as new servers start. Give URLs to open particular pages instead.

rembro picks the first browser it finds, starting with Chromium and its forks and then Firefox. Use `-b firefox` (or set `REMBRO_BROWSER`) to choose. It never uses your regular browser profile unless you ask: each target gets its own profile.

For `--remote`, the target needs a browser of its own. `rembro install TARGET` installs Chromium there, and `rembro install TARGET firefox` installs Firefox.

Programs in the target can open pages in rembro's browser through `BROWSER` and an `xdg-open` wrapper, so OAuth flows use the target's network ([details](docs/usage.md#from-programs-in-the-target)).

`rembro --help` lists every option, target type and example. `rembro run --help` and `rembro install --help` cover the subcommands.

## Security

- With `--debug` and the browser on this machine, a program in the target can read your local files and the profile's cookies, so use `--remote --debug` for a target you don't trust ([details](docs/security.md#debugging)).
- `--x11-trusted` gives the target the same access to your X11 display that `ssh -Y` does, and so does an X server that can't make untrusted connections ([details](docs/security.md#x11)).
- `--remote` and `rembro run` forward this machine's sound server, which programs in the target can record from; `--no-audio` turns it off ([details](docs/security.md#sound)).
- `--trust-target-ca` lets the target read that profile's HTTPS traffic ([details](docs/security.md#trusting-the-targets-ca-certificates)).

## Documentation

- [docs/usage.md](docs/usage.md): targets, browsers, profiles, opening pages, `--debug`, `rembro run`, `rembro install` and every option.
- [docs/how-it-works.md](docs/how-it-works.md): the agent, its transport, other architectures, networking, display and sound forwarding, reconnects, and building rembro.
- [docs/security.md](docs/security.md): what the target can see and do, debugging, X11, sound, CA trust and the local SOCKS server.

## License

rembro is MIT or Apache-2.0, at your option. The waypipe builds in its releases are GPL-3.0-or-later; `licenses/waypipe-NOTICE.md` in each release archive describes them, and `waypipe-source.tar.gz` in each release is their source.
