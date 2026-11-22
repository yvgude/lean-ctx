import { afterEach, describe, expect, it } from "vitest";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  loadPiConfig,
  piConfigPath,
  REPLACEABLE_BUILTIN_TOOLS,
  resolvePiShellPath,
  resolveRouteShell,
  resolveSuppressedBuiltins,
  resolveToolProfile,
} from "../extensions/config.js";

const ENV_KEY = "LEAN_CTX_PI_ROUTE_SHELL";
const PROFILE_ENV_KEYS = ["LEAN_CTX_PI_TOOL_PROFILE", "LEAN_CTX_TOOL_PROFILE"];
const SHELL_ENV_KEYS = ["LEAN_CTX_SHELL", "PI_SHELL_PATH"];

afterEach(() => {
  delete process.env[ENV_KEY];
  delete process.env.PI_CODING_AGENT_DIR;
  for (const key of PROFILE_ENV_KEYS) delete process.env[key];
  for (const key of SHELL_ENV_KEYS) delete process.env[key];
});

describe("resolvePiShellPath", () => {
  it("honors Pi's settings.json under PI_CODING_AGENT_DIR", () => {
    const agentDir = mkdtempSync(join(tmpdir(), "pi-lean-ctx-shell-"));
    const shellPath = join(agentDir, "Git", "bin", "bash.exe");
    mkdirSync(join(agentDir, "Git", "bin"), { recursive: true });
    writeFileSync(shellPath, "");
    writeFileSync(join(agentDir, "settings.json"), JSON.stringify({ shellPath }));
    process.env.PI_CODING_AGENT_DIR = agentDir;

    try {
      expect(resolvePiShellPath()).toBe(shellPath);
    } finally {
      rmSync(agentDir, { recursive: true, force: true });
    }
  });

  it("prefers LEAN_CTX_SHELL, then PI_SHELL_PATH, over settings.json", () => {
    const root = mkdtempSync(join(tmpdir(), "pi-lean-ctx-shell-env-"));
    const configured = join(root, "configured-bash.exe");
    const piEnv = join(root, "pi-env-bash.exe");
    const leanEnv = join(root, "lean-env-bash.exe");
    for (const path of [configured, piEnv, leanEnv]) writeFileSync(path, "");
    writeFileSync(join(root, "settings.json"), JSON.stringify({ shellPath: configured }));
    process.env.PI_CODING_AGENT_DIR = root;
    process.env.PI_SHELL_PATH = piEnv;
    expect(resolvePiShellPath()).toBe(piEnv);
    process.env.LEAN_CTX_SHELL = leanEnv;
    expect(resolvePiShellPath()).toBe(leanEnv);
    rmSync(root, { recursive: true, force: true });
  });

  it("fails open to Pi's normal resolver for missing or malformed paths", () => {
    const agentDir = mkdtempSync(join(tmpdir(), "pi-lean-ctx-shell-missing-"));
    process.env.PI_CODING_AGENT_DIR = agentDir;
    process.env.LEAN_CTX_SHELL = join(agentDir, "missing.exe");
    writeFileSync(join(agentDir, "settings.json"), "{not-json");
    try {
      expect(resolvePiShellPath()).toBeUndefined();
    } finally {
      rmSync(agentDir, { recursive: true, force: true });
    }
  });
});

