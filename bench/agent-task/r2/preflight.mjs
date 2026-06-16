#!/usr/bin/env node
// R2 faithful-arm preflight — prove "installed = running as designed" before a
// priced run. Targets the R1 finding that the agent logged 102 native `bash`
// calls and 0 `ctx_shell` calls, so the heaviest addressable surface in a fix
// task (make / reproducer / test logs) never reached the compressor (#361).
//
// It verifies, on THIS machine, that:
//   1. the lean-ctx binary is present (and reports its version),
//   2. the resolved pi config suppresses native `bash` (mode=replace or
//      routeShell), so the agent must use `ctx_shell` — the suppression itself
//      is the unit-tested invariant `resolveSuppressedBuiltins` in
//      packages/pi-lean-ctx/extensions/config.ts,
//   3. the embedded MCP bridge / session cache is enabled,
//   4. the faithful-arm overhead levers are set (rules_injection=off,
//      tool_profile=minimal, structure_first),
//   5. lean-ctx actually compresses shell output (measured: smaller than raw).
//
// A green run is the precondition devasur asked for: shell routes through
// ctx_shell and is metered, not native bash.
//
// Usage:
//   node bench/agent-task/r2/preflight.mjs [--config <path>]
//
// POSIX shell is assumed for the compression probe (the R2 rail is the
// forge-cli / pi runtime on Linux). Exit code 1 if any hard gate fails.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const RECOMMENDED_MIN_VERSION = "3.8.6"; // anti-inflation guard + bridged ctx_read

// ── tiny PASS/FAIL/WARN reporter ──────────────────────────────────────────
let failed = false;
const pass = (name, detail) => console.log(`  [PASS] ${name.padEnd(24)} ${detail}`);
const warn = (name, detail) => console.log(`  [WARN] ${name.padEnd(24)} ${detail}`);
const fail = (name, detail) => {
  console.log(`  [FAIL] ${name.padEnd(24)} ${detail}`);
  failed = true;
};
const gate = (ok, name, okDetail, failDetail) =>
  ok ? pass(name, okDetail) : fail(name, failDetail);

// ── config resolution (mirrors extensions/config.ts precedence) ───────────
function envFlag(name) {
  const raw = process.env[name];
  if (!raw) return false;
  const v = raw.trim().toLowerCase();
  return v === "1" || v === "true" || v === "yes" || v === "on";
}

function parseConfigArg() {
  const i = process.argv.indexOf("--config");
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : undefined;
}

function resolveConfigPath() {
  const explicit = parseConfigArg();
  if (explicit) return explicit;
  const installed = resolve(
    homedir(), ".pi", "agent", "extensions", "pi-lean-ctx", "config.json",
  );
  if (existsSync(installed)) return installed;
  // Fall back to this repo's reference config so the preflight self-tests
  // locally even when the extension is not installed on the dev machine.
  return resolve(SCRIPT_DIR, "pi-config.json");
}

function readConfig(path) {
  if (!existsSync(path)) return {};
  try {
    const parsed = JSON.parse(readFileSync(path, "utf8"));
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
  } catch (err) {
    fail("config.json", `invalid JSON at ${path} (${err.message})`);
    return {};
  }
}

// Effective env value: an explicit process env always wins over the config
// `env` block the extension forwards to every lean-ctx subprocess.
function effectiveEnv(cfg, key) {
  return process.env[key] ?? (cfg.env && typeof cfg.env === "object" ? cfg.env[key] : undefined);
}

function resolveMode(cfg) {
  const raw = (process.env.LEAN_CTX_PI_MODE ?? cfg.mode ?? "additive").toLowerCase();
  return raw === "replace" ? "replace" : "additive";
}

function resolveRouteShell(mode, cfg) {
  if (mode === "replace") return true;
  if (process.env.LEAN_CTX_PI_ROUTE_SHELL !== undefined) return envFlag("LEAN_CTX_PI_ROUTE_SHELL");
  return cfg.routeShell === true;
}

function resolveEnableMcp(cfg) {
  return process.env.LEAN_CTX_PI_ENABLE_MCP !== undefined
    ? envFlag("LEAN_CTX_PI_ENABLE_MCP")
    : cfg.enableMcp !== false;
}

function resolveBinary() {
  return process.env.LEAN_CTX_BIN || "lean-ctx";
}

function compareSemver(a, b) {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    const d = (pa[i] || 0) - (pb[i] || 0);
    if (d !== 0) return d < 0 ? -1 : 1;
  }
  return 0;
}

// ── individual checks ─────────────────────────────────────────────────────
function checkBinary(bin) {
  try {
    const out = execFileSync(bin, ["--version"], { encoding: "utf8" }).trim();
    pass("lean-ctx binary", out);
    const m = out.match(/(\d+\.\d+\.\d+)/);
    if (m && compareSemver(m[1], RECOMMENDED_MIN_VERSION) < 0) {
      warn("version", `${m[1]} < ${RECOMMENDED_MIN_VERSION} — pin a release with the anti-inflation + routeShell fixes`);
    }
    return true;
  } catch (err) {
    fail("lean-ctx binary", `not runnable (${err.message}) — set LEAN_CTX_BIN or add lean-ctx to PATH`);
    return false;
  }
}

