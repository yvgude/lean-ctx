#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const { execFileSync, execSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");

// The fake binary is a shebang script, which Windows cannot execute.
if (process.platform === "win32") {
  console.log("skip: postinstall.dollar.test.cjs needs a POSIX shell");
  process.exit(0);
}

const postinstall = path.join(__dirname, "postinstall.js");
const source = fs.readFileSync(postinstall, "utf8");
const start = source.indexOf("function runOnboard");
const end = source.indexOf("\nfunction printSuccess", start);
const runOnboard = vm.runInNewContext(`(${source.slice(start, end)})`, {
  console, execSync, execFileSync, process: { env: {} },
});

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "lean-dollar-"));
const binDir = path.join(dir, "cache_$HOME_bin");
fs.mkdirSync(binDir);
const binary = path.join(binDir, "lean-ctx");
const marker = path.join(dir, "ran");
fs.writeFileSync(binary, `#!/bin/sh
test "$1" = onboard && printf ok > ${JSON.stringify(marker)}
`);
fs.chmodSync(binary, 0o755);

let shellThrew = false;
try {
  execSync(`"${binary}" onboard`, { stdio: "ignore" });
} catch {
  shellThrew = true;
}
assert.equal(shellThrew, true, "shell form should fail when the path contains $");
assert.equal(fs.existsSync(marker), false);

runOnboard(binary);
assert.equal(fs.readFileSync(marker, "utf8"), "ok");
fs.rmSync(dir, { recursive: true, force: true });
console.log("dollar path onboard ok");
