import type {
  AgentToolResult,
  BashToolDetails,
  ExtensionAPI,
  ReadToolDetails,
} from "@earendil-works/pi-coding-agent";
import {
  createBashToolDefinition,
  createFindToolDefinition,
  createGrepToolDefinition,
  createLsToolDefinition,
  createReadToolDefinition,
  DEFAULT_MAX_LINES,
  getLanguageFromPath,
  highlightCode,
  truncateHead,
} from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";
import { Type } from "typebox";
import { existsSync, readFileSync } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { extname, resolve } from "node:path";
import { homedir, platform } from "node:os";
import { McpBridge } from "./mcp-bridge.js";
import { loadPiConfig } from "./config.js";
import type { CompressionStats } from "./types.js";

const CODE_EXTENSIONS = new Set([
  ".rs", ".ts", ".tsx", ".js", ".jsx", ".php", ".py", ".go",
  ".java", ".c", ".cc", ".cpp", ".cxx", ".cs", ".kt", ".swift", ".rb",
  ".vue", ".svelte", ".astro", ".html", ".css", ".scss", ".sass", ".less",
  ".lua", ".zig", ".nim", ".ex", ".exs", ".erl", ".hs", ".ml", ".mli",
  ".r", ".jl", ".dart", ".scala", ".groovy", ".pl", ".pm", ".sh", ".bash",
  ".zsh", ".fish", ".ps1", ".bat", ".cmd", ".sql", ".graphql", ".gql",
  ".proto", ".thrift", ".tf", ".hcl", ".nix", ".dhall",
]);

const FULL_READ_EXTENSIONS = new Set([
  ".md", ".txt", ".json", ".json5", ".yaml", ".yml", ".toml",
  ".env", ".ini", ".xml", ".lock",
]);

const IMAGE_EXTENSIONS = new Set([".png", ".jpg", ".jpeg", ".gif", ".webp"]);
const CODE_FULL_READ_MAX_BYTES = 8 * 1024;
const CODE_SIGNATURES_MIN_BYTES = 96 * 1024;

// Pi builtins that can be replaced with ctx_ prefixed versions.
// Settings resolve from (most explicit first): LEAN_CTX_PI_* env vars, then
// ~/.pi/agent/extensions/pi-lean-ctx/config.json, then defaults (issue #344).
//   mode "additive" (default) — keep Pi builtins, add ctx_* alongside
//   mode "replace"            — disable Pi builtins, only expose ctx_*
const DISABLED_BUILTIN_TOOLS = new Set(["read", "bash", "ls", "find", "grep"]);
const PI_CONFIG = loadPiConfig();
const PI_MODE = PI_CONFIG.mode;
// Max bytes constant for truncation warnings (same as Pi's DEFAULT_MAX_BYTES)
const DEFAULT_MAX_BYTES = 8192;

const readModeSchema = Type.Union([
  Type.Literal("full"),
  Type.Literal("map"),
  Type.Literal("signatures"),
], { description: "Override auto-selection: full (complete content), map (deps+API signatures), signatures (AST only)" });

const readSchema = Type.Object({
  path: Type.String({ description: "Path to the file to read (relative or absolute)" }),
  offset: Type.Optional(Type.Number({ description: "Line number to start reading from (1-indexed)" })),
  limit: Type.Optional(Type.Number({ description: "Maximum number of lines to read" })),
  mode: Type.Optional(readModeSchema),
});

// `path` is REQUIRED on ls/find/grep (#395): with an optional path these tools
// silently fell back to the extension's cwd — an agent working in a different
// directory got results from the wrong tree and was derailed. Forcing the
// parameter makes the scope an explicit, visible part of every call.
const lsSchema = Type.Object({
  path: Type.String({ description: "Directory to list. Pass the directory you are working in — there is no cwd fallback." }),
  limit: Type.Optional(Type.Number({ description: "Maximum number of entries to return (default: 500)" })),
});

const findSchema = Type.Object({
  pattern: Type.String({ description: "Glob pattern to match files" }),
  path: Type.String({ description: "Directory to search in. Pass the directory you are working in — there is no cwd fallback." }),
  limit: Type.Optional(Type.Number({ description: "Maximum number of results (default: 1000)" })),
});

const grepSchema = Type.Object({
  pattern: Type.String({ description: "Search pattern (regex or literal string)" }),
  path: Type.String({ description: "Directory or file to search. Pass the directory you are working in — there is no cwd fallback." }),
  glob: Type.Optional(Type.String({ description: "Filter files by glob pattern, e.g. '*.ts'" })),
  ignoreCase: Type.Optional(Type.Boolean({ description: "Case-insensitive search (default: false)" })),
  literal: Type.Optional(Type.Boolean({ description: "Treat pattern as literal string (default: false)" })),
  context: Type.Optional(Type.Number({ description: "Lines of context around each match (default: 0)" })),
  limit: Type.Optional(Type.Number({ description: "Maximum number of matches (default: 100)" })),
});

const multiReadSchema = Type.Object({
  paths: Type.Array(Type.String({ description: "Absolute file paths to read, in order" })),
  mode: Type.Optional(Type.String({ description: "Compression mode (auto, full, raw, map, signatures, diff, aggressive, entropy, task, reference, lines:N-M). Use 'raw' for zero-overhead output." })),
  fresh: Type.Optional(Type.Boolean({ description: "Bypass cache and force a full re-read for all paths. Use when running as a subagent that may not have the parent's context." })),
});

