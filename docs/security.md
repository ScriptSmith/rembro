# Security

This page describes what a target can see and do while rembro is connected to it, and the risks that come with `--debug`, X11 and sound forwarding, and `--trust-target-ca`.

## What the target can see

When the browser runs here, the agent in the target resolves every host name and makes every connection for it. Whoever controls the target can see which hosts and ports the browser contacts, and the contents of plain HTTP. HTTPS stays encrypted between the browser and the site unless you use `--trust-target-ca`. The browser's profile, with its cookies and history, stays on this machine.

With `--remote`, the browser and its profile live in the target, so the target has its cookies, history and everything it shows.

In a chain of targets, the session passes through every target before the last, so each of them can see and change it as the last one can. Hosts that `ssh -J` jumps through are the exception: they forward an encrypted SSH connection.

rembro answers no requests from the agent. The agent sends rembro program output, exit statuses and URLs to open, and it can open connections back to this machine only to the endpoints the session set up: the debugging endpoint, the waypipe socket, the X11 display and the sound server.

Each session gets a new profile unless you name one, and `REMBRO_PROFILE` doesn't apply with `--debug`, so a program that drives the browser gets the cookies of a profile you use only when you pass that profile with `--profile` or `--fork`. Named profiles are kept for each target separately. [Profiles](usage.md#profiles) has the details.

## Debugging

`--debug` exposes the browser's debugging endpoint on the target's loopback. The endpoint has no authentication, so any program in the target that can reach that loopback can use it, including other users' programs on a shared host.

Without `--remote`, the browser runs on this machine, and the debugging protocol can open `file://` URLs and read anything the browser can. A program in the target can therefore read your files and the profile's cookies, and rembro prints a warning saying so. If you don't trust the target, use `--remote --debug`, which keeps the browser and its endpoint inside the target.

## X11

For X11, rembro listens on `127.0.0.1:60NN` in the target and relays each connection to this machine's X server. By default it asks the X server for an untrusted cookie through the SECURITY extension, as `ssh -X` does, and revokes it when the session ends. The X server also deletes the cookie after five minutes with no client connected.

Untrusted programs can read other clients' window properties, but can't capture other windows, send them input, change their properties or server settings, or use extensions other than BIG-REQUESTS and XC-MISC. Typing, pasting into the program and resizing work, but copying out of it into other applications does not, and touchpad scrolling and keyboard layout switching may behave differently.

The relay refuses any connection that doesn't present the cookie it handed out, whatever access the X server would grant. The program finds the cookie in an Xauthority file that only its user can read.

`--x11-trusted` hands over your own cookie instead, which gives the target the same access to your display that `ssh -Y` does: it can see and type into every window. rembro prints a warning when that happens. It also falls back to your own cookie, with the same warning, when the X server can't make an untrusted connection.

## Sound

For `--remote` and `rembro run`, rembro forwards this machine's PulseAudio socket, and its cookie when there is one, to the target. Programs there can then use the sound server as a program running here can, which includes recording from its inputs. `--no-audio` turns this off.

## Trusting the target's CA certificates

A proxy that intercepts TLS, or an internal service with a private CA, presents certificates the browser here does not trust. `--trust-target-ca` makes the profile trust the CA certificates that an administrator added in the target: those in `/usr/local/share/ca-certificates`, `/etc/pki/ca-trust/source/anchors`, `/etc/ca-certificates/trust-source/anchors`, `/etc/pki/trust/anchors`, and `$NODE_EXTRA_CA_CERTS` unless that names the system bundle. It works with Chromium and Firefox, and only when the browser runs here. On Windows it works with Chromium only, since Windows has neither p11-kit nor NSS's `certutil` (Windows's own `certutil` is a different program).

Only that profile trusts them. rembro doesn't change `~/.pki/nssdb`, anything in `/etc`, or your other profiles.

- For Chromium, rembro adds them to the profile's own certificate store (`Default/ServerCertificate`, where chrome://certificate-manager keeps certificates you install), and leaves any you installed yourself alone.
- For Firefox, the profile's `pkcs11.txt` loads p11-kit's trust module with a bundle of the certificates, or, where p11-kit is missing, `certutil` adds them to the profile's `cert9.db`. On macOS rembro always uses `certutil`, which Homebrew's `nss` package provides.

Without the flag, rembro removes certificates that an earlier launch added. On a profile that rembro did not create, it removes them again when the browser exits. If the profile is open in another window, its trusted CAs stay as they were.

Trusting a target's CA lets the target read that profile's HTTPS traffic, so use it only for targets you trust with it.

## The SOCKS server

When the browser runs here, rembro's SOCKS server listens on a random port on 127.0.0.1. On Linux it looks up each connection's owner in `/proc/net/tcp` and `/proc/net/tcp6`, and refuses connections from other users on this machine, which could otherwise use it to reach into the target. macOS and Windows don't show rembro who owns a TCP connection, so there it accepts every connection to that port, and while it runs, other users of the machine who find the port can reach the target through it.

## Pages opened from the target

The socket that `rembro-open` and the `xdg-open` wrapper use is in a directory that only your user in the target can open, `$XDG_RUNTIME_DIR/rembro` or `/tmp/rembro-UID`. Only `http` and `https` URLs without spaces or control characters are accepted, at most ten every ten seconds. The agent in the target checks each URL, and rembro on this machine checks it again, because a compromised target could send anything, and a "URL" that starts with `-` would reach the browser as a command-line option. rembro also applies the rate limit.

## Chromium as root

When the target runs a remote Chromium as root, rembro adds `--no-sandbox`, since Chromium won't start as root with its sandbox on, and prints a warning that the sandbox is off.
