# Appendix — Per-IDE Quickstarts

Concrete, copy-paste setup for the most common editors. Every quickstart is the
same three beats: **install → what gets wired → verify**. For the full per-agent
path table see the [installation matrix](../integrations/installation-matrix.md);
for fixing a broken wiring see [Journey 12](12-troubleshooting.md).

> One command does it all: `lean-ctx onboard` (recommended) or `lean-ctx setup`
> detects every installed editor and wires each one. The quickstarts below are
> for when you want to set up — and verify — **one specific** editor.

---

## Cursor — Hybrid (MCP + shell hooks)

```bash
lean-ctx init --agent cursor       # or: lean-ctx setup
```

**Wires:**
- MCP server → `~/.cursor/mcp.json`
- Shell hooks → `~/.cursor/hooks.json` + `~/.cursor/hooks/lean-ctx-*.sh`
- Rules → `~/.cursor/rules/lean-ctx.mdc`
- Skill → `~/.cursor/skills/lean-ctx/SKILL.md`

**Verify:**
```bash
lean-ctx doctor integrations       # expect Cursor: MCP config ✓, Hooks ✓
```
Then **fully restart Cursor** (MCP servers and hooks load at startup).

---

## Claude Code — Hybrid (MCP + hooks)

```bash
lean-ctx init --agent claude       # or: lean-ctx setup
```

**Wires:**
- MCP server → `~/.claude.json` (MCP enabled)
- Hooks → `~/.claude/settings.json` (Bash rewrite + Read redirect)
- Instructions → `<!-- lean-ctx -->` block in `~/.claude/CLAUDE.md` (no rules file since 3.8)
- Skill → `~/.claude/skills/lean-ctx/SKILL.md`

**Verify:**
```bash
lean-ctx doctor integrations       # expect Claude Code: MCP config ✓, Hooks ✓, Instructions ✓
```
Restart Claude Code. Optional: `lean-ctx harden` forces the compressed `ctx_*`
path (see [Journey 13](13-security-and-governance.md)).

---

## Codex CLI — Hybrid (MCP + hooks.json)

```bash
lean-ctx init --agent codex        # or: lean-ctx setup
```

**Wires:**
- MCP server → `~/.codex/config.toml` (MCP enabled)
- Hooks → `~/.codex/hooks.json` (`SessionStart` + `PreToolUse`)
- Rules → `~/.codex/LEAN-CTX.md` + `~/.codex/AGENTS.md`
- Skill → `~/.codex/skills/lean-ctx/SKILL.md`

**Verify:**
```bash
lean-ctx doctor integrations       # expect Codex CLI: Codex MCP ✓, Codex hooks ✓, hooks.json ✓
```
Restart the Codex CLI session so it re-reads `config.toml` and `hooks.json`.

---

## VS Code — native MCP

```bash
lean-ctx init --agent vscode       # or: lean-ctx setup
```

**Wires:** the **native, user-global** MCP config (VS Code 1.102+ reads it
directly):
- `~/Library/Application Support/Code/User/mcp.json` (macOS)
- `~/.config/Code/User/mcp.json` (Linux)

The repo also ships an **optional** VS Code extension (`vscode-extension`) — a
convenience UI panel (live savings, repo-map, semantic search, one-click MCP
setup) you do **not** need for the MCP server to work, and `setup` does not
install it. Get it from the
[VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=LeanCTX.lean-ctx)
or [Open VSX](https://open-vsx.org/extension/LeanCTX/lean-ctx) (Cursor, VSCodium,
Windsurf), or run `code --install-extension LeanCTX.lean-ctx`.

**Verify:**
```bash
lean-ctx doctor integrations       # expect VS Code: VS Code MCP ✓
```
Reload the VS Code window.

---

## JetBrains IDEs — manual snippet (no auto-wiring)

JetBrains AI Assistant does not auto-load a file, so this one has a manual step:

```bash
lean-ctx init --agent jetbrains    # writes a ready-to-paste snippet
```

**Wires:** a snippet at `~/.jb-mcp.json` plus rules at `~/.jb-rules/lean-ctx.md`.

**Manual step:** open *Settings → Tools → AI Assistant → Model Context Protocol
(MCP)* and paste the `lean-ctx` server from `~/.jb-mcp.json`.

**Verify:**
```bash
lean-ctx doctor integrations       # JetBrains IDEs: MCP snippet ✓ (shows the paste location)
```

---

## Cursor vs Claude vs Codex — at a glance

| | Cursor | Claude Code | Codex CLI |
|---|--------|-------------|-----------|
| Mode | Hybrid | Hybrid | Hybrid |
| MCP config | `~/.cursor/mcp.json` | `~/.claude.json` | `~/.codex/config.toml` |
| Hooks | `hooks.json` + scripts | `settings.json` | `hooks.json` |
| Rules | `*.mdc` | `CLAUDE.md` + rules | `AGENTS.md` + `LEAN-CTX.md` |
| Skill | yes | yes | yes |
| After setup | restart Cursor | restart Claude Code | restart session |

All three intercept terminal commands via their hook **and** expose `ctx_*` MCP
tools. MCP-only editors (Zed, Cline, Roo, JetBrains, …) get the tools but not the
shell-hook compression — see the
[installation matrix](../integrations/installation-matrix.md) for the full list.
