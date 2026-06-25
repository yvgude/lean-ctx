import * as vscode from "vscode";
import { cmdDashboard } from "./dashboard-panel";

/**
 * Deep-link handler for `vscode://LeanCTX.lean-ctx/...` (and the matching scheme
 * on forks: `cursor://`, `vscodium://`, `windsurf://`, `vscode-insiders://`).
 *
 * VS Code routes a URL whose authority equals this extension's id here, which
 * lets an external trigger open extension UI without the user touching the
 * command palette. The `lean-ctx dashboard --vscode` CLI fires
 * `<scheme>://LeanCTX.lean-ctx/dashboard` to open the native dashboard tab — the
 * same panel as the "lean-ctx: Open Web Dashboard" command. We only dispatch on
 * the path; the authority is already matched to us by VS Code.
 */
export function registerUriHandler(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.window.registerUriHandler({
      handleUri(uri: vscode.Uri): void {
        // Tolerate a trailing slash and case so `/dashboard`, `/dashboard/` and
        // a bare authority all open the dashboard.
        const path = uri.path.replace(/\/+$/, "").toLowerCase();
        switch (path) {
          case "":
          case "/dashboard":
            void cmdDashboard(context);
            return;
          default:
            vscode.window.showWarningMessage(
              `lean-ctx: don't know how to handle the link "${uri.path}".`
            );
        }
      },
    })
  );
}
