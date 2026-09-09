#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const assert = require("node:assert/strict");
const { execSync, spawn } = require("node:child_process");
const { once } = require("node:events");
const { setTimeout: delay } = require("node:timers/promises");

const postinstall = path.join(__dirname, "postinstall.js");

function loadRunOnboard(env) {
  const source = fs.readFileSync(postinstall, "utf8");
  const start = source.indexOf("function runOnboard");
  const end = source.indexOf("\nfunction printSuccess", start);
  assert(start >= 0 && end > start, "runOnboard was not found");
  return vm.runInNewContext(`(${source.slice(start, end)})`, {
    console, execSync, process: { env },
  });
}

if (process.argv[2] === "--runner") {
  const env = { ...process.env };
  delete env.CI;
  delete env.LEAN_CTX_NO_ONBOARD;
  loadRunOnboard(env)(process.execPath);
  fs.writeFileSync(env.LEAN1721_RETURNED, "");
} else (async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "lean1721-"));
  const release = path.join(dir, "release");
  const returned = path.join(dir, "returned");
  const pidFile = path.join(dir, "fixture.pid");
  fs.writeFileSync(path.join(dir, "onboard"), `
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const code = "const fs=require('node:fs'); setInterval(() => { if (fs.existsSync(process.env.LEAN1721_RELEASE)) process.exit(0); }, 10); setTimeout(() => process.exit(1), 60000);";
const child = spawn(process.execPath, ["-e", code], { detached: true, stdio: ["ignore", "ignore", "inherit"] });
fs.writeFileSync(process.env.LEAN1721_PID, String(child.pid));
child.unref();
`);
  const env = { ...process.env, LEAN1721_RELEASE: release, LEAN1721_RETURNED: returned, LEAN1721_PID: pidFile };
  delete env.CI;
  delete env.LEAN_CTX_NO_ONBOARD;
  const runner = spawn(process.execPath, [__filename, "--runner"], { cwd: dir, env, stdio: ["ignore", "pipe", "pipe"] });
  runner.stdout.resume();
  runner.stderr.resume();
  let timer;
  try {
    // The descendant stays alive until cleanup: installer pipes must close first.
    const [code] = await Promise.race([
      once(runner, "close"),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("runOnboard kept inherited stdio open")), 5000);
      }),
    ]);
    assert.equal(code, 0);
    assert(fs.existsSync(returned), "runner exited before runOnboard returned");
    assert(fs.existsSync(pidFile), "onboard did not launch the descendant");
  } finally {
    clearTimeout(timer);
    fs.writeFileSync(release, "");
    if (fs.existsSync(pidFile)) {
      const pid = Number(fs.readFileSync(pidFile, "utf8"));
      try { process.kill(pid); } catch {}
      const deadline = Date.now() + 5000;
      while (Date.now() < deadline) {
        try {
          process.kill(pid, 0);
          await delay(50);
        } catch {
          break;
        }
      }
    }
    try { runner.kill(); } catch {}
    try {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 50, retryDelay: 100 });
    } catch (error) {
      // Windows keeps a directory handle alive for a while after
      // TerminateProcess, so the descendant killed just above can still hold
      // `dir` past the retry budget and rmSync throws EBUSY. Every assertion
      // has already run by this point — failing the suite on temp-directory
      // hygiene turns a runner quirk into a red pipeline (it took down
      // #1741, #1742, #1732 and #1749), and the runner is discarded anyway.
      console.warn(`cleanup: could not remove ${dir}: ${error.message}`);
    }
  }
})().catch((error) => { console.error(error); process.exitCode = 1; });
