import * as vscode from "vscode";
import { spawn, ChildProcess } from "child_process";
import * as net from "net";
import * as http from "http";
import { resolveBinaryPath } from "./leanctx";

/**
 * Native webview dashboard (#466 item 3).
 *
 * Instead of printing a URL and asking the user to open Simple Browser manually,
 * `lean-ctx: Open Web Dashboard` starts a headless dashboard server we own
 * (random port + Bearer token, bound to loopback) and embeds it in a proper
 * VS Code editor tab via `createWebviewPanel`. The panel owns the server's
 * lifecycle: closing the tab (or deactivating the extension) stops the server,
 * so nothing is orphaned behind the extension host.
 */

// Singleton panel + the server it owns, so re-invoking the command reveals the
// existing tab instead of spawning a second server.
let panel: vscode.WebviewPanel | undefined;
let serverProc: ChildProcess | undefined;

/** Ask the OS for a free loopback port (listen on :0, read it back, release). */
function findFreePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      srv.close(() => (port ? resolve(port) : reject(new Error("no free port"))));
    });
  });
}

/** A URL-safe random token to pin the dashboard's Bearer auth for this session. */
function randomToken(): string {
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let t = "";
  for (let i = 0; i < 32; i++) {
    t += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return t;
}

/** Poll the dashboard root until it answers (any HTTP reply = server is up). */
function waitForServer(port: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve) => {
    const tick = (): void => {
      const req = http.get(
        { host: "127.0.0.1", port, path: "/", timeout: 1000 },
        (res) => {
          res.resume();
          resolve(true);
        }
      );
      const retry = (): void => {
        if (Date.now() >= deadline) {
          resolve(false);
        } else {
          setTimeout(tick, 200);
        }
      };
      req.on("error", retry);
      req.on("timeout", () => {
        req.destroy();
        retry();
      });
    };
    tick();
  });
}

function stopServer(): void {
  if (serverProc) {
    try {
      serverProc.kill();
    } catch {
      /* already gone */
    }
    serverProc = undefined;
  }
}

export async function cmdDashboard(
  context: vscode.ExtensionContext
): Promise<void> {
  // Reveal an existing panel rather than starting a second server.
  if (panel) {
    panel.reveal(vscode.ViewColumn.Active);
    return;
  }

  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: "lean-ctx: starting dashboard…",
    },
    async () => {
      let port: number;
      try {
        port = await findFreePort();
      } catch {
        vscode.window.showErrorMessage(
          "lean-ctx: could not find a free port for the dashboard."
        );
        return;
      }

      const token = randomToken();
      const bin = resolveBinaryPath();
      const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;

      let spawnError: string | undefined;
      try {
        serverProc = spawn(
          bin,
          [
            "dashboard",
            "--no-open",
            "--host=127.0.0.1",
            `--port=${port}`,
            `--auth-token=${token}`,
          ],
          { cwd, env: { ...process.env, NO_COLOR: "1" } }
        );
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(
          `lean-ctx: could not start the dashboard — ${msg}`
        );
        return;
      }

      // Surface an early spawn failure (e.g. binary not found) rather than a
      // blank tab; clear our handle if the server dies on its own.
      serverProc.on("error", (e: Error) => {
        spawnError = e.message;
      });
      serverProc.on("exit", () => {
        serverProc = undefined;
      });

      const ready = await waitForServer(port, 8000);
      if (spawnError) {
        vscode.window.showErrorMessage(
          `lean-ctx: could not start the dashboard — ${spawnError}`
        );
        stopServer();
        return;
      }
      if (!ready) {
        vscode.window.showErrorMessage(
          "lean-ctx: the dashboard did not come up in time."
        );
        stopServer();
        return;
      }

      // Tunnel the loopback URL so the iframe loads both on desktop (returns the
      // same URI) and in remote/Codespaces (returns a forwarded https host).
      const baseUri = await vscode.env.asExternalUri(
        vscode.Uri.parse(`http://127.0.0.1:${port}/?token=${token}`)
      );

      panel = vscode.window.createWebviewPanel(
        "leanCtxDashboard",
        "lean-ctx Dashboard",
        vscode.ViewColumn.One,
        { enableScripts: true, retainContextWhenHidden: true }
      );
      try {
        panel.iconPath = vscode.Uri.joinPath(
          context.extensionUri,
          "resources",
          "icon.svg"
        );
      } catch {
        /* icon is cosmetic */
      }

      const frameOrigin = `${baseUri.scheme}://${baseUri.authority}`;
      panel.webview.html = dashboardHtml(baseUri.toString(true), frameOrigin);

      panel.onDidDispose(
        () => {
          panel = undefined;
          stopServer();
        },
        null,
        context.subscriptions
      );

      // Reap the server if the extension host shuts down while the tab is open.
      context.subscriptions.push({ dispose: stopServer });
    }
  );
}

/**
 * Full-bleed iframe host. The CSP only permits framing the exact dashboard
 * origin (`http://127.0.0.1:<port>` on desktop, or the asExternalUri tunnel
 * host) and inline styles — nothing else loads in this webview.
 */
function dashboardHtml(src: string, frameOrigin: string): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; frame-src ${frameOrigin}; style-src 'unsafe-inline';" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<style>
  html, body { margin: 0; padding: 0; height: 100%; width: 100%; overflow: hidden; background: var(--vscode-editor-background); }
  iframe { border: 0; width: 100%; height: 100vh; display: block; }
</style>
</head>
<body>
  <iframe src="${src}" allow="clipboard-read; clipboard-write" referrerpolicy="no-referrer"></iframe>
</body>
</html>`;
}
