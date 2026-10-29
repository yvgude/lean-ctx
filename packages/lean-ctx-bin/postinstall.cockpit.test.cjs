#!/usr/bin/env node
"use strict";

// The Cockpit step of postinstall: a verified bundle reaches
// `lean-ctx cockpit install --from <dir>`; anything else is one warning and
// never fails the install. Downloads are replaced by local fixtures.

const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

// The fake engine is a shebang script, which Windows cannot execute.
if (process.platform === "win32") {
  console.log("skip: postinstall.cockpit.test.cjs needs a POSIX shell");
  process.exit(0);
}

const { installCockpit, expectedDigest } = require("./postinstall.js");

const TARGET = "x86_64-unknown-linux-gnu";
const ASSET = `lean-ctx-cockpit-${TARGET}.tar.gz`;

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "lean-cockpit-"));
const bundleRoot = path.join(dir, "payload");
fs.mkdirSync(path.join(bundleRoot, "lean-ctx-cockpit"), { recursive: true });
for (const name of ["channel.json", "channel.sig", "archive.tar.gz", "manifest.json", "manifest.sig"]) {
  fs.writeFileSync(path.join(bundleRoot, "lean-ctx-cockpit", name), name);
}
const tarball = path.join(dir, "fixture.tar.gz");
execFileSync("tar", ["-czf", tarball, "-C", bundleRoot, "lean-ctx-cockpit"]);
const digest = crypto.createHash("sha256").update(fs.readFileSync(tarball)).digest("hex");

const record = path.join(dir, "engine-calls");
const engine = path.join(dir, "cache_$HOME_bin", "lean-ctx");
fs.mkdirSync(path.dirname(engine));
fs.writeFileSync(engine, `#!/bin/sh
if [ -n "$FAKE_REFUSE" ]; then echo "refused: signature" >&2; echo "detail" >&2; exit 1; fi
[ "$1 $2 $3" = "cockpit install --from" ] && [ -f "$4/channel.sig" ] || exit 3
printf '%s\\n' "$4" >> ${JSON.stringify(record)}
`);
fs.chmodSync(engine, 0o755);

const release = {
  tag_name: "v9.9.9",
  assets: [
    { name: ASSET, browser_download_url: `https://example.invalid/${ASSET}` },
    { name: "COCKPIT_SHA256SUMS", browser_download_url: "https://example.invalid/COCKPIT_SHA256SUMS" },
  ],
};

function io(overrides = {}) {
  const lines = [];
  return {
    lines,
    env: {},
    log: (line) => lines.push(line),
    download: async (url, dest) => {
      assert.equal(url, release.assets[0].browser_download_url);
      fs.copyFileSync(tarball, dest);
    },
    fetchText: async () => `${"0".repeat(64)}  lean-ctx-cockpit-other.tar.gz\n${digest}  ${ASSET}\n`,
    ...overrides,
  };
}

(async () => {
  try {
    assert.equal(expectedDigest(`${digest} *${ASSET}\n`, ASSET), digest);
    assert.equal(expectedDigest(`${digest}  x-${ASSET}\n`, ASSET), null);

    // Verified bundle: handed to the engine as an extracted directory.
    let run = io();
    assert.equal(await installCockpit(engine, release, TARGET, run), "installed");
    const calls = fs.readFileSync(record, "utf8").trim().split("\n");
    assert.equal(calls.length, 1);
    assert.match(calls[0], /lean-ctx-cockpit$/);
    assert.equal(fs.existsSync(calls[0]), false, "the temporary bundle is removed");
    assert.match(run.lines.at(-1), /Cockpit installed and verified/);
    fs.rmSync(record);

    // Fail closed before the engine on a missing entry or a different digest.
    for (const sums of [`${digest}  other.tar.gz\n`, `${"f".repeat(64)}  ${ASSET}\n`]) {
      run = io({ fetchText: async () => sums });
      assert.equal(await installCockpit(engine, release, TARGET, run), "unverified");
      assert.match(run.lines.at(-1), /refusing an unverified Cockpit/);
      assert.equal(fs.existsSync(record), false);
    }

    // Engine refusal, download failure, missing asset and opt-out: one warning each.
    run = io({ env: {} });
    process.env.FAKE_REFUSE = "1";
    assert.equal(await installCockpit(engine, release, TARGET, run), "failed");
    delete process.env.FAKE_REFUSE;
    assert.equal(run.lines.length, 1);
    assert.match(run.lines[0], /Cockpit not installed \(refused: signature\); the engine works without it/);

    run = io({ download: async () => { throw new Error("All download methods failed."); } });
    assert.equal(await installCockpit(engine, release, TARGET, run), "failed");
    assert.match(run.lines[0], /not installed \(All download methods failed\.\)/);

    run = io();
    assert.equal(await installCockpit(engine, { tag_name: "v1.0.0", assets: [] }, TARGET, run), "unavailable");
    assert.match(run.lines[0], /release v1\.0\.0 has no Cockpit/);

    run = io({ env: { LEAN_CTX_NO_COCKPIT: "1" } });
    assert.equal(await installCockpit(engine, release, TARGET, run), "skipped");
    assert.equal(fs.existsSync(record), false);
    console.log("postinstall cockpit ok");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
})().catch((error) => { console.error(error); process.exitCode = 1; });
