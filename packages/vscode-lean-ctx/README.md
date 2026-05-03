# lean-ctx for VS Code

**Context Runtime for AI Agents** — Reduces LLM token consumption by up to 99%. 49 MCP tools, 10 read modes, 90+ compression patterns, cross-session memory.

## Features

- **Status Bar** — Live token savings counter, updates automatically
- **Command Palette** — `lean-ctx: Setup`, `Doctor`, `Show Token Savings`, `Open Dashboard`, `Show Context Heatmap`
- **Auto MCP Config** — Detects lean-ctx and offers to configure MCP for GitHub Copilot
- **Output Channel** — All lean-ctx command output in a dedicated panel

## Requirements

- [lean-ctx](https://leanctx.com) binary installed (`cargo install lean-ctx` or `brew install lean-ctx`)

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `lean-ctx.binaryPath` | `""` | Path to lean-ctx binary (auto-detected if empty) |
| `lean-ctx.autoSetup` | `true` | Automatically offer MCP configuration on activation |
| `lean-ctx.statusBar` | `true` | Show token savings in the status bar |
| `lean-ctx.refreshInterval` | `30` | Status bar refresh interval in seconds |

## Commands

| Command | Description |
|---------|-------------|
| `lean-ctx: Setup` | Auto-configure shell hooks + editor integration |
| `lean-ctx: Doctor` | Run diagnostics (PATH, config, MCP) |
| `lean-ctx: Show Token Savings` | Display token savings dashboard |
| `lean-ctx: Open Dashboard` | Open the web dashboard |
| `lean-ctx: Show Context Heatmap` | Display project context heatmap |
| `lean-ctx: Configure MCP for Copilot` | Create/update `.github/copilot/mcp.json` |

## How It Works

1. Install the extension
2. If lean-ctx binary is detected, it auto-activates
3. Status bar shows your cumulative token savings
4. Use the command palette for setup, diagnostics, and analytics
5. MCP integration is auto-configured for GitHub Copilot

## Links

- [Website](https://leanctx.com)
- [Documentation](https://leanctx.com/docs/getting-started)
- [GitHub](https://github.com/yvgude/lean-ctx)
