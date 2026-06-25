import * as vscode from "vscode";
import { SidebarProvider } from "./sidebar/provider";
import { StatusBarManager } from "./statusbar";
import { registerCommands } from "./commands";
import { disposeOutputChannel, offerMcpSetup } from "./cli-commands";
import { isAvailable } from "./leanctx";
import { registerEditorSignal } from "./editor-signal";
import { registerUriHandler } from "./uri-handler";

let statusBar: StatusBarManager | undefined;

export async function activate(
  context: vscode.ExtensionContext
): Promise<void> {
  const available = await isAvailable();
  if (!available) {
    vscode.window.showWarningMessage(
      'lean-ctx binary not found. Install lean-ctx or set "leanctx.binaryPath" in settings.'
    );
  } else if (
    vscode.workspace
      .getConfiguration("leanctx")
      .get<boolean>("autoConfigureMcp", true)
  ) {
    // Binary present but MCP possibly unwired — offer a one-click setup (no-op
    // if already configured). Fire-and-forget so activation stays snappy.
    void offerMcpSetup();
  }

  const sidebarProvider = new SidebarProvider(context.extensionUri);

  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      SidebarProvider.viewType,
      sidebarProvider
    )
  );

  statusBar = new StatusBarManager();
  context.subscriptions.push({ dispose: () => statusBar?.dispose() });
  statusBar.start();

  registerCommands(context, sidebarProvider);
  registerUriHandler(context);
  registerEditorSignal(context);
}

export function deactivate(): void {
  statusBar?.dispose();
  statusBar = undefined;
  disposeOutputChannel();
}
