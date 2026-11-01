#!/usr/bin/env node
"use strict";

// Stop this package's own lean-ctx daemon before npm replaces the binary.
// Only the installed binary is asked to stop the daemon recorded in its PID
// file (`lean-ctx daemon stop`); a machine-wide `pkill -f lean-ctx` or
// `taskkill /IM lean-ctx.exe` would also end the proxy, the dashboard, other
// installs and every command line that merely mentions lean-ctx. A fresh
// install has no binary yet and stops nothing. On Windows, postinstall moves a
// still-open binary aside (`.old`) instead of killing its holders.

const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const IS_WIN = process.platform === "win32";
const BIN_DIR = path.join(__dirname, "bin");
const BINARY_NAME = IS_WIN ? "lean-ctx.exe" : "lean-ctx";
const BINARY_PATH = path.join(BIN_DIR, BINARY_NAME);

function cleanOrphans(binDir = BIN_DIR) {
  if (!fs.existsSync(binDir)) return;
  try {
    const entries = fs.readdirSync(binDir);
    for (const entry of entries) {
      if (entry.endsWith(".old") || entry.endsWith(".old.exe") || entry.endsWith(".tmp")) {
        try {
          fs.unlinkSync(path.join(binDir, entry));
        } catch {}
      }
    }
  } catch {}
}

function stopInstalledDaemon(binaryPath = BINARY_PATH, run = execFileSync) {
  if (!fs.existsSync(binaryPath)) return false;
  try {
    run(binaryPath, ["daemon", "stop"], { stdio: "ignore", timeout: 10000, windowsHide: true });
    return true;
  } catch {
    return false;
  }
}

if (require.main !== module) {
  // Loaded by tests: expose the pieces, run nothing.
  module.exports = { stopInstalledDaemon, cleanOrphans };
} else {
  stopInstalledDaemon();
  cleanOrphans();
}
