#!/usr/bin/env node
"use strict";

const { execFileSync, execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const https = require("https");
const { createGunzip } = require("zlib");
const crypto = require("crypto");

const REPO = "yvgude/lean-ctx";
const BIN_DIR = path.join(__dirname, "bin");
const IS_WIN = process.platform === "win32";
const BINARY_NAME = IS_WIN ? "lean-ctx.exe" : "lean-ctx";
const BINARY_PATH = path.join(BIN_DIR, BINARY_NAME);
const DOWNLOAD_TIMEOUT_MS = 60000;
const API_TIMEOUT_MS = 15000;

// ---------------------------------------------------------------------------
// Platform helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// HTTP with timeout (Node.js native)
// ---------------------------------------------------------------------------

function httpsGet(url, timeoutMs = API_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    const get = (u, redirects = 0) => {
      if (redirects > 5) {
        reject(new Error(`Too many redirects for ${url}`));
        return;
      }
      const req = https.get(u, { headers: { "User-Agent": "lean-ctx-bin-npm" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          get(res.headers.location, redirects + 1);
          return;
        }
        if (res.statusCode !== 200) {
          reject(new Error(`HTTP ${res.statusCode} for ${u}`));
          return;
        }
        resolve(res);
      });
      req.on("error", reject);
      req.setTimeout(timeoutMs, () => {
        req.destroy();
        reject(new Error(
          `Request timed out after ${timeoutMs / 1000}s for ${u}\n` +
          `  This usually means a firewall, proxy, or antivirus is blocking the connection.`
        ));
      });
    };
    get(url);
  });
}

function httpsGetJson(url) {
  return new Promise((resolve, reject) => {
    httpsGet(url, API_TIMEOUT_MS).then((res) => {
      let data = "";
      res.on("data", (c) => (data += c));
      res.on("end", () => {
        try { resolve(JSON.parse(data)); }
        catch (e) { reject(e); }
      });
    }).catch(reject);
  });
}

async function downloadToFileNode(url, dest) {
  const res = await httpsGet(url, DOWNLOAD_TIMEOUT_MS);
  const total = parseInt(res.headers["content-length"] || "0", 10);
  let received = 0;
  let lastPct = -1;

  return new Promise((resolve, reject) => {
    const ws = fs.createWriteStream(dest);
    res.on("data", (chunk) => {
      received += chunk.length;
      if (total > 0) {
        const pct = Math.floor((received / total) * 100);
        if (pct >= lastPct + 10) {
          lastPct = pct;
          process.stdout.write(`\r  ${pct}% (${(received / 1024 / 1024).toFixed(1)} MB)`);
        }
      }
    });
    res.pipe(ws);
    ws.on("finish", () => {
      if (total > 0) process.stdout.write(`\r  100% (${(received / 1024 / 1024).toFixed(1)} MB)\n`);
      resolve();
    });
    ws.on("error", reject);
  });
}

// ---------------------------------------------------------------------------
// Windows PowerShell fallback — uses the OS TLS stack which handles
// enterprise proxies, custom root CAs, and Defender exceptions that
// Node.js's bundled OpenSSL does not.
// ---------------------------------------------------------------------------

function powershellGetJson(url) {
  const ps = `
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
    (Invoke-RestMethod -Uri '${url}' -Headers @{'User-Agent'='lean-ctx-bin-npm'} -TimeoutSec 15) | ConvertTo-Json -Depth 10 -Compress
  `;
  const out = execSync(`powershell -NoProfile -NonInteractive -Command "${ps.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`, {
    encoding: "utf8",
    timeout: 20000,
    windowsHide: true,
  });
  return JSON.parse(out);
}

function powershellDownload(url, dest) {
  const ps = `
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri '${url}' -OutFile '${dest.replace(/'/g, "''")}' -Headers @{'User-Agent'='lean-ctx-bin-npm'} -TimeoutSec 120
  `;
  execSync(`powershell -NoProfile -NonInteractive -Command "${ps.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`, {
    stdio: "inherit",
    timeout: 180000,
    windowsHide: true,
  });
}

function powershellDownloadDirect(releaseUrl, assetName, dest) {
  const url = `${releaseUrl}/download/${assetName}`;
  console.log(`lean-ctx: downloading via PowerShell from ${url}...`);
  powershellDownload(url, dest);
  if (!fs.existsSync(dest) || fs.statSync(dest).size < 1000) {
    throw new Error(`PowerShell download produced empty or missing file: ${dest}`);
  }
  console.log(`lean-ctx: downloaded ${(fs.statSync(dest).size / 1024 / 1024).toFixed(1)} MB`);
}

// ---------------------------------------------------------------------------
// curl fallback — available on macOS and most Linux, also on Windows 10+
// ---------------------------------------------------------------------------