const searchSchema = Type.Object({
  pattern: Type.String({ description: "Regex pattern" }),
  path: Type.Optional(Type.String({ description: "Directory to search" })),
  paths: Type.Optional(Type.Array(Type.String({ description: "Multiple roots (alternative to path)" }))),
  include: Type.Optional(Type.String({ description: "Glob filter, e.g. *.ts, src/**/*.rs" })),
  max_results: Type.Optional(Type.Number({ description: "Default 20" })),
  ignore_gitignore: Type.Optional(Type.Boolean({ description: "Also scan gitignored files (needs role)" })),
});

const treeSchema = Type.Object({
  path: Type.Optional(Type.String({ description: "Directory (default: .)" })),
  paths: Type.Optional(Type.Array(Type.String({ description: "Multiple roots (alternative to path)" }))),
  depth: Type.Optional(Type.Number({ description: "Max depth (default 3)" })),
  show_hidden: Type.Optional(Type.Boolean({ description: "Show hidden files" })),
  respect_gitignore: Type.Optional(Type.Boolean({ description: "Default true" })),
});

const leanCtxSchema = Type.Object({
  args: Type.Array(
    Type.String({
      description:
        "Arguments after 'lean-ctx'. Example: ['overview'] or ['knowledge','recall','Pi']",
    }),
  ),
});

function shellQuote(value: string): string {
  if (!value) return "''";
  if (/^[A-Za-z0-9_./=:@,+%^-]+$/.test(value)) return value;
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

// Environment for every lean-ctx subprocess: config.json `env` overrides
// (lowest precedence) < the caller's env < the flags lean-ctx must always see.
function leanCtxEnv(base: NodeJS.ProcessEnv = process.env): NodeJS.ProcessEnv {
  return {
    ...PI_CONFIG.forwardedEnv,
    ...base,
    LEAN_CTX_COMPRESS: "1",
    LEAN_CTX_SAVINGS_FOOTER: "always",
  };
}

function resolveBinary(): string {
  const envBin = process.env.LEAN_CTX_BIN;
  if (envBin && existsSync(envBin)) return envBin;
  if (PI_CONFIG.binaryOverride && existsSync(PI_CONFIG.binaryOverride)) {
    return PI_CONFIG.binaryOverride;
  }

  const home = homedir();
  const isWin = platform() === "win32";
  const candidates = isWin
    ? [
        resolve(home, ".cargo", "bin", "lean-ctx.exe"),
        resolve(home, "AppData", "Local", "lean-ctx", "lean-ctx.exe"),
      ]
    : [
        resolve(home, ".cargo", "bin", "lean-ctx"),
        resolve(home, ".local", "bin", "lean-ctx"),
        "/usr/local/bin/lean-ctx",
      ];

  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }

  return "lean-ctx";
}

function normalizePathArg(path: string): string {
  return path.startsWith("@") ? path.slice(1) : path;
}

async function chooseReadMode(path: string): Promise<"full" | "map" | "signatures"> {
  const ext = extname(path).toLowerCase();
  if (FULL_READ_EXTENSIONS.has(ext)) return "full";

  const fileStat = await stat(path);
  const size = fileStat.size;

  if (!CODE_EXTENSIONS.has(ext)) return size > 48 * 1024 ? "map" : "full";
  if (size >= CODE_SIGNATURES_MIN_BYTES) return "signatures";
  if (size >= CODE_FULL_READ_MAX_BYTES) return "map";
  return "full";
}

async function readSlice(path: string, offset?: number, limit?: number) {
  const content = await readFile(path, "utf8");
  const lines = content.split("\n");
  const startLine = offset ? Math.max(0, offset - 1) : 0;
  const endLine = limit ? startLine + limit : lines.length;
  const selected = lines.slice(startLine, endLine).join("\n");
  const truncation = truncateHead(selected, {
    maxLines: DEFAULT_MAX_LINES,
    maxBytes: DEFAULT_MAX_BYTES,
  });
  return { text: truncation.content, lines: lines.length, truncated: truncation.truncated };
}

function estimateTokens(text: string) {
  return Math.ceil(text.length / 4);
}

function clampStats(original: number, compressed: number): CompressionStats {
  const orig = Math.max(0, original);
  const comp = Math.max(0, Math.min(orig, compressed));
  const saved = Math.max(0, orig - comp);
  const percentSaved = orig > 0 ? Math.round((saved / orig) * 100) : 0;
  return { originalTokens: orig, compressedTokens: comp, percentSaved };
}

function parseLeanCtxOutput(text: string) {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  let stats: CompressionStats | undefined;
  const kept: string[] = [];

  for (const line of lines) {
    const trimmed = line.trim();

    const shellMatch = trimmed.match(/^\[lean-ctx:\s*(\d+)\s*→\s*(\d+)\s*tok,\s*-?(\d+)%\]$/);
    if (shellMatch) {
      stats = clampStats(Number(shellMatch[1]), Number(shellMatch[2]));
      continue;
    }

    const savedMatch = trimmed.match(/^\[(\d+)\s+tok saved(?:\s+\((\d+)%\))?\]$/);
    if (savedMatch) {
      const saved = Number(savedMatch[1]);
      const pct = savedMatch[2] ? Number(savedMatch[2]) : 0;
      if (pct > 0) {
        const original = Math.round((saved * 100) / pct);
        stats = clampStats(original, Math.max(0, original - saved));
      } else {
        stats = clampStats(saved, saved);
      }
      continue;
    }

    kept.push(line);
  }

  return { text: kept.join("\n").replace(/\n{3,}/g, "\n\n").trimEnd(), stats };
}