describe("piConfigPath", () => {
  it("uses PI_CODING_AGENT_DIR as the agent directory", () => {
    const agentDir = mkdtempSync(join(tmpdir(), "pi-lean-ctx-direct-"));
    process.env.PI_CODING_AGENT_DIR = agentDir;
    try {
      expect(piConfigPath()).toBe(join(agentDir, "extensions", "pi-lean-ctx", "config.json"));
    } finally {
      rmSync(agentDir, { recursive: true, force: true });
    }
  });

  it("falls back to the legacy nested agent directory", () => {
    const root = mkdtempSync(join(tmpdir(), "pi-lean-ctx-config-"));
    const legacy = join(root, "agent", "extensions", "pi-lean-ctx");
    mkdirSync(legacy, { recursive: true });
    writeFileSync(join(legacy, "config.json"), "{}");
    process.env.PI_CODING_AGENT_DIR = root;

    try {
      expect(piConfigPath()).toBe(join(legacy, "config.json"));
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("resolveRouteShell", () => {
  it("replace mode always routes shell (every builtin is suppressed anyway)", () => {
    expect(resolveRouteShell("replace", false)).toBe(true);
    expect(resolveRouteShell("replace", undefined)).toBe(true);
  });

  it("additive mode defaults off so native bash stays available (non-regressive)", () => {
    expect(resolveRouteShell("additive", undefined)).toBe(false);
    expect(resolveRouteShell("additive", false)).toBe(false);
  });

  it("additive mode honors the file flag when no env var is set", () => {
    expect(resolveRouteShell("additive", true)).toBe(true);
  });

  it("env var wins over the file flag in additive mode", () => {
    process.env[ENV_KEY] = "0";
    expect(resolveRouteShell("additive", true)).toBe(false);
    process.env[ENV_KEY] = "1";
    expect(resolveRouteShell("additive", false)).toBe(true);
  });
});

describe("resolveSuppressedBuiltins", () => {
  it("replace mode suppresses all five natives (only ctx_* remain)", () => {
    const suppressed = resolveSuppressedBuiltins("replace", true);
    expect([...suppressed].sort()).toEqual(["bash", "find", "grep", "ls", "read"]);
  });

  it("additive + routeShell suppresses only native bash (the R1 102-bash/0-ctx_shell guard)", () => {
    const suppressed = resolveSuppressedBuiltins("additive", true);
    expect([...suppressed]).toEqual(["bash"]);
    // read/ls/find/grep stay available next to their ctx_* counterparts.
    expect(suppressed.has("read")).toBe(false);
  });

  it("additive without routeShell suppresses nothing (non-regressive default)", () => {
    expect(resolveSuppressedBuiltins("additive", false).size).toBe(0);
  });

  it("any faithful arm (replace or routeShell) removes native bash so shell must route through ctx_shell", () => {
    expect(resolveSuppressedBuiltins("replace", true).has("bash")).toBe(true);
    expect(resolveSuppressedBuiltins("additive", true).has("bash")).toBe(true);
  });

  it("never suppresses a builtin without shipping a ctx_* replacement", () => {
    const replaceable = new Set<string>(REPLACEABLE_BUILTIN_TOOLS);
    for (const mode of ["additive", "replace"] as const) {
      for (const routeShell of [false, true]) {
        for (const name of resolveSuppressedBuiltins(mode, routeShell)) {
          expect(replaceable.has(name)).toBe(true);
        }
      }
    }
  });
});

describe("resolveToolProfile", () => {
  it("defaults to lean (parity with a normal default install)", () => {
    expect(resolveToolProfile(undefined)).toBe("lean");
    expect(resolveToolProfile("")).toBe("lean");
  });

  it("honors the file value when no env var is set", () => {
    expect(resolveToolProfile("power")).toBe("power");
    expect(resolveToolProfile("standard")).toBe("standard");
    expect(resolveToolProfile("lean")).toBe("lean");
  });

  it("treats full/all as aliases for power and std for standard", () => {
    expect(resolveToolProfile("full")).toBe("power");
    expect(resolveToolProfile("all")).toBe("power");
    expect(resolveToolProfile("std")).toBe("standard");
  });

  it("is case-insensitive and trims surrounding whitespace", () => {
    expect(resolveToolProfile("  Power ")).toBe("power");
    expect(resolveToolProfile("STANDARD")).toBe("standard");
  });

  it("falls back to lean on an unrecognized value (never crashes the agent)", () => {
    expect(resolveToolProfile("bogus")).toBe("lean");
    expect(resolveToolProfile("minimal")).toBe("lean");
  });

  it("env LEAN_CTX_PI_TOOL_PROFILE wins over the file value", () => {
    process.env.LEAN_CTX_PI_TOOL_PROFILE = "power";
    expect(resolveToolProfile("lean")).toBe("power");
    process.env.LEAN_CTX_PI_TOOL_PROFILE = "lean";
    expect(resolveToolProfile("power")).toBe("lean");
  });
});

describe("loadPiConfig tool-profile mapping", () => {
  it("maps a non-lean profile onto LEAN_CTX_TOOL_PROFILE for the spawned engine", () => {
    process.env.LEAN_CTX_PI_TOOL_PROFILE = "power";
    const cfg = loadPiConfig();
    expect(cfg.toolProfile).toBe("power");
    expect(cfg.forwardedEnv.LEAN_CTX_TOOL_PROFILE).toBe("power");
  });

  it("never overrides an explicit LEAN_CTX_TOOL_PROFILE (most explicit wins)", () => {
    process.env.LEAN_CTX_PI_TOOL_PROFILE = "power";
    process.env.LEAN_CTX_TOOL_PROFILE = "minimal";
    const cfg = loadPiConfig();
    // Pi-facing resolution still reports what the user asked for…
    expect(cfg.toolProfile).toBe("power");
    // …but the pre-existing engine env var is left untouched.
    expect(cfg.forwardedEnv.LEAN_CTX_TOOL_PROFILE).toBeUndefined();
  });
});

describe("loadPiConfig guidance switch", () => {
  afterEach(() => {
    delete process.env.LEAN_CTX_PI_GUIDANCE;
  });

  it("defaults on, honors config.json false, and lets the env var win", () => {
    const agentDir = mkdtempSync(join(tmpdir(), "pi-lean-ctx-guidance-"));
    process.env.PI_CODING_AGENT_DIR = agentDir;
    try {
      expect(loadPiConfig().guidance).toBe(true);

      mkdirSync(join(agentDir, "extensions", "pi-lean-ctx"), { recursive: true });
      writeFileSync(
        join(agentDir, "extensions", "pi-lean-ctx", "config.json"),
        JSON.stringify({ guidance: false }),
      );
      expect(loadPiConfig().guidance).toBe(false);

      process.env.LEAN_CTX_PI_GUIDANCE = "1";
      expect(loadPiConfig().guidance).toBe(true);
    } finally {
      rmSync(agentDir, { recursive: true, force: true });
    }
  });
});
