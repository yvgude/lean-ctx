# Installation Matrix (Setup / Init / Update)

> **Status: local Runtime implementation matrix.** This records setup paths for
> existing agents; it does not establish a LeanCTX Cloud, generic agent platform,
> hosted execution, or performance guarantee. LeanCTX is **The Context SDK for
> AI Agents**. Current product scope and status are governed by
> `docs/internal/README.md` (internal, not in this repository).

This document records the **current local wiring** implemented by `setup` and
`init`. A row proves neither first-class support nor a performance result:
Codex, Claude Code, and Cursor are the current first-class local setup paths;
every other row is an implementation reference that must be checked against the
installed host with `lean-ctx doctor`.

## Installation paths (entry points)

- **`lean-ctx setup`** (recommended): detects installed IDEs/agents, picks a default `HookMode`, installs shell hook + rules + skills + hooks, and applies repairs (`--fix`) when needed.
- **`lean-ctx init --global`**: installs shell aliases/hook only (no IDE MCP wiring).
- **`lean-ctx init --agent <name> [--mode <mcp|hybrid>]`**: installs IDE-specific hook/rules and configures **MCP**. The mode is auto-detected per agent (`recommend_hook_mode`); override it with `--mode mcp` or `--mode hybrid`.
- **`lean-ctx update`**: updates the binary, then runs a non-interactive **setup refresh** (`setup --non-interactive --yes --fix`) so wiring stays consistent.

## Integration modes

lean-ctx has exactly two integration modes (`HookMode` in `rust/src/hooks/mod.rs`):

- **Hybrid** — MCP server (cached reads/search + all `ctx_*` tools) **plus** shell hooks that compress command output. The default for every agent with reliable shell access.
- **MCP** — MCP server only. Used for IDE-extension agents without a reliable shell-hook surface.

The default per agent comes from `recommend_hook_mode`: agents in the `HYBRID_AGENTS` list get **Hybrid**, everything else gets **MCP**.