function checkShellRouting(mode, routeShell) {
  // resolveSuppressedBuiltins (config.ts, unit-tested): replace ⇒ all natives,
  // additive+routeShell ⇒ just bash, additive ⇒ none. bash gone ⟺ the agent
  // cannot pick native bash and must use ctx_shell.
  const bashSuppressed = mode === "replace" || routeShell;
  gate(
    bashSuppressed,
    "shell routing",
    `native bash suppressed (mode=${mode}, routeShell=${routeShell}) → ctx_shell only`,
    `native bash still exposed (mode=${mode}, routeShell=${routeShell}) — set "mode":"replace" or "routeShell":true, else the agent reproduces R1's 102 bash / 0 ctx_shell`,
  );
}

function checkBridge(enableMcp) {
  gate(
    enableMcp,
    "session cache",
    "embedded MCP bridge enabled (unchanged re-reads ~13 tokens)",
    'embedded bridge disabled — remove "enableMcp":false / LEAN_CTX_PI_ENABLE_MCP=0',
  );
}

function checkFaithfulLevers(cfg) {
  const rules = (effectiveEnv(cfg, "LEAN_CTX_RULES_INJECTION") || "").toLowerCase();
  gate(rules === "off", "rules_injection", "off (no per-turn rule-file prefix)", `"${rules || "unset"}" — set LEAN_CTX_RULES_INJECTION=off`);

  const profile = (effectiveEnv(cfg, "LEAN_CTX_TOOL_PROFILE") || "").toLowerCase();
  gate(profile === "minimal", "tool_profile", "minimal (6-tool core)", `"${profile || "unset"}" — set LEAN_CTX_TOOL_PROFILE=minimal`);

  const structureRaw = effectiveEnv(cfg, "LEAN_CTX_STRUCTURE_FIRST");
  const structureOn = ["1", "true", "yes", "on"].includes((structureRaw || "").toLowerCase());
  gate(structureOn, "structure_first", "on (capability-safe cold-read bias)", `"${structureRaw || "unset"}" — set LEAN_CTX_STRUCTURE_FIRST=1`);
}

function checkCompression(bin) {
  // Real proof that shell output is compressed (and therefore metered), without
  // depending on a footer string: run a log-like command raw vs through
  // lean-ctx and assert the lean-ctx output is strictly smaller. The generator is
  // a single `awk` BEGIN loop: it avoids shell command-substitution (so the probe
  // runs under shell_strict_mode) and `awk` is in the default shell_allowlist —
  // unlike `seq`, which mode=replace blocks, making the probe false-fail (#361).
  const cmd =
    'awk \'BEGIN { for (i = 1; i <= 80; i++) printf "[INFO] building module %d of 80 ... ok\\n", i }\'';
  try {
    const raw = execFileSync("/bin/sh", ["-c", cmd], { encoding: "utf8" });
    const compressed = execFileSync(bin, ["-c", cmd], {
      encoding: "utf8",
      env: { ...process.env, LEAN_CTX_COMPRESS: "1", LEAN_CTX_SAVINGS_FOOTER: "always" },
    });
    const pct = raw.length > 0 ? Math.round((1 - compressed.length / raw.length) * 100) : 0;
    gate(
      compressed.length < raw.length,
      "shell compression",
      `${raw.length} → ${compressed.length} bytes (-${pct}%) via ctx_shell path`,
      `lean-ctx output (${compressed.length}B) not smaller than raw (${raw.length}B)`,
    );
  } catch (err) {
    fail("shell compression", `probe failed (${err.message})`);
  }
}

function checkProxy(bin) {
  // Soft: the proxy is usually started just-in-time for the run. Report status
  // when reachable so a stale/missing proxy is visible, but never gate on it.
  try {
    const out = execFileSync(bin, ["proxy", "status"], { encoding: "utf8" }).trim();
    const running = /running|listening|:\d+/i.test(out) && !/not running|stopped/i.test(out);
    (running ? pass : warn)("proxy", out.split("\n")[0] || "status reported");
  } catch {
    warn("proxy", "not running — start with `lean-ctx proxy start --port=4444` before the run");
  }
}

// ── main ──────────────────────────────────────────────────────────────────
const configPath = resolveConfigPath();
const cfg = readConfig(configPath);
const mode = resolveMode(cfg);
const routeShell = resolveRouteShell(mode, cfg);
const bin = resolveBinary();

console.log("R2 faithful-arm preflight");
console.log(`  config: ${configPath}${existsSync(configPath) ? "" : " (absent — using env + defaults)"}`);
console.log("");

checkBinary(bin);
checkShellRouting(mode, routeShell);
checkBridge(resolveEnableMcp(cfg));
checkFaithfulLevers(cfg);
checkCompression(bin);
checkProxy(bin);

console.log("");
if (failed) {
  console.log("preflight FAILED — fix the gates above before any priced run.");
  process.exit(1);
}
console.log("preflight PASSED — installed = running as designed.");
