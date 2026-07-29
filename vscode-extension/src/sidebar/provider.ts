import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import {
  getSessionStats,
  getKnowledge,
  getRepoMap,
  semanticSearch,
  getVersion,
} from "../leanctx";

interface WebviewMessage {
  type: string;
  query?: string;
  tab?: string;
}

export class SidebarProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = "leanctx.sidebar";

  private view?: vscode.WebviewView;
  private refreshTimer?: NodeJS.Timeout;

  constructor(private readonly extensionUri: vscode.Uri) {}

  public resolveWebviewView(
    webviewView: vscode.WebviewView,
    _context: vscode.WebviewViewResolveContext,
    _token: vscode.CancellationToken
  ): void {
    this.view = webviewView;

    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [this.extensionUri],
    };

    webviewView.webview.html = this.getHtml(webviewView.webview);
    this.setupMessageHandler(webviewView.webview);
    this.startAutoRefresh();

    webviewView.onDidDispose(() => {
      this.stopAutoRefresh();
    });
  }

  public async refresh(): Promise<void> {
    if (!this.view) {
      return;
    }
    const stats = await getSessionStats();
    this.view.webview.postMessage({ type: "stats", data: stats });
  }

  public async showTab(tab: string): Promise<void> {
    if (!this.view) {
      return;
    }
    this.view.webview.postMessage({ type: "switchTab", tab });
    await this.loadTabData(tab);
  }

  private setupMessageHandler(webview: vscode.Webview): void {
    webview.onDidReceiveMessage(async (msg: WebviewMessage) => {
      switch (msg.type) {
        case "ready":
          await this.loadTabData("stats");
          break;

        case "loadTab":
          if (msg.tab) {
            await this.loadTabData(msg.tab);
          }
          break;

        case "search":
          if (msg.query) {
            await this.handleSearch(msg.query);
          }
          break;

        case "refresh":
          await this.refresh();
          break;

        case "openFile":
          if (msg.query) {
            const uri = vscode.Uri.file(msg.query);
            await vscode.window.showTextDocument(uri);
          }
          break;
      }
    });
  }

  private async loadTabData(tab: string): Promise<void> {
    if (!this.view) {
      return;
    }
    const webview = this.view.webview;

    switch (tab) {
      case "stats": {
        const [stats, version] = await Promise.all([
          getSessionStats(),
          getVersion(),
        ]);
        webview.postMessage({ type: "stats", data: stats });
        webview.postMessage({ type: "version", data: version });
        break;
      }

      case "knowledge": {
        const facts = await getKnowledge();
        webview.postMessage({ type: "knowledge", data: facts });
        break;
      }

      case "repomap": {
        const entries = await getRepoMap();
        webview.postMessage({ type: "repomap", data: entries });
        break;
      }

      case "search":
        break;
    }
  }

  private async handleSearch(query: string): Promise<void> {
    if (!this.view) {
      return;
    }
    this.view.webview.postMessage({ type: "searchLoading" });
    const results = await semanticSearch(query);
    this.view.webview.postMessage({ type: "searchResults", data: results });
  }

  private startAutoRefresh(): void {
    const intervalSec = vscode.workspace
      .getConfiguration("leanctx")
      .get<number>("refreshInterval", 30);

    this.refreshTimer = setInterval(() => {
      this.refresh();
    }, intervalSec * 1000);
  }

  private stopAutoRefresh(): void {
    if (this.refreshTimer) {
      clearInterval(this.refreshTimer);
      this.refreshTimer = undefined;
    }
  }

  private getHtml(webview: vscode.Webview): string {
    const htmlPath = path.join(
      this.extensionUri.fsPath,
      "src",
      "sidebar",
      "panel.html"
    );
    let html = fs.readFileSync(htmlPath, "utf-8");

    const nonce = getNonce();
    html = html.replace(/{{nonce}}/g, nonce);
    html = html.replace(
      /{{cspSource}}/g,
      webview.cspSource
    );

    return html;
  }
}

function getNonce(): string {
  let text = "";
  const chars =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (let i = 0; i < 32; i++) {
    text += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return text;
}
