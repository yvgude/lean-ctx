import { createHash, randomUUID } from "node:crypto";
import {
  accessSync,
  constants,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { delimiter, dirname, join, resolve } from "node:path";
import { homedir } from "node:os";
import type { McpTool } from "./mcp-bridge.js";

/** Bump when the on-disk cache representation changes incompatibly. */
export const MCP_SCHEMA_CACHE_FORMAT_VERSION = 1;
/** Bump when the Pi extension's schema-producing behavior changes. */
export const PI_EXTENSION_VERSION = "3.10.1";
/** MCP client/server contract version used by the embedded bridge. */
export const MCP_ENGINE_VERSION = "2.0.0";

export type McpSchemaCacheInputs = {
  extensionVersion: string;
  engineVersion: string;
  binaryPath: string;
  binaryIdentity: string;
  toolProfile: string;
  disabledTools: readonly string[];
  toolPrefix?: string;
  forwardedEnv: Readonly<Record<string, string>>;
  localTools: readonly string[];
  mode: string;
  routeShell: boolean;
  toolSurfaceConfiguration: string;
};

export type McpSchemaCache = {
  formatVersion: number;
  key: string;
  tools: McpTool[];
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isJsonValue(value: unknown): boolean {
  if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return true;
  }
  if (Array.isArray(value)) return value.every(isJsonValue);
  return isRecord(value) && Object.values(value).every(isJsonValue);
}

/** Guard the schema shapes consumed by `propToTypebox` during warm startup. */
function isJsonSchema(value: unknown): value is Record<string, unknown> {
  if (!isRecord(value)) return false;
  if (value.description !== undefined && typeof value.description !== "string") return false;
  if (value.type !== undefined && typeof value.type !== "string") return false;
  if (value.enum !== undefined && (!Array.isArray(value.enum) || !value.enum.every(isJsonValue))) {
    return false;
  }
  if (value.required !== undefined && (
    !Array.isArray(value.required)
    || !value.required.every((item) => typeof item === "string")
  )) {
    return false;
  }
  if (value.properties !== undefined && (
    !isRecord(value.properties)
    || !Object.values(value.properties).every(isJsonSchema)
  )) {
    return false;
  }
  if (value.items !== undefined && !isJsonSchema(value.items)) return false;
  return Object.values(value).every(isJsonValue);
}

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map((item) => canonicalJson(item)).join(",")}]`;
  }
  if (isRecord(value)) {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

function sortedRecord(value: Readonly<Record<string, string>>): Record<string, string> {
  return Object.fromEntries(
    Object.entries(value).sort(([left], [right]) => compareStrings(left, right)),
  );
}

function compareStrings(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function sortedNames(values: readonly string[], lowerCase: boolean): string[] {
  return [...new Set(values.map((value) => lowerCase ? value.toLowerCase() : value))]
    .sort(compareStrings);
}

function normalizedInputs(inputs: McpSchemaCacheInputs): Record<string, unknown> {
  return {
    binaryIdentity: inputs.binaryIdentity,
    binaryPath: inputs.binaryPath,
    disabledTools: sortedNames(inputs.disabledTools, true),
    engineVersion: inputs.engineVersion,
    extensionVersion: inputs.extensionVersion,
    forwardedEnv: sortedRecord(inputs.forwardedEnv),
    localTools: sortedNames(inputs.localTools, false),
    mode: inputs.mode,
    routeShell: inputs.routeShell,
    toolSurfaceConfiguration: inputs.toolSurfaceConfiguration,
    toolPrefix: inputs.toolPrefix ?? "",
    toolProfile: inputs.toolProfile,
    cacheFormatVersion: MCP_SCHEMA_CACHE_FORMAT_VERSION,
  };
}

/**
 * Return the effective engine environment that can affect its advertised
 * tool surface without persisting any raw environment values to disk.
 */
export function toolSurfaceEnvironment(
  forwardedEnv: Readonly<Record<string, string>>,
  processEnv: Readonly<NodeJS.ProcessEnv> = process.env,
): Record<string, string> {
  const effective: Record<string, string> = { ...forwardedEnv };
  for (const key of Object.keys(forwardedEnv)) {
    const value = processEnv[key];
    if (value !== undefined) effective[key] = value;
  }
  for (const [key, value] of Object.entries(processEnv)) {
    if (key.startsWith("LEAN_CTX_") && value !== undefined) effective[key] = value;
  }
  effective.LEAN_CTX_COMPRESS = "1";
  return sortedRecord(effective);
}

/** Hash all inputs that can change which direct MCP tools are advertised. */
export function createMcpSchemaCacheKey(inputs: McpSchemaCacheInputs): string {
  return createHash("sha256")
    .update(canonicalJson(normalizedInputs(inputs)), "utf8")
    .digest("hex");
}

const DEFAULT_PATHEXT = ".COM;.EXE;.BAT;.CMD";

/**
 * Every filename `PATH` lookup should try for a bare command name, in order.
 *
 * On Windows a bare `lean-ctx` only ever exists on disk as `lean-ctx.exe`, so
 * a scan without `PATHEXT` never resolves it and the identity degenerates to
 * "missing" — which means a binary upgrade stops invalidating the schema cache.
 * Exported so the platform-specific ordering is unit-testable off-Windows.
 */
export function binaryPathCandidates(
  binaryName: string,
  processEnv: Readonly<NodeJS.ProcessEnv> = process.env,
  platform: NodeJS.Platform = process.platform,
): string[] {
  const extensions = platform === "win32"
    ? ["", ...(processEnv.PATHEXT ?? DEFAULT_PATHEXT)
      .split(";")
      .map((extension) => extension.trim())
      .filter((extension) => extension.length > 0)]
    : [""];

  const candidates: string[] = [];
  for (const directory of processEnv.PATH?.split(delimiter) ?? []) {
    if (!directory) continue;
    for (const extension of extensions) {
      candidates.push(join(directory, `${binaryName}${extension}`));
    }
  }
  return candidates;
}

function isExecutable(candidate: string): boolean {
  try {
    accessSync(candidate, constants.F_OK | constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** Include the binary path plus stable file identity in cache invalidation. */
export function identifyBinary(binaryPath: string): string {
  const explicitPath = binaryPath.includes("/") || binaryPath.includes("\\");
  const pathOnDisk = explicitPath
    ? resolve(binaryPath)
    : binaryPathCandidates(binaryPath).find(isExecutable);

  if (pathOnDisk === undefined) {
    // An unresolvable bare name must stay unresolved: `resolve()`-ing it would
    // make the identity — and therefore the cache key — depend on the directory
    // Pi happened to start in.
    return canonicalJson({ missing: true, path: binaryPath });
  }

  try {
    const info = statSync(pathOnDisk);
    return canonicalJson({
      dev: info.dev,
      ino: info.ino,
      mtimeMs: info.mtimeMs,
      size: info.size,
      path: resolve(pathOnDisk),
    });
  } catch {
    return canonicalJson({ missing: true, path: resolve(pathOnDisk) });
  }
}

function ancestorDirectories(start: string): string[] {
  const directories: string[] = [];
  let current = resolve(start);
  while (true) {
    directories.push(current);
    const parent = resolve(current, "..");
    if (parent === current) return directories;
    current = parent;
  }
}

function addFileSnapshot(paths: Set<string>, path: string): void {
  paths.add(resolve(path));
}

function addRoleDirectorySnapshots(paths: Set<string>, directory: string): void {
  const resolved = resolve(directory);
  paths.add(`${resolved}/`);
  try {
    for (const name of readdirSync(resolved)) {
      if (name.endsWith(".toml")) addFileSnapshot(paths, join(resolved, name));
    }
  } catch {
    // A missing or unreadable role directory still contributes its stable path.
  }
}

/**
 * Fingerprint files that can change the MCP server's advertised tool surface.
 * The server loads global/project config and role policy independently of the
 * Pi extension config, so those inputs must invalidate a warm schema cache too.
 */
export function identifyToolSurfaceConfiguration(
  processEnv: Readonly<NodeJS.ProcessEnv> = process.env,
  cwd: string = process.cwd(),
): string {
  const home = processEnv.HOME ?? homedir();
  const xdgConfigHome = processEnv.XDG_CONFIG_HOME ?? resolve(home, ".config");
  const xdgDataHome = processEnv.XDG_DATA_HOME ?? resolve(home, ".local", "share");
  const paths = new Set<string>();

  for (const directory of [
    processEnv.LEAN_CTX_CONFIG_DIR,
    resolve(xdgConfigHome, "lean-ctx"),
    resolve(home, ".lean-ctx"),
    processEnv.LEAN_CTX_DATA_DIR,
  ]) {
    if (directory) addFileSnapshot(paths, join(directory, "config.toml"));
  }

  const projectRoots = new Set<string>(ancestorDirectories(cwd));
  if (processEnv.LEAN_CTX_PROJECT_ROOT) {
    projectRoots.add(resolve(processEnv.LEAN_CTX_PROJECT_ROOT));
  }
  for (const root of projectRoots) {
    addFileSnapshot(paths, join(root, ".lean-ctx.toml"));
    addRoleDirectorySnapshots(paths, join(root, ".lean-ctx", "roles"));
  }

  for (const directory of [
    resolve(home, ".lean-ctx", "roles"),
    resolve(xdgDataHome, "lean-ctx", "roles"),
    processEnv.LEAN_CTX_DATA_DIR && resolve(processEnv.LEAN_CTX_DATA_DIR, "roles"),
  ]) {
    if (directory) addRoleDirectorySnapshots(paths, directory);
  }

  const snapshots = [...paths].sort().map((path) => {
    if (path.endsWith("/")) {
      try {
        return [path, readdirSync(path).sort()] as const;
      } catch {
        return [path, "missing"] as const;
      }
    }
    try {
      return [
        path,
        createHash("sha256").update(readFileSync(path)).digest("hex"),
      ] as const;
    } catch {
      return [path, "missing"] as const;
    }
  });

  return createHash("sha256")
    .update(canonicalJson({ cwd: resolve(cwd), snapshots }), "utf8")
    .digest("hex");
}

function isMcpTool(value: unknown): value is McpTool {
  if (!isRecord(value) || typeof value.name !== "string" || value.name.trim().length === 0) {
    return false;
  }
  if (value.description !== undefined && typeof value.description !== "string") {
    return false;
  }
  return value.inputSchema === undefined || isJsonSchema(value.inputSchema);
}

/** Validate the version, key, and complete direct-tool metadata before use. */
export function validateMcpSchemaCache(
  value: unknown,
  expectedKey: string,
): McpTool[] | undefined {
  if (!isRecord(value)) return undefined;
  if (value.formatVersion !== MCP_SCHEMA_CACHE_FORMAT_VERSION) return undefined;
  if (value.key !== expectedKey || !Array.isArray(value.tools)) return undefined;

  const tools = value.tools;
  if (!tools.every(isMcpTool)) return undefined;
  const names = tools.map((tool) => tool.name);
  if (new Set(names).size !== names.length) return undefined;
  return tools as McpTool[];
}

/** Read a cache entry; all corruption and incompatibility paths are cache misses. */
export function loadMcpSchemaCache(
  cachePath: string,
  expectedKey: string,
): McpTool[] | undefined {
  try {
    const parsed: unknown = JSON.parse(readFileSync(cachePath, "utf8"));
    return validateMcpSchemaCache(parsed, expectedKey);
  } catch {
    return undefined;
  }
}

/**
 * Refresh the cache with a same-directory write followed by atomic rename.
 * A failed refresh leaves the previous valid cache untouched.
 */
export function writeMcpSchemaCache(
  cachePath: string,
  key: string,
  tools: McpTool[],
): void {
  const cache: McpSchemaCache = {
    formatVersion: MCP_SCHEMA_CACHE_FORMAT_VERSION,
    key,
    tools,
  };
  const temporaryPath = `${cachePath}.tmp-${process.pid}-${randomUUID()}`;
  mkdirSync(dirname(cachePath), { recursive: true });
  try {
    writeFileSync(
      temporaryPath,
      `${JSON.stringify(cache)}\n`,
      { encoding: "utf8", mode: 0o600 },
    );
    renameSync(temporaryPath, cachePath);
  } catch (error) {
    try {
      unlinkSync(temporaryPath);
    } catch {
      // Preserve the original write/rename error.
    }
    throw error;
  }
}

/** Keep cache placement beside the Pi extension config, outside the package. */
export function mcpSchemaCachePath(configPath: string): string {
  return resolve(dirname(configPath), "mcp-schema-cache.json");
}
