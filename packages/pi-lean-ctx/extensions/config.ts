import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { resolve } from "node:path";

/**
 * Shape of the optional Pi override file
 * `~/.pi/agent/extensions/pi-lean-ctx/config.json`.
 *
 * It lets users who only run lean-ctx through Pi keep every setting inside
 * their Pi configuration instead of juggling `LEAN_CTX_PI_*` environment
 * variables and `~/.lean-ctx/config.toml` (see issue #344). All fields are
 * optional; an absent or malformed file simply falls back to env vars and
 * built-in defaults.
 */
export interface PiLeanCtxFileConfig {
  /** Tool exposure: "additive" (Pi builtins + ctx_*) or "replace" (ctx_* only). */
  mode?: string;
  /**
   * Suppress only the native `bash` builtin (keep the other Pi builtins) so all
   * shell runs through `ctx_shell`. In `additive` mode both `bash` and
   * `ctx_shell` are active and agents tend to pick the uncompressed native
   * `bash` (the R1 finding: 102 bash / 0 ctx_shell), so build/test output never
   * gets compressed or metered. Default `false`; `replace` mode already implies
   * it. Equivalent to `LEAN_CTX_PI_ROUTE_SHELL=1`.
   */
  routeShell?: boolean;
  /**
   * Start the embedded MCP bridge (the persistent session cache). Default
   * `true`; set `false` (or `LEAN_CTX_PI_ENABLE_MCP=0`) to force the one-shot
   * CLI path, which cannot cache across calls.
   */
  enableMcp?: boolean;
  /** Absolute path to the lean-ctx binary (equivalent to `LEAN_CTX_BIN`). */
  binary?: string;
  /**
   * Extra environment forwarded to every lean-ctx subprocess. Use this to
   * override `~/.lean-ctx/config.toml` engine settings without touching that
   * file, since the engine honours `LEAN_CTX_*` env vars
   * (e.g. `{ "LEAN_CTX_COMPRESSION": "aggressive" }`).
   */
  env?: Record<string, string>;
  /**
   * Tool names lean-ctx must NOT register, handing them to another Pi
   * extension instead (issue #359). Use this when coexisting with
   * magic-context / AFT so duplicate `ctx_memory` / `ctx_search` / `ctx_expand`
   * tools don't confuse smaller models. Equivalent to
   * `LEAN_CTX_PI_DISABLE_TOOLS` (the env list and this list are merged).
   */
  disableTools?: string[];
  /**
   * Optional prefix applied to bridge-registered MCP tools (e.g. `"lc_"` turns
   * `ctx_expand` into `lc_expand`) to sidestep name collisions entirely while
   * still exposing the tool (issue #359). The signature tools (`ctx_read`,
   * `ctx_shell`, …) keep their stable names. Equivalent to
   * `LEAN_CTX_PI_TOOL_PREFIX`.
   */
  toolPrefix?: string;
  /**
   * Which lean-ctx tool surface the embedded MCP bridge requests. Maps to the
   * engine's `LEAN_CTX_TOOL_PROFILE`:
   *   "lean" (default) — 12 lazy-core tools incl. `ctx_patch` and the `ctx_call`
   *                      gateway (exact parity with a normal default install;
   *                      ctx_edit and every other tool stay reachable through
   *                      `ctx_call`).
   *   "standard"       — 16 balanced tools.
   *   "power"          — the whole registry as first-class Pi tools (ctx_edit,
   *                      ctx_patch, architecture/quality tools, …). Higher token
   *                      cost. Pi's native `edit`/`write` stay available in every
   *                      mode regardless of this setting.
   * Env `LEAN_CTX_PI_TOOL_PROFILE` overrides this; `full`/`all` alias `power`.
   */
  toolProfile?: string;
}

export type PiMode = "additive" | "replace";

/** The lean-ctx tool surface the embedded bridge advertises (→ LEAN_CTX_TOOL_PROFILE). */
export type PiToolProfile = "lean" | "standard" | "power";

/** Fully resolved configuration after merging file, env vars and defaults. */
export interface ResolvedPiConfig {
  mode: PiMode;
  /** Force shell through `ctx_shell` by suppressing the native `bash` builtin. */
  routeShell: boolean;
  enableMcp: boolean;
  /** Binary path from the file; `LEAN_CTX_BIN` still takes precedence at use time. */
  binaryOverride?: string;
  /** Engine env overrides forwarded to lean-ctx subprocesses. */
  forwardedEnv: Record<string, string>;
  /** Lower-cased tool names handed to other extensions / never registered (#359). */
  disabledTools: Set<string>;
  /** Optional prefix for bridge-registered MCP tools (#359). */
  toolPrefix?: string;
  /** Tool surface the embedded bridge advertises (maps to LEAN_CTX_TOOL_PROFILE). */
  toolProfile: PiToolProfile;
  /** Absolute path the loader looked at (whether or not it existed). */
  configPath: string;
  /** True when the file existed and parsed into a JSON object. */
  loaded: boolean;
}

