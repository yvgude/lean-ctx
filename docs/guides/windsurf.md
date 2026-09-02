# Windsurf + lean-ctx Integration Guide

> **Status: local implementation reference — not a first-class LeanCTX
> integration promise.** Codex, Claude Code, and Cursor are the current
> first-class local setup paths. Verify this generic Attach wiring against the
> installed Windsurf version; compatibility alone is not a performance or
> evidence guarantee. Product scope is governed by
> `docs/internal/README.md` (internal, not in this repository).

Complete guide to setting up and optimally using lean-ctx with Windsurf (Codeium's AI-native IDE).

## Overview

| Property | Value |
|----------|-------|
| Integration mode | **Hybrid** (MCP reads + shell hooks) |
| Config file | MCP JSON config |
| Rules file | `~/.codeium/windsurf/rules/lean-ctx.md` (dedicated) |
| Setup command | `lean-ctx init --agent windsurf` |

### What `lean-ctx init --agent windsurf` installs

| Component | Path | Notes |
|-----------|------|-------|
| **MCP server** | `~/.codeium/windsurf/mcp_config.json` | The `ctx_*` tools Cascade calls |
| **Dedicated rules** | `~/.codeium/windsurf/rules/lean-ctx.md` | Tool-mapping + output-style guidance |
| **Project rules** | `.windsurfrules` (project root) | Strong "always call `ctx_*`" directive |
| **Cascade hooks** | `~/.codeium/windsurf/hooks.json` | `observe` + `pre_mcp_tool_use` telemetry/redirect |
| **Skill** | — | **N/A by design.** Windsurf consumes the dedicated rules above; there is no on-demand `SKILL.md` (that pattern is Claude Code / CodeBuddy only). A missing skill is **not** a fault. |

`lean-ctx doctor` prints this exact breakdown under **Windsurf** (MCP / Rules / Cascade hooks / Skill = N/A) plus the time of the last real `ctx_*` MCP call.

## Quick Setup

```bash
# One command — configures MCP, rules, and shell hook
lean-ctx init --agent windsurf

# Verify
lean-ctx doctor

# Restart Windsurf to load the MCP server
```

lean-ctx auto-detects Windsurf by checking for `~/.codeium/windsurf/`.

## Manual Setup

### Step 1: MCP Server Registration

Add lean-ctx to Windsurf's MCP configuration:

```json
{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx"
    }
  }
}
```

> **Note**: lean-ctx auto-detects its data directory at runtime — don't hardcode `LEAN_CTX_DATA_DIR` unless you intentionally relocate it (a wrong path splits config and data across two locations). Running `lean-ctx init --agent windsurf` writes this config for you.

### Step 2: Agent Rules

lean-ctx creates `~/.codeium/windsurf/rules/lean-ctx.md` with dedicated rules:

```markdown
# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules -->

## Mode Selection
1. Editing the file? → `anchored` first (full text + anchors), then `diff` for re-reads
2. Need API surface only? → `map` or `signatures`
3. Large file, context only? → `entropy` or `aggressive`
4. Specific lines? → `lines:N-M`
5. Active task set? → `task`
6. Unsure? → `auto` (system selects optimal mode)

Anti-pattern: NEVER use `full` for files you won't edit — use `map` or `signatures`.

## File Editing
Use native Edit/StrReplace if available. Write, Delete, Glob → use normally.
If native Edit is unavailable, use the anchored editor: `ctx_read(mode="anchored")` →
`ctx_patch` (reachable via `ctx_call` in the default profile).

## Proactive (use without being asked)
- `ctx_overview(task)` at session start
- `ctx_compress` when context grows large

## Session Documentation
After significant work, document progress:
- ctx_knowledge(action=remember, category=decision, content=what and why)
- ctx_session(action=task, value=task description with progress)
When you see [CHECKPOINT] → document current status immediately.

Fallback only if a lean-ctx tool is unavailable: use native equivalents.
<!-- /lean-ctx -->
```

### Step 3: Shell Hook

Windsurf has shell access through Cascade. lean-ctx installs compression hooks:

```bash
lean-ctx init --global
```

## Cascade Workflow Optimization

Windsurf's Cascade is an agentic AI that flows through your codebase. lean-ctx enhances Cascade in several ways:

### Faster Context Gathering

Cascade reads many files to build context. With lean-ctx:

```
# Instead of reading full file content (~2000 tokens)
ctx_read("src/api/routes.rs", "map")        # ~400 tokens — structure + exports
ctx_read("src/api/routes.rs", "signatures") # ~200 tokens — API surface only

# Re-reads cost ~13 tokens (cached)
ctx_read("src/api/routes.rs", "full")       # ~13 tokens on second read
```

### Intelligent Search

```
# Find code by meaning, not just text
ctx_semantic_search("how does the payment flow work?")

# Token-efficient grep
ctx_search("async fn handle_payment", "src/")
```

### Impact Analysis

Before Cascade makes changes:

```
ctx_graph("impact", "src/models/user.rs")
# Returns: what files import/depend on this file

ctx_impact("src/models/user.rs")
# Returns: blast radius analysis
```

## Windsurf-Specific Best Practices

### 1. Use Map Mode for Cascade's Context Sweeps

When Cascade reads multiple files to understand context:

```
# Good: compressed context
ctx_read("src/auth/mod.rs", "map")
ctx_read("src/auth/jwt.rs", "map")
ctx_read("src/auth/middleware.rs", "map")

# Bad: full reads waste tokens
ctx_read("src/auth/mod.rs", "full")  # Only if you'll edit this file
```

### 2. Session Continuity Across Cascades

Each Cascade conversation can build on previous sessions:

```
# Start of new Cascade
ctx_session(action="load")  # Restore previous context

# During work
ctx_knowledge(action="remember", category="decision", content="Chose OAuth2 PKCE flow for mobile")

# End of Cascade
ctx_session(action="task", value="OAuth2 implementation [50%]")
```

### 3. Compress Before Long Conversations

Windsurf conversations can get long. Proactively manage context:

```
ctx_compress  # Creates memory checkpoint, frees context space
ctx_metrics   # Check current token savings
```

### 4. Use ctx_overview for Flow Starts

At the beginning of each Cascade flow:

```
ctx_overview("implement rate limiting for API endpoints")
```

This gives Cascade immediate project orientation with task-relevant files and context.

## Advanced Configuration

### Project-Level Rules

Create project-specific rules in your project's Windsurf rules directory:

```bash
mkdir -p .windsurf/rules
```

Then create `.windsurf/rules/lean-ctx.md` with project-specific overrides.

### Global vs. Project Config

| Scope | Rules Path | Effect |
|-------|-----------|--------|
| Global | `~/.codeium/windsurf/rules/lean-ctx.md` | Active in all projects |
| Project | `.windsurf/rules/lean-ctx.md` | Active in this project only |

### Custom Shell Compression

lean-ctx compresses 56 shell pattern modules by default. For project-specific commands:

```toml
# .lean-ctx.toml (project root)
shell_activation = "always"
```

## Token Savings Examples

| Operation | Without lean-ctx | With lean-ctx | Savings |
|-----------|-----------------|---------------|---------|
| Read `src/main.rs` (first time) | ~2000 tokens | ~2000 tokens | 0% (first read) |
| Read `src/main.rs` (re-read) | ~2000 tokens | ~13 tokens | 99.4% |
| Read `src/main.rs` (map mode) | ~2000 tokens | ~400 tokens | 80% |
| `git status` | ~800 tokens | ~120 tokens | 85% |
| `git log -20 --oneline` | ~600 tokens | ~150 tokens | 75% |
| `cargo test` output | ~2000 tokens | ~300 tokens | 85% |

## Troubleshooting

### MCP server not connecting

```bash
# Check lean-ctx is accessible
which lean-ctx

# Test MCP server directly
echo '{"jsonrpc":"2.0","method":"initialize","params":{"capabilities":{}},"id":1}' | lean-ctx mcp

# Re-run setup
lean-ctx init --agent windsurf

# Restart Windsurf
```

### Rules not being applied

```bash
# Check rules file
cat ~/.codeium/windsurf/rules/lean-ctx.md

# Verify version
grep "lean-ctx-rules-v" ~/.codeium/windsurf/rules/lean-ctx.md

# Re-inject rules
lean-ctx setup
```

### Shell hook not active

```bash
# Check hook status
echo $LEAN_CTX_ACTIVE

# Re-install
lean-ctx init --global

# Restart terminal in Windsurf
```

### Cascade not using lean-ctx tools

1. Verify MCP server is connected in Windsurf settings
2. Check that rules file exists at `~/.codeium/windsurf/rules/lean-ctx.md`
3. Start a new Cascade conversation (rules load at conversation start)
4. Try explicitly asking: "Use ctx_read to read this file"

### `lean-ctx watch` stays empty

`watch` shows real **`ctx_*` MCP tool calls** — it measures *usage*, not whether
lean-ctx is installed. An empty feed means Cascade has not called a `ctx_*` tool
yet, not that the integration is broken.

1. Run `lean-ctx doctor` — the **Windsurf** block confirms MCP / rules / Cascade
   hooks are wired and shows the **last `ctx_*` call** (`never` if none yet).
2. If `doctor` is green but the last call is `never`, Cascade is answering with
   its **built-in** tools instead of `ctx_*`. The empty-state panel in `watch`
   distinguishes this ("IDE hooks are firing → agent using native tools") from a
   missing install.
3. Nudge it: start a fresh Cascade (rules reload per conversation) and the
   `.windsurfrules` "MANDATORY: call `ctx_*`" directive will steer it.

### Model choice (GLM 5.2, etc.) does not toggle lean-ctx

lean-ctx is **model-agnostic** — it activates from the MCP/rules/hooks wiring
above, not from which model Cascade runs. Switching Cascade to GLM 5.2 (or any
other model) neither enables nor disables lean-ctx. Weaker models sometimes
*ignore* tool-use rules and reach for built-in tools; that surfaces as an empty
`watch` (see above) and is addressed by the forceful `.windsurfrules` directive,
not by a config switch. (The earlier GLM raw-mode handling is already resolved in
the 3.8.x line.)

### Performance issues

```bash
# Check daemon status
lean-ctx status

# Pre-start daemon
lean-ctx daemon start

# Monitor savings
lean-ctx gain --live
```

## Further Reading

- [lean-ctx Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [Windsurf Documentation](https://docs.codeium.com/windsurf/)
- [MCP Protocol](https://modelcontextprotocol.io/)
