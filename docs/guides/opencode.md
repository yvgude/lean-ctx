# OpenCode + lean-ctx Integration Guide

> **Status: local implementation reference — not a first-class LeanCTX
> integration promise.** Codex, Claude Code, and Cursor are the current
> first-class local setup paths. Verify this generic Attach wiring against the
> installed OpenCode version; compatibility alone is not a performance or
> evidence guarantee. See the [canonical positioning](../POSITIONING_CANONICAL.md)
> for current product scope and claim boundaries.

Complete guide to setting up and optimally using lean-ctx with OpenCode (open-source AI coding agent).

## Overview

| Property | Value |
| Integration mode | **Hybrid** (MCP reads + shell hooks) |
| Config file | `opencode.json` (project) or `~/.config/opencode/config.json` (global) |
| Rules file | `~/.config/opencode/AGENTS.md` (shared, appended) |
| Setup command | `lean-ctx init --agent opencode` |
| Tool interception | Opt-in via `shadow_mode` (default **off**) — see [Tool Interception](#tool-interception-shadow_mode) |

## Quick Setup

```bash
# One command — configures MCP, rules, and shell hook
lean-ctx init --agent opencode

# Verify
lean-ctx doctor
```

lean-ctx auto-detects OpenCode by checking for `~/.config/opencode/`.

## Manual Setup

### Step 1: MCP Server Registration

lean-ctx configures OpenCode's MCP settings with the OpenCode-specific format:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "lean-ctx": {
      "type": "local",
      "command": ["lean-ctx"],
      "enabled": true
    }
  }
}
```

> **Key differences from other agents**:
>
> - Uses `"type": "local"` instead of `"type": "stdio"`
> - `"command"` is an array `["lean-ctx"]` instead of a string
> - Uses `"environment"` instead of `"env"`
> - Has an `"enabled": true` field
> - Includes `"$schema"` for config validation

If the config file already exists, lean-ctx merges the `lean-ctx` entry into the existing `mcp` object.

### Step 2: Rules (AGENTS.md)

OpenCode uses `~/.config/opencode/AGENTS.md` for global agent instructions. lean-ctx **appends** its rules (shared format — your existing content is preserved):

```markdown
# Your existing OpenCode AGENTS.md content here

...

# LeanCTX — Context Gateway for AI Systems

<!-- lean-ctx-rules -->

## Mode Selection

- Editing the file? → `anchored` first (full text + anchors), then `diff` for re-reads
- Context only? → `map` or `signatures`
- Large file? → `aggressive` or `entropy`
- Specific lines? → `lines:N-M`
- Unsure? → `auto`

Anti-pattern: NEVER use `full` for files you won't edit — use `map` or `signatures`.

## File Editing

Use native Edit/Write/StrReplace — unchanged. lean-ctx replaces READ only.
If native Edit is unavailable, use the anchored editor: `ctx_read(mode="anchored")` →
`ctx_patch` (reachable via `ctx_call` in the default profile).

## Session Documentation

After significant work: ctx_knowledge(action=remember, category=decision, content=...)
When you see [CHECKPOINT] → call ctx_session(action=task, value=current status).

Fallback only if a lean-ctx tool is unavailable: use native equivalents.

<!-- /lean-ctx -->
```

The section between the markers is auto-managed. Your existing content above and below is preserved.

### Step 3: Shell Hook

OpenCode has shell access. lean-ctx installs compression hooks:

```bash
lean-ctx init --global
```

## Tool Interception (`shadow_mode`)

When `shadow_mode` is enabled, lean-ctx **denies** native tool access
(`read`, `grep`, `glob`, `bash`) at the `opencode.json` permission level, so the
agent must use the `ctx_*` equivalents via the MCP server. The MCP server is
registered regardless of `shadow_mode` — both paths expose `ctx_*` tools; shadow
mode just removes the native alternative.

| `shadow_mode` | Behaviour |
| `false` (default) | `ctx_*` tools are available via MCP; native `read`/`grep`/`glob`/`bash` are untouched. |
| `true` | Native `read`/`grep`/`glob`/`bash` are **denied** via `opencode.json` `permission` object. The agent **must** use `ctx_read`/`ctx_search`/`ctx_glob`/`ctx_shell`. |

### Enabling shadow mode

```bash
lean-ctx config set shadow_mode true
lean-ctx init --agent opencode    # denies native tools, registers MCP
```

This adds `"read": "deny"`, `"grep": "deny"`, `"glob": "deny"`, `"bash": "deny"`
to the `"permission"` object in `~/.config/opencode/opencode.json`.

### Disabling shadow mode (back to opt-in tools)

```bash
lean-ctx config set shadow_mode false
lean-ctx init --agent opencode    # removes native-tool denies, keeps MCP
```

Only `"deny"` entries set by lean-ctx are removed — your user-set permission
values (e.g. `"edit": "allow"`) are preserved.

### Rules injected in both modes

Unlike the previous plugin-based design (which skipped rules to avoid token
waste), the current design **always** injects the "prefer `ctx_*`" rules block.
In shadow mode the agent has no native alternative, so the rules are even more
important.

### Known limitation

`shadow_mode` and `permission_inheritance` are mutually exclusive. When shadow
mode is active, permission inheritance is automatically disabled because both
features write to and read from the same `opencode.json` `permission` object —
enabling both would create a deadlock where native tools are denied (shadow mode)
and `ctx_*` tools are also denied (permission inheritance mirroring the deny
rules back).

## Multi-Model Workflow

OpenCode can connect to multiple providers. This guide describes the lean-ctx
integration points; available models, tokenization, context limits, caching,
usage reporting, and billing vary by provider and configuration.

### Small Context Windows (Local Models)

For a task that needs structure rather than full source, choose a narrower view:

```
# Structural views can reduce emitted source context
ctx_read("src/main.rs", "map")
ctx_read("src/lib.rs", "signatures")
```

### Large Context Windows (Cloud Models)

The same selection and cache mechanisms apply where supported:

```
# An eligible unchanged read may return a compact cache reference
ctx_read("src/main.rs", "full")