/** Absolute path to the Pi override file (Pi's per-extension config convention).
 *  Respects `PI_CODING_AGENT_DIR` when set (#930). */
export function piConfigPath(): string {
  if (process.env.PI_CODING_AGENT_DIR) {
    const primary = resolve(
      process.env.PI_CODING_AGENT_DIR,
      "extensions",
      "pi-lean-ctx",
      "config.json",
    );
    if (existsSync(primary)) return primary;
    const fallback = resolve(
      process.env.PI_CODING_AGENT_DIR,
      "agent",
      "extensions",
      "pi-lean-ctx",
      "config.json",
    );
    if (existsSync(fallback)) return fallback;
    return primary;
  }
  return resolve(homedir(), ".pi", "agent", "extensions", "pi-lean-ctx", "config.json");
}

function envFlag(name: string): boolean {
  const raw = process.env[name];
  if (!raw) return false;
  const v = raw.trim().toLowerCase();
  return v === "1" || v === "true" || v === "yes" || v === "on";
}

function readFileConfig(path: string): { cfg: PiLeanCtxFileConfig; loaded: boolean } {
  if (!existsSync(path)) return { cfg: {}, loaded: false };
  try {
    const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { cfg: parsed as PiLeanCtxFileConfig, loaded: true };
    }
    console.error(`[pi-lean-ctx] ${path}: expected a JSON object — ignoring.`);
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error(`[pi-lean-ctx] ${path}: invalid JSON (${msg}) — ignoring.`);
  }
  return { cfg: {}, loaded: false };
}

function resolveMode(fileMode: string | undefined): PiMode {
  const raw = (process.env.LEAN_CTX_PI_MODE ?? fileMode ?? "additive").toLowerCase();
  return raw === "replace" ? "replace" : "additive";
}

/**
 * Whether the native `bash` builtin should be suppressed so shell runs through
 * `ctx_shell`. `replace` mode already hides every builtin, so it implies this;
 * otherwise the env var wins over the file flag, defaulting off (non-regressive
 * — `additive` users keep native `bash` unless they opt in).
 */
export function resolveRouteShell(mode: PiMode, fileRouteShell: unknown): boolean {
  if (mode === "replace") return true;
  if (process.env.LEAN_CTX_PI_ROUTE_SHELL !== undefined) {
    return envFlag("LEAN_CTX_PI_ROUTE_SHELL");
  }
  return fileRouteShell === true;
}

/**
 * The five Pi builtins lean-ctx ships compressed `ctx_*` replacements for.
 * Suppressing one removes it from the agent's tool set, so the agent must reach
 * for the metered ctx_* equivalent instead of the uncompressed native.
 */
export const REPLACEABLE_BUILTIN_TOOLS = ["read", "bash", "ls", "find", "grep"] as const;

/**
 * The Pi builtins to suppress for a resolved config. Single source of truth for
 * the R1 "102 native bash / 0 ctx_shell" fix (#361): whenever the returned set
 * contains `bash`, the native shell is gone and the agent must route through
 * `ctx_shell` (compressed + metered).
 *
 *   replace             → all five natives suppressed (only ctx_* exposed)
 *   additive+routeShell → only `bash` suppressed (read/ls/find/grep stay)
 *   additive            → nothing suppressed (fully non-regressive default)
 *
 * Invariant: every suppressed name has a ctx_* replacement (a subset of
 * REPLACEABLE_BUILTIN_TOOLS), so a builtin is never removed without a substitute.
 */
export function resolveSuppressedBuiltins(mode: PiMode, routeShell: boolean): Set<string> {
  if (mode === "replace") return new Set(REPLACEABLE_BUILTIN_TOOLS);
  if (routeShell) return new Set(["bash"]);
  return new Set<string>();
}

/** Split a comma/whitespace-separated tool list into trimmed, non-empty names. */
function parseToolList(raw: string | undefined): string[] {
  if (!raw) return [];
  return raw
    .split(/[,\s]+/)
    .map((t) => t.trim())
    .filter((t) => t.length > 0);
}

/**
 * Union of the file `disableTools` and the `LEAN_CTX_PI_DISABLE_TOOLS` env list,
 * lower-cased. A deny-list is additive by nature, so both sources contribute
 * (rather than env replacing file) — the intent is always "do not register X".
 */