function formatFooter(stats: CompressionStats) {
  const pct = stats.percentSaved > 0 ? `-${stats.percentSaved}%` : "0%";
  return `Compressed ${stats.originalTokens} → ${stats.compressedTokens} tokens (${pct})`;
}

function withFooter(text: string, opts?: {
  originalText?: string;
  limit?: number;
  always?: boolean;
  preferEstimate?: boolean;
  suppressIfNoSaving?: boolean;
}) {
  const parsed = parseLeanCtxOutput(text);
  const limited = limitLines(parsed.text, opts?.limit);

  let stats = parsed.stats;
  if (opts?.originalText !== undefined && (opts.preferEstimate || !stats)) {
    stats = clampStats(estimateTokens(opts.originalText), estimateTokens(limited.text));
  }
  if (!stats && opts?.always) {
    const tokens = estimateTokens(limited.text);
    stats = clampStats(tokens, tokens);
  }
  if (!stats) return { text: limited.text, stats: undefined, truncated: limited.truncated };

  // On tiny files compression cannot beat the envelope, so a "0%" footer would
  // be pure overhead — larger payload than the source for no gain (#361). Keep
  // the computed stats for telemetry (`details.compression`) but drop the
  // visible footer when nothing was actually saved.
  if (opts?.suppressIfNoSaving && stats.percentSaved <= 0) {
    return { text: limited.text, stats, truncated: limited.truncated };
  }

  const footer = formatFooter(stats);
  const base = limited.text.trimEnd();
  return {
    text: base ? `${base}\n\n${footer}` : footer,
    stats,
    truncated: limited.truncated,
  };
}

function limitLines(text: string, limit?: number) {
  if (!limit || limit <= 0) return { text, truncated: false };
  const lines = text.split("\n");
  if (lines.length <= limit) return { text, truncated: false };
  return {
    text: lines.slice(0, limit).join("\n") + `\n\n[Output truncated to ${limit} lines]`,
    truncated: true,
  };
}

function replaceTabs(text: string) {
  return text.replace(/\t/g, "    ");
}

function trimTrailingEmpty(lines: string[]) {
  let end = lines.length;
  while (end > 0 && lines[end - 1] === "") end--;
  return lines.slice(0, end);
}

function splitFooter(text: string) {
  const normalized = text.replace(/\r\n/g, "\n").trimEnd();
  const match = normalized.match(/\n\n(Compressed \d+ → \d+ tokens \((?:-?\d+|0)%\))$/);
  if (!match) return { body: normalized, footer: undefined as string | undefined };
  return { body: normalized.slice(0, -match[0].length), footer: match[1] };
}

function isMcpAdapterConfigured(): boolean {
  const home = homedir();
  const mcpConfigPaths = [
    resolve(home, ".pi", "agent", "mcp.json"),
    resolve(process.cwd(), ".pi", "mcp.json"),
  ];

  for (const configPath of mcpConfigPaths) {
    if (!existsSync(configPath)) continue;
    try {
      const content = readFileSync(configPath, "utf8");
      const json = JSON.parse(content);
      const servers = json?.mcpServers ?? {};
      if ("lean-ctx" in servers) return true;
    } catch {
      continue;
    }
  }
  return false;
}

async function execLeanCtx(pi: ExtensionAPI, args: string[]) {
  const bin = resolveBinary();
  const result = await pi.exec(bin, args);
  if (result.code !== 0) {
    const msg = (result.stderr || result.stdout || `lean-ctx failed: ${args.join(" ")}`).trim();
    throw new Error(msg);
  }
  return result.stdout;
}

