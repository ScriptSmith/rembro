# Using rembro

This page covers targets, containers and VMs, choosing a browser, profiles, opening pages, letting a program drive the browser, `--stealth`, `rembro run`, `rembro install`, displays on macOS and Windows, and the command-line options.

## Targets

| Target | rembro runs the agent with |
|---|---|
| `local` | this machine, on Linux only |
| `ssh:[USER@]HOST[:PORT]` | `ssh HOST`, using your ssh config |
| `docker:CONTAINER`, `podman:CONTAINER`, `nerdctl:CONTAINER` | `docker exec -i`, `podman exec -i` or `nerdctl exec -i` |
| `apple-container:CONTAINER` | `container exec -i`, with Apple's container tool on macOS |
| `sbx:SANDBOX` | `sbx exec -i` |
| `k8s:[NAMESPACE/]POD[/CONTAINER]` | `kubectl exec -i`, in kubectl's current context |
| `incus:INSTANCE`, `lxc:INSTANCE` | `incus exec` or `lxc exec` |
| `wsl:DISTRO` | `wsl.exe --distribution DISTRO --exec`, on Windows |
| `exec:COMMAND` | `COMMAND sh -c ...`, for example `exec:'docker compose exec -T web'` or `exec:'kubectl --context prod exec -i web --'` |
| `TARGET,TARGET...` | each target's command inside the one before it, so `ssh:bastion,ssh:box,docker:web` runs `ssh -J bastion box docker exec -i web sh -c ...` ([Targets inside targets](#targets-inside-targets)) |

`localhost` is another name for `local`, and `kube:` and `kubectl:` are other names for `k8s:`. A `//` after the colon is ignored, so `ssh://me@box:2222` also works.

With no target, rembro uses `local`. `rembro` then opens this machine's dev servers, and `rembro --debug` gives programs on this machine a browser to drive. When the first argument is not a target but starts with `http://`, `https://`, `localhost:`, `127.` or `[`, it is the first URL and the target is `local`, as in `rembro localhost:5173`. Any other first argument has to be a target, and a mistyped one such as `sbx-myapp` is an error rather than a URL. `rembro run` and `rembro install` always need a target. On macOS and Windows, where `local` can't run, leaving out the target is an error.

`exec:` splits COMMAND into words the way a shell would, without expansions, and appends its own arguments to them as separate words. That suits the `exec` subcommands of container tools. It does not quote the arguments for a remote shell, so rembro refuses an `exec:` command that runs `ssh`. Use `ssh:`, or a chain such as `ssh:box,docker:web`, and put ssh options in your ssh config.