function resolveDisabledTools(fileList: unknown): Set<string> {
  const set = new Set<string>();
  if (Array.isArray(fileList)) {
    for (const t of fileList) {
      if (typeof t === "string" && t.trim().length > 0) set.add(t.trim().toLowerCase());
    }
  }
  for (const t of parseToolList(process.env.LEAN_CTX_PI_DISABLE_TOOLS)) {
    set.add(t.toLowerCase());
  }
  return set;
}

/** Env `LEAN_CTX_PI_TOOL_PREFIX` wins over the file `toolPrefix`; empty ⇒ none. */
function resolveToolPrefix(filePrefix: unknown): string | undefined {
  const raw = process.env.LEAN_CTX_PI_TOOL_PREFIX
    ?? (typeof filePrefix === "string" ? filePrefix : undefined);
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

/**
 * The lean-ctx tool surface the embedded bridge requests, mapped to the engine's
 * `LEAN_CTX_TOOL_PROFILE`. Env `LEAN_CTX_PI_TOOL_PROFILE` wins over the file
 * `toolProfile`; `full`/`all` alias `power`; anything unset or unrecognized falls
 * back to `lean` — the lazy-core default that matches a normal install (incl. the
 * anchored editor `ctx_patch`; every other tool stays reachable via `ctx_call`).
 * `power` promotes the whole registry (ctx_edit included) to first-class Pi tools.
 */
export function resolveToolProfile(fileProfile: unknown): PiToolProfile {
  const raw = (process.env.LEAN_CTX_PI_TOOL_PROFILE
    ?? (typeof fileProfile === "string" ? fileProfile : undefined)
    ?? "lean")
    .trim()
    .toLowerCase();
  switch (raw) {
    case "power":
    case "full":
    case "all":
      return "power";
    case "standard":
    case "std":
      return "standard";
    default:
      return "lean";
  }
}

/**
 * Loads and resolves the Pi override config. Precedence per setting is
 * "most explicit wins": an explicit `LEAN_CTX_PI_*` / `LEAN_CTX_BIN` env var
 * overrides `config.json`, which overrides the built-in default. This keeps
 * shareable, file-only setups working (no env vars needed) while still
 * allowing ad-hoc env overrides on a single machine.
 */
export function loadPiConfig(): ResolvedPiConfig {
  const configPath = piConfigPath();
  const { cfg, loaded } = readFileConfig(configPath);

  // The embedded MCP bridge holds the persistent session cache, so unchanged
  // re-reads cost ~13 tokens and reads register as CEP sessions. That is
  // lean-ctx's core value prop, so the bridge is ON by default; the one-shot CLI
  // path cannot cache across calls (#361). Opt out with LEAN_CTX_PI_ENABLE_MCP=0
  // or "enableMcp": false in config.json.
  const enableMcp =
    process.env.LEAN_CTX_PI_ENABLE_MCP !== undefined
      ? envFlag("LEAN_CTX_PI_ENABLE_MCP")
      : cfg.enableMcp !== false;

  const forwardedEnv: Record<string, string> = {};
  if (cfg.env && typeof cfg.env === "object" && !Array.isArray(cfg.env)) {
    for (const [key, value] of Object.entries(cfg.env)) {
      if (typeof value === "string") forwardedEnv[key] = value;
    }
  }

  const binaryOverride =
    typeof cfg.binary === "string" && cfg.binary.length > 0 ? cfg.binary : undefined;

  const mode = resolveMode(cfg.mode);

  // Translate the Pi-facing tool profile into the engine env the spawned MCP
  // server reads, so `standard`/`power` widen the advertised surface (ctx_edit
  // and the rest become first-class Pi tools via the bridge). Never
  // override an explicit LEAN_CTX_TOOL_PROFILE — whether from the real env or the
  // config.json `env` map — so "most explicit wins" holds. `lean` is the default
  // and adds nothing (identical to a normal default install).
  const toolProfile = resolveToolProfile(cfg.toolProfile);
  if (
    toolProfile !== "lean"
    && forwardedEnv.LEAN_CTX_TOOL_PROFILE === undefined
    && process.env.LEAN_CTX_TOOL_PROFILE === undefined
  ) {
    forwardedEnv.LEAN_CTX_TOOL_PROFILE = toolProfile;
  }

  return {
    mode,
    routeShell: resolveRouteShell(mode, cfg.routeShell),
    enableMcp,
    binaryOverride,
    forwardedEnv,
    disabledTools: resolveDisabledTools(cfg.disableTools),
    toolPrefix: resolveToolPrefix(cfg.toolPrefix),
    toolProfile,
    configPath,
    loaded,
  };
}
