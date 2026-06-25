# lean-ctx VS Code Extension

[![VS Code Marketplace](https://img.shields.io/visual-studio-marketplace/v/LeanCTX.lean-ctx?label=VS%20Code%20Marketplace)](https://marketplace.visualstudio.com/items?itemName=LeanCTX.lean-ctx)
[![Open VSX](https://img.shields.io/open-vsx/v/LeanCTX/lean-ctx?label=Open%20VSX)](https://open-vsx.org/extension/LeanCTX/lean-ctx)

VS Code sidebar extension for [lean-ctx](https://github.com/yvgude/lean-ctx) — the context engineering layer for AI agents. Works in VS Code, Cursor, VSCodium and Windsurf.

## Features

- **Dashboard** — Token savings, session stats, and file activity at a glance
- **Knowledge Panel** — Browse decisions, discoveries, blockers, and insights from the current session
- **Repo Map** — Interactive view of the most important files in your project, ranked by relevance
- **Semantic Search** — Search your codebase by meaning, not just text
- **Status Bar** — Live token savings counter with one-click dashboard access
- **Setup & Doctor** — Configure shell + editors and run diagnostics from the command palette
- **One-click MCP wiring** — Write a workspace `.vscode/mcp.json` entry for lean-ctx
- **Visualizer** — Launch the lean-ctx call graph visualizer

## Prerequisites

- [lean-ctx](https://github.com/yvgude/lean-ctx) installed (`curl -fsSL https://leanctx.com/install.sh | sh`). The extension auto-detects the binary on `PATH`, `~/.cargo/bin` and Homebrew.
- VS Code 1.80.0 or later (or a compatible editor: Cursor, VSCodium, Windsurf)

## Installation

### From the VS Code Marketplace

In VS Code: open **Extensions** (`Ctrl/Cmd+Shift+X`), search **lean-ctx**, click **Install** — or:

```bash
code --install-extension LeanCTX.lean-ctx
```

### From Open VSX (Cursor, VSCodium, Windsurf)

These editors use the [Open VSX](https://open-vsx.org/extension/LeanCTX/lean-ctx) registry. Search **lean-ctx** in the Extensions view, or:

```bash
cursor --install-extension LeanCTX.lean-ctx     # Cursor
codium --install-extension LeanCTX.lean-ctx     # VSCodium
```

### From Source

```bash
cd vscode-extension
npm install
npm run compile
npm run package
code --install-extension lean-ctx-0.1.0.vsix
```

### Development

```bash
cd vscode-extension
npm install
npm run watch
# Press F5 in VS Code to launch Extension Development Host
```

## Configuration

| Setting | Default | Description |
|---------|---------|-------------|
| `leanctx.binaryPath` | `""` (auto-detect) | Path to the lean-ctx binary. Empty → auto-detect on `PATH`, `~/.cargo/bin`, Homebrew |
| `leanctx.refreshInterval` | `30` | Status bar refresh interval (seconds) |

## Commands

| Command | Description |
|---------|-------------|
| `lean-ctx: Semantic Search` | Opens a search input for semantic code search |
| `lean-ctx: Show Repo Map` | Switches to the repo-map tab in the sidebar |
| `lean-ctx: Knowledge Panel` | Switches to the knowledge tab in the sidebar |
| `lean-ctx: Open Visualizer` | Launches the lean-ctx call graph visualizer |
| `lean-ctx: Refresh Dashboard` | Manually refreshes all dashboard data |
| `lean-ctx: Setup` | Auto-configures shell + editors (`lean-ctx setup`) |
| `lean-ctx: Doctor` | Runs diagnostics (`lean-ctx doctor`) in an output channel |
| `lean-ctx: Show Token Savings` | Shows the savings recap (`lean-ctx gain`) |
| `lean-ctx: Show Context Heatmap` | Shows the context heatmap (`lean-ctx heatmap`) |
| `lean-ctx: Open Web Dashboard` | Opens the dashboard as a native editor tab (webview). Also reachable from the terminal via `lean-ctx dashboard --vscode` |
| `lean-ctx: Configure MCP for this workspace` | Writes a `.vscode/mcp.json` stdio entry for lean-ctx |

## Architecture

```
src/
├── extension.ts          # Entry point: activate/deactivate
├── leanctx.ts            # CLI interface + binary auto-detection
├── commands.ts           # Sidebar command handlers (search, repomap, …)
├── cli-commands.ts       # CLI-backed commands (setup, doctor, MCP wiring)
├── dashboard-panel.ts    # Native dashboard webview tab (owns a private server)
├── uri-handler.ts        # vscode://LeanCTX.lean-ctx/… deep links (--vscode)
├── statusbar.ts          # Status bar item with auto-refresh
└── sidebar/
    ├── provider.ts       # Webview view provider
    └── panel.html        # Dashboard UI (HTML/CSS/JS)
```

All communication with lean-ctx happens via CLI subprocess calls in `leanctx.ts`.
The sidebar uses a webview with VS Code's native CSS variables for seamless theming.
