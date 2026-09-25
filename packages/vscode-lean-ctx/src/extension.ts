// One quiet status bar item: what lean-ctx did in this project.
// Numbers come from `lean-ctx prompt-segment --json`; nothing is computed here.

import * as fs from "fs";
import * as vscode from "vscode";
import { resolveBinary, run } from "./binary";
import { parsePayload, statusText, tooltipMarkdown } from "./value";

/** Also re-checks staleness: the binary drops numbers older than 12 h. */
const FALLBACK_REFRESH_MS = 60_000;
const THROTTLE_MS = 1_500;

export function activate(context: vscode.ExtensionContext): void {
  const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  item.name = "lean-ctx";
  item.command = "leanctx.showProof";
  context.subscriptions.push(item);

  let watcher: fs.FSWatcher | undefined;
  let watchedDir: string | null = null;
  let pending: NodeJS.Timeout | undefined;
  let inFlight = false;
  let again = false;

  const config = () => vscode.workspace.getConfiguration("leanctx");
  const binary = () => resolveBinary(config().get<string>("binaryPath", ""));

  const projectDir = (): string | undefined => {
    const uri = vscode.window.activeTextEditor?.document.uri;
    const folder = (uri && vscode.workspace.getWorkspaceFolder(uri)) ?? vscode.workspace.workspaceFolders?.[0];
    return folder?.uri.scheme === "file" ? folder.uri.fsPath : undefined;
  };

  const watch = (dir: string | null) => {
    if (dir === watchedDir) return;
    watcher?.close();
    watcher = undefined;
    watchedDir = dir;
    if (!dir) return;
    try {
      watcher = fs.watch(dir, () => schedule());
      watcher.on("error", () => watch(null));
    } catch {
      // The directory appears with the first measured tool call; the interval covers it.
      watchedDir = null;
    }
  };

  const refresh = async () => {
    if (inFlight) {
      again = true;
      return;
    }
    inFlight = true;
    try {
      const bin = binary();
      const dir = projectDir();
      if (!config().get<boolean>("statusBar.enabled", true) || !bin || !dir) {
        item.hide();
        return;
      }
      const payload = parsePayload((await run(bin, ["prompt-segment", "--json", "--dir", dir])) ?? "");
      watch(payload?.watch ?? null);
      const text = payload && statusText(payload);
      if (!payload || !text) {
        item.hide();
        return;
      }
      item.text = text;
      const tip = new vscode.MarkdownString(tooltipMarkdown(payload));
      tip.isTrusted = false;
      item.tooltip = tip;
      item.accessibilityInformation = { label: `lean-ctx: ${payload.tooltip.join(", ")}` };
      item.show();
    } finally {
      inFlight = false;
      if (again) {
        again = false;
        schedule();
      }
    }
  };

  // A throttle, not a debounce: during a busy session the snapshot changes
  // every second, and a debounce would never fire.
  function schedule() {
    if (pending) return;
    pending = setTimeout(() => {
      pending = undefined;
      void refresh();
    }, THROTTLE_MS);
  }

  const inTerminal = (name: string, args: string[]) => {
    const bin = binary();
    if (!bin) {
      void vscode.window.showWarningMessage(
        "lean-ctx was not found. Install it, or set `leanctx.binaryPath`.",
      );
      return;
    }
    vscode.window.createTerminal({ name, shellPath: bin, shellArgs: args }).show();
  };

  const interval = setInterval(() => void refresh(), FALLBACK_REFRESH_MS);
  context.subscriptions.push(
    vscode.commands.registerCommand("leanctx.showProof", () => inTerminal("lean-ctx proof", ["value"])),
    vscode.commands.registerCommand("leanctx.openDashboard", () => inTerminal("lean-ctx dashboard", ["dashboard"])),
    vscode.commands.registerCommand("leanctx.refresh", () => void refresh()),
    vscode.window.onDidChangeActiveTextEditor(schedule),
    vscode.workspace.onDidChangeWorkspaceFolders(schedule),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("leanctx")) schedule();
    }),
    {
      dispose: () => {
        clearInterval(interval);
        if (pending) clearTimeout(pending);
        watcher?.close();
      },
    },
  );
  void refresh();
}

export function deactivate(): void {}
