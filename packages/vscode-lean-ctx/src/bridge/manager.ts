// SPDX-License-Identifier: Apache-2.0
// Runs one semantic bridge per local workspace folder while
// `leanctx.semanticBridge.enabled` is on, and announces each to lean-ctx.

import { createHash, randomBytes } from "crypto";
import * as path from "path";
import * as vscode from "vscode";
import { run } from "../binary";
import { announce, pruneStale, withdraw } from "./announce";
import { BridgeServer, startBridge } from "./server";
import { vscodeNavigator } from "./vscodeNavigator";

interface Running {
  server: BridgeServer;
  file: string;
}

/**
 * Where bridges are announced: `$LEAN_CTX_DATA_DIR/editor-bridges` when set
 * (lean-ctx gives that variable precedence too), otherwise whatever the
 * binary reports — the data directory has several legacy layouts, so it is
 * not re-derived here.
 */
async function bridgeDir(bin: string | null): Promise<string | null> {
  const env = process.env.LEAN_CTX_DATA_DIR?.trim();
  if (env) return path.join(env, "editor-bridges");
  if (!bin) return null;
  const out = await run(bin, ["editor-bridge", "dir"]);
  return out?.trim() || null;
}

/**
 * Digest of the installed extensions and their versions: the language
 * providers behind the bridge's answers. lean-ctx keys cached answers on it,
 * so an updated language extension retires them.
 */
function providerFingerprint(extensionVersion: string): string {
  const ids = vscode.extensions.all
    .map((e) => `${e.id}@${String((e.packageJSON as { version?: string }).version ?? "")}`)
    .sort();
  return createHash("sha256")
    .update([extensionVersion, ...ids].join("\n"))
    .digest("hex")
    .slice(0, 16);
}

export class BridgeManager implements vscode.Disposable {
  private readonly running = new Map<string, Running>();
  private syncing: Promise<void> = Promise.resolve();
  private disposed = false;

  constructor(
    private readonly binary: () => string | null,
    private readonly extensionVersion: string,
    private readonly log: vscode.OutputChannel,
  ) {}

  /** Brings running bridges in line with the folders and the setting. */
  sync(): Promise<void> {
    this.syncing = this.syncing
      .then(() => this.syncNow())
      .catch((e: unknown) => {
        this.log.appendLine(`semantic bridge: ${e instanceof Error ? e.message : String(e)}`);
      });
    return this.syncing;
  }

  private async syncNow(): Promise<void> {
    const enabled =
      !this.disposed &&
      vscode.workspace.getConfiguration("leanctx").get<boolean>("semanticBridge.enabled", true);
    const wanted = new Set(
      enabled
        ? (vscode.workspace.workspaceFolders ?? [])
            .filter((f) => f.uri.scheme === "file")
            .map((f) => f.uri.fsPath)
        : [],
    );
    for (const [root, r] of this.running) {
      if (!wanted.has(root)) await this.stop(root, r);
    }
    const missing = [...wanted].filter((root) => !this.running.has(root));
    if (missing.length === 0) return;
    const dir = await bridgeDir(this.binary());
    if (!dir) {
      this.log.appendLine("semantic bridge: lean-ctx not found (or too old), not announcing");
      return;
    }
    pruneStale(dir);
    const editor = vscode.env.uriScheme || "vscode";
    const fingerprint = providerFingerprint(this.extensionVersion);
    for (const root of missing) {
      // Deactivation may have happened while awaiting above.
      if (this.disposed) return;
      const token = randomBytes(32).toString("hex");
      const server = await startBridge({
        root,
        token,
        navigator: vscodeNavigator,
        info: { editor, version: vscode.version },
      });
      let file: string;
      try {
        if (this.disposed) throw new Error("extension deactivated");
        file = announce(dir, {
          port: server.port,
          token,
          pid: process.pid,
          project_root: root,
          editor,
          editor_version: vscode.version,
          provider_fingerprint: fingerprint,
          extension_version: this.extensionVersion,
          started_at: new Date().toISOString(),
        });
      } catch (e) {
        // Never leave a server running that nobody can find.
        await server.close();
        throw e;
      }
      this.running.set(root, { server, file });
      this.log.appendLine(`semantic bridge: serving ${root} on 127.0.0.1:${server.port}`);
    }
  }

  private async stop(root: string, r: Running): Promise<void> {
    withdraw(r.file);
    await r.server.close();
    this.running.delete(root);
    this.log.appendLine(`semantic bridge: stopped for ${root}`);
  }

  dispose(): void {
    this.disposed = true;
    for (const [root, r] of this.running) {
      withdraw(r.file);
      void r.server.close();
      this.running.delete(root);
    }
  }
}
