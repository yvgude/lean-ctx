import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import {
  createBashToolDefinition,
  createReadToolDefinition,
  DEFAULT_MAX_BYTES,
  DEFAULT_MAX_LINES,
  getLanguageFromPath,
  highlightCode,
  truncateHead,
} from "@mariozechner/pi-coding-agent";
import { Text } from "@mariozechner/pi-tui";
import { Type } from "@sinclair/typebox";
import { existsSync, readFileSync } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { extname, resolve } from "node:path";
import { homedir, platform } from "node:os";
import { McpBridge } from "./mcp-bridge.js";
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

const readSchema = Type.Object({
  path: Type.String({ description: "Path to the file to read (relative or absolute)" }),
  offset: Type.Optional(Type.Number({ description: "Line number to start reading from (1-indexed)" })),
  limit: Type.Optional(Type.Number({ description: "Maximum number of lines to read" })),
});

const lsSchema = Type.Object({
  path: Type.Optional(Type.String({ description: "Directory to list (default: current directory)" })),
  limit: Type.Optional(Type.Number({ description: "Maximum number of entries to return (default: 500)" })),
});

const findSchema = Type.Object({
  pattern: Type.String({ description: "Glob pattern to match files" }),
  path: Type.Optional(Type.String({ description: "Directory to search in (default: current directory)" })),
  limit: Type.Optional(Type.Number({ description: "Maximum number of results (default: 1000)" })),
});

const grepSchema = Type.Object({
  pattern: Type.String({ description: "Search pattern (regex or literal string)" }),
  path: Type.Optional(Type.String({ description: "Directory or file to search (default: current directory)" })),
  glob: Type.Optional(Type.String({ description: "Filter files by glob pattern, e.g. '*.ts'" })),
  ignoreCase: Type.Optional(Type.Boolean({ description: "Case-insensitive search (default: false)" })),
  literal: Type.Optional(Type.Boolean({ description: "Treat pattern as literal string (default: false)" })),
  context: Type.Optional(Type.Number({ description: "Lines of context around each match (default: 0)" })),
  limit: Type.Optional(Type.Number({ description: "Maximum number of matches (default: 100)" })),
});

