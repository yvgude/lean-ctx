# Cursor + lean-ctx Integration Guide

> **Status: Available first-class local setup path.** LeanCTX attaches to
> Cursor through local Runtime wiring; its coverage is limited to observable
> context behavior. Product scope and claim discipline are governed by
> [`docs/internal/README.md`](../internal/README.md).

Complete guide to setting up and optimally using lean-ctx with Cursor IDE.

## Overview

| Property | Value |
|----------|-------|
| Integration mode | **Hybrid** (MCP reads + shell hooks) |
| Config file | Cursor Settings UI (MCP section) |
| Rules file | `~/.cursor/rules/lean-ctx.mdc` (Cursor MDC format) |
| Skill file | `~/.cursor/skills/lean-ctx/SKILL.md` |
| Setup command | `lean-ctx init --agent cursor` |

## Quick Setup

```bash
# One command — configures MCP, rules, shell hook, and skill
lean-ctx init --agent cursor

# Verify
lean-ctx doctor

# Restart Cursor to load the MCP server
```

lean-ctx auto-detects Cursor by checking for `~/.cursor/`.

## Manual Setup

### Step 1: MCP Server Registration

Open Cursor Settings → MCP → Add Server, or add directly to your MCP configuration:

```json
{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx"
    }
  }
}
```

> **Note**: lean-ctx auto-detects its data directory (`~/.lean-ctx` by default). Do not hardcode `LEAN_CTX_DATA_DIR` unless you intentionally relocate it — a wrong path splits your stats across two locations. Running `lean-ctx setup` (or `lean-ctx init --agent cursor`) writes this config for you.

After adding, restart Cursor. You should see "lean-ctx" listed as a connected MCP server in Cursor Settings → MCP.

### Step 2: Agent Rules (MDC Format)

lean-ctx creates `~/.cursor/rules/lean-ctx.mdc` with Cursor-specific MDC
frontmatter (`alwaysApply: true`, `globs: **/*`). The **content is
hook-aware** (GL #1153):

- **Hooks installed** (the default — `init --agent cursor` writes
  `~/.cursor/hooks.json` with the `rewrite` + `redirect` PreToolUse hooks):
  the mdc carries the *hook-covered* profile. It states honestly that native
  Shell/Read/Grep are already compressed transparently by the hooks, and
  advertises only the tools with no native equivalent — `ctx_compose`,
  `ctx_symbol` / `ctx_callgraph`, `ctx_semantic_search`,
  `ctx_knowledge` / `ctx_session`, `ctx_expand`.
- **No hooks** (MCP-only install): the mdc carries the full tool-mapping
  profile (`ctx_read` over Read, `ctx_search` over Grep, `ctx_shell` over
  Shell, …), because nothing else routes native calls through lean-ctx.

The injector re-syncs the profile automatically when hooks are installed or
removed later — no manual step. Rationale: Cursor's harness makes native
tools first-class and MCP tools two-step; a "NEVER use native tools" rule
there is unenforceable and only creates instruction dissonance, while the
hooks already bank the savings on every native call.

Editing is native-first in both profiles: use Cursor's Edit/StrReplace (Write,
Delete, Glob as normal). If native Edit is ever unavailable, the anchored
editor covers it — `ctx_read(mode="anchored")` → `ctx_patch` (reachable via
`ctx_call` in the default profile); `ctx_edit` (str_replace) is the legacy
power-profile fallback.

### Step 3: Shell Hook

Cursor's Agent mode has shell access. lean-ctx installs compression hooks:

```bash
lean-ctx init --global
```

### Step 4: SKILL.md

lean-ctx installs a skill file at `~/.cursor/skills/lean-ctx/SKILL.md`. This gives Cursor detailed knowledge of all 79 tools, modes, and best practices.

## Hybrid Mode: MCP Reads + CLI Shell

Cursor's lean-ctx integration uses a hybrid approach for maximum efficiency:

### MCP Tools (for reads and search)