function curlAvailable() {
  try {
    execFileSync("curl", ["--version"], { stdio: "ignore", timeout: 3000 });
    return true;
  } catch { return false; }
}

// Paths and URLs go to curl/tar/the binary as argv, never through a shell:
// a `$` or `%` in the install path must reach the program unexpanded.
function curlDownload(url, dest) {
  console.log(`lean-ctx: downloading via curl...`);
  execFileSync("curl", ["-fSL", "--connect-timeout", "15", "--max-time", "120", "-o", dest, url], {
    stdio: "inherit",
    timeout: 180000,
  });
}

// ---------------------------------------------------------------------------
// Unified download with automatic fallback chain:
// Node.js HTTPS → PowerShell (Windows) / curl (Unix) → error with manual URL
// ---------------------------------------------------------------------------

async function downloadToFile(url, dest) {
  console.log("lean-ctx: downloading binary...");

  // Attempt 1: Node.js native HTTPS
  try {
    await downloadToFileNode(url, dest);
    return;
  } catch (nodeErr) {
    console.error(`\nlean-ctx: Node.js download failed: ${nodeErr.message}`);
  }

  // Attempt 2: platform-native fallback
  if (IS_WIN) {
    console.log("lean-ctx: retrying with PowerShell (uses Windows TLS stack)...");
    try {
      powershellDownload(url, dest);
      if (fs.existsSync(dest) && fs.statSync(dest).size > 1000) {
        console.log(`lean-ctx: downloaded ${(fs.statSync(dest).size / 1024 / 1024).toFixed(1)} MB via PowerShell`);
        return;
      }
    } catch (psErr) {
      console.error(`lean-ctx: PowerShell download failed: ${psErr.message}`);
    }
  } else if (curlAvailable()) {
    console.log("lean-ctx: retrying with curl...");
    try {
      curlDownload(url, dest);
      if (fs.existsSync(dest) && fs.statSync(dest).size > 1000) return;
    } catch (curlErr) {
      console.error(`lean-ctx: curl download failed: ${curlErr.message}`);
    }
  }

  throw new Error(
    `All download methods failed.\n` +
    `  Download manually: https://github.com/${REPO}/releases/latest`
  );
}

// ---------------------------------------------------------------------------
// Archive extraction
// ---------------------------------------------------------------------------

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
            const out = path.join(destDir, binaryName);
            fs.writeFileSync(out, buf.subarray(offset, offset + size));
            if (!IS_WIN) fs.chmodSync(out, 0o755);
            resolve(out);
            return;
          }
          offset += Math.ceil(size / 512) * 512;
        }
        reject(new Error(`${binaryName} not found in archive`));
      })
      .on("error", reject);
  });
}

// ---------------------------------------------------------------------------
// Post-install lifecycle
// ---------------------------------------------------------------------------

function runOnboard(binaryPath) {
  if (process.env.CI || process.env.LEAN_CTX_NO_ONBOARD === "1") return;
  try {
    console.log("");
    console.log("Running onboard (connecting your AI tools)...");
    execFileSync(binaryPath, ["onboard"], { stdio: "ignore", timeout: 30000 });
  } catch {
    // Non-fatal: onboard may fail in restricted envs
  }
}

function printSuccess() {
  console.log("");
  console.log("\x1b[1m\u250c\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2510\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m  \x1b[32m\x1b[1m\u2713 lean-ctx installed successfully\x1b[0m                  \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m                                                     \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m  Quick start:                                       \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m    \x1b[1mlean-ctx wrap cursor\x1b[0m  (one-command setup)        \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m    \x1b[1mlean-ctx wrap claude\x1b[0m  (Claude Code)              \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m    \x1b[1mlean-ctx wrap codex\x1b[0m   (Codex CLI)                \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m                                                     \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m  Full control: \x1b[2mlean-ctx setup\x1b[0m                        \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2502\x1b[0m  \x1b[2mDocs: https://leanctx.com/docs\x1b[0m                     \x1b[1m\u2502\x1b[0m");
  console.log("\x1b[1m\u2514\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2518\x1b[0m");
}

// ---------------------------------------------------------------------------
// GitHub release discovery with fallback
// ---------------------------------------------------------------------------

