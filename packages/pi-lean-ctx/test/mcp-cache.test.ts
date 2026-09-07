import { mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import type { McpTool } from "../extensions/mcp-bridge.js";
import {
  binaryPathCandidates,
  createMcpSchemaCacheKey,
  identifyBinary,
  loadMcpSchemaCache,
  MCP_SCHEMA_CACHE_FORMAT_VERSION,
  type McpSchemaCacheInputs,
  identifyToolSurfaceConfiguration,
  PI_EXTENSION_VERSION,
  toolSurfaceEnvironment,
  validateMcpSchemaCache,
  writeMcpSchemaCache,
} from "../extensions/mcp-cache.js";

const temporaryDirectories: string[] = [];

afterEach(async () => {
  const { rm } = await import("node:fs/promises");
  await Promise.all(temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

function inputs(overrides: Partial<McpSchemaCacheInputs> = {}): McpSchemaCacheInputs {
  return {
    extensionVersion: "3.10.1",
    engineVersion: "2.0.0",
    binaryPath: "/opt/lean-ctx",
    binaryIdentity: "size=10;mtime=20",
    toolProfile: "lean",
    disabledTools: ["ctx_memory"],
    toolPrefix: "lc_",
    forwardedEnv: { LEAN_CTX_COMPRESSION: "aggressive" },
    localTools: ["ctx_read", "ctx_shell"],
    mode: "additive",
    routeShell: false,
    toolSurfaceConfiguration: "config:v1",
    ...overrides,
  };
}

const tools: McpTool[] = [{
  name: "ctx_search",
  description: "Search",
  inputSchema: {
    type: "object",
    properties: { query: { type: "string" } },
    required: ["query"],
  },
}];

describe("MCP schema cache", () => {
  it("treats corruption, incompatible versions, and malformed tools as misses", () => {
    const root = mkdtempSync(join(tmpdir(), "pi-lean-ctx-cache-"));
    temporaryDirectories.push(root);
    const path = join(root, "schema-cache.json");
    const key = createMcpSchemaCacheKey(inputs());

    writeFileSync(path, "{not-json", "utf8");
    expect(loadMcpSchemaCache(path, key)).toBeUndefined();

    writeFileSync(path, JSON.stringify({
      formatVersion: MCP_SCHEMA_CACHE_FORMAT_VERSION + 1,
      key,
      tools,
    }), "utf8");
    expect(loadMcpSchemaCache(path, key)).toBeUndefined();

    expect(validateMcpSchemaCache({
      formatVersion: MCP_SCHEMA_CACHE_FORMAT_VERSION,
      key,
      tools: [{ name: "ctx_search" }, { name: "ctx_search" }],
    }, key)).toBeUndefined();
    expect(validateMcpSchemaCache({
      formatVersion: MCP_SCHEMA_CACHE_FORMAT_VERSION,
      key,
      tools: [{ name: "ctx_search", inputSchema: "not-a-schema" }],
    }, key)).toBeUndefined();
    expect(validateMcpSchemaCache({
      formatVersion: MCP_SCHEMA_CACHE_FORMAT_VERSION,
      key,
      tools: [{
        name: "ctx_search",
        inputSchema: { properties: { query: null } },
      }],
    }, key)).toBeUndefined();
  });

  it("round-trips a valid cache through an atomic refresh", () => {
    const root = mkdtempSync(join(tmpdir(), "pi-lean-ctx-cache-"));
    temporaryDirectories.push(root);
    const path = join(root, "nested", "schema-cache.json");
    const key = createMcpSchemaCacheKey(inputs());

    writeMcpSchemaCache(path, key, tools);

    expect(loadMcpSchemaCache(path, key)).toEqual(tools);
    expect(readdirSync(join(root, "nested"))).toEqual(["schema-cache.json"]);
    expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({
      formatVersion: MCP_SCHEMA_CACHE_FORMAT_VERSION,
      key,
      tools,
    });
  });

  it("changes the key for every advertised-surface input", () => {
    const baseline = inputs();
    const key = createMcpSchemaCacheKey(baseline);
    const variants: Array<Partial<McpSchemaCacheInputs>> = [
      { extensionVersion: "3.10.2" },
      { engineVersion: "2.1.0" },
      { binaryPath: "/other/lean-ctx" },
      { binaryIdentity: "size=11;mtime=20" },
      { toolProfile: "power" },
      { disabledTools: ["ctx_memory", "ctx_search"] },
      { toolPrefix: "other_" },
      { forwardedEnv: { LEAN_CTX_COMPRESSION: "safe" } },
      { localTools: ["ctx_read", "ctx_shell", "ctx_edit"] },
      { mode: "replace" },
      { routeShell: true },
      { toolSurfaceConfiguration: "config:v2" },
    ];

    for (const variant of variants) {
      expect(createMcpSchemaCacheKey({ ...baseline, ...variant })).not.toBe(key);
    }
  });

  it("keeps the key deterministic across equivalent set/map orderings", () => {
    const baseline = inputs();
    expect(createMcpSchemaCacheKey(baseline)).toBe(createMcpSchemaCacheKey({
      ...baseline,
      disabledTools: ["ctx_memory"],
      localTools: ["ctx_shell", "ctx_read"],
      forwardedEnv: { LEAN_CTX_COMPRESSION: "aggressive" },
    }));
  });

  it("normalizes effective engine environment without losing forwarded inputs", () => {
    expect(toolSurfaceEnvironment(
      { CUSTOM_ENGINE_FLAG: "from-config", LEAN_CTX_TOOL_PROFILE: "lean" },
      { LEAN_CTX_TOOL_PROFILE: "power", LEAN_CTX_DISABLED_TOOLS: "ctx_old", PATH: "/bin" },
    )).toEqual({
      CUSTOM_ENGINE_FLAG: "from-config",
      LEAN_CTX_COMPRESS: "1",
      LEAN_CTX_DISABLED_TOOLS: "ctx_old",
      LEAN_CTX_TOOL_PROFILE: "power",
    });
    expect(toolSurfaceEnvironment(
      { CUSTOM_ENGINE_FLAG: "from-config" },
      { CUSTOM_ENGINE_FLAG: "from-process" },
    )).toEqual({
      CUSTOM_ENGINE_FLAG: "from-process",
      LEAN_CTX_COMPRESS: "1",
    });
  });

  it("invalidates when engine tool-surface config changes", () => {
    const root = mkdtempSync(join(tmpdir(), "pi-lean-ctx-config-"));
    temporaryDirectories.push(root);
    const configDir = join(root, "config");
    const project = join(root, "project");
    mkdirSync(configDir, { recursive: true });
    mkdirSync(project, { recursive: true });
    const env = {
      HOME: root,
      XDG_CONFIG_HOME: join(root, "xdg"),
      XDG_DATA_HOME: join(root, "data"),
      LEAN_CTX_CONFIG_DIR: configDir,
      LEAN_CTX_PROJECT_ROOT: project,
    };
    const first = identifyToolSurfaceConfiguration(env, project);

    writeFileSync(join(configDir, "config.toml"), "tool_profile = 'power'\n", "utf8");
    const second = identifyToolSurfaceConfiguration(env, project);
    expect(second).not.toBe(first);

    writeFileSync(join(project, ".lean-ctx.toml"), "tool_profile = 'standard'\n", "utf8");
    expect(identifyToolSurfaceConfiguration(env, project)).not.toBe(second);
  });

  it("pins PI_EXTENSION_VERSION to the published package version (#1446 F3)", () => {
    // The constant is part of the cache key: if it lags a release that changed
    // the advertised tool surface, the key is unchanged and the new version
    // silently serves the old schemas. `scripts/check-package-versions.py`
    // enforces the same equality against the engine version at release time.
    const manifest = JSON.parse(
      readFileSync(new URL("../package.json", import.meta.url), "utf8"),
    ) as { version: string };
    expect(PI_EXTENSION_VERSION).toBe(manifest.version);
  });

  it("scans PATHEXT variants for a bare binary name on Windows (#1446 F8)", () => {
    const env = {
      PATH: ["/opt/bin", "/usr/bin"].join(delimiter),
      PATHEXT: ".COM;.EXE;.CMD",
    };

    expect(binaryPathCandidates("lean-ctx", env, "linux")).toEqual([
      join("/opt/bin", "lean-ctx"),
      join("/usr/bin", "lean-ctx"),
    ]);

    const windows = binaryPathCandidates("lean-ctx", env, "win32");
    expect(windows).toHaveLength(8);
    expect(windows.slice(0, 4)).toEqual([
      join("/opt/bin", "lean-ctx"),
      join("/opt/bin", "lean-ctx.COM"),
      join("/opt/bin", "lean-ctx.EXE"),
      join("/opt/bin", "lean-ctx.CMD"),
    ]);

    // Without PATHEXT set, the documented Windows default still finds the .exe.
    expect(binaryPathCandidates("lean-ctx", { PATH: "/opt/bin" }, "win32"))
      .toContain(join("/opt/bin", "lean-ctx.EXE"));
  });

  it("keeps an unresolvable bare binary name independent of cwd (#1446 F8)", () => {
    const root = mkdtempSync(join(tmpdir(), "pi-lean-ctx-bin-"));
    temporaryDirectories.push(root);
    const originalPath = process.env.PATH;
    process.env.PATH = root;
    try {
      const identity = identifyBinary("lean-ctx-not-installed");
      expect(identity).toContain('"path":"lean-ctx-not-installed"');
      // cwd-resolving an unresolvable name made the cache key depend on the
      // directory Pi happened to start in.
      expect(identity).not.toContain(process.cwd());
    } finally {
      process.env.PATH = originalPath;
    }
  });
});
