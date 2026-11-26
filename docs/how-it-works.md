# How rembro works

This page describes how rembro starts its agent in a target and talks to it, how it handles targets of other architectures, how the browser's traffic reaches the target, how windows and sound come back, what happens when a connection drops, and how to build rembro.

## The agent

rembro runs a small POSIX `sh` script in the target through the target's exec command (`ssh`, `docker exec -i` and so on; [Targets](usage.md#targets) lists them). For the `local` target, which exists only on Linux, it starts its own binary directly.

The script looks for a cached agent in `~/.cache/rembro` (or `$XDG_CACHE_HOME/rembro`), `${TMPDIR:-/tmp}/rembro-UID` and `/dev/shm/rembro-UID`, in that order. The cached file is named after a hash of the binary, so a different rembro build never runs a stale agent. If the target has no copy that runs, rembro sends its own binary (1.2 to 1.5 MB, depending on the architecture) over the exec command's stdin. On macOS and Windows, where rembro's own binary is not a Linux program, it sends its Linux build for this machine's architecture instead. The script saves it in the first of those directories that it can write, checks that it runs, and removes older agent builds from that directory. Every later session starts the cached copy. rembro and the agent then check that they speak the same protocol version, and if they don't, rembro asks for a matching build with `--agent`.

rembro carries every connection, command and file over that one stdin/stdout pair. Calls, events and any number of byte streams share it, and each stream has its own 1 MiB send window in each direction, so one slow reader can't stall the others. Both sides send a ping every 15 seconds. Nothing listens on the target's network except the `--debug` endpoint and the X11 display described below.

rembro runs `ssh` with `-T -e none -o ServerAliveInterval=15 -o ServerAliveCountMax=4`, and Incus and LXD with `exec --mode=non-interactive`. For a chain of targets, it nests their commands, quotes the inner command line for each `ssh`, and joins adjacent `ssh:` targets into one `ssh -J`.

## Other architectures

Targets can have a different architecture from this machine. The bootstrap script reports `uname -m`, and for x86_64, aarch64 and armv7 rembro sends the matching build. On macOS and Windows, rembro first offers the Linux build for this machine's architecture, which Docker Desktop, WSL and most local VMs share, and switches in the same way when the target reports another. Other architectures need `--agent PATH` with a static build of rembro for them. rembro never swaps a binary given with `--agent` for another build, and stops with an error if it doesn't match the target's architecture.

rembro looks for the builds it copies into targets (`rembro-ARCH` for a target of another architecture, or for any target on macOS and Windows, and `waypipe-ARCH`) next to its own executable first, which is where the release archives put them. Then it looks in `~/.cache/rembro/VERSION` (under `$XDG_CACHE_HOME` if you set it), which is `~/Library/Caches/rembro/VERSION` on macOS and `%LOCALAPPDATA%\rembro\VERSION` on Windows. Failing both, it downloads the file with `curl` or `wget` (Windows 10 and later include `curl`) from the GitHub release of the same version, checks it against the release's `SHA256SUMS`, and keeps it in that cache directory. `REMBRO_RELEASES` changes where it downloads from (default `https://github.com/ScriptSmith/rembro/releases/download`).

A rembro linked against glibc copies that same binary into the target, so it fails in Alpine containers and on distributions with an older glibc. When an upload fails that way, rembro suggests a static build (`x86_64-unknown-linux-musl`), which runs on any Linux.

## Networking when the browser runs here

rembro starts a SOCKS5 server on a random port on 127.0.0.1 and points the browser at it. The browser passes host names to it unresolved, and the agent in the target resolves each name and makes the connection, so `localhost` is the target's loopback and names resolve with the target's DNS.

rembro also configures the browser so that traffic does not go around the proxy:

- Chromium gets `--proxy-server` and a `--proxy-bypass-list` that sends loopback addresses through the proxy too. `--host-resolver-rules` stop its DNS prefetcher from resolving names locally, and WebRTC is limited to proxied connections.
- Firefox gets prefs in `user.js`. It resolves names through the proxy, sends `localhost` through it, has DNS over HTTPS, DNS prefetching, HTTP/3 and direct failover turned off, and keeps WebRTC to the proxy.
- Epiphany follows GNOME's proxy settings, so rembro gives it a private GSettings keyfile in its profile that names the SOCKS server.
- MiniBrowser gets `--proxy`.

SOCKS carries only TCP, so QUIC and WebRTC media cannot reach the target. Use `--remote` for those.

`--loopback-only` sends only `localhost`, 127.0.0.0/8 and `::1` through the target, and everything else connects from this machine. Chromium does this with its bypass list. Firefox and Epiphany get a PAC file, `rembro.pac` in the profile, that sends `localhost`, `*.localhost`, `127.*` and `::1` to the SOCKS server and everything else direct. MiniBrowser has no way to do this.

### The target's HTTP proxy