async function fetchRelease() {
  const apiUrl = `https://api.github.com/repos/${REPO}/releases/latest`;

  // Attempt 1: Node.js
  try {
    return await httpsGetJson(apiUrl);
  } catch (nodeErr) {
    console.error(`lean-ctx: GitHub API via Node.js failed: ${nodeErr.message}`);
  }

  // Attempt 2: PowerShell (Windows) — uses OS certificate store
  if (IS_WIN) {
    console.log("lean-ctx: retrying GitHub API via PowerShell...");
    try {
      return powershellGetJson(apiUrl);
    } catch (psErr) {
      console.error(`lean-ctx: PowerShell API call failed: ${psErr.message}`);
    }
  }

  // Attempt 3: curl
  if (curlAvailable()) {
    console.log("lean-ctx: retrying GitHub API via curl...");
    try {
      const out = execFileSync(
        "curl",
        ["-fsSL", "--connect-timeout", "10", "--max-time", "15", "-H", "User-Agent: lean-ctx-bin-npm", apiUrl],
        { encoding: "utf8", timeout: 20000 }
      );
      return JSON.parse(out);
    } catch (curlErr) {
      console.error(`lean-ctx: curl API call failed: ${curlErr.message}`);
    }
  }

  throw new Error(
    `Cannot reach GitHub API. All methods failed (Node.js, ${IS_WIN ? "PowerShell" : "curl"}).\n` +
    `  Download manually: https://github.com/${REPO}/releases/latest`
  );
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  if (fs.existsSync(BINARY_PATH)) {
    console.log("lean-ctx binary already exists, skipping download");
    runOnboard(BINARY_PATH);
    printSuccess();
    return;
  }

  const target = getTarget();
  console.log(`lean-ctx: installing for ${target}...`);

  console.log("lean-ctx: fetching release info from GitHub...");
  const release = await fetchRelease();
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

    // SHA256 verification
    const sumsAsset = (release.assets || []).find((a) => a.name === "SHA256SUMS");
    if (sumsAsset) {
      try {
        const sumsText = await new Promise((resolve, reject) => {
          httpsGet(sumsAsset.browser_download_url).then((res) => {
            let data = "";
            res.on("data", (c) => (data += c));
            res.on("end", () => resolve(data));
          }).catch(reject);
        });
        const expectedLine = sumsText.split("\n").find((l) => l.includes(assetName));
        if (expectedLine) {
          const expectedHash = expectedLine.trim().split(/\s+/)[0].toLowerCase();
          const fileHash = crypto.createHash("sha256").update(fs.readFileSync(archivePath)).digest("hex");
          if (fileHash !== expectedHash) {
            throw new Error(`SHA256 mismatch: expected ${expectedHash}, got ${fileHash}. Binary may be compromised.`);
          }
          console.log("lean-ctx: SHA256 verified \u2713");
        }
      } catch (hashErr) {
        if (hashErr.message.includes("mismatch")) throw hashErr;
        console.log("lean-ctx: SHA256 check skipped (checksum fetch failed)");
      }
    }

    console.log("lean-ctx: extracting...");

    fs.mkdirSync(BIN_DIR, { recursive: true });

    if (IS_WIN) {
      // Stop processes that may hold the binary open (defense-in-depth; preinstall
      // should have done this already, but npm doesn't guarantee hook ordering on
      // every version and the user may run postinstall manually).
      try { execFileSync(BINARY_PATH, ["stop"], { stdio: "ignore", timeout: 10000 }); } catch {}
      try { execSync('taskkill /F /IM "lean-ctx.exe" /T', { stdio: "ignore", timeout: 5000 }); } catch {}
      try { execSync("timeout /T 1 /NOBREAK >NUL 2>&1", { stdio: "ignore" }); } catch {}

      // Clean up orphans from previous failed installs
      try {
        for (const f of fs.readdirSync(BIN_DIR)) {
          if (f.endsWith(".old") || f.endsWith(".old.exe") || f.endsWith(".tmp")) {
            try { fs.unlinkSync(path.join(BIN_DIR, f)); } catch {}
          }
        }
      } catch {}

      const oldBin = BINARY_PATH + ".old";
      try { fs.unlinkSync(oldBin); } catch {}
      try { fs.renameSync(BINARY_PATH, oldBin); } catch {}
      execFileSync("tar", ["-xf", archivePath, "-C", BIN_DIR], { stdio: "ignore" });
      try { fs.unlinkSync(oldBin); } catch {}
    } else {
      await extractTarGz(archivePath, BIN_DIR, "lean-ctx");
    }

    console.log(`lean-ctx: installed to ${BINARY_PATH}`);
    runOnboard(BINARY_PATH);
    printSuccess();
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(`\nlean-ctx: installation failed: ${err.message}`);
  console.error("");
  console.error("Manual install options:");
  console.error("  1. Download from https://github.com/yvgude/lean-ctx/releases/latest");
  if (IS_WIN) {
    console.error("  2. PowerShell one-liner:");
    console.error("     irm https://leanctx.com/install.ps1 | iex");
  } else {
    console.error("  2. curl -fsSL https://leanctx.com/install.sh | sh");
  }
  console.error("  3. cargo install lean-ctx");
  console.error("  4. brew install yvgude/tap/lean-ctx  (macOS/Linux)");
  process.exit(1);
});