| Agent key | Default mode in `setup` | Rationale |
|----------|--------------------------|-----------|
| `cursor` | **Hybrid** | `hooks.json` compresses Shell output; MCP for cached reads/search |
| `codex` | **Hybrid** | `hooks.json` (SessionStart/PreToolUse) for Bash; MCP for reads (Desktop/Cloud variants have no hooks) |
| `grok` | **Replace** | Claude-compatible `~/.grok/hooks/*.json` (PreToolUse rewrite/redirect/deny); MCP in `~/.grok/config.toml` |
| `gemini` | **Hybrid** | BeforeTool hooks for shell; MCP for reads/search |
| `claude` / `claude-code` | **Hybrid** | PreToolUse Bash hooks + MCP (hooks don't fire in headless `-p` mode → MCP guarantees reads) |
| `codebuddy` | **Hybrid** | Same architecture as Claude Code — PreToolUse Bash hooks + MCP |
| `windsurf` | **Hybrid** | `~/.codeium/windsurf/hooks.json` for shell + MCP for full Context OS |
| `copilot` | **Hybrid** | `.github/hooks/hooks.json` for shell + MCP |
| `qoder` | **Hybrid** | Bash hook in `settings.json` + MCP for reads |
| `crush` / `hermes` / `opencode` / `pi` / `amp` | **Hybrid** | Rules/plugin/MCP wiring + shell where available |
| all others (JetBrains, Cline, Roo, Kiro, Zed, Qwen, Trae, Amazon Q, Verdent, …) | **MCP** | Extension/plugin agents without a reliable shell-hook surface |

## What gets installed per agent (canonical files)

Legend:
- **MCP config**: editor/agent config file contains a `lean-ctx` server entry (tool schemas available to host).
- **MCP disabled**: any existing `lean-ctx` entry is removed from the config file.

| Agent | MCP config path | Rules path | Hooks/scripts | Skill |
|------|------------------|-----------|--------------|-------|
| Cursor (`cursor`) | `~/.cursor/mcp.json` (MCP enabled — Hybrid) | `~/.cursor/rules/lean-ctx.mdc` | `~/.cursor/hooks.json` + `~/.cursor/hooks/lean-ctx-*.sh` | `~/.cursor/skills/lean-ctx/SKILL.md` |
| Claude Code (`claude`) | `~/.claude.json` (MCP enabled — Hybrid) | `~/.claude/CLAUDE.md` block (no rules file since 3.8) | `~/.claude/settings.json` hook wiring (Bash rewrite + Read redirect) | `~/.claude/skills/lean-ctx/SKILL.md` |
| CodeBuddy (`codebuddy`) | `~/.codebuddy.json` (MCP enabled — Hybrid) | `~/.codebuddy/CODEBUDDY.md` block | `~/.codebuddy/settings.json` hook wiring (Bash rewrite + Read redirect) | `~/.codebuddy/skills/lean-ctx/SKILL.md` |
| Codex (`codex`) | `~/.codex/config.toml` (MCP enabled — Hybrid) | `~/.codex/LEAN-CTX.md` + `~/.codex/AGENTS.md` | `~/.codex/hooks.json` (SessionStart/PreToolUse) | `~/.codex/skills/lean-ctx/SKILL.md` |
| Grok (`grok`) | `~/.grok/config.toml` (`[mcp_servers.lean-ctx]`) | `~/.grok/AGENTS.md` | `~/.grok/hooks/lean-ctx.json` (PreToolUse + SessionStart) | `~/.grok/skills/lean-ctx/SKILL.md` |

### LLM proxy wiring (`lean-ctx proxy enable`)

| Agent | Config / env written | Upstream on lean-ctx proxy |
|------|----------------------|---------------------------|
| Claude | `ANTHROPIC_BASE_URL` (API-key mode only) | `anthropic` → `api.anthropic.com` |
| Codex | `openai_base_url` / optional ChatGPT rail | `openai` / ChatGPT backend |
| Pi | `~/.pi/agent/models.json` `baseUrl` | Anthropic + OpenAI built-ins |
| Grok (subscription / `grok login`) | shell `GROK_CLI_CHAT_PROXY_BASE_URL` only — **never** `models_base_url` (that forces API-key auth) | `/providers/grok-chat/v1` → `https://cli-chat-proxy.grok.com` (session Bearer forwarded) |
| Grok (API key / `XAI_API_KEY`) | `[endpoints].models_base_url` + `GROK_MODELS_BASE_URL` | `/providers/xai/v1` → `https://api.x.ai` |
| OpenCode (`opencode`) | `~/.config/opencode/opencode.json` (MCP enabled — Hybrid) | `~/.config/opencode/rules/lean-ctx.md` | `~/.config/opencode/plugins/lean-ctx.ts` | — |
| Windsurf (`windsurf`) | `~/.codeium/windsurf/mcp_config.json` | `~/.codeium/windsurf/rules/lean-ctx.md` (global) + project `.windsurfrules` | `~/.codeium/windsurf/hooks.json` (`observe` + `pre_mcp_tool_use`) | — (N/A by design) |
| VS Code (`vscode`) | `~/Library/Application Support/Code/User/mcp.json` (macOS) · `~/.config/Code/User/mcp.json` (Linux) — native MCP, written by `setup` | `~/Library/Application Support/Code/User/.../copilot-instructions.md` | — | — |
| GitHub Copilot CLI (`copilot`) | `~/.copilot/mcp-config.json` | (Copilot CLI reads MCP server instructions) | `~/.copilot` Bash hook (Hybrid) | — |
| JetBrains (`jetbrains`) | `~/.jb-mcp.json` (snippet — **manual paste**, no auto-wiring) | `~/.jb-rules/lean-ctx.md` | — | — |
| Cline (`cline`) | Cline MCP settings JSON | `~/.cline/rules/lean-ctx.md` | — | — |
| Roo (`roo`) | Roo MCP settings JSON | `~/.roo/rules/lean-ctx.md` | — | — |
| Kiro (`kiro`) | `~/.kiro/settings/mcp.json` | `~/.kiro/steering/lean-ctx.md` | — | — |
| Gemini (`gemini`) | `~/.gemini/settings.json` | `~/.gemini/GEMINI.md` | Gemini hooks (if present) | — |
| Antigravity (`antigravity`) | `~/.gemini/antigravity/mcp_config.json` | `~/.gemini/antigravity/rules/lean-ctx.md` | — | — |
| Crush (`crush`) | `~/.config/crush/crush.json` (MCP enabled — Hybrid) | `~/.config/crush/rules/lean-ctx.md` | — | — |
| Hermes (`hermes`) | `~/.hermes/config.yaml` (MCP enabled — Hybrid) | `~/.hermes/HERMES.md` or project `.hermes.md` | — | — |
| Amp (`amp`) | `~/.config/amp/settings.json` | `~/.ampcoder/rules/lean-ctx.md` | — | — |
| Pi (`pi`) | `~/.pi/agent/mcp.json` | `~/.pi/agent/rules/lean-ctx.md` | — | — |
| Qwen (`qwen`) | `~/.qwen/settings.json` | `~/.qwen/rules/lean-ctx.md` | — | — |
| Trae (`trae`) | `~/.trae/mcp.json` | `~/.trae/rules/lean-ctx.md` | — | — |
| Amazon Q (`amazonq`) | `~/.aws/amazonq/default.json` | `~/.aws/amazonq/rules/lean-ctx.md` | — | — |
| Verdent (`verdent`) | `~/.verdent/mcp.json` | `~/.verdent/rules/lean-ctx.md` | — | — |
| Zed (`zed`) | `~/Library/Application Support/Zed/settings.json` (macOS) · `~/.config/zed/settings.json` (Linux) — `context_servers` entry | `<zed-config-dir>/rules/lean-ctx.md` (same OS-aware dir as the settings file) | — | — |
| Qoder (`qoder`) | `~/.qoder/settings.json` | `~/.qoder/rules/lean-ctx.md` (Hybrid mode) | `~/.qoder/settings.json` Bash hook | — |
| Aider (`aider`) | `~/.aider/mcp.json` | — (MCP instructions) | — | — |
| Neovim (`neovim`, mcphub.nvim) | `~/.config/mcphub/servers.json` | — (MCP instructions) | — | — |
| Emacs (`emacs`, mcp.el) | `~/.emacs.d/mcp.json` | — (MCP instructions) | — | — |
| Sublime Text (`sublime`) | `~/.config/sublime-text/mcp.json` | — (MCP instructions) | — | — |

### The Skill column: `—` means "none, by design"

A `SKILL.md` is the **Claude Code / CodeBuddy / Cursor / Codex** on-demand
instruction format. Agents that consume a **dedicated rules file** (Windsurf,
OpenCode, Cline, Roo, Gemini, …) get their guidance from that rules file plus
the MCP server, so a skill would be redundant — `—` is the intended state, not a
missing feature. For **Windsurf** specifically this is a common point of
confusion (GH #593): the integration is MCP + rules + Cascade hooks, and
`lean-ctx doctor` shows `Skill  N/A by design` next to those checks so the
absence is never misread. An empty `lean-ctx watch` likewise reflects no `ctx_*`
**tool calls yet**, not a broken install — see `docs/guides/windsurf.md`.

### Rules delivery: dedicated files vs. MCP instructions

lean-ctx delivers its usage guidance through **two** channels, and which one an
agent gets depends on whether it has a standard, global instruction-file
location:

- **Dedicated rules file** — for agents with a well-defined global rules /
  instructions path (Cursor `*.mdc`, Claude/Gemini/OpenCode markdown, Windsurf,
  Zed, Cline, Roo, Continue, Amp, JetBrains, …). See the `build_rules_targets`
  list in `rust/src/rules_inject.rs`.
- **MCP server instructions** — for MCP-bridge agents that have **no** standard
  global rules-file convention (**Aider, Neovim/mcphub, Emacs/mcp.el, Sublime**).
  These receive the same guidance through the MCP server's `instructions` field
  and tool descriptions, so no (non-functional) rules file is written for them.
  This is intentional, not a gap: writing a rules file an agent never reads
  would be dead config.

### VS Code & JetBrains: what `setup` wires vs. what is manual

These two editors have more than one possible integration surface, so it is
worth being explicit about what `lean-ctx setup` actually configures:

- **VS Code** — `setup` writes the **native, user-global** MCP config at
  `…/Code/User/mcp.json` (VS Code 1.102+ reads this directly; this is the path
  `doctor integrations` verifies). The repo also ships an **optional** editor
  extension (`vscode-extension`) — a convenience UI panel (live savings,
  repo-map, semantic search, one-click MCP wiring) on top of the same daemon.
  You do **not** need it for the MCP server to work, and `setup` does not
  install it. Get it from the
  [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=LeanCTX.lean-ctx)
  or [Open VSX](https://open-vsx.org/extension/LeanCTX/lean-ctx) (Cursor,
  VSCodium, Windsurf) if you want the in-editor panel.
- **JetBrains** — there is **no auto-wiring**. `setup` writes a ready-to-paste
  snippet to `~/.jb-mcp.json` and prints a one-line manual step. You must open
  *Settings → Tools → AI Assistant → Model Context Protocol (MCP)* once and
  paste the `lean-ctx` server. `doctor integrations` reports this entry as an
  **“MCP snippet”** (not “MCP config”) and shows the paste location, so the
  manual step is never silently assumed to be done.

## Idempotency & repairs

- `setup --fix` and `update` are intended to be **safe and repeatable**:
  - Hybrid and MCP modes both ensure the `lean-ctx` MCP server entry is present in editor configs.
  - Hybrid additionally (re-)installs shell hooks; `update` refreshes them so they always point at the current binary (see `refresh_installed_hooks`).
  - Rules and skills are overwritten to the mode-correct versions.
  - Hook installation is merge-based where supported (preserves other hooks/plugins).
