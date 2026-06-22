# OpenCode + lean-ctx Integration Guide

Complete guide to setting up and optimally using lean-ctx with OpenCode (open-source AI coding agent).

## Overview

| Property | Value |
|----------|-------|
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

# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules -->

## Mode Selection
- Editing the file? → `full` first, then `diff` for re-reads
- Context only? → `map` or `signatures`
- Large file? → `aggressive` or `entropy`
- Specific lines? → `lines:N-M`
- Unsure? → `auto`

Anti-pattern: NEVER use `full` for files you won't edit — use `map` or `signatures`.

## File Editing
Use native Edit/Write/StrReplace — unchanged. lean-ctx replaces READ only.
If Edit requires Read and Read is unavailable, use `ctx_edit(path, old_string, new_string)`.
NEVER loop on Edit failures — switch to ctx_edit immediately.

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

lean-ctx offers OpenCode **two mutually exclusive** integration surfaces. Exactly
one is active at a time, decided by the `shadow_mode` config flag — running both
would expose lean-ctx twice (the plugin spawns its own lean-ctx MCP client *in
addition* to the `mcp.lean-ctx` server), wasting tokens and confusing the model.

| `shadow_mode` | Active surface | Behaviour |
|---------------|----------------|-----------|
| `false` (default) | **MCP config** (`mcp.lean-ctx`) | `ctx_*` tools are offered; the model *chooses* when to use them. Native `read`/`grep`/`glob`/`edit`/`bash` are untouched. |
| `true` | **Interception plugin** (`~/.config/opencode/plugins/lean-ctx.ts`) | Native `read`/`grep`/`glob`/`edit`/`bash` are transparently routed through `ctx_read`/`ctx_search`/`ctx_glob`/`ctx_edit`/`ctx_shell`. The `mcp.lean-ctx` entry is removed automatically. |

### Enabling interception

```bash
lean-ctx config set shadow_mode true
lean-ctx init --agent opencode    # installs the plugin, removes mcp.lean-ctx
```

This writes the interception plugin to `~/.config/opencode/plugins/lean-ctx.ts`
plus a `package.json` declaring its npm dependencies
(`@modelcontextprotocol/sdk`, `@opencode-ai/plugin`). Install them once from the
plugin directory:

```bash
cd ~/.config/opencode/plugins && npm install   # or: bun install
```

### Disabling interception (back to opt-in tools)

```bash
lean-ctx config set shadow_mode false
lean-ctx init --agent opencode    # removes the plugin, restores mcp.lean-ctx
```

The plugin file is deleted so interception actually stops. Its `package.json` is
left in place (it may contain dependencies you manage).

### No redundant rules

While the interception plugin is active, native tools *are* the lean-ctx tools, so
the "prefer `ctx_*`" rules block would be redundant. lean-ctx therefore **skips
the dedicated rules registration** when `shadow_mode` is on, so you never pay for
duplicate instructions (rules + plugin) out of the context budget.

> **Plugin vs. MCP — never both.** Don't hand-add `mcp.lean-ctx` to
> `opencode.json` while the interception plugin is installed (or vice versa).
> `lean-ctx init --agent opencode` always reconciles the two for you based on
> `shadow_mode`.

## Multi-Model Workflow

OpenCode supports multiple LLM providers. lean-ctx works identically across all of them:

### Provider-Agnostic Benefits

| Provider | Context Window | lean-ctx Benefit |
|----------|---------------|------------------|
| Claude (Anthropic) | 200K tokens | Cost reduction, session memory |
| GPT-4 (OpenAI) | 128K tokens | Context space optimization |
| Gemini (Google) | 1M+ tokens | Cost reduction, focus |
| Local models (Ollama) | 8-32K tokens | Critical context management |

### Small Context Windows (Local Models)

For local models with limited context windows, lean-ctx is especially valuable:

```
# Compressed reads leave room for actual reasoning
ctx_read("src/main.rs", "map")        # ~400 tokens instead of ~2000
ctx_read("src/lib.rs", "signatures")  # ~200 tokens instead of ~2000

# Combined savings: 4x more files fit in context
```

### Large Context Windows (Cloud Models)

Even with large context windows:

```
# Cost reduction: fewer tokens = lower API bills
ctx_read("src/main.rs", "full")  # Cached: ~13 tokens on re-read

# Quality improvement: focused context = better responses
ctx_overview("implement user authentication")  # Task-relevant context only
```

## OpenCode-Specific Workflow

### Session Start

```
# 1. Fast project orientation
ctx_overview("your task description")

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
ctx_semantic_search("where is user validation?")

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
ctx_callgraph("src/api/mod.rs", "handle_request")  # Call graph analysis
ctx_refactor("references", "src/models/user.rs", "User")  # Find all references
ctx_smells("src/api/handler.rs")  # Code smell detection

# Architecture analysis
ctx_architecture("src/")  # Architecture overview
ctx_impact("src/models/user.rs")  # Blast radius analysis

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

## Token Savings

| Operation | Without lean-ctx | With lean-ctx | Savings |
|-----------|-----------------|---------------|---------|
| File read (cached re-read) | ~2000 tokens | ~13 tokens | 99.4% |
| File read (map mode) | ~2000 tokens | ~400 tokens | 80% |
| File read (signatures) | ~2000 tokens | ~200 tokens | 90% |
| `git status` | ~800 tokens | ~120 tokens | 85% |
| `cargo test` | ~2000 tokens | ~300 tokens | 85% |
| `npm install` | ~1500 tokens | ~200 tokens | 87% |

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
