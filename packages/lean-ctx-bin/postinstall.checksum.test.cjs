#!/usr/bin/env node
"use strict";

// GHSA-rxv5-4c3x-g4q6 D: the npm postinstall must refuse to install a binary
// whose SHA-256 it cannot verify against the release's SHA256SUMS.

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const crypto = require("node:crypto");
const assert = require("node:assert/strict");

// A Windows checkout may carry CRLF line endings.
const source = fs
  .readFileSync(path.join(__dirname, "postinstall.js"), "utf8")
  .replace(/\r\n/g, "\n");
const start = source.indexOf("function expectedSha256");
const end = source.indexOf("\n// ---------------------------------------------------------------------------\n// Main", start);
assert(start >= 0 && end > start, "checksum helpers were not found");
const logs = [];
const { verifyArchiveChecksum, expectedSha256 } = vm.runInNewContext(
  `${source.slice(start, end)}\n({ verifyArchiveChecksum, expectedSha256 })`,
  { console: { log: (m) => logs.push(m) }, crypto, fs, path, require, IS_WIN: false },
);

const asset = "lean-ctx-x86_64-unknown-linux-musl.tar.gz";
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "lean-ctx-sums-test-"));
const archive = path.join(dir, asset);
fs.writeFileSync(archive, "archive bytes");
const digest = crypto.createHash("sha256").update("archive bytes").digest("hex");
const other = "0".repeat(64);
const release = (assets) => ({ tag_name: "v9.9.9", assets });
const withSums = release([{ name: "SHA256SUMS", browser_download_url: "https://example.invalid/SHA256SUMS" }]);
const serve = (text) => async () => text;

(async () => {
  try {
    // Exact name match only; `*` binary-mode prefix accepted; CRLF tolerated.
    assert.equal(expectedSha256(`${digest}  ${asset}\r\n`, asset), digest);
    assert.equal(expectedSha256(`${digest} *${asset}\n`, asset), digest);
    assert.equal(expectedSha256(`${digest}  ${asset}.sig\n`, asset), null);
    assert.equal(expectedSha256(`${digest}  ${asset}\n${other}  ${asset}\n`, asset), null);
    assert.equal(expectedSha256(`nothex  ${asset}\n`, asset), null);

    await verifyArchiveChecksum(withSums, asset, archive, serve(`${other}  LICENSE.md\n${digest}  ${asset}\n`));
    assert(logs.some((l) => l.includes("SHA256 verified")));

    const rejects = async (rel, fetch, pattern) =>
      assert.rejects(verifyArchiveChecksum(rel, asset, archive, fetch), pattern);
    await rejects(release([]), serve(""), /no SHA256SUMS asset/);
    await rejects(withSums, async () => { throw new Error("HTTP 403"); }, /could not download SHA256SUMS \(HTTP 403\)/);
    await rejects(withSums, serve(`${digest}  other.tar.gz\n`), /no unique entry/);
    await rejects(withSums, serve(`${other}  ${asset}\n`), /SHA256 mismatch/);
    // The verified release is the package's own version, never `latest`.
    const version = require("./package.json").version;
    assert.doesNotMatch(source, /releases\/latest/);
    assert.match(source, /releases\/tags\/\$\{RELEASE_TAG\}/);
    assert.match(source, /const VERSION = require\("\.\/package\.json"\)\.version;/);
    assert.match(version, /^\d+\.\d+\.\d+$/);
    console.log("postinstall checksum verification fails closed: ok");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
