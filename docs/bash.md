# The bash version

[rembro.sh](../rembro.sh) does the core of rembro on Linux in under 100 lines of bash. It opens Chromium or Firefox here with its connections made from the target, and with no URL it opens a tab for each HTTP server listening there. Each target gets its own browser profile, kept in `~/.local/share/rembro-sh`.

```sh
curl -fsSLO https://raw.githubusercontent.com/ScriptSmith/rembro/main/rembro.sh && chmod +x rembro.sh
./rembro.sh ssh:devbox
./rembro.sh docker:shop-web-1 http://api:8000
```

It takes `ssh:`, `docker:`, `podman:`, `sbx:`, `k8s:` and `exec:` targets, written the same way as rembro's but without the `kube:` and `kubectl:` aliases or a `//` after the colon. `REMBRO_BROWSER` sets the browser command; by default it uses the first Chromium-based browser it finds, then Firefox. `./rembro.sh --help` lists the targets.

It needs bash 4.4 or later and curl here, plus socat for targets other than `ssh:`, and bash or `nc` in those targets.

## How it works

For `ssh:` targets, the script runs `ssh -D` on a random port on 127.0.0.1 and points the browser at it. For the other targets, it runs a SOCKS5 server under socat. Each connection starts a bash handler that reads the browser's SOCKS request, then runs an exec in the target that connects with bash's `/dev/tcp`, or with `nc` where the target has no bash. busybox `nc` tries only the first address of a name, which for `localhost` is often `::1`, so for `localhost` the handler checks 127.0.0.1 first with `nc -z`.

To find servers, it reads `/proc/net/tcp` and `/proc/net/tcp6` in the target, and requests `/` with curl through the proxy on each listening port below 32768. Ports that answer with HTTP get a tab.

The browser gets the same proxy settings that rembro gives it: Chromium's flags send loopback addresses through the proxy and keep its DNS prefetcher and WebRTC from going around it, and Firefox gets the equivalent prefs in the profile's `user.js`.

## Compared with rembro

- Each connection the browser opens starts a new exec in the target, which takes about 50 ms with Docker and half a second with sbx. rembro carries every connection over one exec.
- It opens tabs only for the servers listening when it starts, and doesn't reconnect when the connection drops.
- `ssh:` targets need an SSH server that allows TCP forwarding (`AllowTcpForwarding`). rembro runs its agent as an ordinary remote command, so it doesn't.
- Any user on this machine can reach the target through its SOCKS port while it runs. rembro refuses other users' connections on Linux.
- A page that can't connect shows an empty response, because the script answers the browser's SOCKS request before the target has connected.
- It has none of `--remote`, `--debug`, `rembro run`, `rembro install`, sound forwarding, `--loopback-only`, `--target-proxy` or `--trust-target-ca`, and programs in the target can't open pages in its browser.
- It runs only on Linux, with Chromium-based browsers and Firefox.