function shellQuote(value: string): string {
  if (!value) return "''";
  if (/^[A-Za-z0-9_./=:@,+%^-]+$/.test(value)) return value;
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

function resolveBinary(): string {
  const envBin = process.env.LEAN_CTX_BIN;
  if (envBin && existsSync(envBin)) return envBin;

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
  const result = await pi.exec(bin, args, { env: { ...process.env, LEAN_CTX_COMPRESS: "1" } });
  if (result.code !== 0) {
    const msg = (result.stderr || result.stdout || `lean-ctx failed: ${args.join(" ")}`).trim();
    throw new Error(msg);
  }
  return result.stdout;
}

export default async function (pi: ExtensionAPI) {
  const baseBashTool = createBashToolDefinition(process.cwd(), {
    spawnHook: ({ command, cwd, env }) => {
      const bin = resolveBinary();
      return {
        command: `${shellQuote(bin)} -c sh -lc ${shellQuote(command)}`,
        cwd,
        env: { ...env, LEAN_CTX_COMPRESS: "1" },
      };
    },
  });

  const rawBash = createBashToolDefinition(process.cwd());

  const bashSchemaWithRaw = Type.Object({
    command: Type.String({ description: "Bash command to execute" }),
    timeout: Type.Optional(Type.Number({ description: "Timeout in seconds to prevent hanging commands" })),
    raw: Type.Optional(Type.Boolean({ description: "Skip compression, return full uncompressed output" })),
  });

  pi.registerTool({
    ...baseBashTool,
    parameters: bashSchemaWithRaw,
    description:
      "Execute a bash command. Output is auto-compressed by lean-ctx. "
      + "IMPORTANT: Do NOT use bash to read files (cat/head/tail) — use the read tool instead. "
      + "Do NOT use bash for grep/find/ls — use the dedicated tools. "
      + "Set raw=true to skip compression when exact output matters. "
      + "Use timeout (seconds) to prevent hanging commands.",
    promptSnippet: "Run shell commands (not for file reading — use read tool)",
    promptGuidelines: [
      "Use bash only for commands with side effects: build, test, install, git, run scripts.",
    ],
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

  const nativeReadTool = createReadToolDefinition(process.cwd());

  pi.registerTool({
    name: "read",
    label: "Read",
    description:
      "Read file contents. ALWAYS use this instead of cat/head/tail via bash. "
      + "Auto-selects mode: configs (.yaml/.json/.toml/.env) are always full-read. "
      + "Code files: full (<8KB), map (8-96KB), signatures (>96KB). "
      + "Use offset and limit to read specific line ranges.",
    promptSnippet: "Read file contents (always use instead of cat)",
    promptGuidelines: [
      "Use read to inspect file contents instead of cat or less.",
    ],
    parameters: readSchema,
    renderCall(args, theme, context) {
      return nativeReadTool.renderCall
        ? nativeReadTool.renderCall(args, theme, context)
        : (context.lastComponent ?? new Text("", 0, 0));
    },
    renderResult(result, options, theme, context) {
      if (result.content.some((block) => block.type === "image")) {
        return nativeReadTool.renderResult
          ? nativeReadTool.renderResult(result, options, theme, context)
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
          text += `\n${theme.fg("warning", `[First line exceeds ${Math.round((truncation.maxBytes ?? DEFAULT_MAX_BYTES) / 1024)}KB limit]`)}`;
        } else if (truncation.truncatedBy === "lines") {
          text += `\n${theme.fg("warning", `[Truncated: ${truncation.outputLines} of ${truncation.totalLines} lines]`)}`;
        } else {
          text += `\n${theme.fg("warning", `[Truncated: ${truncation.outputLines} lines (${Math.round((truncation.maxBytes ?? DEFAULT_MAX_BYTES) / 1024)}KB limit)]`)}`;
        }
      }

      if (footer) {
        text += `\n\n${theme.fg("muted", footer)}`;
      }

      const component = context.lastComponent ?? new Text("", 0, 0);
      component.setText(text);
      return component;
    },
    async execute(_toolCallId, params, signal, onUpdate, ctx) {
      const requestedPath = normalizePathArg(params.path);
      const absolutePath = resolve(ctx.cwd, requestedPath);

      if (params.offset !== undefined || params.limit !== undefined) {
        const startLine = params.offset ?? 1;
        const endLine = params.limit ? startLine + params.limit - 1 : 999999;
        const args = ["read", absolutePath, "-m", `lines:${startLine}-${endLine}`];
        try {
          const output = await execLeanCtx(pi, args);
          const originalSlice = await readSlice(absolutePath, params.offset, params.limit);
          const decorated = withFooter(output, { originalText: originalSlice.text, always: true, preferEstimate: true });
          return {
            content: [{ type: "text", text: decorated.text }],
            details: { path: absolutePath, lines: originalSlice.lines, source: "lean-ctx", mode: `lines:${startLine}-${endLine}`, compression: decorated.stats },
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

      const mode = await chooseReadMode(absolutePath);
      const args = mode === "full" ? ["read", absolutePath] : ["read", absolutePath, "-m", mode];
      const output = await execLeanCtx(pi, args);
      const originalText = await readFile(absolutePath, "utf8");
      const decorated = withFooter(output, { originalText, always: true, preferEstimate: true });

      return {
        content: [{ type: "text", text: decorated.text }],
        details: { path: absolutePath, source: "lean-ctx", mode, compression: decorated.stats },
      };
    },
  });

  pi.registerTool({
    name: "ls",
    label: "ls",
    description: "List directory contents. Use limit to reduce output size.",
    promptSnippet: "List directory contents",
    parameters: lsSchema,
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const requestedPath = normalizePathArg(params.path || ".");
      const absolutePath = resolve(ctx.cwd, requestedPath);
      const output = await execLeanCtx(pi, ["ls", absolutePath]);
      const decorated = withFooter(output, { limit: params.limit, always: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { path: absolutePath, source: "lean-ctx", truncated: decorated.truncated, compression: decorated.stats },
      };
    },
  });

  pi.registerTool({
    name: "find",
    label: "find",
    description: "Find files by glob pattern (respects .gitignore). Use limit to reduce output size.",
    promptSnippet: "Find files by glob pattern",
    parameters: findSchema,
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const requestedPath = normalizePathArg(params.path || ".");
      const absolutePath = resolve(ctx.cwd, requestedPath);
      const output = await execLeanCtx(pi, ["find", params.pattern, absolutePath]);
      const decorated = withFooter(output, { limit: params.limit, always: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { path: absolutePath, pattern: params.pattern, source: "lean-ctx", truncated: decorated.truncated, compression: decorated.stats },
      };
    },
  });

  pi.registerTool({
    name: "grep",
    label: "grep",
    description: "Search file contents with ripgrep. Use limit to cap matches and context for surrounding lines.",
    promptSnippet: "Search file contents for patterns",
    parameters: grepSchema,
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const requestedPath = normalizePathArg(params.path || ".");
      const absolutePath = resolve(ctx.cwd, requestedPath);
      const searchArgs = ["rg", "--line-number", "--color=never"];
      if (params.ignoreCase) searchArgs.push("-i");
      if (params.literal) searchArgs.push("-F");
      if (params.context && params.context > 0) searchArgs.push(`-C${params.context}`);
      if (params.glob) searchArgs.push("--glob", params.glob);
      if (params.limit && params.limit > 0) searchArgs.push("-m", String(params.limit));
      searchArgs.push(params.pattern, absolutePath);

      const output = await execLeanCtx(pi, ["-c", ...searchArgs]);
      const decorated = withFooter(output, { always: true });
      return {
        content: [{ type: "text", text: decorated.text }],
        details: { path: absolutePath, pattern: params.pattern, source: "lean-ctx", compression: decorated.stats },
      };
    },
  });

  const mcpBridge = new McpBridge(resolveBinary());

  if (!isMcpAdapterConfigured()) {
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
      const status = mcpBridge.getStatus();

      const lines: string[] = [];
      lines.push(found ? `Binary: ${bin}` : "Binary: NOT FOUND — install: cargo install lean-ctx");
      lines.push(`MCP bridge: ${status.mode} (${status.connected ? "connected" : "disconnected"})`);
      lines.push(`Reconnect attempts: ${status.reconnectAttempts}`);
      lines.push(`MCP tools: ${status.toolCount} registered`);
      if (status.toolNames.length > 0) {
        lines.push(`  ${status.toolNames.join(", ")}`);
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

      ctx.ui.notify(lines.join("\n"), found && status.connected ? "info" : "warning");
    },
  });
}