The target must run Linux, and needs only `sh` and a writable directory. [How it works](how-it-works.md#the-agent) describes what rembro copies there.

### Targets inside targets

Join targets with commas, outermost first, to reach one through the others:

```sh
rembro ssh:bastion,ssh:box,sbx:dev,docker:web
```

rembro runs each target's command inside the one before it, and quotes the rest of the command line for the remote shell of each SSH host. Each tool has to be installed where its part of the chain runs: here `sbx` on box, and `docker` in the dev sandbox. Only the last target needs a writable directory for the agent.

Adjacent `ssh:` targets become one `ssh -J`, so `ssh:bastion,ssh:box` runs `ssh -J bastion box`. Your keys and ssh config stay on this machine, and the jump hosts need to allow TCP forwarding, as they do for any `ssh -J`. For one bastion, you can instead set `ProxyJump bastion` for box in your ssh config and use `ssh:box`.

A comma starts the next target only when a target type and a colon follow it, so commas inside an `exec:` command stay part of the command. `local` can't be part of a chain. Each chain gets its own profiles, as other targets do.

### Containers and VMs

- Docker Desktop, OrbStack, Colima, Rancher Desktop and Podman Desktop run containers in a Linux VM on macOS and Windows. `docker:`, `podman:` and `nerdctl:` reach those containers through the same CLIs, and `k8s:` reaches the Kubernetes clusters that some of these tools run.
- [Apple's container tool](https://github.com/apple/container) runs each container in its own lightweight VM on Apple silicon Macs. `rembro apple-container:web` reaches servers that listen only on the container's loopback, and names that resolve only inside it.
- On Windows, `wsl:DISTRO` reaches a WSL distribution, by the name `wsl --list` shows. With `--remote` and `rembro run`, windows from it appear through WSLg when Windows has no X server ([Displays on macOS and Windows](#displays-on-macos-and-windows)).

rembro reaches a VM over SSH, or through any command that runs programs in it:

| VM | Target |
|---|---|
| QEMU with user networking | `ssh:me@localhost:2222` |
| libvirt, UTM, Hyper-V, Parallels, VirtualBox | `ssh:me@ADDRESS` |
| Lima | `exec:'limactl shell default'` |
| Multipass | `exec:'multipass exec myvm --'` |
| WSL | `wsl:Ubuntu` |

A QEMU VM needs one forwarded port for the guest's SSH server. Through it, rembro reaches every port in the guest, which QEMU's user networking otherwise hides from the host:

```sh
qemu-system-x86_64 ... -nic user,hostfwd=tcp:127.0.0.1:2222-:22
rembro ssh:me@localhost:2222
```

A guest that emulates another architecture gets the matching agent when rembro has a build for it (x86_64, aarch64 or armv7). For any other, such as riscv64, pass a static build with `--agent`.

## Choosing a browser

`--browser` (`-b`, or the `REMBRO_BROWSER` variable) takes one of these names, or a command or path:

- `chromium`, `chrome`, `brave`, `edge`, `vivaldi`
- `firefox`, `librewolf`, `zen`
- `epiphany` (GNOME Web), `minibrowser` (WebKitGTK's MiniBrowser)
- the Playwright builds `playwright-chromium`, `playwright-firefox` and `playwright-webkit`, from `~/.cache/ms-playwright`
- `webkit`, which takes the first of Epiphany, MiniBrowser and Playwright WebKit that is installed

On macOS, rembro looks for these browsers in `/Applications` and `~/Applications`, and on Windows in `%ProgramFiles%`, `%ProgramFiles(x86)%` and `%LOCALAPPDATA%`, where their installers put them. The Playwright builds come from `~/Library/Caches/ms-playwright` on macOS and `%LOCALAPPDATA%\ms-playwright` on Windows. Epiphany, MiniBrowser and Playwright WebKit are Linux programs, so with a macOS or Windows host they work only with `--remote`.

`chromium` and `firefox` also fall back to the Playwright builds. The default, `auto`, takes the first browser it finds in this order: Chromium, Chrome, Brave, Edge, Vivaldi, Playwright Chromium, Firefox, LibreWolf, Zen, Playwright Firefox, Epiphany, MiniBrowser, Playwright WebKit. When the browser runs here, rembro looks on this machine. With `--remote` it looks in the target, and if it finds nothing it suggests `rembro install`. rembro skips the wrapper scripts that Ubuntu installs in place of snap packages, since they only ask you to install the snap.

For a command or path, rembro guesses the family from the file name: names containing `chrom`, `brave`, `edge`, `vivaldi`, `opera`, `thorium`, `yandex` or `helium` are Chromium, and names containing `firefox`, `librewolf`, `zen`, `floorp`, `waterfox`, `icecat` or `mullvad` are Firefox. For anything else, give `--family chromium|firefox|epiphany|minibrowser|playwright-webkit`.

| Family | On this machine | `--loopback-only` | `--remote` | `--debug` | `--trust-target-ca` |
|---|---|---|---|---|---|
| Chromium and its forks | flags | yes | yes | CDP | yes |
| Firefox and its forks | `user.js` prefs in the profile | yes, with a PAC file | yes | WebDriver BiDi | yes |
| Epiphany (GNOME Web) | private GSettings file | yes, with a PAC file | yes | no | no |
| WebKitGTK MiniBrowser | `--proxy` | no | yes | no | no |
| Playwright WebKit | no | no | yes | no | no |

Playwright WebKit takes no proxy on its command line, so it only works with `--remote`. On this machine, use Epiphany or MiniBrowser for WebKit.

Arguments after `--` go to the browser: `rembro sbx:myapp -- --incognito`.

## Profiles

rembro never uses your regular browser profile unless you ask it to. Each session gets a new profile, deleted when the browser exits, so every browser starts with no cookies, logins or history, and two sessions never share them. To keep a browser's logins for next time, give its profile a name:

- `--profile NAME` keeps the profile, and the next session with the same name, target and browser gets it back. `REMBRO_PROFILE=NAME` does the same for every session without `--debug`, and `--temp-profile` gives one session a new profile anyway.
- `--profile PATH` uses any directory as the profile. rembro treats a value as a path when it contains `/` (or on Windows, `\`) or is `~`.
- `--profile system` uses the browser's own default profile, the one it opens when you start it yourself, and `--profile system:NAME` uses its profile called NAME ([The browser's own profiles](#the-browsers-own-profiles)). rembro can't keep a profile of its own named `system`.
- `--fork NAME`, `--fork PATH` or `--fork system[:NAME]` starts from a copy of a profile, and deletes the copy when the browser exits. Log in once with `--profile work`, then run as many `--fork work` sessions as you like, at the same time, without changing `work`. The copy leaves out caches and the profile's locks. A fork of a profile that is open in another window has what that browser last saved to disk.

rembro keeps named profiles in `~/.local/share/rembro/profiles/TARGET/BROWSER/NAME` on this machine (under `$XDG_DATA_HOME` if you set it), where TARGET is the target written as a directory name, such as `sbx-myapp`. On macOS they live in `~/Library/Application Support/rembro/profiles`, and on Windows in `%LOCALAPPDATA%\rembro\profiles`. With `--remote` they live in `~/.local/share/rembro/profiles/BROWSER/NAME` in the target. Temporary profiles are `rembro-profile-XXXXXX` directories in the system's temporary directory, here or in the target. The links that a program under `rembro run` opens share one, in a private `rembro-run-XXXXXX` directory there. rembro 0.2 and earlier kept every session's data in a profile named `default`, or `agent` with `--debug`, and `--profile default` and `--profile agent` still open those.

Chromium downloads over 100 MB of optional components into a profile, such as the Widevine DRM module and certificate revocation lists. rembro starts it with `--disable-component-update` in temporary profiles, so that it doesn't download them again for each session.

To start from your regular logins without touching them, fork your browser's own profile: `rembro --fork system sbx:myapp`.

When a profile you chose with `--profile` is already open in another rembro window for the same target, the URLs open in that window, and the new rembro exits. A Chromium with a debugging endpoint gets them through it, which is the only way a headless one takes them. When the other rembro is still starting its browser, the new one waits up to 30 seconds for it.

When the browser runs here, rembro refuses two uses of `--profile PATH` or `--profile system` that would go wrong, unless you add `--force-profile`:

- The profile is open in a browser that rembro did not start, or in a rembro session for another target. The browser would hand the pages to that window, which uses a different network. With `--force-profile`, the pages open there anyway.
- The profile is a Firefox profile that rembro did not create, given as a path. rembro configures Firefox through prefs in `user.js`, which Firefox copies into `prefs.js`, so Firefox started on its own afterwards would use a proxy that no longer exists. With `--force-profile`, rembro saves the profile's `user.js` and the prefs it changes, and puts them back when the browser exits. If rembro is killed first, the next rembro launch with that profile puts them back. `--profile system` asks for this already, so it needs no `--force-profile`.

Chromium is configured with command-line flags only, so rembro can use a regular Chromium profile that is closed without changing it. `--trust-target-ca` on a profile rembro did not create removes the target's CAs again when the browser exits. rembro marks the profiles it creates with a `rembro-profile` file, locks a `rembro-session.lock` file in them while a session uses them, and keeps its prefs in `user.js` between two marker comments. For other profiles, the lock file and the file that names the session are in `rembro/locks` in rembro's data directory.

### The browser's own profiles

`system` stands for the profiles that the browser keeps for itself, which it opens when you start it without rembro:

- `--profile system` and `--fork system` use the browser's default profile. For Chromium and its forks, that is the one in `profile.last_used` in its `Local State` file, or else `Default`. For Firefox and its forks, it is the default of the first installation in `profiles.ini`, or else the profile marked `Default=1`, or the only one there is.
- `--profile system:NAME` and `--fork system:NAME` use the profile with that name, as the browser shows it, ignoring case. When two names differ only in case, NAME must match one of them exactly. When no profile has the name, NAME can be the profile's directory instead, such as `Profile 1` for Chromium or `x8k2q1tv.default-release` for Firefox. Quote a name with spaces: `--fork 'system:Work stuff'`.
- `REMBRO_PROFILE` takes the same values. `rembro profiles` lists the profiles it finds.

rembro looks for them where each browser keeps them, and takes the first place that has any:

| Browser | Linux | macOS, in `~/Library/Application Support` | Windows |
|---|---|---|---|
| `chromium` | `~/.config/chromium` | `Chromium` | `%LOCALAPPDATA%\Chromium\User Data` |
| `chrome` | `~/.config/google-chrome` | `Google/Chrome` | `%LOCALAPPDATA%\Google\Chrome\User Data` |
| `brave` | `~/.config/BraveSoftware/Brave-Browser` | `BraveSoftware/Brave-Browser` | `%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data` |
| `edge` | `~/.config/microsoft-edge` | `Microsoft Edge` | `%LOCALAPPDATA%\Microsoft\Edge\User Data` |
| `vivaldi` | `~/.config/vivaldi` | `Vivaldi` | `%LOCALAPPDATA%\Vivaldi\User Data` |
| `firefox` | `~/.mozilla/firefox`, then `~/.config/mozilla/firefox` | `Firefox` | `%APPDATA%\Mozilla\Firefox` |
| `librewolf` | `~/.librewolf`, then `~/.config/librewolf` | `librewolf` | `%APPDATA%\librewolf` |
| `zen` | `~/.zen`, then `~/.config/zen` | `zen` | `%APPDATA%\zen` |

On Linux, `~/.config` is `$XDG_CONFIG_HOME` when you set it, and rembro also looks in each browser's snap and flatpak, such as `~/snap/chromium/common/chromium`, `~/snap/firefox/common/.mozilla/firefox` and `~/.var/app/org.mozilla.firefox/.mozilla/firefox`. It looks there first when the browser it runs is the snap, or the flatpak by its ID, such as `-b org.mozilla.firefox`. With `--remote`, it looks in the target, at the Linux paths in the target's home directory, and a fork is made there. The Playwright builds, Epiphany and MiniBrowser have no `system` profiles; give another browser's profile with `--profile PATH`.

Chromium keeps all of its profiles in one directory, so rembro starts it with `--user-data-dir` set to that directory and `--profile-directory` set to the profile's. A Chromium that has any of them open has that whole directory open, so rembro refuses `--profile system:NAME` while you have that browser open, as it does for any profile that is open in a browser rembro did not start. A fork copies only the chosen profile and `Local State`, which holds the key that the profile's cookies and passwords are encrypted with. `--trust-target-ca` works with Chromium's `Default` profile only.

Google Chrome from version 136 ignores `--remote-debugging-port` on its own data directory, and so does Microsoft Edge ([Chrome's announcement](https://developer.chrome.com/blog/remote-debugging-port)). rembro therefore refuses `--debug` with `--profile system` for them, and suggests `--fork system`, whose copy lives in another directory. Chromium applies this rule only to Google Chrome's builds, so Chromium, Brave and Vivaldi still open the port.

On Windows, Chrome, Edge and Brave protect cookies with App-Bound Encryption, and decrypt them only when they run on their own data directory. A fork of their own profile can therefore start signed out of most sites, and rembro warns about it. On Linux and macOS, the browser takes its key from the system's keyring or Keychain wherever its data directory is, so a fork keeps the logins.

### Listing and deleting profiles

`rembro profiles` lists the profiles that rembro keeps on this machine, with their size and the day a session last used them. After them come the browsers' own profiles that it finds here, under the `system:NAME` that picks each one, with each browser's default marked. A Chromium profile that has been used for a while takes 150 to 200 MB.

`rembro profiles --prune DAYS` deletes the profiles that no session has used for DAYS days, and the temporary profiles that sessions which were killed left behind. Forking a profile counts as using it. Pruning never touches the browsers' own profiles, and leaves profiles that are open and profiles without the `rembro-profile` mark, such as one copied into rembro's directory by hand. It doesn't reach the profiles that `--remote` keeps in targets, which you can delete there.

## Opening pages

With no URL, rembro looks for HTTP servers on the target's loopback. It takes the ports that are listening on a loopback or wildcard address, sends each one a `GET /`, and opens a tab at `http://localhost:PORT` for each one that answers with HTTP.

rembro adds `http://` to a URL that starts with `localhost:`, `127.` or `[`, since Chromium doesn't always open those without it.

`--ports` (or `REMBRO_PORTS`) says which ports to look at: single ports such as `8080`, ranges such as `3000-3999`, patterns such as `41xx`, a comma-separated list of those, `all` or `none`. The default is `1-32767`, because Linux hands out ports from 32768 up to programs that bind port 0, such as language servers and debug adapters. rembro leaves out the ports it listens on itself in the target, for `--debug` and X11.

While the browser is open, rembro checks the target's listening ports every two seconds and opens a tab for each new HTTP server, giving a server 20 seconds to start answering. Each port is considered once: a port that was listening when rembro started, or that rembro has seen before, doesn't get a new tab. Watching is on by default when you give no URL. `--watch` turns it on when you do, and `--no-watch` turns it off.

### From programs in the target

Programs in the target can open pages in rembro's browser. rembro writes `rembro-open` and an `xdg-open` wrapper to a `bin` directory next to its agent in the target, usually `~/.cache/rembro/bin`. Programs that rembro starts get that directory at the front of `PATH` and `BROWSER` set to its `rembro-open`, so OAuth flows and "open in browser" actions land in the browser that shares their network. For programs started some other way, set `BROWSER` to the same path in the target, or call `rembro-open URL`.

`rembro-open` sends the URL to the newest rembro session for that user, through a socket in `$XDG_RUNTIME_DIR/rembro` or `/tmp/rembro-UID`. Only `http` and `https` URLs are accepted, at most ten every ten seconds. The `xdg-open` wrapper passes anything else to the target's own `xdg-open`, and does the same with web URLs when no rembro session is running.

Where the pages open depends on the mode:

- When the browser runs here, they open as new tabs.
- With `--remote`, rembro starts the browser in the target again, which hands the pages to the running one. This can fail with Firefox on Wayland (see [Limitations](#limitations)).
- With `--headless`, rembro opens them in Chromium through its debugging endpoint. Headless Firefox can't open them ([Letting a program drive the browser](#letting-a-program-drive-the-browser)).
- With `rembro run`, rembro starts `rembro TARGET URL` on this machine for the first one, and later pages go to that browser. Its profile is temporary unless `REMBRO_PROFILE` names one.

## Letting a program drive the browser

`--debug` exposes the browser's debugging endpoint inside the target, on `127.0.0.1:9222` (or a free port if 9222 is taken), and opens a page that shows how to connect in the first tab:

```
$ rembro --debug sbx:myapp
Programs in sbx:myapp can drive this browser over CDP at http://127.0.0.1:9222
  playwright-cli attach --cdp=http://127.0.0.1:9222
  ws://127.0.0.1:9222/devtools/browser/3fc09add-65ff-4426-9ef0-235c27b80487
```

From inside the target, Playwright's `chromium.connectOverCDP('http://127.0.0.1:9222')` and Puppeteer's `connect({ browserURL })` work against Chromium. Firefox serves WebDriver BiDi at `ws://127.0.0.1:PORT/session`, which Puppeteer reaches with `connect({ browserWSEndpoint, protocol: 'webDriverBiDi' })`.

`--debug=PORT` asks for a particular port, and rembro stops with an error if that port is taken. `--debug` works with Chromium and Firefox only. `REMBRO_PROFILE` doesn't apply to it, so the browser gets a new profile unless you give `--profile` or `--fork`. When the browser runs here, the endpoint also listens on `[::1]`, for clients that resolve `localhost` to `::1`. A Firefox that runs here listens on a port of its own, and it refuses requests whose `Host` header names any other port, so rembro changes the target's port in that header to Firefox's. rembro refuses a profile given with `--profile` that is already open in another window, since that browser can't be given a debugging port; `--fork` it instead.

With `--debug`, rembro starts Chromium with four flags that Playwright also passes when it starts Chromium: `--enable-features=CDPScreenshotNewSurface`, `--disable-backgrounding-occluded-windows`, `--disable-renderer-backgrounding` and `--disable-background-timer-throttling`. Without them, Chromium throttles pages in background tabs and in covered or minimized windows, and screenshots of a minimized window time out, so a program driving the browser can fail while its window is out of sight. Firefox gets Playwright's prefs that stop it throttling timers in background tabs.

`--headless` runs the browser with no window, for when nobody needs to watch it. It needs `--debug`, and works with Chromium and Firefox. rembro prints how to connect, and doesn't open the tab that shows it. Headless Chromium gets `--mute-audio`, and Playwright's `--blink-settings` so that pages see a mouse and lay out as they would on a desktop. It accepts only one page on its command line, so rembro opens the others through its debugging endpoint. Headless Firefox opens the pages it starts with but no more after that: a second launch can't hand it pages, and the program driving it holds the one WebDriver BiDi session that Firefox allows. rembro therefore doesn't watch for new servers with headless Firefox. A headless browser runs until you stop rembro, and with `--remote` the target needs no display.

Without `--remote`, the browser runs on this machine, so a program in the target that drives it can read your files. [Security](security.md#debugging) has the details.

### Looking like an ordinary browser

Bot and scraping checks look for the flags and settings that automation gives a browser, and compare its timezone and language with where its traffic comes from. `--stealth` leaves out the flags and prefs that rembro adds and that pages can detect, and when the browser runs here, gives it the target's timezone and language. It works with or without `--debug`.

For Chromium, `--stealth` changes these:

- Temporary profiles don't get `--disable-component-update`, so Chromium downloads its components into each one as a new install does, the Widevine module among them, about a minute into the session. On Linux, Chromium loads Widevine only when it starts, so in a new temporary profile pages that ask for the `com.widevine.alpha` key system get an error for the whole session. A profile kept with `--profile` has Widevine from its second session on, and so do forks of it. Google Chrome ships with Widevine and has it from the start.
- Chromium sets `navigator.webdriver` to true when it has a debugging port, so with `--debug` it gets `--disable-blink-features=AutomationControlled`, which turns that off. A windowed Chromium shows a warning bar about that flag in its first tab, which makes the page there shorter.
- With `--debug`, Chromium gets `--enable-features=CDPScreenshotNewSurface` but not the three flags that keep it from throttling pages in background tabs and in covered or minimized windows. A page there runs its timers slower, as it would for anyone, and a program that drives it may wait while its window is out of sight.
- Headless Chromium puts `HeadlessChrome` in its user agent and renders WebGL with SwiftShader even where there is a GPU. With `--stealth` it gets a `--user-agent` that says `Chrome` with its own major version, and `--enable-gpu` with `--enable-unsafe-swiftshader`, so WebGL uses the GPU where there is one and SwiftShader where there isn't. A `--user-agent` of your own after `--` takes the place of rembro's.
- With `--remote`, Chromium doesn't get `--disable-gpu`. Through Wayland it then uses the target's GPU if it has one. Through X11 it can't use a GPU, and pages find WebGL missing either way.

Firefox has nothing for `--stealth` to leave out unless `--debug` is given. While its remote agent runs, Firefox sets `navigator.webdriver` to true, and no pref turns that off ([Mozilla bug 1719505](https://bugzilla.mozilla.org/show_bug.cgi?id=1719505)), so rembro refuses `--stealth` with `--debug` in Firefox.

rembro keeps the settings that hold the browser's traffic inside the target, which are Chromium's `--host-resolver-rules` and WebRTC policy, and Firefox's DNS, DNS over HTTPS, HTTP/3 and WebRTC prefs ([How it works](how-it-works.md#networking-when-the-browser-runs-here)). A page can tell that the browser never uses HTTP/3, but a page that learnt this machine's address or DNS server as well as the target's would have a stronger sign.

When the browser runs here, it gets the target's timezone and language where the target names them:

- The timezone comes from the target's `TZ`, or else the link at `/etc/localtime`, or else `/etc/timezone`, and counts only when it names a place, such as `America/New_York`. `UTC` and the `Etc/` zones don't count. A container usually has UTC because nobody set a zone in it, and a container on this machine shares this machine's address, so the browser keeps this machine's timezone then. rembro sets `TZ` for the browser.
- The language comes from the target's `LC_ALL`, `LC_MESSAGES` or `LANG`, or from `/etc/locale.conf` or `/etc/default/locale` when none of them is set, and counts unless it is `C`, `POSIX` or `C.UTF-8`. Chromium gets it in `LANGUAGE` on Linux and in `--lang` on Windows, which also changes the language of its menus. Firefox gets `intl.accept_languages` and `intl.locale.requested`, which change `navigator.languages` and the `Accept-Language` header; `Intl` keeps formatting dates in Firefox's own language unless the language pack for the target's is installed. A later session that starts the same Firefox profile without them takes them out of the profile again.
- On Windows, Chromium and Firefox take the timezone from Windows and ignore `TZ`, so the browser keeps this machine's timezone. On macOS, Chromium takes its language from macOS, so there only Firefox gets the target's language. Epiphany and MiniBrowser get the timezone but not the language.
- With `--loopback-only`, the browser connects to other sites from this machine, so it keeps this machine's timezone and language. With `--remote`, it has the target's own.

`--verbose` lists each flag, environment variable and pref that `--stealth` changes.

`--stealth` can't hide:

- A new profile. Each temporary profile starts with no cookies, history or saved components. Keep one with `--profile` to build them up.
- What a program that drives the browser does, such as calling `Runtime.enable` over CDP, which Playwright and Puppeteer do and some bot checks detect.
- Headless Chromium's 800x600 screen, and the empty architecture, bitness and full version that `navigator.userAgentData.getHighEntropyValues()` gives once `--user-agent` replaces Chromium's own. Leave out `--headless` where these matter.
- Missing or software WebGL with `--remote`. In a target without a GPU, windowed Chromium has no WebGL and headless Chromium renders it with SwiftShader, and a Chromium whose window comes through X11 has no WebGL whatever the target has. rembro warns about both, taking a target without `/dev/dri` or `/dev/dxg` to have no GPU.
- `--no-sandbox` in a target that runs Chromium as root, which Chromium needs there and shows a warning bar about.
- Firefox's `navigator.webdriver` with `--debug`, as above.

## Running other programs

`rembro run TARGET COMMAND [ARGS]...` runs any program in the target with the same display, sound and `BROWSER` setup as `--remote`. Its output comes back on rembro's stdout and stderr, and rembro exits with its exit status. Its stdin is not connected, so use `ssh` or `docker exec -it` for interactive terminal programs.

rembro's options go before the command, and everything from the command on belongs to it. `rembro run` takes `--display`, `--x11-trusted`, `--no-audio`, `--agent`, `--quiet` and `--verbose`. `--display none` runs a program without a display. When neither this machine nor the target has a display, `rembro run` prints a warning and does the same.

## Installing a browser in the target

`--remote` needs a browser in the target. `rembro install TARGET [chromium|firefox|webkit]` installs one, Chromium by default.

Where the target has Node.js (`npx`) on x86_64 or aarch64, it runs `npx -y playwright@latest install`, adding `--with-deps` when it can act as root, because Playwright publishes builds for those architectures and knows each distribution's library dependencies. WebKit only comes this way. Otherwise rembro uses the distribution's package manager (apk, apt-get, dnf, pacman or zypper).

That is not possible on Ubuntu, because Ubuntu packages Chromium and Firefox as snaps, which don't run in containers. There rembro stops and suggests installing Node.js and npm so that it can use Playwright's builds.

rembro acts as root when the target runs it as root or has passwordless `sudo`. When it can't become root, it prints the command to run instead.

## Displays on macOS and Windows

`--remote` and `rembro run` show windows from the target through waypipe or X11 ([How it works](how-it-works.md#display-forwarding)). macOS and Windows have no Wayland sessions, so they need an X server:

- On macOS, install [XQuartz](https://www.xquartz.org), then log out and back in. Programs then have `DISPLAY` set to XQuartz's socket, and connecting to it starts XQuartz.
- On Windows, start an X server such as VcXsrv or X410, and set `DISPLAY=127.0.0.1:0`.

When this machine has no display and the target has one of its own, programs use the target's display. That is how a WSL distribution with WSLg shows its windows on Windows.

Sound plays here when `PULSE_SERVER` names a PulseAudio server on this machine, either a `unix:` socket or a `tcp:` address such as `tcp:127.0.0.1:4713`. Otherwise programs keep the target's own sound server, which in WSL is WSLg's.

## Options

```
rembro [OPTIONS] [TARGET] [URL]... [-- BROWSER_ARGS...]
rembro run [OPTIONS] TARGET COMMAND [ARGS]...
rembro install [OPTIONS] TARGET [BROWSER]
rembro profiles [--prune DAYS]

-b, --browser BROWSER     browser to use (default: auto; also REMBRO_BROWSER)
    --family FAMILY       chromium, firefox, epiphany, minibrowser or
                          playwright-webkit, for a browser rembro does not know
-r, --remote              run the browser in the target and show its window here
    --loopback-only       send only localhost, 127.0.0.0/8 and ::1 through the target
    --target-proxy        connect through the HTTP proxy named by the target's
                          https_proxy, http_proxy and no_proxy
    --trust-target-ca     trust the CA certificates added to the target's trust store
    --profile NAME|PATH   keep the browser's data in this profile (also REMBRO_PROFILE);
                          system or system:NAME is one of the browser's own
    --fork NAME|PATH      start from a copy of this profile
    --temp-profile        a new profile, even when REMBRO_PROFILE is set
    --force-profile       use a profile that is open elsewhere, or a Firefox profile
                          rembro did not create
    --debug[=PORT]        expose the browser's debugging endpoint on the target's
                          127.0.0.1:PORT (default 9222, or a free port)
    --headless            run the browser with no window (needs --debug)
    --stealth             leave out the flags and prefs that pages can detect, and
                          use the target's timezone and language
    --ports PORTS         where to look for HTTP servers: ports such as 8080, ranges
                          such as 3000-3999, patterns such as 41xx, all or none
                          (default 1-32767; also REMBRO_PORTS)
    --watch, --no-watch   whether to open servers that start later (default: watch
                          when no URL is given)
    --display auto|wayland|x11|none
    --x11-trusted         give programs full access to this X11 display
    --no-audio
    --agent PATH          binary to copy into the target
-q, --quiet               hide the program's own output
-v, --verbose             explain what rembro is doing
-V, --version
```

`--loopback-only`, `--target-proxy` and `--trust-target-ca` apply when the browser runs here, so they can't be combined with `--remote`. [How it works](how-it-works.md) describes `--loopback-only`, `--target-proxy`, `--display` and `--agent` in more detail, and [Security](security.md) covers `--trust-target-ca` and `--x11-trusted`.

## Limitations

- Epiphany reads its proxy settings from GSettings, so when it runs here rembro gives it a private `XDG_CONFIG_HOME` inside its profile, and it doesn't see your GNOME settings.
- Opening more pages in a Firefox that runs in the target can fail: on Wayland, Firefox hands pages to its running instance over D-Bus, and containers rarely have a session bus.
- UDP is not carried, so QUIC and WebRTC media don't go through the target when the browser runs here. Use `--remote` for those.
- The `local` target works only on Linux, because the agent is a Linux program.
- On Windows, `--trust-target-ca` works with Chromium browsers only, and Firefox on macOS needs `certutil` from Homebrew's `nss` package for it ([Security](security.md#trusting-the-targets-ca-certificates)).
