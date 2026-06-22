# Gemini CLI + lean-ctx Integration Guide

Complete guide to setting up and optimally using lean-ctx with Gemini CLI (Google's AI coding agent).

## Overview

| Property | Value |
|----------|-------|
| Integration mode | **Hybrid** (MCP reads + shell hooks) |
| Config file | `~/.gemini/settings.json` |
| Rules file | `~/.gemini/GEMINI.md` (shared, appended) |
| Setup command | `lean-ctx init --agent gemini` |

## Quick Setup

```bash
# One command — configures MCP, rules, and shell hook
lean-ctx init --agent gemini

# Verify
lean-ctx doctor
```

lean-ctx auto-detects Gemini CLI by checking for `~/.gemini/`.

## Manual Setup

### Step 1: MCP Server Registration

lean-ctx configures `~/.gemini/settings.json` with the Gemini-specific format:

```json
{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx",
      "trust": true
    }
  }
}
```

> **Key difference**: Gemini CLI uses a `"trust": true` field to auto-approve the MCP server. This is set automatically by `lean-ctx init --agent gemini`.

If the file already exists, lean-ctx merges the `lean-ctx` entry into the existing `mcpServers` object without modifying other servers.

### Step 2: Rules (GEMINI.md)

Gemini CLI reads `~/.gemini/GEMINI.md` for global instructions. lean-ctx **appends** its rules to this file (shared format — your existing content is preserved):

```markdown
# Your existing GEMINI.md content here
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

The section between `<!-- lean-ctx-rules -->` and `<!-- /lean-ctx -->` is auto-managed by lean-ctx. When you run `lean-ctx setup`, it updates only this section while preserving everything else in your `GEMINI.md`.

### Step 3: Shell Hook

Gemini CLI has shell access. lean-ctx installs compression hooks:

```bash
lean-ctx init --global
```

## Gemini-Specific Optimizations

### Long Context Window

Gemini models have large context windows (1M+ tokens). lean-ctx still provides value because:

1. **Cost reduction** — fewer tokens = lower API costs, even if they fit in the window
2. **Focus** — compressed context helps the model focus on relevant information
3. **Speed** — less data to process = faster responses
4. **Caching** — re-reads cost ~13 tokens regardless of file size

### Gemini's Thinking Mode

When using Gemini's thinking mode with lean-ctx:

```
# Provide structured context for better reasoning
ctx_overview("analyze the authentication flow for security vulnerabilities")

# Use map mode to give Gemini structural understanding
ctx_read("src/auth/mod.rs", "map")

# Let Gemini's thinking work on the compressed, focused context
```

### Multi-Turn Conversations

Gemini CLI supports multi-turn conversations. lean-ctx enhances this with session state:

```
# Turn 1: Research
ctx_search("fn authenticate", "src/")
ctx_read("src/auth/jwt.rs", "map")

# Turn 2: Gemini remembers the lean-ctx context from Turn 1
# Re-reads cost ~13 tokens
ctx_read("src/auth/jwt.rs", "full")  # Almost free from cache

# Turn 3: Document findings
ctx_knowledge(action="remember", category="insight", content="JWT uses HS256, should migrate to RS256")
```

## Workflow Examples

### Code Review

```
# Get an overview of changes
ctx_shell("git diff --stat HEAD~5")

# Read changed files in map mode
ctx_read("src/api/handler.rs", "map")
ctx_read("src/api/middleware.rs", "map")

# Deep dive into specific changes
ctx_read("src/api/handler.rs", "full")

# Review with ctx_review
ctx_review("src/api/handler.rs")
```

### Feature Development

```
# Start with project orientation
ctx_overview("add rate limiting to API endpoints")

# Understand the codebase structure
ctx_tree("src/api/", 3)

# Find existing patterns
ctx_semantic_search("how are API endpoints defined?")
ctx_search("rate.*limit", "src/")

# Check what needs to change
ctx_graph("impact", "src/api/router.rs")

# Document decisions
ctx_knowledge(action="remember", category="decision", content="Using token bucket algorithm with Redis backend for rate limiting")
```

### Debugging

```
# Find error patterns
ctx_search("unwrap\\(\\)", "src/")

# Trace call paths
ctx_callgraph("src/api/handler.rs", "handle_request")

# Check error handling
ctx_read("src/error.rs", "signatures")
```

## Project-Level Configuration

### Per-Project GEMINI.md

Gemini CLI also reads `GEMINI.md` in the project root. You can add project-specific lean-ctx rules there:

```markdown
# Project: My API Server

## lean-ctx project rules
- Always use `map` mode for files in `vendor/`
- Use `ctx_overview` at the start of every task
```

### .lean-ctx.toml

Create a project-level configuration:

```toml
# .lean-ctx.toml (project root)
shell_activation = "always"
```

## Token Savings with Gemini

Even with Gemini's large context window, lean-ctx provides measurable savings:

| Operation | Raw | With lean-ctx | Savings |
|-----------|-----|---------------|---------|
| File read (cached) | ~2000 tok | ~13 tok | 99.4% |
| File read (map) | ~2000 tok | ~400 tok | 80% |
| `git status` | ~800 tok | ~120 tok | 85% |
| `git log -20` | ~600 tok | ~150 tok | 75% |
| `npm test` output | ~3000 tok | ~400 tok | 87% |

Monitor in real-time:

```bash
lean-ctx gain --live
```

## Troubleshooting

### MCP server not connecting

```bash
# Check settings.json
cat ~/.gemini/settings.json | python3 -m json.tool

# Verify "trust": true is set
grep -A5 "lean-ctx" ~/.gemini/settings.json

# Test MCP server
echo '{"jsonrpc":"2.0","method":"initialize","params":{"capabilities":{}},"id":1}' | lean-ctx mcp

# Re-run setup
lean-ctx init --agent gemini
```

### Rules not appearing in GEMINI.md

```bash
# Check GEMINI.md content
cat ~/.gemini/GEMINI.md

# Look for lean-ctx section
grep "lean-ctx" ~/.gemini/GEMINI.md

# Re-inject rules
lean-ctx setup
```

### Gemini not using lean-ctx tools

1. Check that `~/.gemini/settings.json` has the MCP config
2. Verify `"trust": true` is set for the lean-ctx server
3. Restart Gemini CLI
4. Try explicitly: "Use the ctx_read tool to read this file"

### Shell hook not active

```bash
# Check hook status
echo $LEAN_CTX_ACTIVE

# Re-install
lean-ctx init --global

# Restart shell
exec $SHELL
```

### "trust" field missing

The `trust` field is Gemini-specific. Without it, Gemini may prompt for approval on every tool call:

```bash
# Re-run setup to ensure trust is set
lean-ctx init --agent gemini

# Or manually add to settings.json
# The lean-ctx entry should have "trust": true
```

## Gemini CLI + GEMINI.md vs. Global Rules

| File | Scope | How lean-ctx uses it |
|------|-------|---------------------|
| `~/.gemini/settings.json` | Global MCP config | MCP server registration |
| `~/.gemini/GEMINI.md` | Global agent rules | Shared rules (appended) |
| `./GEMINI.md` (project root) | Project rules | Not auto-managed by lean-ctx |

## Further Reading

- [lean-ctx Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [Gemini CLI Documentation](https://github.com/google-gemini/gemini-cli)
- [MCP Protocol](https://modelcontextprotocol.io/)
