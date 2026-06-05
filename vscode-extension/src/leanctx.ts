import { spawn, execFileSync } from "child_process";
import * as vscode from "vscode";

export interface KnowledgeFact {
  category: string;
  content: string;
  timestamp?: string;
}

export interface SessionStats {
  totalReads: number;
  totalSearches: number;
  totalShells: number;
  tokensSaved: number;
  sessionDuration: string;
  filesTouched: number;
}

export interface RepoMapEntry {
  path: string;
  rank: number;
  symbols: string[];
}

export interface SearchResult {
  file: string;
  line: number;
  content: string;
  score?: number;
}

let cachedBinaryPath: string | null = null;

/**
 * Resolves the lean-ctx binary. An explicit `leanctx.binaryPath` setting always wins.
 * Otherwise we probe the common install locations, because GUI-launched editors
 * (VS Code, Cursor, VSCodium) frequently inherit a stripped PATH that omits
 * `~/.cargo/bin` and Homebrew — the usual reason "lean-ctx not found" despite a
 * working terminal. The first responsive candidate is cached for the session.
 */
function getBinaryPath(): string {
  const inspected = vscode.workspace
    .getConfiguration("leanctx")
    .inspect<string>("binaryPath");
  const explicit =
    inspected?.workspaceFolderValue ??
    inspected?.workspaceValue ??
    inspected?.globalValue;
  if (explicit && explicit.trim()) {
    return explicit.trim();
  }

  if (cachedBinaryPath) {
    return cachedBinaryPath;
  }

  const home = process.env.HOME ?? process.env.USERPROFILE ?? "";
  const candidates = [
    "lean-ctx",
    home ? `${home}/.cargo/bin/lean-ctx` : "",
    "/opt/homebrew/bin/lean-ctx",
    "/usr/local/bin/lean-ctx",
  ].filter(Boolean);

  for (const candidate of candidates) {
    try {
      execFileSync(candidate, ["--version"], { timeout: 5_000, stdio: "pipe" });
      cachedBinaryPath = candidate;
      return candidate;
    } catch {
      continue;
    }
  }

  // Nothing responded — fall back to the bare name so the caller surfaces a
  // clear "not found" error rather than silently doing nothing.
  return "lean-ctx";
}

export function runLeanCtx(
  args: string[],
  cwd?: string
): Promise<string> {
  return new Promise((resolve, reject) => {
    const bin = getBinaryPath();
    const workspaceCwd =
      cwd ?? vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;

    const proc = spawn(bin, args, {
      cwd: workspaceCwd,
      env: { ...process.env, NO_COLOR: "1" },
      timeout: 30_000,
    });

    let stdout = "";
    let stderr = "";

    proc.stdout.on("data", (data: Buffer) => {
      stdout += data.toString();
    });

    proc.stderr.on("data", (data: Buffer) => {
      stderr += data.toString();
    });

    proc.on("error", (err: Error) => {
      reject(new Error(`Failed to run ${bin}: ${err.message}`));
    });

    proc.on("close", (code: number | null) => {
      if (code === 0) {
        resolve(stdout.trim());
      } else {
        reject(
          new Error(
            `${bin} exited with code ${code}: ${stderr || stdout}`.trim()
          )
        );
      }
    });
  });
}

/** Exposes the resolved binary path (incl. auto-detection) to other modules,
 *  e.g. for writing an MCP `command` that the editor's launcher can find. */
export function resolveBinaryPath(): string {
  return getBinaryPath();
}

export interface CommandResult {
  stdout: string;
  stderr: string;
  code: number | null;
}

/**
 * Runs lean-ctx and resolves with the captured streams regardless of exit code.
 * Used for informational, output-channel commands (`setup`, `doctor`, `gain`,
 * `heatmap`) where a non-zero exit (e.g. `doctor` reporting findings) is still a
 * result worth showing verbatim rather than an error to swallow.
 */
export function runLeanCtxCapture(
  args: string[],
  cwd?: string
): Promise<CommandResult> {
  return new Promise((resolve) => {
    const bin = getBinaryPath();
    const workspaceCwd =
      cwd ?? vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;

    const proc = spawn(bin, args, {
      cwd: workspaceCwd,
      env: { ...process.env, NO_COLOR: "1" },
      timeout: 120_000,
    });

    let stdout = "";
    let stderr = "";
    proc.stdout.on("data", (data: Buffer) => (stdout += data.toString()));
    proc.stderr.on("data", (data: Buffer) => (stderr += data.toString()));
    proc.on("error", (err: Error) => {
      resolve({ stdout: "", stderr: `Failed to run ${bin}: ${err.message}`, code: null });
    });
    proc.on("close", (code: number | null) => {
      resolve({ stdout: stdout.trim(), stderr: stderr.trim(), code });
    });
  });
}