```
ctx_read(path, mode)     → replaces native Read tool
ctx_search(pattern, path) → replaces native Grep tool
ctx_tree(path, depth)     → replaces native ls/find
```

MCP tools benefit from session caching — re-reads cost ~13 tokens instead of re-reading the full file.

### CLI Commands (for shell operations)

```bash
lean-ctx -c "git status"       # compressed shell output
lean-ctx -c "cargo test"       # compressed test output  
lean-ctx -c "npm install"      # compressed install output
lean-ctx ls src/               # compact directory map
lean-ctx grep "pattern" src/   # compact search results
```

Using the CLI for shell commands avoids MCP schema overhead. The shell hook also compresses commands run directly via Cursor's Shell tool.

## Cursor-Specific Workflows

### Agent Mode

In Agent mode, Cursor has full tool access. The lean-ctx rules instruct the agent to:

1. Use `ctx_read` instead of the native `Read` tool
2. Use `ctx_search` instead of the native `Grep` tool
3. Use `lean-ctx -c "<cmd>"` for shell commands
4. Use native `Edit`/`StrReplace` for file modifications (lean-ctx only handles reads)

### Ask Mode

In Ask mode (read-only), Cursor benefits from:

- `ctx_read(path, "map")` — get file structure without reading full content
- `ctx_read(path, "signatures")` — API surface only
- `ctx_search(pattern)` — find code patterns efficiently
- `ctx_semantic_search(query)` — understand code by meaning

### @-Reference Workflow

When you use Cursor's `@file` or `@folder` references, lean-ctx complements them:

```
@src/auth/ — Cursor provides the file context
ctx_read("src/auth/middleware.rs", "map") — lean-ctx adds structural understanding
ctx_graph("impact", "src/auth/middleware.rs") — lean-ctx shows what depends on this file
```

### Composer/Multi-File Edits

For multi-file edits in Composer:

1. Use `ctx_read(path, "map")` to understand each file's structure first
2. Use `ctx_read(path, "full")` only for files being edited
3. After edits, use `ctx_read(path, "diff")` to verify changes
4. Use `ctx_impact(path)` to find files that might need related changes

## Project-Level Configuration

### Per-Project Rules

Add project-specific lean-ctx rules alongside the global ones. Create `.cursor/rules/lean-ctx.mdc` in your project root:

```markdown
---
description: "Project-specific lean-ctx overrides"
globs: **/*
alwaysApply: true
---

# Project lean-ctx rules
<!-- lean-ctx-rules -->
## Mode Selection
- Editing → `full` then `diff` for re-reads
- Context only → `map` or `signatures`
<!-- /lean-ctx -->
```

### AGENTS.md

For projects using the `AGENTS.md` convention, lean-ctx's rules can also be placed there. The shared rules format is used:

```markdown
# Your project agent instructions

<!-- lean-ctx section (auto-managed) -->
# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules -->
...
<!-- /lean-ctx -->
```

### .cursorrules

If your project uses `.cursorrules`, lean-ctx can inject its rules there too. The section between the lean-ctx markers is auto-managed.

## Advanced Features

### Session Continuity

lean-ctx persists session state across Cursor restarts:

```
ctx_session(action="task", value="Implementing auth middleware [60%]")
ctx_knowledge(action="remember", category="decision", content="Using bcrypt for password hashing")
```

When you start a new Cursor session, lean-ctx restores:
- Recent tool results (reads, searches, test outcomes)
- Architecture decisions made during previous sessions
- Touched files with summaries
- Task completion state and next steps

### Context Manager Dashboard

Monitor real-time token savings:

```bash
lean-ctx gain --live        # real-time savings
lean-ctx dashboard          # browser-based dashboard
lean-ctx watch              # TUI monitor
```

### Multi-Agent with Cursor Subagents

**Important: Cursor blocks MCP tools in readonly subagents.**

When spawning subagents that need lean-ctx tools, always set `readonly: false`:

```
# WRONG — lean-ctx tools will fail:
Task(subagent_type="explore", ...)  # explore is always readonly

# CORRECT — lean-ctx tools work:
Task(subagent_type="generalPurpose", readonly=false, prompt="Use ctx_compose to...")
```

lean-ctx's read-only tools declare `readOnlyHint: true` in MCP annotations —
`ctx_read`, `ctx_search`, `ctx_glob`, `ctx_tree` and the rest of the inspection
surface. When Cursor starts respecting this hint (per MCP spec), readonly
subagents will gain access to them. `ctx_compose` is deliberately *not* in that
set: it records co-access for the session, so it mutates state and says so.

Within subagents that have MCP access:

```
# Set fresh=true in subagents to bypass cache
ctx_read(path, "full", fresh=true)

# Subagents can share knowledge
ctx_knowledge(action="remember", category="insight", content="Found the bug in auth.rs:42")

# Main agent sees it
ctx_knowledge(action="recall", query="auth bug")
```

**Fallback for readonly subagents:** If a subagent must be readonly (e.g. for
safety), lean-ctx hooks still compress native Read/Shell via CLI subprocess.
The subagent loses session memory and caching but still gets compression.

## Troubleshooting

### MCP server not showing in Cursor

1. Check Cursor Settings → MCP — lean-ctx should be listed
2. If not, re-run `lean-ctx init --agent cursor`
3. Restart Cursor completely (not just reload window)
4. Check the MCP server log for errors

### "lean-ctx" tools not appearing

```bash
# Verify the binary is accessible
which lean-ctx

# Test MCP server
echo '{"jsonrpc":"2.0","method":"initialize","params":{"capabilities":{}},"id":1}' | lean-ctx mcp

# Check Cursor's MCP connection
# In Cursor: Cmd+Shift+P → "MCP: List Servers"
```

### Rules not applied (agent ignores lean-ctx)

```bash
# Check the global rules file
cat ~/.cursor/rules/lean-ctx.mdc

# Verify MDC frontmatter is present
head -5 ~/.cursor/rules/lean-ctx.mdc
# Should show: ---\ndescription:...\nalwaysApply: true\n---

# Re-inject rules
lean-ctx setup
```

### Shell hook not compressing commands

```bash
# Check if hook is active
echo $LEAN_CTX_ACTIVE

# Re-install shell hook
lean-ctx init --global

# Restart terminal in Cursor (kill terminal, open new one)
```

### Cursor using native Read instead of ctx_read

With the hooks installed this is **expected and fine**: the `redirect` hook
compresses native Read/Grep and the `rewrite` hook compresses native Shell
transparently — the savings are banked either way (verify with
`lean-ctx gain --live`). The hook-covered rules profile documents exactly
this.

Only in an MCP-only install (no `~/.cursor/hooks.json` entries) should the
agent prefer `ctx_read`/`ctx_search` directly. If it doesn't there:

1. Check `~/.cursor/rules/lean-ctx.mdc` exists and has `alwaysApply: true`
2. Restart Cursor after rule changes
3. In a new chat, verify the agent uses `ctx_read` — if not, the rules may be overridden by project-level rules with conflicting instructions

### High latency on first tool call

The first MCP tool call in a session starts the lean-ctx daemon. Subsequent calls are fast. To pre-warm:

```bash
# Start daemon before opening Cursor
lean-ctx daemon start
```

## Performance Tips

1. **Use `map` mode aggressively** — most context reads don't need full file content
2. **Let the cache work** — re-reads cost ~13 tokens vs. ~2000 for native reads
3. **Use `ctx_overview` at session start** — primes the cache for common files
4. **Monitor with `lean-ctx gain --live`** — see savings in real time
5. **Use `ctx_compress` proactively** — when context grows large, create a checkpoint

## Further Reading

- [lean-ctx Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [Cursor Documentation](https://docs.cursor.com/)
- [MCP Protocol](https://modelcontextprotocol.io/)
