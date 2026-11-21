# Installing rembro

On Linux and macOS, the [install script](../README.md#install) is the quickest way to install rembro. This page covers installing a release archive by hand, which also works on Windows, where the script doesn't run.

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