function formatSpan(fromIso?: string, toIso?: string): string {
  if (!fromIso || !toIso) return "—";
  const from = Date.parse(fromIso);
  const to = Date.parse(toIso);
  if (!Number.isFinite(from) || !Number.isFinite(to) || to < from) return "—";
  const minutes = Math.round((to - from) / 60_000);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

export async function getSessionStats(): Promise<SessionStats> {
  try {
    // `lean-ctx stats json` is the authoritative per-tool breakdown: a `commands`
    // map keyed by tool name plus lifetime input/output token totals. (The former
    // `metrics` subcommand never existed, so the previous call always threw.)
    const raw = await runLeanCtx(["stats", "json"]);
    const data = JSON.parse(raw);
    const commands = (data.commands ?? {}) as Record<string, { count?: number }>;
    const count = (tool: string): number => commands[tool]?.count ?? 0;

    const inputTokens: number = data.total_input_tokens ?? 0;
    const outputTokens: number = data.total_output_tokens ?? 0;

    return {
      totalReads: count("ctx_read"),
      totalSearches: count("ctx_search"),
      totalShells: count("ctx_shell"),
      tokensSaved: Math.max(0, inputTokens - outputTokens),
      sessionDuration: formatSpan(data.first_use, data.last_use),
      filesTouched: 0,
    };
  } catch {
    return {
      totalReads: 0,
      totalSearches: 0,
      totalShells: 0,
      tokensSaved: 0,
      sessionDuration: "—",
      filesTouched: 0,
    };
  }
}

export async function getKnowledge(): Promise<KnowledgeFact[]> {
  try {
    const raw = await runLeanCtx(["knowledge", "recall", "--json"]);
    const data = JSON.parse(raw);
    if (Array.isArray(data)) {
      return data.map((item: Record<string, string>) => ({
        category: item.category ?? "unknown",
        content: item.content ?? "",
        timestamp: item.timestamp,
      }));
    }
    return [];
  } catch {
    return [];
  }
}

export async function getRepoMap(): Promise<RepoMapEntry[]> {
  try {
    const raw = await runLeanCtx(["repomap", "--json"]);
    const data = JSON.parse(raw);
    if (Array.isArray(data)) {
      return data.map((item: Record<string, unknown>) => ({
        path: (item.path as string) ?? "",
        rank: (item.rank as number) ?? 0,
        symbols: Array.isArray(item.symbols)
          ? (item.symbols as string[])
          : [],
      }));
    }
    return [];
  } catch {
    return [];
  }
}

export async function semanticSearch(
  query: string
): Promise<SearchResult[]> {
  try {
    const raw = await runLeanCtx(["knowledge", "search", query]);

    // Header: "Cross-session search '...' (N results):" or "No results found..."
    if (!raw || !raw.includes("\n")) {
      return [];
    }

    const lines = raw.split("\n").slice(1);
    const results: SearchResult[] = [];

    for (const line of lines) {
      // Pattern: "  [cat/key] value text (project: path, conf: XX%)"
      const match = line.match(
        /^\s+\[([^\]]+)\]\s+(.+?)\s+\(project:\s*(.+?),\s*conf:\s*(\d+)%\)$/
      );
      if (match) {
        const [, categoryKey, content, project, confStr] = match;

        // Extract file + line from content patterns:
        //   "Read example.js (394L)"  → file=example.js, line=394
        //   "Found … in N files, example.js"         → file, line=0
        let file = project;
        let line = 0;

        const readMatch = content.match(/^Read\s+(\S+)\s+\((\d+)L\)$/);
        if (readMatch) {
          file = readMatch[1];
          line = parseInt(readMatch[2], 10);
        } else {
          const foundMatch = content.match(
            /(?:in \d+ files?,\s*)(\S+)\s*$/
          );
          if (foundMatch) {
            file = foundMatch[1].replace(/[,;]$/, "");
          }
        }

        results.push({
          file,
          line,
          content: `[${categoryKey}] ${content}`,
          score: parseInt(confStr, 10) / 100,
        });
      }
    }

    return results;
  } catch {
    return [];
  }
}

export async function isAvailable(): Promise<boolean> {
  try {
    await runLeanCtx(["--version"]);
    return true;
  } catch {
    return false;
  }
}

export async function openVisualizer(): Promise<void> {
  await runLeanCtx(["visualize", "--open"]);
}

export async function getVersion(): Promise<string> {
  try {
    const raw = await runLeanCtx(["--version"]);
    return raw.replace(/^lean-ctx\s*/i, "").trim();
  } catch {
    return "unknown";
  }
}
