# Pi Coding Agent + lean-ctx Integration Guide

Complete guide to setting up and optimally using lean-ctx with [Pi Coding Agent](https://github.com/badlogic/pi-mono).

## Overview

| Property | Value |
|----------|-------|
| Integration mode | **Hybrid** (CLI tools + optional MCP bridge) |
| Package | [`pi-lean-ctx`](https://pi.dev/packages/pi-lean-ctx) |
| npm | [`pi-lean-ctx`](https://www.npmjs.com/package/pi-lean-ctx) |
| Rules file | `AGENTS.md` (auto-generated in project root) |
| Setup command | `lean-ctx init --agent pi` |

## Quick Setup

```bash
# Install the Pi extension
pi install npm:pi-lean-ctx

# Or use lean-ctx's setup
lean-ctx init --agent pi

# Verify
lean-ctx doctor
```

## Tool Modes

pi-lean-ctx supports two operational modes:

### Additive Mode (Default)

Pi's built-in tools (`read`, `bash`, `ls`, `find`, `grep`) remain available alongside `ctx_*` tools. The agent can choose either set.

### Replace Mode

Disables Pi builtins — only `ctx_*` tools available:

```bash
export LEAN_CTX_PI_MODE=replace
```

## Available Tools

### CLI-backed Tools (Always Available)

| Tool | Replaces | What it does |
|------|----------|-------------|
| `ctx_read` | `read` | Smart mode selection (full/map/signatures) based on file type and size |
| `ctx_shell` | `bash` | All shell commands compressed via lean-ctx's 95+ patterns |
| `ctx_grep` | `grep` | Results grouped and compressed via ripgrep + lean-ctx |
| `ctx_find` | `find` | File listings compressed and .gitignore-aware |
| `ctx_ls` | `ls` | Directory output compressed |
| `lean_ctx` | — | Direct lean-ctx CLI access (overview, session, knowledge, gain) |

Pi's `edit` and `write` builtins remain unchanged in both modes.

### MCP Tools (Optional)

Enable advanced MCP tools by setting:

```bash
export LEAN_CTX_PI_ENABLE_MCP=1
```

Or during setup:

```bash
lean-ctx init --agent pi --mode mcp
```

This spawns lean-ctx as an embedded MCP server and registers additional tools:

| Tool | Purpose |
|------|---------|
| `ctx_session` | Session state management and persistence |
| `ctx_knowledge` | Project knowledge graph with temporal validity |
| `ctx_semantic_search` | Find code by meaning, not exact text |
| `ctx_overview` | Codebase overview and architecture analysis |
| `ctx_repomap` | PageRank-based repo map (most important symbols) |
| `ctx_callgraph` | Multi-hop call graph traversal and risk analysis |
| `ctx_impact` | Blast radius analysis for code changes |
| `ctx_pack` | Context packaging (export project as AI-friendly format) |
| `ctx_compress` | Manual compression control |
| `ctx_metrics` | Token savings dashboard |
| `ctx_multi_read` | Batch file reads |

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LEAN_CTX_PI_MODE` | `additive` | `additive` or `replace` |
| `LEAN_CTX_PI_ENABLE_MCP` | `0` | Set to `1` to enable MCP bridge |
| `LEAN_CTX_PI_MCP_TOOLS` | (all) | Comma-separated list of MCP tools to register |
| `LEAN_CTX_EMBEDDING_MODEL` | `minilm` | Embedding model: `minilm`, `jina-code`, `nomic` |

### Config file (`config.json`)

If you only use lean-ctx through Pi, you can keep every setting in one file
instead of juggling env vars and `~/.lean-ctx/config.toml`. Create:

```
~/.pi/agent/extensions/pi-lean-ctx/config.json
```

```json
{
  "mode": "replace",
  "enableMcp": true,
  "binary": "/opt/lean-ctx/bin/lean-ctx",
  "env": {
    "LEAN_CTX_COMPRESSION": "aggressive"
  }
}
```

| Key | Equivalent to | Notes |
|-----|---------------|-------|
| `mode` | `LEAN_CTX_PI_MODE` | `additive` (default) or `replace` |
| `enableMcp` | `LEAN_CTX_PI_ENABLE_MCP` | Start the embedded MCP bridge |
| `binary` | `LEAN_CTX_BIN` | Absolute path to the `lean-ctx` binary |
| `env` | — | Extra env forwarded to every `lean-ctx` subprocess; use it to override `~/.lean-ctx/config.toml` engine settings (the engine honours `LEAN_CTX_*` vars) |

**Precedence (most explicit wins):** an explicit `LEAN_CTX_PI_*` / `LEAN_CTX_BIN`
environment variable overrides `config.json`, which overrides the built-in
default. This keeps a shared, file-only config working with no env vars while
still allowing ad-hoc env overrides on a single machine. Run `/lean-ctx` inside
Pi to see which config file (if any) was loaded.

### AGENTS.md

lean-ctx auto-generates an `AGENTS.md` file in your project root with Pi-optimized instructions:

```bash
lean-ctx init --agent pi
# Creates AGENTS.md with lean-ctx tool usage patterns
```

The `AGENTS.md` instructs Pi to prefer `ctx_*` tools over builtins for token efficiency.

## Recommended Workflow

### Basic (CLI-only)

Best for simple tasks — no MCP overhead:

```
You (in Pi): "Read the auth module and find security issues"

Pi uses:
  ctx_read src/auth/mod.rs    → compressed, ~60% smaller
  ctx_grep "password" src/    → grouped results
  ctx_shell "cargo clippy"    → compressed output
```

### Advanced (MCP-enabled)

Best for complex tasks — full lean-ctx power:

```
You (in Pi): "Understand the architecture and find what's affected by changing the User model"

Pi uses:
  ctx_overview                  → project architecture
  ctx_repomap                   → most important symbols
  ctx_callgraph action=risk symbol=User → impact analysis
  ctx_semantic_search query="user model" → find related code
  ctx_knowledge recall          → previous findings about User
```

### Session Continuity

lean-ctx persists context across Pi sessions:

```
# Session 1: investigate a bug
Pi → ctx_knowledge remember --category=blocker --content="Race condition in auth middleware"

# Session 2 (next day): lean-ctx auto-restores context
Pi → ctx_knowledge recall → "Race condition in auth middleware" (from yesterday)
```

## Complementary Pi Extensions

Users have found these extensions work well alongside pi-lean-ctx:

| Extension | Purpose | Synergy with lean-ctx |
|-----------|---------|----------------------|
| `pi-git` | Git operations | lean-ctx compresses git output |
| `pi-search` | Web search | Combine with ctx_knowledge for persistence |
| `pi-test` | Test runner | lean-ctx compresses test output |

### Coexisting with AFT and magic-context

lean-ctx, [AFT](https://github.com/cortexkit/aft) and
[magic-context](https://github.com/cortexkit/magic-context) compose cleanly when
each owns a distinct concern: **AFT** symbol-aware file ops, **lean-ctx** context
compression + the session cache, **magic-context** long-horizon memory/compaction.

Keep lean-ctx in its default **additive** mode (don't set `LEAN_CTX_PI_MODE=replace`)
so it never contends for AFT's hoisted `read`/`write`/`edit`/`bash` slots. If two
extensions register the same tool name (e.g. magic-context's `ctx_expand`), the
extension that loads second would normally crash Pi — pi-lean-ctx instead **skips
the clashing tool with a warning** and keeps loading (#359). To control the split:

```bash
# Hand specific names to the other extension:
export LEAN_CTX_PI_DISABLE_TOOLS="ctx_memory,ctx_expand,ctx_search"
# …or prefix all bridge tools so nothing collides:
export LEAN_CTX_PI_TOOL_PREFIX="lc_"   # ctx_expand → lc_ctx_expand
```

Run `/lean-ctx` inside Pi to see exactly which tools were registered, handed off,
or skipped. Full reference:
[pi-lean-ctx README → Coexisting with AFT and magic-context](https://github.com/yvgude/lean-ctx/blob/main/packages/pi-lean-ctx/README.md#coexisting-with-aft-and-magic-context).

## Troubleshooting

### lean-ctx binary not found

```bash
# Ensure lean-ctx is in PATH
which lean-ctx

# If not installed
curl -fsSL https://leanctx.com/install.sh | sh
```

### MCP tools not appearing

```bash
# Check if MCP is enabled
echo $LEAN_CTX_PI_ENABLE_MCP  # Should be "1"

# Check MCP server health
lean-ctx doctor integrations
```

### High latency on first use

lean-ctx builds indexes on first run. Subsequent uses are cached:

```bash
# Pre-build index
lean-ctx index build
```

### Proxy configuration

If using lean-ctx's API proxy:

```bash
lean-ctx proxy enable
# Sets ANTHROPIC_BASE_URL, OPENAI_BASE_URL, GEMINI_API_BASE_URL
# All three providers are configured (not just Gemini)
```

## Performance

Typical token savings with pi-lean-ctx:

| Operation | Without lean-ctx | With lean-ctx | Savings |
|-----------|-----------------|---------------|---------|
| Read large file (1000 LOC) | ~4000 tokens | ~400 tokens | 90% |
| `git status` | ~200 tokens | ~50 tokens | 75% |
| `cargo test` output | ~2000 tokens | ~100 tokens | 95% |
| `grep` results (50 matches) | ~1500 tokens | ~300 tokens | 80% |

## Further Reading

- [pi-lean-ctx README](https://github.com/yvgude/lean-ctx/tree/main/packages/pi-lean-ctx)
- [Pi Coding Agent Docs](https://github.com/badlogic/pi-mono)
- [lean-ctx Documentation](https://leanctx.com/docs)
