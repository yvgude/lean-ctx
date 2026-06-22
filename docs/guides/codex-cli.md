# Codex CLI + lean-ctx Integration Guide

Complete guide to setting up and optimally using lean-ctx with Codex CLI (OpenAI's terminal-based coding agent).

## Overview

| Property | Value |
|----------|-------|
| Integration mode | **Hybrid** (MCP reads + shell hooks) |
| Config file | `~/.codex/config.toml` |
| Rules file | `~/.codex/instructions.md` (shared block) |
| Setup command | `lean-ctx init --agent codex` |

## Quick Setup

```bash
# One command — configures MCP, rules, and shell hook
lean-ctx init --agent codex

# Verify
lean-ctx doctor
```

lean-ctx auto-detects Codex CLI by checking for `~/.codex/` or the `codex` binary in `$PATH`.

> **Note**: The Codex CLI config directory can be customized via the `CODEX_HOME` environment variable. lean-ctx respects this setting.

## Manual Setup

### Step 1: MCP Server Registration

Codex CLI uses TOML configuration. lean-ctx writes to `~/.codex/config.toml`:

```toml
[mcp_servers.lean-ctx]
command = "lean-ctx"
args = []
```

If the file already exists, lean-ctx merges the `[mcp_servers.lean-ctx]` section without modifying other settings.

> **Key difference from JSON agents**: Codex uses TOML format with `[mcp_servers.<name>]` sections instead of JSON `mcpServers` objects.

### Step 2: Agent Rules

Codex CLI shares its rules infrastructure with Claude Code. lean-ctx creates dedicated rules at the Claude rules directory:

```markdown
# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules -->

## Mode Selection
1. Editing the file? → `full` first, then `diff` for re-reads
2. Need API surface only? → `map` or `signatures`
3. Large file, context only? → `entropy` or `aggressive`
4. Specific lines? → `lines:N-M`
5. Active task set? → `task`
6. Unsure? → `auto` (system selects optimal mode)

Anti-pattern: NEVER use `full` for files you won't edit — use `map` or `signatures`.

## File Editing
Use native Edit/StrReplace if available. If Edit requires Read and Read is unavailable, use ctx_edit.
Write, Delete, Glob → use normally. NEVER loop on Edit failures — switch to ctx_edit immediately.

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

Codex CLI has shell access. lean-ctx installs compression hooks:

```bash
lean-ctx init --global
```

## Sandbox Workflow

Codex CLI runs in a sandboxed environment for safety. lean-ctx integrates with this:

### How the Sandbox Affects lean-ctx

| Aspect | Behavior |
|--------|----------|
| File reads | Work normally — lean-ctx reads files within the sandbox |
| Shell commands | Compressed within sandbox constraints |
| Data directory | the lean-ctx data directory (`~/.lean-ctx` / XDG, or `LEAN_CTX_DATA_DIR` if you relocated it) must be accessible from the sandbox |
| Network | lean-ctx is local-first, no network needed |

### Sandbox Permissions

Codex CLI uses different permission levels. lean-ctx works with all of them:

- **suggest** — lean-ctx provides read-only context (map, signatures, search)
- **auto-edit** — lean-ctx provides reads + context for edits
- **full-auto** — lean-ctx provides full hybrid integration

### Running with Full Auto

```bash
codex --approval-mode full-auto "refactor the auth module"
```

lean-ctx tools are available in all modes since they're read-only MCP tools.

## Background Agent Integration

Codex CLI supports background agents for long-running tasks. lean-ctx enhances this:

### Context Persistence

Background agents can lose context between steps. lean-ctx prevents this:

```
# Background agent step 1: Research
ctx_overview("migrate database from SQLite to PostgreSQL")
ctx_search("sqlite", "src/")
ctx_knowledge(action="remember", category="discovery", content="15 files reference SQLite directly")

# Background agent step 2: Plan (context persists via lean-ctx)
ctx_knowledge(action="recall", query="SQLite references")  # Returns the discovery from step 1
ctx_session(action="task", value="SQLite to PostgreSQL migration [25%]")

# Background agent step 3: Implement
ctx_read("src/db/connection.rs", "full")  # Cached from step 1's overview
```

### Task Tracking

```
# Set task at the start
ctx_session(action="task", value="Database migration [0%]")

# Update as you go
ctx_session(action="task", value="Database migration [50%] — schema converted")

# Complete
ctx_session(action="task", value="Database migration [100%]")
```

## Codex-Specific Workflow

### Interactive Mode

```bash
codex
```

In interactive mode, lean-ctx tools are available directly:

```
> Use ctx_read to read src/main.rs in map mode
> Search for "async fn" using ctx_search
> Show me the impact of changing src/models/user.rs
```

### One-Shot Mode

```bash
codex "add error handling to all API endpoints"
```

lean-ctx provides context compression during the one-shot execution:

1. Codex reads files → lean-ctx caches and compresses
2. Codex runs commands → shell hook compresses output
3. Codex makes edits → native edit tools (lean-ctx handles reads)

### Quiet Mode

```bash
codex --quiet "fix the failing tests"
```

lean-ctx works in quiet mode without any additional output.

## TOML Configuration Details

### Full config.toml Example

```toml
# ~/.codex/config.toml

# MCP servers
[mcp_servers.lean-ctx]
command = "lean-ctx"
args = []

# Other Codex settings can coexist
# [other_section]
# ...
```

### Custom Binary Path

If lean-ctx is installed in a non-standard location:

```toml
[mcp_servers.lean-ctx]
command = "/path/to/lean-ctx"
args = []
```

### Custom CODEX_HOME

```bash
export CODEX_HOME=/custom/path
lean-ctx init --agent codex  # Writes to /custom/path/config.toml
```

## Token Savings

| Operation | Without lean-ctx | With lean-ctx | Savings |
|-----------|-----------------|---------------|---------|
| File read (cached) | ~2000 tokens | ~13 tokens | 99.4% |
| File read (map) | ~2000 tokens | ~400 tokens | 80% |
| `git diff` | ~1200 tokens | ~200 tokens | 83% |
| `cargo test` | ~2000 tokens | ~300 tokens | 85% |
| `npm run build` | ~1500 tokens | ~250 tokens | 83% |

Monitor savings:

```bash
lean-ctx gain --live
```

## Advanced Features

### Context Packs for Codex Tasks

Bundle context for complex tasks:

```
# Create a context pack
ctx_pack("create", "auth-refactor")

# Later, in a new Codex session
ctx_pack("load", "auth-refactor")
```

### Code Review with Codex

```
# Get PR context
ctx_shell("git diff main...HEAD --stat")

# Review changes
ctx_review("src/api/handler.rs")

# Check for code smells
ctx_smells("src/api/handler.rs")
```

### Multi-Step Refactoring

```
# Step 1: Analyze
ctx_overview("rename UserService to AccountService across the codebase")
ctx_refactor("references", "src/services/user.rs", "UserService")

# Step 2: Plan
ctx_impact("src/services/user.rs")
ctx_knowledge(action="remember", category="decision", content="Renaming UserService to AccountService — 12 files affected")

# Step 3: Execute
ctx_refactor("rename", "src/services/user.rs", "UserService", "AccountService")
```

## Troubleshooting

### MCP server not connecting

```bash
# Check config.toml
cat ~/.codex/config.toml

# Verify TOML syntax
# Should have [mcp_servers.lean-ctx] section

# Test MCP server
echo '{"jsonrpc":"2.0","method":"initialize","params":{"capabilities":{}},"id":1}' | lean-ctx mcp

# Re-run setup
lean-ctx init --agent codex
```

### Custom CODEX_HOME not detected

```bash
# Ensure CODEX_HOME is set
echo $CODEX_HOME

# Re-run with explicit path
CODEX_HOME=/your/path lean-ctx init --agent codex
```

### TOML parsing errors

If Codex reports config errors:

```bash
# Validate TOML syntax
python3 -c "import tomllib; tomllib.load(open('$HOME/.codex/config.toml', 'rb'))"

# Common issues:
# - Missing quotes around paths with spaces
# - Duplicate section headers
# - Trailing commas (not valid in TOML)
```

### Sandbox blocking lean-ctx

If the sandbox prevents lean-ctx from accessing files:

```bash
# Ensure lean-ctx data dir is accessible
ls -la ~/.lean-ctx/

# Check if the binary is accessible from sandbox
which lean-ctx
```

### Shell hook not working in sandbox

The shell hook may not activate in Codex's sandbox. lean-ctx's MCP tools (`ctx_shell`) still work:

```
# Use ctx_shell instead of direct shell commands
ctx_shell("git status")
ctx_shell("cargo test")
```

> **Codex Desktop / Codex Cloud:** these clients' models instinctively reach for a
> tool literally named `shell` (or `bash`) rather than `ctx_shell`. lean-ctx
> registers a `shell` tool that is a 1:1 alias of `ctx_shell` — same pattern
> compression, same allowlist — so commands stay compressed even when the model
> never learns the `ctx_` prefix. Nothing to configure; it ships in every profile.

### Tools not available

```bash
# Verify Codex sees the MCP server
codex --list-mcp-servers

# Check binary path
which lean-ctx

# Re-install
lean-ctx init --agent codex
```

## Further Reading

- [lean-ctx Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [Codex CLI Documentation](https://github.com/openai/codex)
- [MCP Protocol](https://modelcontextprotocol.io/)
