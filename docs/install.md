# Installing rembro

This page covers the install script, the npm and PyPI packages, installing a release archive by hand, which also works on Windows, where the script doesn't run, and installing with Cargo.

## With the install script

```sh
curl -fsSL https://raw.githubusercontent.com/ScriptSmith/rembro/main/install.sh | sh
```

The script works on Linux and macOS. It downloads the latest release for this machine, checks it against the release's `SHA256SUMS`, unpacks it into `~/.local/lib/rembro` and links `~/.local/bin/rembro` to it. `curl ... | REMBRO_VERSION=0.1.0 sh` installs a particular release, and `REMBRO_PREFIX` puts it somewhere other than `~/.local`.

## With npm, uv or pipx

The `@scriptsmith/rembro` package on npm and the `rembro` package on PyPI work on Linux, macOS and Windows. `npx` and `uvx` run rembro without installing it:

```sh
npx @scriptsmith/rembro ssh:devbox
uvx rembro ssh:devbox
```

To install it as a `rembro` command:

```sh
npm install -g @scriptsmith/rembro
uv tool install rembro    # or: pipx install rembro
```

Each package holds all six release builds of rembro (Linux on x86_64, aarch64 and armv7, macOS on aarch64 and x86_64, and Windows on x86_64) and a small launcher that runs the one for this machine. Windows on Arm runs the x86_64 build under emulation. The Linux builds that rembro copies into targets are already next to it, so the only file rembro downloads on first use is waypipe. It needs waypipe on Linux, when `--remote` or `rembro run` forwards windows over Wayland to a target that has none, and downloads and caches it as a [Cargo install](#with-cargo) does.

## From a release archive

Take an archive from the [releases page](https://github.com/ScriptSmith/rembro/releases). There are static builds for x86_64, aarch64 and armv7 Linux, builds for x86_64 and aarch64 macOS, and an x86_64 build for Windows. Each archive holds `rembro` and the Linux builds of rembro that it copies into targets: the other two architectures in a Linux archive, and all three in a macOS or Windows one. The Linux archives also hold static waypipe builds for all three architectures. Keep these files next to `rembro`:

```sh
mkdir -p ~/.local/lib/rembro ~/.local/bin
curl -L https://github.com/ScriptSmith/rembro/releases/latest/download/rembro-x86_64-linux.tar.gz |
  tar -xz -C ~/.local/lib/rembro --strip-components=1
ln -sf ~/.local/lib/rembro/rembro ~/.local/bin/rembro
```

## Windows

Unzip `rembro-x86_64-windows.zip` and add the folder it makes to your `PATH`. Windows 11 on Arm runs the x86_64 build under emulation.

## macOS

If you download a macOS archive with a web browser, macOS won't run the unsigned binary until you clear the quarantine flag with `xattr -dr com.apple.quarantine` on the unpacked folder.

## With Cargo

```sh
cargo install rembro --target x86_64-unknown-linux-musl  # on Linux
cargo install rembro                                     # on macOS and Windows
```

A Cargo install has no builds for other architectures or waypipe next to it. rembro downloads the ones it needs from the GitHub release of the same version, checks them against the release's `SHA256SUMS`, and keeps them in its cache: `~/.cache/rembro/VERSION` on Linux, `~/Library/Caches/rembro/VERSION` on macOS and `%LOCALAPPDATA%\rembro\VERSION` on Windows. On macOS and Windows that includes the Linux build it copies into every target. On Linux, a plain `cargo install rembro` also works, but it links against this machine's glibc. Since rembro copies its own binary into the target, that build fails in Alpine containers and on distributions with an older glibc. The musl build runs on any Linux.
