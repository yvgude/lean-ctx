import * as vscode from "vscode";
import { getSessionStats, isAvailable } from "./leanctx";

export class StatusBarManager {
  private item: vscode.StatusBarItem;
  private refreshTimer?: NodeJS.Timeout;

  constructor() {
    this.item = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Right,
      100
    );
    this.item.command = "leanctx.sidebar.focus";
    this.item.tooltip = "lean-ctx — Click to open dashboard";
    this.item.text = "$(symbol-misc) lean-ctx";
    this.item.show();
  }

  public async start(): Promise<void> {
    const available = await isAvailable();
    if (!available) {
      this.item.text = "$(warning) lean-ctx: not found";
      this.item.tooltip =
        "lean-ctx binary not found. Install it or set leanctx.binaryPath.";
      return;
    }

    await this.update();
    this.startAutoRefresh();
  }

  public async update(): Promise<void> {
    try {
      const stats = await getSessionStats();
      const saved = this.formatTokens(stats.tokensSaved);
      this.item.text = `$(symbol-misc) lean-ctx: ${saved} saved`;
      this.item.tooltip = [
        `lean-ctx — Token Savings: ${saved}`,
        `Reads: ${stats.totalReads}`,
        `Searches: ${stats.totalSearches}`,
        `Shells: ${stats.totalShells}`,
        `Files: ${stats.filesTouched}`,
        `Read Cache: ${stats.cacheHits}/${stats.cacheReads}`,
        `Content Dedup: ${stats.dedupHits}/${stats.dedupReads} (${this.formatTokens(stats.dedupTokensSaved)} tokens saved)`,
        `Session: ${stats.sessionDuration}`,
      ].join("\n");
    } catch {
      this.item.text = "$(symbol-misc) lean-ctx";
    }
  }

  public dispose(): void {
    this.stopAutoRefresh();
    this.item.dispose();
  }

  private startAutoRefresh(): void {
    const intervalSec = vscode.workspace
      .getConfiguration("leanctx")
      .get<number>("refreshInterval", 30);

    this.refreshTimer = setInterval(() => {
      this.update();
    }, intervalSec * 1000);
  }

  private stopAutoRefresh(): void {
    if (this.refreshTimer) {
      clearInterval(this.refreshTimer);
      this.refreshTimer = undefined;
    }
  }

  private formatTokens(n: number): string {
    if (n >= 1_000_000) {
      return (n / 1_000_000).toFixed(1) + "M";
    }
    if (n >= 1_000) {
      return (n / 1_000).toFixed(1) + "K";
    }
    return String(n);
  }
}
