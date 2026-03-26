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
import { existsSync } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { dirname, extname, resolve } from "node:path";
import { homedir, platform } from "node:os";

const CODE_EXTENSIONS = new Set([
  ".rs", ".ts", ".tsx", ".js", ".jsx", ".php", ".py", ".go",
  ".java", ".c", ".cc", ".cpp", ".cxx", ".cs", ".kt", ".swift", ".rb",
]);

const FULL_READ_EXTENSIONS = new Set([
  ".md", ".txt", ".json", ".json5", ".yaml", ".yml", ".toml",
  ".env", ".ini", ".xml", ".lock",
]);

const IMAGE_EXTENSIONS = new Set([".png", ".jpg", ".jpeg", ".gif", ".webp"]);

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
  if (size >= 160 * 1024) return "signatures";
  if (size >= 24 * 1024) return "map";
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

type CompressionStats = {
  originalTokens: number;
  compressedTokens: number;
  percentSaved: number;
};

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

async function execLeanCtx(pi: ExtensionAPI, args: string[]) {
  const bin = resolveBinary();
  const result = await pi.exec(bin, args, {});
  if (result.code !== 0) {
    const msg = (result.stderr || result.stdout || `lean-ctx failed: ${args.join(" ")}`).trim();
    throw new Error(msg);
  }
  return result.stdout;
}

export default function (pi: ExtensionAPI) {
  const baseBashTool = createBashToolDefinition(process.cwd(), {
    spawnHook: ({ command, cwd, env }) => {
      const bin = resolveBinary();
      return {
        command: `${shellQuote(bin)} -c sh -lc ${shellQuote(command)}`,
        cwd,
        env: { ...env },
      };
    },
  });

  pi.registerTool({
    ...baseBashTool,
    description:
      "Execute a bash command through lean-ctx compression for 60-90% smaller output.",
    promptSnippet: "Run shell commands through lean-ctx compression.",
    promptGuidelines: [
      "Use bash normally — commands are automatically routed through lean-ctx.",
      "lean-ctx compresses verbose CLI output (git, cargo, npm, docker, kubectl, etc.) automatically.",
    ],
    async execute(toolCallId, params, signal, onUpdate, ctx) {
      try {
        const result = await baseBashTool.execute(toolCallId, params, signal, onUpdate, ctx);
        const text = result.content?.[0]?.type === "text" ? result.content[0].text : "";
        const decorated = withFooter(text, { always: true });
        return {
          ...result,
          content: [{ type: "text", text: decorated.text }],
          details: { ...(result.details ?? {}), compression: decorated.stats },
        };
      } catch (error) {
        if (error instanceof Error) {
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
      "Read file contents through lean-ctx with automatic mode selection (full/map/signatures) based on file type and size.",
    promptSnippet: "Read files through lean-ctx compression with smart mode selection.",
    promptGuidelines: [
      "Use read normally — lean-ctx automatically selects the optimal compression mode.",
      "Small files get full reads, large code files get map/signatures mode.",
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
        const sliced = await readSlice(absolutePath, params.offset, params.limit);
        return {
          content: [{ type: "text", text: sliced.text }],
          details: { path: absolutePath, lines: sliced.lines, source: "local-slice", truncated: sliced.truncated },
        };
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
    description: "List directory contents through lean-ctx compression.",
    promptSnippet: "List directory contents with token-optimized output.",
    promptGuidelines: ["Use ls normally — output is automatically compressed by lean-ctx."],
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
    description: "Find files by glob pattern through lean-ctx compression.",
    promptSnippet: "Find files with compressed output.",
    promptGuidelines: ["Use find normally — output respects .gitignore and is compressed by lean-ctx."],
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
    description: "Search file contents through ripgrep + lean-ctx compression.",
    promptSnippet: "Search code with compressed, grouped results.",
    promptGuidelines: ["Use grep normally — results are compressed and grouped by lean-ctx."],
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

  pi.registerCommand("lean-ctx", {
    description: "Show the lean-ctx binary currently used by the Pi integration",
    handler: async (_args, ctx) => {
      const bin = resolveBinary();
      const found = existsSync(bin);
      ctx.ui.notify(
        found ? `pi-lean-ctx using: ${bin}` : `lean-ctx not found. Install: cargo install lean-ctx`,
        found ? "info" : "warning",
      );
    },
  });
}
