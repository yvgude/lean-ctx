import { afterEach, describe, expect, it } from "vitest";

import {
  loadPiConfig,
  piConfigPath,
  REPLACEABLE_BUILTIN_TOOLS,
  resolveRouteShell,
  resolveSuppressedBuiltins,
  resolveToolProfile,
} from "../extensions/config.js";

const ENV_KEY = "LEAN_CTX_PI_ROUTE_SHELL";
const PROFILE_ENV_KEYS = ["LEAN_CTX_PI_TOOL_PROFILE", "LEAN_CTX_TOOL_PROFILE"];

afterEach(() => {
  delete process.env[ENV_KEY];
  for (const key of PROFILE_ENV_KEYS) delete process.env[key];
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

describe("piConfigPath", () => {
  const AGENT_DIR_KEY = "PI_CODING_AGENT_DIR";

  afterEach(() => {
    delete process.env[AGENT_DIR_KEY];
  });

  it("resolves under ~/.pi/agent/extensions when PI_CODING_AGENT_DIR is unset", () => {
    delete process.env[AGENT_DIR_KEY];
    expect(piConfigPath()).toContain(".pi/agent/extensions/pi-lean-ctx/config.json");
  });

  it("resolves directly under PI_CODING_AGENT_DIR/extensions when set", () => {
    process.env[AGENT_DIR_KEY] = "/tmp/custom-pi-agent";
    expect(piConfigPath()).toBe("/tmp/custom-pi-agent/extensions/pi-lean-ctx/config.json");
  });
});