# Gather task-oriented project context
ctx_compose("implement user authentication")
```

## OpenCode-Specific Workflow

### Session Start

```
# 1. Fast project orientation
ctx_compose("your task description")

# 2. Understand project structure
ctx_tree("src/", 3)

# 3. Read key files in map mode
ctx_read("src/lib.rs", "map")
ctx_read("src/main.rs", "map")
```

### During Development

```
# Search efficiently
ctx_search("fn handle_request", "src/")
ctx_search(action="semantic", query="where is user validation?")

# Read files you'll edit
ctx_read("src/api/handler.rs", "full")

# After editing, verify changes
ctx_read("src/api/handler.rs", "diff")

# Check impact
ctx_graph("impact", "src/api/handler.rs")
```

### Session Documentation

```
# Record decisions
ctx_knowledge(action="remember", category="decision", content="Using SQLx for async database access")

# Track progress
ctx_session(action="task", value="Database layer implementation [40%]")

# Compress when context grows
ctx_compress
```

## Project-Level Configuration

### opencode.json

Each project can have its own `opencode.json` with lean-ctx MCP config:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "lean-ctx": {
      "type": "local",
      "command": ["lean-ctx"],
      "enabled": true
    }
  }
}
```

### Project-Level .lean-ctx.toml

```toml
# .lean-ctx.toml (project root)
shell_activation = "always"
```

### AGENTS.md (Project-Level)

OpenCode also reads `AGENTS.md` in the project root. You can add project-specific lean-ctx instructions there manually.

## Advanced Features

### Context-Aware Tool Selection

OpenCode can use lean-ctx's full tool suite:

```
# Code intelligence
ctx_callgraph(action="callers", symbol="handle_request")  # Call graph analysis
ctx_search(action="symbol", name="User")  # Find the definition
ctx_search("User", "src/")  # Find all references

# Architecture analysis
ctx_graph(action="diagram", kind="deps")  # Dependency diagram (Mermaid)
ctx_graph(action="impact", path="src/models/user.rs")  # Blast radius analysis

# Context packages
ctx_pack("create", "feature-auth")  # Bundle context for sharing
```

### Multi-Agent Handoff

If using OpenCode in a multi-agent setup:

```
# Agent 1: research phase
ctx_knowledge(action="remember", category="insight", content="Auth module uses JWT with HS256")
ctx_agent(action="handoff", target="agent-2", context="Implement the auth refactor")

# Agent 2: implementation phase
ctx_agent(action="sync")  # Receives Agent 1's context
```

## Context measurement

`map`, `signatures`, focused search, and supported shell summaries can reduce
the context emitted for a task. The result depends on the file, mode, command,
integration, provider route, and recovery behavior.

`lean-ctx gain --live` and `lean-ctx benchmark report .` show local token and
cost estimates for activity they observe. Provider-reported usage is available
only for supported routed paths that record it; local estimates do not
establish invoice savings or answer quality.

## Troubleshooting

### MCP server not connecting

```bash
# Check config file
cat ~/.config/opencode/config.json | python3 -m json.tool

# Verify lean-ctx entry format
# Must have: "type": "local", "command": ["lean-ctx"], "enabled": true

# Test MCP server
echo '{"jsonrpc":"2.0","method":"initialize","params":{"capabilities":{}},"id":1}' | lean-ctx mcp

# Re-run setup
lean-ctx init --agent opencode
```

### Rules not appearing

```bash
# Check AGENTS.md
cat ~/.config/opencode/AGENTS.md

# Look for lean-ctx section
grep "lean-ctx" ~/.config/opencode/AGENTS.md

# Re-inject rules
lean-ctx setup
```

### "enabled" field missing

OpenCode requires `"enabled": true` in the MCP config. If tools aren't available:

```bash
# Re-run setup to ensure correct format
lean-ctx init --agent opencode
```

### Shell hook not active

```bash
echo $LEAN_CTX_ACTIVE  # Should show "1" or similar

# Re-install
lean-ctx init --global
exec $SHELL
```

### OpenCode not finding lean-ctx binary

```bash
# Check PATH
which lean-ctx

# If installed via cargo
export PATH="$HOME/.cargo/bin:$PATH"

# If installed via npm
export PATH="$HOME/.npm-global/bin:$PATH"

# Then re-setup
lean-ctx init --agent opencode
```

## Further Reading

- [lean-ctx Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [OpenCode Documentation](https://opencode.ai/docs)
- [MCP Protocol](https://modelcontextprotocol.io/)
