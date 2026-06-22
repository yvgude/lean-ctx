import * as vscode from "vscode";
import * as fs from "fs";
import * as path from "path";
import { runLeanCtxCapture, resolveBinaryPath } from "./leanctx";

let outputChannel: vscode.OutputChannel | undefined;

function channel(): vscode.OutputChannel {
  if (!outputChannel) {
    outputChannel = vscode.window.createOutputChannel("lean-ctx");
  }
  return outputChannel;
}

/** Runs a finite, informational lean-ctx command and streams its output to the
 *  shared "lean-ctx" output channel. A non-zero exit (e.g. `doctor` with
 *  findings) is shown verbatim, not treated as a hard failure. */
async function runInChannel(title: string, args: string[]): Promise<void> {
  const ch = channel();
  ch.show(true);
  ch.appendLine(`\n━━━ ${title} ━━━`);
  ch.appendLine(`> lean-ctx ${args.join(" ")}\n`);

  await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Window, title: `lean-ctx: ${title}…` },
    async () => {
      const { stdout, stderr, code } = await runLeanCtxCapture(args);
      if (stdout) {
        ch.appendLine(stdout);
      }
      if (stderr) {
        ch.appendLine(stderr);
      }
      if (code !== 0 && code !== null) {
        ch.appendLine(`\n(exit code ${code})`);
      }
    }
  );
}

export function cmdSetup(): Promise<void> {
  return runInChannel("Setup", ["setup"]);
}

export function cmdDoctor(): Promise<void> {
  return runInChannel("Doctor", ["doctor"]);
}

export function cmdGain(): Promise<void> {
  return runInChannel("Token Savings", ["gain"]);
}

export function cmdHeatmap(): Promise<void> {
  return runInChannel("Context Heatmap", ["heatmap"]);
}

interface McpConfig {
  // VS Code / Windsurf use `servers`; Cursor uses `mcpServers`.
  servers?: Record<string, unknown>;
  mcpServers?: Record<string, unknown>;
}

/** True if any known MCP config (workspace `.vscode/mcp.json` or Cursor's
 *  `~/.cursor/mcp.json`) already registers a `lean-ctx` server. */
function isMcpConfigured(): boolean {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  const home = process.env.HOME ?? process.env.USERPROFILE ?? "";
  const candidates = [
    root ? path.join(root, ".vscode", "mcp.json") : "",
    home ? path.join(home, ".cursor", "mcp.json") : "",
  ].filter(Boolean);

  for (const configPath of candidates) {
    try {
      if (!fs.existsSync(configPath)) {
        continue;
      }
      const cfg = JSON.parse(fs.readFileSync(configPath, "utf-8")) as McpConfig;
      if (cfg.servers?.["lean-ctx"] || cfg.mcpServers?.["lean-ctx"]) {
        return true;
      }
    } catch {
      continue;
    }
  }
  return false;
}

/** On activation, gently offer to wire MCP for this workspace if it isn't
 *  already — the first-run nudge a manual installer needs. No-op when already
 *  configured. */
export async function offerMcpSetup(): Promise<void> {
  if (isMcpConfigured()) {
    return;
  }
  const action = await vscode.window.showInformationMessage(
    "lean-ctx: MCP isn't configured for this workspace yet. Configure now?",
    "Configure",
    "Later"
  );
  if (action === "Configure") {
    await cmdConfigureMcp();
  }
}

/** Writes a workspace-local `.vscode/mcp.json` entry for lean-ctx (stdio), using
 *  the resolved binary path so the editor's MCP launcher can find it even under
 *  a stripped GUI PATH. Existing entries/servers are preserved. */
export async function cmdConfigureMcp(): Promise<void> {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!root) {
    vscode.window.showErrorMessage("lean-ctx: open a workspace folder first.");
    return;
  }

  const configPath = path.join(root, ".vscode", "mcp.json");
  try {
    fs.mkdirSync(path.dirname(configPath), { recursive: true });

    let config: McpConfig = { servers: {} };
    // Read-and-handle rather than exists-then-read: a separate existence check
    // would be a TOCTOU race (the file can change between the check and the use).
    try {
      config = JSON.parse(fs.readFileSync(configPath, "utf-8")) as McpConfig;
    } catch (readErr: unknown) {
      const code = (readErr as NodeJS.ErrnoException)?.code;
      if (code !== "ENOENT") {
        // File exists but is unreadable/invalid JSON — preserve it as a .bak
        // instead of clobbering blind (best-effort; ignore backup failure).
        try {
          fs.copyFileSync(configPath, `${configPath}.bak`);
        } catch {
          /* backup is best-effort */
        }
      }
      // ENOENT (no config yet) → keep the default empty config.
    }
    if (!config.servers) {
      config.servers = {};
    }
    config.servers["lean-ctx"] = { type: "stdio", command: resolveBinaryPath() };

    fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
    vscode.window.showInformationMessage(
      `lean-ctx: MCP configured in ${path.relative(root, configPath)}`
    );
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`lean-ctx: could not write mcp.json — ${message}`);
  }
}

export function disposeOutputChannel(): void {
  outputChannel?.dispose();
  outputChannel = undefined;
}
