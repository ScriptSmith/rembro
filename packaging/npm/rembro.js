#!/usr/bin/env node
// Runs the rembro build for this system from the package directory, where the Linux builds
// that rembro copies into targets sit next to it.

"use strict";

const { spawn } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const builds = {
  "linux x64": "rembro-x86_64",
  "linux arm64": "rembro-aarch64",
  "linux arm": "rembro-armv7",
  "darwin x64": "rembro-macos-x86_64",
  "darwin arm64": "rembro-macos-aarch64",
  "win32 x64": "rembro-windows-x86_64.exe",
  // Windows on Arm runs the x86_64 build under emulation.
  "win32 arm64": "rembro-windows-x86_64.exe",
};

const system = `${process.platform} ${process.arch}`;
if (!builds[system]) {
  console.error(`rembro: there is no rembro build for ${system}`);
  process.exit(1);
}
const exe = path.join(__dirname, builds[system]);

if (process.platform !== "win32") {
  try {
    fs.accessSync(exe, fs.constants.X_OK);
  } catch {
    // The installer dropped the executable bits.
    try {
      fs.chmodSync(exe, fs.statSync(exe).mode | 0o111);
    } catch {}
  }
}

const child = spawn(exe, process.argv.slice(2), { stdio: "inherit", argv0: "rembro" });
child.on("error", (e) => {
  console.error(`rembro: cannot run ${exe}: ${e.message}`);
  process.exit(1);
});

// rembro closes its browser and deletes temporary profiles on these signals, so pass them on
// and wait for it to exit. On Windows the console sends Ctrl-C and Ctrl-Break to rembro as
// well, and killing the child there would end it without that cleanup.
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK"]) {
  process.on(sig, () => {
    if (process.platform !== "win32") {
      child.kill(sig);
    }
  });
}

child.on("exit", (code, signal) => {
  if (signal) {
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
    process.exit(128 + (os.constants.signals[signal] || 0));
  }
  process.exit(code);
});
