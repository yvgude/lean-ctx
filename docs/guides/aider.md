# Aider + lean-ctx Integration Guide

Complete guide to setting up and optimally using lean-ctx with Aider (AI pair programming in your terminal).

## Overview

| Property | Value |
|----------|-------|
| Integration mode | **MCP-only** (no shell hooks) |
| Rules file | Dedicated `.md` (via lean-ctx rules) |
| Setup command | `lean-ctx init --agent aider` |

## Quick Setup

```bash
# Configure lean-ctx for Aider
lean-ctx init --agent aider

# Verify
lean-ctx doctor
```

## How Aider Uses lean-ctx

Aider operates differently from IDE-based agents. It uses its own repository map and file management. lean-ctx complements Aider by providing:

1. **Compressed file reads** — token savings on file context
2. **Semantic search** — find relevant code by meaning
3. **Knowledge persistence** — maintain decisions across sessions
4. **Code graph** — understand impact of changes

## Configuration

### Aider MCP Setup

Aider supports MCP servers. Configure lean-ctx in your `.aider.conf.yml`:

```yaml
# ~/.aider.conf.yml
mcp-servers:
  - lean-ctx:
      command: lean-ctx
      args: []
```

> **Note**: lean-ctx auto-detects its data directory at runtime — don't hardcode `LEAN_CTX_DATA_DIR` unless you intentionally relocate it. Running `lean-ctx init --agent aider` writes this config for you.

Or pass it via command line:

```bash
aider --mcp-server "lean-ctx:lean-ctx"
```

### Agent Rules

lean-ctx injects dedicated rules that guide Aider to use lean-ctx tools:

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

## lean-ctx as Repo Map Complement

Aider has its own repo map feature. lean-ctx's `ctx_read` with mode `map` or `signatures` provides a complementary view:

### Aider Repo Map vs. lean-ctx Map Mode

| Feature | Aider Repo Map | lean-ctx `map` mode |
|---------|----------------|---------------------|
| Scope | Full repository | Single file |
| Content | Function/class names | Dependencies + exports + key signatures |
| Token cost | Grows with repo size | Fixed per file |
| Caching | Per session | Persistent across sessions |

### Using Both Together

```
# Aider's repo map gives you the big picture
/map

# lean-ctx fills in structural details for specific files
ctx_read("src/database/connection.rs", "map")
# Returns: deps, exports, key function signatures — ~60-80% fewer tokens than full read

# For API surface only
ctx_read("src/database/connection.rs", "signatures")
# Returns: public function signatures only — ~70-90% fewer tokens
```

## Workflow: Large Refactors with Aider + lean-ctx

### Step 1: Understand the Codebase

```
# Start with lean-ctx overview
ctx_overview("refactor database layer to use connection pooling")

# Search for relevant code
ctx_search("connection", "src/database/")
ctx_semantic_search("where are database connections created?")

# Map out the files you'll touch
ctx_read("src/database/mod.rs", "map")
ctx_read("src/database/pool.rs", "map")
ctx_read("src/database/query.rs", "map")
```

### Step 2: Analyze Impact

```
# What depends on the files you're changing?
ctx_graph("impact", "src/database/connection.rs")

# Find all references
ctx_refactor("references", "src/database/connection.rs", "ConnectionPool")
```

### Step 3: Add Files to Aider

Based on lean-ctx's analysis, add the relevant files to Aider:

```
/add src/database/connection.rs src/database/pool.rs src/database/mod.rs
```

### Step 4: Make Changes

Let Aider handle the edits. lean-ctx continues to provide compressed reads and search during the refactoring.

### Step 5: Document

```
ctx_knowledge(action="remember", category="decision", content="Refactored to connection pooling with max 10 connections, r2d2 crate")
ctx_session(action="task", value="Database connection pooling refactor [100%]")
```

## Token Savings with Aider

Aider sends full file contents to the LLM. lean-ctx helps by:

1. **Pre-filtering context** — use `map`/`signatures` to understand structure before adding files
2. **Cached reads** — if Aider triggers a re-read through MCP, it costs ~13 tokens
3. **Search efficiency** — `ctx_search` returns compact results vs. raw grep output
4. **Knowledge persistence** — avoid re-discovering things in new sessions

## Advanced: Pre-Prompt Integration

You can use lean-ctx output in Aider's pre-prompt:

```bash
# Generate a context summary and pass to Aider
lean-ctx read src/main.rs -m map > /tmp/ctx.md
aider --message-file /tmp/ctx.md src/main.rs
```

Or use lean-ctx's CLI for quick context gathering before starting Aider:

```bash
# Understand the project structure
lean-ctx ls src/ --depth 3

# Find relevant files
lean-ctx grep "async fn" src/

# Read key files in map mode
lean-ctx read src/lib.rs -m map
```

## Troubleshooting

### MCP connection issues

```bash
# Verify lean-ctx binary is accessible
which lean-ctx

# Test MCP server
echo '{"jsonrpc":"2.0","method":"initialize","params":{"capabilities":{}},"id":1}' | lean-ctx mcp

# Check Aider MCP config
aider --show-mcp-servers
```

### Tools not available in Aider

Aider's MCP support may have limitations on which tools are exposed. If specific tools aren't available:

1. Check Aider's MCP documentation for supported features
2. Use lean-ctx CLI as a fallback:

```bash
# Instead of MCP ctx_read
lean-ctx read src/file.rs -m map

# Instead of MCP ctx_search
lean-ctx grep "pattern" src/
```

### Session state not persisting

lean-ctx session state is tied to the project directory. Make sure you're running Aider from the same project root:

```bash
cd /path/to/your/project
aider
```

### Aider ignoring lean-ctx rules

Aider may not process lean-ctx rules the same way as IDE-based agents. Use explicit prompts:

```
Use ctx_read instead of reading files directly. Use mode "map" for files I won't edit.
```

## Further Reading

- [lean-ctx Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [Aider Documentation](https://aider.chat/docs/)
- [MCP Protocol](https://modelcontextprotocol.io/)
