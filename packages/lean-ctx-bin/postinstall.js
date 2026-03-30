#!/usr/bin/env node
"use strict";

const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const https = require("https");
const { createGunzip } = require("zlib");

const REPO = "yvgude/lean-ctx";
const BIN_DIR = path.join(__dirname, "bin");
const IS_WIN = process.platform === "win32";
const BINARY_NAME = IS_WIN ? "lean-ctx.exe" : "lean-ctx";
const BINARY_PATH = path.join(BIN_DIR, BINARY_NAME);

function getGlibcVersion() {
  try {
    const out = execSync("ldd --version 2>&1 || true", { encoding: "utf8" });
    const match = out.match(/(\d+)\.(\d+)\s*$/m);
    if (match) return { major: parseInt(match[1]), minor: parseInt(match[2]) };
  } catch {}
  return null;
}

function getTarget() {
  const platform = process.platform;
  const arch = process.arch;

  if (platform === "linux") {
    let libc = "musl";
    const glibc = getGlibcVersion();
    if (glibc && (glibc.major > 2 || (glibc.major === 2 && glibc.minor >= 35))) {
      libc = "gnu";
    }
    const archMap = { x64: "x86_64", arm64: "aarch64" };
    const rustArch = archMap[arch];
    if (!rustArch) {
      console.error(`Unsupported architecture: ${arch}`);
      process.exit(1);
    }
    return `${rustArch}-unknown-linux-${libc}`;
  }

  const key = `${platform}-${arch}`;
  const targets = {
    "darwin-x64": "x86_64-apple-darwin",
    "darwin-arm64": "aarch64-apple-darwin",
    "win32-x64": "x86_64-pc-windows-msvc",
  };

  const target = targets[key];
  if (!target) {
    console.error(`Unsupported platform: ${key}`);
    console.error("Build from source instead: cargo install lean-ctx");
    process.exit(1);
  }
  return target;
}

function httpsGet(url) {
  return new Promise((resolve, reject) => {
    const get = (u) => {
      https.get(u, { headers: { "User-Agent": "lean-ctx-bin-npm" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          get(res.headers.location);
          return;
        }
        if (res.statusCode !== 200) {
          reject(new Error(`HTTP ${res.statusCode} for ${u}`));
          return;
        }
        resolve(res);
      }).on("error", reject);
    };
    get(url);
  });
}

function httpsGetJson(url) {
  return new Promise((resolve, reject) => {
    httpsGet(url).then((res) => {
      let data = "";
      res.on("data", (c) => (data += c));
      res.on("end", () => {
        try { resolve(JSON.parse(data)); }
        catch (e) { reject(e); }
      });
    }).catch(reject);
  });
}

async function downloadToFile(url, dest) {
  const res = await httpsGet(url);
  return new Promise((resolve, reject) => {
    const ws = fs.createWriteStream(dest);
    res.pipe(ws);
    ws.on("finish", resolve);
    ws.on("error", reject);
  });
}

function extractTarGz(archive, destDir, binaryName) {
  const gunzip = createGunzip();
  const input = fs.createReadStream(archive);

  return new Promise((resolve, reject) => {
    const chunks = [];
    input.pipe(gunzip)
      .on("data", (c) => chunks.push(c))
      .on("end", () => {
        const buf = Buffer.concat(chunks);
        let offset = 0;
        while (offset < buf.length) {
          const header = buf.subarray(offset, offset + 512);
          if (header.every((b) => b === 0)) break;

          const name = header.subarray(0, 100).toString("utf8").replace(/\0/g, "");
          const sizeStr = header.subarray(124, 136).toString("utf8").replace(/\0/g, "").trim();
          const size = parseInt(sizeStr, 8) || 0;
          offset += 512;

          const baseName = path.basename(name);
          if (baseName === binaryName && size > 0) {
            const dest = path.join(destDir, binaryName);
            fs.writeFileSync(dest, buf.subarray(offset, offset + size));
            if (!IS_WIN) fs.chmodSync(dest, 0o755);
            resolve(dest);
            return;
          }
          offset += Math.ceil(size / 512) * 512;
        }
        reject(new Error(`${binaryName} not found in archive`));
      })
      .on("error", reject);
  });
}

async function main() {
  if (fs.existsSync(BINARY_PATH)) {
    console.log("lean-ctx binary already exists, skipping download");
    return;
  }

  const target = getTarget();
  console.log(`lean-ctx: installing for ${target}...`);

  const release = await httpsGetJson(`https://api.github.com/repos/${REPO}/releases/latest`);
  const tag = release.tag_name;
  console.log(`lean-ctx: latest release ${tag}`);

  const ext = IS_WIN ? ".zip" : ".tar.gz";
  const assetName = `lean-ctx-${target}${ext}`;
  const asset = (release.assets || []).find((a) => a.name === assetName);
  if (!asset) {
    console.error(`No binary for ${target}. Install from source: cargo install lean-ctx`);
    process.exit(1);
  }

  const tmpDir = fs.mkdtempSync(path.join(require("os").tmpdir(), "lean-ctx-"));
  const archivePath = path.join(tmpDir, assetName);

  try {
    await downloadToFile(asset.browser_download_url, archivePath);
    console.log("lean-ctx: downloaded, extracting...");

    fs.mkdirSync(BIN_DIR, { recursive: true });

    if (IS_WIN) {
      execSync(`tar -xf "${archivePath}" -C "${BIN_DIR}"`, { stdio: "ignore" });
    } else {
      await extractTarGz(archivePath, BIN_DIR, "lean-ctx");
    }

    console.log(`lean-ctx: installed to ${BINARY_PATH}`);
    console.log("");
    console.log("Next: run \x1b[1mlean-ctx setup\x1b[0m to configure your shell and editors automatically.");
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(`lean-ctx: installation failed: ${err.message}`);
  console.error("Install from source instead: cargo install lean-ctx");
  process.exit(1);
});
