/**
 * Zero-dependency discovery of the local lean-ctx proxy endpoint.
 *
 * Mirrors the daemon's own resolution so `compress()` works out of the box once
 * `lean-ctx proxy enable` has run, while every step stays overridable via the
 * same environment variables the CLI honours.
 */

import { readFileSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { join } from "node:path";

/** Base port the daemon derives per-UID (see proxy_setup::uid_based_port). */
const DEFAULT_PORT = 4444;

/**
 * A TCP port the daemon could actually bind (1–65535). Ports parsed from a
 * config file or env var are validated through this before they shape an
 * outbound URL, so malformed/out-of-range values fall back instead of leaking
 * into a request.
 */
function isValidPort(value: number): boolean {
  return Number.isInteger(value) && value >= 1 && value <= 65535;
}

/**
 * Strip trailing `/` in a single linear pass. Replaces `.replace(/\/+$/, "")`,
 * whose backtracking is super-linear on inputs with many `/` (ReDoS).
 */
function stripTrailingSlashes(value: string): string {
  let end = value.length;
  while (end > 0 && value.charCodeAt(end - 1) === 47 /* "/" */) end--;
  return value.slice(0, end);
}

/**
 * Whether `url` targets the local machine. The on-disk session token is only
 * attached to loopback proxies (see {@link resolveToken}) so a local credential
 * never rides along to a remote URL configured via `LEAN_CTX_PROXY_URL`.
 */
function isLoopbackUrl(url: string): boolean {
  let host: string;
  try {
    host = new URL(url).hostname;
  } catch {
    return false;
  }
  return (
    host === "127.0.0.1" ||
    host === "localhost" ||
    host === "::1" ||
    host === "[::1]" ||
    host.endsWith(".localhost")
  );
}

function candidateDirs(): string[] {
  const dirs: string[] = [];
  const dataDir = (process.env.LEAN_CTX_DATA_DIR ?? "").trim();
  if (dataDir) dirs.push(dataDir);

  const home = homedir();
  dirs.push(join(home, ".lean-ctx"));

  const xdgData = (process.env.XDG_DATA_HOME ?? "").trim();
  dirs.push(xdgData ? join(xdgData, "lean-ctx") : join(home, ".local", "share", "lean-ctx"));

  const xdgConfig = (process.env.XDG_CONFIG_HOME ?? "").trim();
  dirs.push(xdgConfig ? join(xdgConfig, "lean-ctx") : join(home, ".config", "lean-ctx"));

  return [...new Set(dirs)];
}

/** Pure UID→port mapping, matching proxy_setup::uid_based_port. Exported for tests. */
export function portForUid(uid: number): number {
  if (!Number.isFinite(uid) || uid < 1000) return DEFAULT_PORT;
  return DEFAULT_PORT + ((uid - 1000) % 1000);
}

function uidPort(): number {
  try {
    return portForUid(userInfo().uid);
  } catch {
    return DEFAULT_PORT;
  }
}

function configPort(): number | undefined {
  for (const dir of candidateDirs()) {
    let text: string;
    try {
      text = readFileSync(join(dir, "config.toml"), "utf-8");
    } catch {
      continue;
    }
    for (const line of text.split("\n")) {
      const stripped = line.trim();
      if (!stripped.startsWith("proxy_port")) continue;
      const raw = stripped.split("=", 2)[1];
      if (raw === undefined) break;
      const value = Number.parseInt(raw.trim().replace(/^["']|["']$/g, ""), 10);
      if (isValidPort(value)) return value;
      break;
    }
  }
  return undefined;
}

export function resolvePort(): number {
  const env = (process.env.LEAN_CTX_PROXY_PORT ?? "").trim();
  if (env) {
    const value = Number.parseInt(env, 10);
    if (isValidPort(value)) return value;
  }
  return configPort() ?? uidPort();
}

export function resolveBaseUrl(baseUrl?: string): string {
  if (baseUrl) return stripTrailingSlashes(baseUrl);
  const env = (process.env.LEAN_CTX_PROXY_URL ?? "").trim();
  if (env) return stripTrailingSlashes(env);
  return `http://127.0.0.1:${resolvePort()}`;
}

export function resolveToken(token?: string, baseUrl?: string): string | undefined {
  if (token) return token;
  const env = (process.env.LEAN_CTX_PROXY_TOKEN ?? "").trim();
  if (env) return env;
  // The on-disk session token authenticates to the LOCAL daemon only. Never
  // forward it to a non-loopback proxy URL — that would send a local credential
  // (file data) to a remote host. A remote proxy must supply its own token via
  // LEAN_CTX_PROXY_TOKEN.
  if (!isLoopbackUrl(baseUrl ?? resolveBaseUrl())) return undefined;
  for (const dir of candidateDirs()) {
    try {
      const value = readFileSync(join(dir, "session_token"), "utf-8").trim();
      if (value) return value;
    } catch {
      continue;
    }
  }
  return undefined;
}