export default async function (pi: ExtensionAPI) {
  // pi.exec()'s ExecOptions carries no `env`, so lean-ctx subprocesses inherit
  // THIS process's environment. Seed it once with the config.json `env` overrides
  // (issue #344) plus the flags lean-ctx must always see, so every path — pi.exec,
  // the bash spawnHook, and the MCP bridge — shares one environment. An explicitly
  // set environment variable always wins over the config file.
  for (const [key, value] of Object.entries(PI_CONFIG.forwardedEnv)) {
    if (process.env[key] === undefined) process.env[key] = value;
  }
  process.env.LEAN_CTX_COMPRESS = "1";
  process.env.LEAN_CTX_SAVINGS_FOOTER ??= "always";

  // Defer setActiveTools to session_start — runtime actions aren't available during extension load
  // In "replace" mode, disable Pi builtins and only expose ctx_* tools.
  // In "additive" mode (default), keep Pi builtins alongside ctx_* tools.
  if (PI_MODE === "replace") {
    pi.on("session_start", () => {
      const activeTools = pi.getActiveTools().filter((name) => !DISABLED_BUILTIN_TOOLS.has(name));
      pi.setActiveTools(activeTools);
    });
  }

  // Declared up-front so the ctx_read handler (registered below) can route
  // through the embedded bridge once it connects. Assigned after the tools are
  // registered (the bridge is started at the end of this function).
  let mcpBridge: McpBridge | null = null;

  // ── Collision-safe registration (#359) ───────────────────────────────────
  // lean-ctx must coexist with other Pi extensions (AFT, magic-context). If a
  // tool name is already claimed, skip it with a warning instead of letting the
  // whole agent crash on load. Users can also hand a name to another extension
  // via LEAN_CTX_PI_DISABLE_TOOLS / config.json `disableTools`. All ctx_* tools
  // below register through this wrapper instead of pi.registerTool directly.
  const skippedExtensionTools: string[] = [];
  const disabledExtensionTools: string[] = [];
  const registerTool = ((def: { name?: unknown }): void => {
    const name = typeof def.name === "string" ? def.name : String(def.name);
    if (PI_CONFIG.disabledTools.has(name.toLowerCase())) {
      disabledExtensionTools.push(name);
      return;
    }
    try {
      (pi.registerTool as (d: unknown) => void)(def);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      skippedExtensionTools.push(name);
      console.error(
        `[pi-lean-ctx] Skipped tool "${name}" — already registered elsewhere? (${msg})`,
      );
    }
  }) as unknown as ExtensionAPI["registerTool"];

  const baseBashTool = createBashToolDefinition(process.cwd(), {
    spawnHook: ({ command, cwd, env }) => {
      const bin = resolveBinary();
      return {
        command: `${shellQuote(bin)} -c ${shellQuote(command)}`,
        cwd,
        env: leanCtxEnv(env),
      };
    },
  });

  const rawBash = createBashToolDefinition(process.cwd());

  const bashSchemaWithRaw = Type.Object({
    command: Type.String({ description: "Bash command to execute" }),
    timeout: Type.Optional(Type.Number({ description: "Timeout in seconds to prevent hanging commands" })),
    raw: Type.Optional(Type.Boolean({ description: "Skip compression, return full uncompressed output" })),
  });

  // ── ctx_shell (replaces bash) ─────────────────────────────────────────
  registerTool({
    name: "ctx_shell",
    label: "ctx_shell",
    description:
      "Run shell commands. Prefer over native Bash/shell (auto-compressed output). " +
      "IMPORTANT: Do NOT use ctx_shell to read files (cat/head/tail) — use ctx_read instead. " +
      "Do NOT use ctx_shell for grep/find/ls — use ctx_grep, ctx_find, ctx_ls. " +
      "Set raw=true to skip compression when exact output matters. " +
      "Use timeout (seconds) to prevent hanging commands.",
    promptSnippet: "Run shell commands (not for file reading — use ctx_read)",
    promptGuidelines: [
      "Use ctx_shell only for commands with side effects: build, test, install, git, run scripts.",
    ],
    parameters: bashSchemaWithRaw,
    renderCall(args, theme, context) {
      return baseBashTool.renderCall
        ? baseBashTool.renderCall(args, theme, context)
        : (context.lastComponent ?? new Text("", 0, 0));
    },
    renderResult(result, options, theme, context) {
      // ctx_shell wraps Pi's bash tool; its renderer is typed for BashToolDetails,
      // while our result adds compression stats on top of the same shape.
      return baseBashTool.renderResult
        ? baseBashTool.renderResult(
            result as AgentToolResult<BashToolDetails | undefined>,
            options,
            theme,
            context,
          )
        : (context.lastComponent ?? new Text("", 0, 0));
    },
    async execute(toolCallId, params, signal, onUpdate, ctx) {
      const isRaw = !!params.raw;
      const toolParams = { command: params.command, timeout: params.timeout };
      const tool = isRaw ? rawBash : baseBashTool;
      try {
        const result = await tool.execute(toolCallId, toolParams, signal, onUpdate, ctx);
        const text = result.content?.[0]?.type === "text" ? result.content[0].text : "";
        if (isRaw) {
          return { ...result, content: [{ type: "text", text }], details: { raw: true } };
        }
        const decorated = withFooter(text, { always: true });
        return {
          ...result,
          content: [{ type: "text", text: decorated.text }],
          details: { ...(result.details ?? {}), compression: decorated.stats },
        };
      } catch (error) {
        if (error instanceof Error) {
          if (isRaw) throw error;
          const decorated = withFooter(error.message, { always: true });
          throw new Error(decorated.text);
        }
        throw error;
      }
    },
  });

  // ── ctx_read (replaces read) ──────────────────────────────────────────
  const nativeReadTool = createReadToolDefinition(process.cwd());

  registerTool({
    name: "ctx_read",
    label: "ctx_read",
    description:
      "Read a file. Prefer over native Read/cat/head/tail (cached, compressed). " +
      "Unchanged re-reads cost ~13 tokens. " +
      "Auto-selects mode: configs (.yaml/.json/.toml/.env) are always full-read. " +
      "Code files: full (<8KB), map (8-96KB), signatures (>96KB). " +
      "Add mode=full to get complete file content. " +
      "Use offset and limit to read specific line ranges.",
    promptSnippet: "Read file contents (always use instead of cat)",
    promptGuidelines: [
      "Use ctx_read to inspect file contents instead of cat or less.",
      "Use mode=full if you need the complete file content.",
    ],
    parameters: readSchema,
    renderCall(args, theme, context) {
      return nativeReadTool.renderCall
        ? nativeReadTool.renderCall(args, theme, context)
        : (context.lastComponent ?? new Text("", 0, 0));
    },
    renderResult(result, options, theme, context) {
      if (result.content.some((block) => block.type === "image")) {
        // Reuse Pi's read renderer for images; its detail type is ReadToolDetails.
        return nativeReadTool.renderResult
          ? nativeReadTool.renderResult(
              result as AgentToolResult<ReadToolDetails | undefined>,
              options,
              theme,
              context,
            )
          : (context.lastComponent ?? new Text("", 0, 0));
      }

      const textBlock = result.content.find((block) => block.type === "text");
      const rawText = textBlock?.type === "text" ? textBlock.text : "";
      const { body, footer } = splitFooter(rawText);
      const rawPath = typeof context.args?.path === "string" ? context.args.path : undefined;
      const lang = rawPath ? getLanguageFromPath(rawPath) : undefined;
      const renderedLines = lang ? highlightCode(replaceTabs(body), lang) : body.split("\n");
      const lines = trimTrailingEmpty(renderedLines);
      const maxLines = options.expanded ? lines.length : 10;
      const displayLines = lines.slice(0, maxLines);
      const remaining = lines.length - maxLines;

      let text = `\n${displayLines
        .map((line) => (lang ? replaceTabs(line) : theme.fg("toolOutput", replaceTabs(line))))
        .join("\n")}`;

      if (remaining > 0) {
        text += `${theme.fg("muted", `\n... (${remaining} more lines, ctrl+o to expand)`)}`;
      }

      const truncation = (result.details as Record<string, unknown> | undefined)?.truncation as
        | { truncated?: boolean; firstLineExceedsLimit?: boolean; truncatedBy?: string; outputLines?: number; totalLines?: number; maxLines?: number; maxBytes?: number }
        | undefined;
      if (truncation?.truncated) {
        if (truncation.firstLineExceedsLimit) {
          text += `\n${theme.fg("warning", `[First line exceeds ${Math.round((truncation.maxBytes ?? 8192) / 1024)}KB limit]`)}`;
        } else if (truncation.truncatedBy === "lines") {
          text += `\n${theme.fg("warning", `[Truncated: ${truncation.outputLines} of ${truncation.totalLines} lines]`)}`;
        } else {
          text += `\n${theme.fg("warning", `[Truncated: ${truncation.outputLines} lines (${Math.round((truncation.maxBytes ?? 8192) / 1024)}KB limit)]`)}`;
        }
      }

      if (footer) {
        text += `\n\n${theme.fg("muted", footer)}`;
      }

      // setText only exists on Text; lastComponent is the wider Component type.
      const component = context.lastComponent instanceof Text
        ? context.lastComponent
        : new Text("", 0, 0);
      component.setText(text);
      return component;
    },
    async execute(_toolCallId, params, signal, onUpdate, ctx) {
      const requestedPath = normalizePathArg(params.path);
      const absolutePath = resolve(ctx.cwd, requestedPath);

      if (params.offset !== undefined || params.limit !== undefined) {
        const startLine = params.offset ?? 1;
        const endLine = params.limit ? startLine + params.limit - 1 : 999999;
        const mode = `lines:${startLine}-${endLine}`;
        // Route line-range reads through the bridge too, so re-reading the same
        // slice hits the session cache instead of re-spawning a CLI per call (#361).
        if (mcpBridge?.isConnected()) {
          try {
            const bridged = await mcpBridge.callTool("ctx_read", { path: absolutePath, mode }, signal);
            const bridgedText = bridged.content.map((block) => block.text).join("");
            const originalSlice = await readSlice(absolutePath, params.offset, params.limit);
            const decorated = withFooter(bridgedText, { originalText: originalSlice.text, always: true, preferEstimate: true, suppressIfNoSaving: true });
            return {
              content: [{ type: "text", text: decorated.text }],
              details: { path: absolutePath, lines: originalSlice.lines, source: "lean-ctx-bridge", mode, compression: decorated.stats },
            };
          } catch (err) {
            console.error(`[pi-lean-ctx] ctx_read(${mode}) bridge call failed, falling back to CLI: ${err}`);
          }
        }
        const args = ["read", absolutePath, "-m", mode];
        try {
          const output = await execLeanCtx(pi, args);
          const originalSlice = await readSlice(absolutePath, params.offset, params.limit);
          const decorated = withFooter(output, { originalText: originalSlice.text, always: true, preferEstimate: true, suppressIfNoSaving: true });
          return {
            content: [{ type: "text", text: decorated.text }],
            details: { path: absolutePath, lines: originalSlice.lines, source: "lean-ctx", mode, compression: decorated.stats },
          };
        } catch {
          const sliced = await readSlice(absolutePath, params.offset, params.limit);
          return {
            content: [{ type: "text", text: sliced.text }],
            details: { path: absolutePath, lines: sliced.lines, source: "local-slice-fallback", truncated: sliced.truncated },
          };
        }
      }

      if (IMAGE_EXTENSIONS.has(extname(absolutePath).toLowerCase())) {
        return nativeReadTool.execute(_toolCallId, { ...params, path: absolutePath }, signal, onUpdate, ctx);
      }

      const isExplicitFull = params.mode === "full";
      const mode = params.mode ?? await chooseReadMode(absolutePath);

      // When the embedded MCP bridge is connected, route the read through it so
      // the persistent session cache engages: an unchanged re-read then costs
      // ~13 tokens instead of the full file, and the read registers as a real
      // CEP session (counted by `lean-ctx gain`). The one-shot CLI path below
      // spawns a fresh `lean-ctx read` per call and therefore cannot cache
      // across calls — it is used only as a fallback when the bridge is
      // unavailable or errors.
      if (mcpBridge?.isConnected()) {
        try {
          const bridged = await mcpBridge.callTool(
            "ctx_read",
            { path: absolutePath, mode, ...(isExplicitFull ? { fresh: true } : {}) },
            signal,
          );
          const bridgedText = bridged.content.map((block) => block.text).join("");
          const originalText = await readFile(absolutePath, "utf8");
          const decorated = withFooter(bridgedText, { originalText, always: true, preferEstimate: true, suppressIfNoSaving: true });
          return {
            content: [{ type: "text", text: decorated.text }],
            details: { path: absolutePath, source: "lean-ctx-bridge", mode, compression: decorated.stats },
          };
        } catch (err) {
          console.error(`[pi-lean-ctx] ctx_read bridge call failed, falling back to CLI: ${err}`);
        }
      }

      const args = ["read", absolutePath, "-m", mode, ...(isExplicitFull ? ["--fresh"] : [])];
      const output = await execLeanCtx(pi, args);
      const originalText = await readFile(absolutePath, "utf8");
      const decorated = withFooter(output, { originalText, always: true, preferEstimate: true, suppressIfNoSaving: true });

      return {
        content: [{ type: "text", text: decorated.text }],
        details: { path: absolutePath, source: "lean-ctx", mode, compression: decorated.stats },
      };
    },
  });

  // Native tool definitions are reused purely for their renderCall: Pi then
  // shows the invocation with its arguments ("grep /pattern/ in dir") instead
  // of a bare tool name, making the searched directory visible at a glance
  // (#395). The args shapes are supersets of the native ones.
  const nativeLsTool = createLsToolDefinition(process.cwd());
  const nativeFindTool = createFindToolDefinition(process.cwd());
  const nativeGrepTool = createGrepToolDefinition(process.cwd());

  // ── ctx_ls (replaces ls) ──────────────────────────────────────────────
  registerTool({
    name: "ctx_ls",
    label: "ctx_ls",
    description: "List a directory. Prefer over native ls (compact, summarized). `path` is required — pass the directory you are working in. Use limit to reduce output size.",
    promptSnippet: "List directory contents",
    parameters: lsSchema,
    renderCall(args, theme, context) {
      return nativeLsTool.renderCall
        ? nativeLsTool.renderCall(args, theme, context)
        : (context.lastComponent ?? new Text("", 0, 0));
    },
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const requestedPath = normalizePathArg(params.path);
      const absolutePath = resolve(ctx.cwd, requestedPath);
      const output = await execLeanCtx(pi, ["ls", absolutePath]);
      const decorated = withFooter(output, { limit: params.limit, always: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { path: absolutePath, source: "lean-ctx", truncated: decorated.truncated, compression: decorated.stats },
      };
    },
  });

  // ── ctx_find (replaces find) ──────────────────────────────────────────
  registerTool({
    name: "ctx_find",
    label: "ctx_find",
    description: "Find files by glob. Prefer over native find/fd (gitignore-aware). `path` is required — pass the directory you are working in. Use limit to reduce output size.",
    promptSnippet: "Find files by glob pattern",
    parameters: findSchema,
    renderCall(args, theme, context) {
      return nativeFindTool.renderCall
        ? nativeFindTool.renderCall(args, theme, context)
        : (context.lastComponent ?? new Text("", 0, 0));
    },
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const requestedPath = normalizePathArg(params.path);
      const absolutePath = resolve(ctx.cwd, requestedPath);
      const output = await execLeanCtx(pi, ["find", params.pattern, absolutePath]);
      const decorated = withFooter(output, { limit: params.limit, always: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { path: absolutePath, pattern: params.pattern, source: "lean-ctx", truncated: decorated.truncated, compression: decorated.stats },
      };
    },
  });

  // ── ctx_grep (replaces grep) ──────────────────────────────────────────
  registerTool({
    name: "ctx_grep",
    label: "ctx_grep",
    description: "Search code. Prefer over native Grep/ripgrep (compact, ranked). `path` is required — pass the directory you are working in. Use limit to cap matches, context for surrounding lines.",
    promptSnippet: "Search file contents for patterns",
    parameters: grepSchema,
    renderCall(args, theme, context) {
      return nativeGrepTool.renderCall
        ? nativeGrepTool.renderCall(args, theme, context)
        : (context.lastComponent ?? new Text("", 0, 0));
    },
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const requestedPath = normalizePathArg(params.path);
      const absolutePath = resolve(ctx.cwd, requestedPath);
      const searchArgs = ["rg", "--line-number", "--color=never"];
      if (params.ignoreCase) searchArgs.push("-i");
      if (params.literal) searchArgs.push("-F");
      if (params.context && params.context > 0) searchArgs.push(`-C${params.context}`);
      if (params.glob) searchArgs.push("--glob", params.glob);
      if (params.limit && params.limit > 0) searchArgs.push("-m", String(params.limit));
      searchArgs.push(params.pattern, absolutePath);

      const bin = resolveBinary();
      const result = await pi.exec(bin, ["-c", ...searchArgs]);
      if (result.code >= 2) {
        const msg = (result.stderr || result.stdout || `lean-ctx grep failed: ${params.pattern}`).trim();
        throw new Error(msg);
      }
      const output = result.code === 1 ? "(no matches)" : result.stdout;
      const decorated = withFooter(output, { always: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { path: absolutePath, pattern: params.pattern, source: "lean-ctx", compression: decorated.stats },
      };
    },
  });

  // ── ctx_multi_read (batch read) ─────────────────────────────────────────
  registerTool({
    name: "ctx_multi_read",
    label: "ctx_multi_read",
    description:
      "Batch read multiple files in one call. Prefer over multiple ctx_read calls (single session cache hit). " +
      "Same modes as ctx_read. Use fresh=true to bypass cache when running as a subagent.",
    promptSnippet: "Read multiple files at once",
    promptGuidelines: [
      "Use ctx_multi_read when you need to read several files at once — it's more token-efficient than multiple ctx_read calls.",
      "Use fresh=true if you're a subagent that may not have the parent session's cache.",
    ],
    parameters: multiReadSchema,
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const paths = params.paths?.map((p: string) => normalizePathArg(p)) ?? [];
      if (paths.length === 0) {
        throw new Error("ctx_multi_read: paths array is required and must not be empty");
      }
      const absolutePaths = paths.map((p: string) => resolve(ctx.cwd, p));

      const mode = params.mode ?? "auto";
      const isFresh = !!params.fresh;

      // When the embedded MCP bridge is connected, route through it so the
      // persistent session cache engages: unchanged re-reads cost ~13 tokens
      // and register as real CEP sessions (counted by `lean-ctx gain`).
      if (mcpBridge?.isConnected()) {
        try {
          const bridged = await mcpBridge.callTool(
            "ctx_multi_read",
            { paths: absolutePaths, mode, ...(isFresh ? { fresh: true } : {}) },
            signal,
          );
          const bridgedText = bridged.content.map((block) => block.text).join("");
          // Estimate original tokens from the raw files for the footer
          let originalText = "";
          for (const ap of absolutePaths) {
            try {
              originalText += await readFile(ap, "utf8");
            } catch {
              // ignore unreadable files
            }
          }
          const decorated = withFooter(bridgedText, { originalText, always: true, preferEstimate: true, suppressIfNoSaving: true });
          return {
            content: [{ type: "text", text: decorated.text }],
            details: { paths: absolutePaths, source: "lean-ctx-bridge", mode, compression: decorated.stats },
          };
        } catch (err) {
          console.error(`[pi-lean-ctx] ctx_multi_read bridge call failed, falling back to CLI: ${err}`);
        }
      }

      const args = ["read", ...absolutePaths, "-m", mode, ...(isFresh ? ["--fresh"] : [])];
      const output = await execLeanCtx(pi, args);
      let originalText = "";
      for (const ap of absolutePaths) {
        try {
          originalText += await readFile(ap, "utf8");
        } catch {
          // ignore unreadable files
        }
      }
      const decorated = withFooter(output, { originalText, always: true, preferEstimate: true, suppressIfNoSaving: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { paths: absolutePaths, source: "lean-ctx", mode, compression: decorated.stats },
      };
    },
  });

  // ── ctx_search (regex code search) ──────────────────────────────────────
  registerTool({
    name: "ctx_search",
    label: "ctx_search",
    description:
      "Regex code search. Prefer over native Grep/rg/find (compact, .gitignore-aware). " +
      "Use include for glob filtering (e.g. '*.ts'). Use max_results to cap output.",
    promptSnippet: "Search code with regex",
    promptGuidelines: [
      "Use ctx_search for code search instead of grep/ripgrep — it respects .gitignore and returns compressed results.",
      "Use include to filter by glob (e.g., '*.rs', 'src/**/*.ts').",
      "Use max_results to limit output size for large codebases.",
    ],
    parameters: searchSchema,
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      // Resolve search roots: path (single) or paths (multiple); fallback to cwd
      const roots: string[] = [];
      if (params.paths && params.paths.length > 0) {
        roots.push(...params.paths);
      } else if (params.path) {
        roots.push(params.path);
      } else {
        roots.push(ctx.cwd);
      }
      const absoluteRoots = roots.map((p: string) => resolve(ctx.cwd, normalizePathArg(p)));

      const searchArgs = ["rg", "--line-number", "--color=never"];
      if (params.ignore_gitignore) {
        // --no-ignore tells rg to also search gitignored files
        searchArgs.push("--no-ignore");
      }
      if (params.include) {
        searchArgs.push("--glob", params.include);
      }
      if (params.max_results && params.max_results > 0) {
        searchArgs.push("-m", String(params.max_results));
      }
      searchArgs.push(params.pattern, ...absoluteRoots);

      const bin = resolveBinary();
      const result = await pi.exec(bin, ["-c", ...searchArgs]);
      if (result.code >= 2) {
        const msg = (result.stderr || result.stdout || `lean-ctx search failed: ${params.pattern}`).trim();
        throw new Error(msg);
      }
      const output = result.code === 1 ? "(no matches)" : result.stdout;
      const decorated = withFooter(output, { always: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { paths: absoluteRoots, pattern: params.pattern, source: "lean-ctx", compression: decorated.stats },
      };
    },
  });

  // ── ctx_tree (directory tree) ───────────────────────────────────────────
  registerTool({
    name: "ctx_tree",
    label: "ctx_tree",
    description:
      "List a directory as a compact tree. Prefer over native ls/find (counts, compact tree). " +
      "`path` is required — pass the directory you are working in. " +
      "Use depth to control tree depth, show_hidden to include dotfiles, respect_gitignore to honor .gitignore.",
    promptSnippet: "List directory tree",
    promptGuidelines: [
      "Use ctx_tree for a visual directory tree instead of ls/find.",
      "Depth defaults to 3; increase for deeper trees, decrease for overview.",
      "Set respect_gitignore=false to include ignored files in the tree.",
    ],
    parameters: treeSchema,
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      // Resolve roots: path (single) or paths (multiple); fallback to cwd
      const roots: string[] = [];
      if (params.paths && params.paths.length > 0) {
        roots.push(...params.paths);
      } else if (params.path) {
        roots.push(params.path);
      } else {
        roots.push(ctx.cwd);
      }
      const absoluteRoots = roots.map((p: string) => resolve(ctx.cwd, normalizePathArg(p)));

      // lean-ctx ls supports --depth, --hidden, --no-ignore
      const outputs: string[] = [];
      for (const root of absoluteRoots) {
        const lsArgs = ["ls", root];
        if (params.depth) lsArgs.push("--depth", String(params.depth));
        if (params.show_hidden) lsArgs.push("--hidden");
        if (params.respect_gitignore === false) lsArgs.push("--no-ignore");
        const output = await execLeanCtx(pi, lsArgs);
        outputs.push(output);
      }
      const combined = outputs.join("\n\n");
      const decorated = withFooter(combined, { always: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { paths: absoluteRoots, source: "lean-ctx", depth: params.depth ?? 3, compression: decorated.stats },
      };
    },
  });

  // ── lean_ctx (CLI passthrough) ────────────────────────────────────────
  registerTool({
    name: "lean_ctx",
    label: "lean_ctx",
    description:
      "Run lean-ctx CLI directly (CLI-first; no MCP required). " +
      "Use this for advanced commands like session/knowledge/overview/gain/stats/index/pack.",
    promptSnippet: "Run lean-ctx CLI directly",
    parameters: leanCtxSchema,
    async execute(_toolCallId, params) {
      const output = await execLeanCtx(pi, params.args);
      return {
        content: [{ type: "text", text: output.trimEnd() }],
        details: { source: "lean-ctx", args: params.args },
      };
    },
  });

  const enableMcpBridge = PI_CONFIG.enableMcp;
  const adapterConfigured = isMcpAdapterConfigured();
  // An explicit opt-in to the embedded bridge wins over mcp.json detection (#361).
  // A `lean-ctx` entry in ~/.pi/agent/mcp.json does NOT prove that pi-mcp-adapter
  // is actually serving it — pi has no native MCP support, and `lean-ctx init
  // --agent pi` writes that entry by default — so it must not silently disable the
  // bridge a user explicitly requested via LEAN_CTX_PI_ENABLE_MCP=1 / enableMcp.
  mcpBridge = enableMcpBridge
    ? new McpBridge(resolveBinary(), PI_CONFIG.forwardedEnv, {
        disabledTools: PI_CONFIG.disabledTools,
        toolPrefix: PI_CONFIG.toolPrefix,
      })
    : null;

  if (mcpBridge) {
    pi.on("session_shutdown", async () => {
      await mcpBridge?.shutdown();
    });

    try {
      await mcpBridge.start(pi);
    } catch (err) {
      console.error(`[pi-lean-ctx] MCP bridge startup failed: ${err}`);
    }
  }

  pi.registerCommand("lean-ctx", {
    description: "Show lean-ctx status: binary path, MCP bridge, and registered tools",
    handler: async (_args, ctx) => {
      const bin = resolveBinary();
      const found = existsSync(bin);
      const status = mcpBridge?.getStatus();

      const lines: string[] = [];
      lines.push(found ? `Binary: ${bin}` : "Binary: NOT FOUND — install: cargo install lean-ctx");
      if (PI_CONFIG.loaded) {
        lines.push(`Config: ${PI_CONFIG.configPath}`);
      }
      lines.push(`Mode: ${PI_MODE}`);
      if (!enableMcpBridge) {
        lines.push("MCP bridge: disabled (CLI-first)");
        lines.push('  Enable: LEAN_CTX_PI_ENABLE_MCP=1 or "enableMcp": true in config.json, then restart Pi');
      } else if (status) {
        lines.push(`MCP bridge: ${status.mode} (${status.connected ? "connected" : "disconnected"})`);
        lines.push(`Reconnect attempts: ${status.reconnectAttempts}`);
        lines.push(`MCP tools: ${status.toolCount} registered`);
        if (status.toolNames.length > 0) {
          lines.push(`  ${status.toolNames.join(", ")}`);
        }
        if (adapterConfigured) {
          lines.push(
            "  Note: ~/.pi/agent/mcp.json also has a lean-ctx entry. The embedded bridge is serving tools; if you additionally run pi-mcp-adapter you may see duplicates.",
          );
        }
        if (status.lastHungTool) {
          lines.push(`Last hung tool: ${status.lastHungTool}`);
        }
        if (status.lastRetry) {
          lines.push(
            `Last retry: ${status.lastRetry.toolName} (${status.lastRetry.reason}) at ${status.lastRetry.timestamp}`,
          );
        }
        if (status.lastError) {
          lines.push(`Last bridge error: ${status.lastError}`);
        }
      }

      // Coexistence diagnostics (#359): the active prefix plus which tools we
      // handed off or skipped, so a user stacking AFT / magic-context can see
      // the exact split at a glance.
      const skipped = [...(status?.skippedTools ?? []), ...skippedExtensionTools];
      const disabled = [...(status?.disabledTools ?? []), ...disabledExtensionTools];
      const prefix = status?.toolPrefix ?? PI_CONFIG.toolPrefix;
      if (prefix) {
        lines.push(`Tool prefix: "${prefix}" (bridge tools exposed as ${prefix}<name>)`);
      }
      if (disabled.length > 0) {
        lines.push(`Disabled (handed to other extensions): ${disabled.join(", ")}`);
      }
      if (skipped.length > 0) {
        lines.push(`Skipped (name already taken): ${skipped.join(", ")}`);
      }

      // Show active ctx_ tools
      const ctxTools = pi.getActiveTools().filter((n) => n.startsWith("ctx_") || n === "lean_ctx");
      if (ctxTools.length > 0) {
        lines.push(`Active tools: ${ctxTools.join(", ")}`);
      }

      const ok = found && (adapterConfigured || !enableMcpBridge || (status?.connected ?? false));
      ctx.ui.notify(lines.join("\n"), ok ? "info" : "warning");
    },
  });
}
