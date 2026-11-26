"""Runs the rembro build for this system from the package directory, where the Linux builds
that rembro copies into targets sit next to it."""

import os
import platform
import signal
import subprocess
import sys

ARCHES = {
    "x86_64": "x86_64",
    "amd64": "x86_64",
    "aarch64": "aarch64",
    "arm64": "aarch64",
    "armv7l": "armv7",
    "armv7": "armv7",
    "armv8l": "armv7",
}


def build():
    arch = ARCHES.get(platform.machine().lower())
    if sys.platform == "linux" and arch:
        return "rembro-" + arch
    if sys.platform == "darwin" and arch in ("x86_64", "aarch64"):
        return "rembro-macos-" + arch
    # Windows on Arm runs the x86_64 build under emulation.
    if sys.platform == "win32" and arch in ("x86_64", "aarch64"):
        return "rembro-windows-x86_64.exe"
    return None


def main():
    name = build()
    if name is None:
        sys.exit(f"rembro: there is no rembro build for {sys.platform} {platform.machine()}")
    exe = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    argv = ["rembro", *sys.argv[1:]]
    try:
        if sys.platform == "win32":
            child = subprocess.Popen(argv, executable=exe)
            # The console sends Ctrl-C and Ctrl-Break to rembro as well, which cleans up and exits.
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            signal.signal(signal.SIGBREAK, signal.SIG_IGN)
            sys.exit(child.wait())
        if not os.access(exe, os.X_OK):
            # The installer dropped the executable bits.
            os.chmod(exe, os.stat(exe).st_mode | 0o111)
        # rembro replaces this process, so signals reach it directly.
        os.execv(exe, argv)
    except OSError as e:
        sys.exit(f"rembro: cannot run {exe}: {e.strerror}")
