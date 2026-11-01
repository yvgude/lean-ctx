#!/usr/bin/env node
"use strict";

// preinstall stops only the installed binary's own daemon: nothing on a fresh
// install, `<bin> daemon stop` on an upgrade, and never a sweep by process name
// that would also end the proxy, the dashboard or other installs.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { stopInstalledDaemon } = require("./preinstall.js");

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "lean-preinstall-"));
try {
  const binary = path.join(dir, process.platform === "win32" ? "lean-ctx.exe" : "lean-ctx");
  const calls = [];
  const run = (file, args) => calls.push([file, args]);

  // Fresh install: no binary, nothing is stopped.
  assert.equal(stopInstalledDaemon(binary, run), false);
  assert.deepEqual(calls, []);

  // Upgrade: the installed binary stops its own daemon.
  fs.writeFileSync(binary, "");
  assert.equal(stopInstalledDaemon(binary, run), true);
  assert.deepEqual(calls, [[binary, ["daemon", "stop"]]]);

  // A failing stop never fails the install.
  assert.equal(stopInstalledDaemon(binary, () => { throw new Error("no daemon"); }), false);

  for (const script of ["preinstall.js", "postinstall.js"]) {
    const source = fs.readFileSync(path.join(__dirname, script), "utf8")
      .split(/\r?\n/).filter((line) => !line.trim().startsWith("//")).join("\n");
    assert.doesNotMatch(source, /pkill|taskkill|killall/, `${script} must not kill processes by name`);
  }
  console.log("ok: preinstall stops only the installed daemon");
} finally {
  fs.rmSync(dir, { recursive: true, force: true });
}