By default the agent connects directly from the target and ignores proxy variables in its environment. With `--target-proxy` it tunnels through the target's HTTP proxy with `CONNECT`, the way command-line tools there would. It uses `https_proxy` for every port except 80, which uses `http_proxy`, falls back to `all_proxy`, accepts the upper-case names too, and skips hosts that `no_proxy` matches (host names, domain suffixes, CIDR ranges and `*`). It only understands `http://` proxies, and sends a user name and password in the proxy URL as basic authentication. When the target names no proxy, rembro says that `--target-proxy` changes nothing.

Docker sbx sandboxes are an example of why the direct default exists. Their environment names a proxy that intercepts TLS, while direct connections to allowed hosts carry the sites' real certificates and blocked hosts fail to resolve. `--trust-target-ca` makes the browser accept such a proxy's certificates; [Security](security.md#trusting-the-targets-ca-certificates) describes it.

## Display forwarding

`--display auto` uses Wayland through waypipe when this session is Wayland and waypipe is installed here, and X11 otherwise. When this machine has neither, as on macOS without XQuartz or on Windows without an X server, and the target has a display of its own, such as WSLg's in a WSL distribution, programs use the target's display and rembro forwards none. If the target has no waypipe, rembro copies in a static build (about 2 MB) and keeps it next to the agent. When it can't provide one, for example on an architecture it has no build for, it says so and uses X11. `--display wayland` and `--display x11` force one or the other.

waypipe runs with `--no-gpu`, since containers rarely have a GPU, and window titles get a `[TARGET]` prefix.

For X11, rembro listens on `127.0.0.1:60NN` in the target, sets `DISPLAY=127.0.0.1:NN` and `XAUTHORITY` for the program, and relays each connection to this machine's X server. It reaches the X server through the socket that `DISPLAY` names, which on macOS is a path to XQuartz's socket, or over TCP, which is how Windows X servers listen (`DISPLAY=127.0.0.1:0` means port 6000). By default the program gets an untrusted cookie, as with `ssh -X`. [Security](security.md#x11) describes what that allows and what `--x11-trusted` changes.

Distributions built Chromium without Wayland support until about version 98, and those builds crash under waypipe. With `--display auto`, rembro asks a Chromium in the target for its version and uses X11 for older ones. If a browser exits with an error within 20 seconds under Wayland, rembro suggests `--display x11`.

For a remote Chromium, rembro adds `--disable-gpu` and `--password-store=basic`, `--no-sandbox` when the target runs it as root, `--no-xshm` over X11, and `--disable-dev-shm-usage` when the target's `/dev/shm` is smaller than 1 GiB (Docker's default is 64 MiB).

When the target has no usable `XDG_RUNTIME_DIR`, rembro sets it to the session's temporary directory there.

## Sound

Sound from the target plays here through this machine's PulseAudio server, or PipeWire's PulseAudio server, when one is running. rembro forwards it (the first server in `$PULSE_SERVER`, which may be a `unix:` socket or a `tcp:` address, or else `$XDG_RUNTIME_DIR/pulse/native`) to a socket in the session's directory in the target, and sets `PULSE_SERVER` and `PULSE_COOKIE` for the program. `--no-audio` turns this off.

## Lifecycle

rembro exits when the browser or program does, with its exit status. On Ctrl-C, SIGTERM or SIGHUP it asks the program to close, kills it after five seconds, and exits with 128 plus the signal number. On Windows, Ctrl-C, Ctrl-Break and closing the console window do the same, and rembro closes a browser on this machine with `taskkill`, adding `/F` after five seconds. On Linux and macOS, when a browser here exits, rembro waits up to three seconds for the rest of its processes, and kills those still running, before it deletes a temporary profile.

When the browser runs here and the connection to the target drops, the browser stays open and rembro reconnects, retrying after one second and then with a delay that doubles up to 15 seconds. Pages that load in the meantime fail. If the `--debug` endpoint has to move to another port after a reconnect, rembro says so. With `--remote` and `rembro run`, the program lives in the target and ends with the connection.

The agent in the target stops when its stdin closes, when it gets SIGINT, SIGTERM or SIGHUP, or after 60 seconds without hearing from rembro, since some transports (older `kubectl`) don't pass on the end of input. It then sends SIGTERM to everything it started, sends SIGKILL to what is left three seconds later, and deletes its temporary directory, any temporary profile and its URL socket. Programs it starts are also killed if the agent itself dies. With the `local` target, the agent runs in a process group of its own, so that Ctrl-C reaches only rembro. rembro gives up on an agent it hasn't heard from in 60 seconds in the same way.

## Building from source

```
cargo test
cargo build --release --target x86_64-unknown-linux-musl
```

On macOS and Windows, `cargo build --release` builds rembro for that system. It copies Linux builds into targets: those next to it, those in its cache, or those from the release of the same version. To try a change to the agent from there, build rembro for Linux, for example with `cargo zigbuild --release --target aarch64-unknown-linux-musl`, and pass it with `--agent`.

`--agent PATH` copies a different build into the target, for example a musl build while you work with a glibc one. `REMBRO_RELEASES` changes where rembro downloads other architectures' builds and waypipe from ([Other architectures](#other-architectures)).

`scripts/build-waypipe.sh TARGET OUTPUT` builds the static waypipe with cargo-zigbuild, and `.github/workflows/release.yml` shows how releases are put together.
