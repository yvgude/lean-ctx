import * as vscode from "vscode";
import { openVisualizer, semanticSearch } from "./leanctx";
import {
  cmdSetup,
  cmdDoctor,
  cmdGain,
  cmdHeatmap,
  cmdConfigureMcp,
} from "./cli-commands";
import { cmdDashboard } from "./dashboard-panel";
import { SidebarProvider } from "./sidebar/provider";

export function registerCommands(
  context: vscode.ExtensionContext,
  sidebarProvider: SidebarProvider
): void {
  context.subscriptions.push(
    vscode.commands.registerCommand("leanctx.search", () =>
      handleSearch(sidebarProvider)
    ),
    vscode.commands.registerCommand("leanctx.repomap", () =>
      sidebarProvider.showTab("repomap")
    ),
    vscode.commands.registerCommand("leanctx.knowledge", () =>
      sidebarProvider.showTab("knowledge")
    ),
    vscode.commands.registerCommand("leanctx.visualize", handleVisualize),
    vscode.commands.registerCommand("leanctx.refresh", () =>
      sidebarProvider.refresh()
    ),
    // CLI-backed commands (setup, diagnostics, MCP wiring, web dashboard).
    vscode.commands.registerCommand("leanctx.setup", cmdSetup),
    vscode.commands.registerCommand("leanctx.doctor", cmdDoctor),
    vscode.commands.registerCommand("leanctx.gain", cmdGain),
    vscode.commands.registerCommand("leanctx.heatmap", cmdHeatmap),
    vscode.commands.registerCommand("leanctx.dashboard", () =>
      cmdDashboard(context)
    ),
    vscode.commands.registerCommand("leanctx.configureMcp", cmdConfigureMcp)
  );
}

async function handleSearch(sidebar: SidebarProvider): Promise<void> {
  const query = await vscode.window.showInputBox({
    prompt: "Semantic search query",
    placeHolder: "e.g. How does authentication work?",
  });

  if (!query) {
    return;
  }

  await sidebar.showTab("search");

  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: "lean-ctx: Searching…",
      cancellable: false,
    },
    async () => {
      const results = await semanticSearch(query);

      if (results.length === 0) {
        vscode.window.showInformationMessage(
          `lean-ctx: No results for "${query}"`
        );
        return;
      }

      const items = results.map((r) => ({
        label: r.file,
        description: `Line ${r.line}`,
        detail: r.content,
        filePath: r.file,
        line: r.line,
      }));

      const selected = await vscode.window.showQuickPick(items, {
        placeHolder: `${results.length} results for "${query}"`,
        matchOnDetail: true,
      });

      if (selected) {
        const uri = vscode.Uri.file(selected.filePath);
        const doc = await vscode.workspace.openTextDocument(uri);
        const line = Math.max(0, selected.line - 1);
        await vscode.window.showTextDocument(doc, {
          selection: new vscode.Range(line, 0, line, 0),
        });
      }
    }
  );
}

async function handleVisualize(): Promise<void> {
  try {
    await openVisualizer();
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`lean-ctx visualizer: ${message}`);
  }
}
