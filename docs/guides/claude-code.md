# Claude Code + lean-ctx Integration Guide

Complete guide to setting up and optimally using lean-ctx with Claude Code (Anthropic's CLI coding agent).

## Overview

| Property | Value |
|----------|-------|
| Integration mode | **Hybrid** (MCP reads + shell hooks) |
| Config file | `~/.claude.json` |
| Rules file | `~/.claude/rules/lean-ctx.md` (dedicated) |
| Rules format | DedicatedMarkdown |
| Skill file | `~/.claude/skills/lean-ctx/SKILL.md` |
| Setup command | `lean-ctx init --agent claude` |

## Quick Setup

```bash
# One command — configures MCP, rules, shell hook, and skill
lean-ctx init --agent claude

# Verify
lean-ctx doctor
```

That's it. lean-ctx auto-detects Claude Code by checking for the `claude` binary in `$PATH` or the existence of `~/.claude.json` / `~/.claude/`.

## Manual Setup

If you prefer manual configuration or need to customize the setup.

### Step 1: MCP Server Registration

lean-ctx registers itself via `claude mcp add-json --scope user` when available. The resulting entry in `~/.claude.json`:

```json
{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx",
      "env": {
        "LEAN_CTX_DATA_DIR": "~/.lean-ctx"
      },
      "autoApprove": [
        "ctx_read",
        "ctx_shell",
        "ctx_search",
        "ctx_tree",
        "ctx_overview",
        "ctx_preload",
        "ctx_compress",
        "ctx_metrics",
        "ctx_session",
        "ctx_knowledge",
        "ctx_agent",
        "ctx_share",
        "ctx_analyze",
        "ctx_semantic_search",
        "ctx_graph",
        "ctx_refactor",
        "ctx_expand",
        "ctx_impact",
        "ctx_review",
        "ctx_pack"
      ]
    }
  }
}
```

> **Note**: The `autoApprove` list includes all read-only and safe tools so Claude Code doesn't prompt for confirmation on every call. lean-ctx supports 72 tools total — the full list is auto-configured.

If `claude mcp add-json` is not available (older Claude Code versions), lean-ctx falls back to directly writing `~/.claude.json`.

### Step 2: Agent Rules

lean-ctx creates `~/.claude/rules/lean-ctx.md` with dedicated rules:

```markdown
# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v10 -->

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

To manually create this file:

```bash
mkdir -p ~/.claude/rules
# lean-ctx setup will write the content automatically
lean-ctx setup
```

### Step 3: Shell Hook

Claude Code has shell access, so lean-ctx installs compression hooks for common commands:

```bash
# Activate shell hook (done by lean-ctx setup)
lean-ctx init --global
```

This enables transparent compression for 56 pattern modules (git, npm, cargo, docker, kubectl, terraform, and more).

### Step 4: SKILL.md (Optional)

lean-ctx installs a skill file at `~/.claude/skills/lean-ctx/SKILL.md` that provides Claude Code with detailed knowledge about all lean-ctx capabilities, modes, and best practices.

## Optimal Workflow

### Session Start

When Claude Code starts a new session, it should:

1. **Call `ctx_overview(task)`** — fast project orientation with task-relevant context
2. **Use `ctx_read(path, "map")`** for context files — dependencies, exports, key signatures
3. **Use `ctx_read(path, "full")`** only for files it will edit

### During Development

```
Read file for context    → ctx_read("src/auth.rs", "map")
Read file to edit        → ctx_read("src/auth.rs", "full")
Re-read after editing    → ctx_read("src/auth.rs", "diff")
Search for patterns      → ctx_search("fn authenticate", "src/")
Run shell commands       → Uses shell hook automatically (or ctx_shell)
Find by meaning          → ctx_semantic_search("how does auth work?")
Check code relationships → ctx_graph("impact", "src/auth.rs")
```

### Session Documentation

After significant work (implementation, bugfix, refactoring):

```
ctx_knowledge(action="remember", category="decision", content="Chose JWT over sessions for stateless auth")
ctx_session(action="task", value="Implement auth module [75%]")
```

When lean-ctx emits `[CHECKPOINT]` (after 30+ tool calls without documentation):

```
ctx_session(action="task", value="Current task status description")
```

### Context Management

```
When context grows large  → ctx_compress (creates memory checkpoint)
Check token savings       → ctx_metrics
Per-tool cost breakdown   → ctx_cost
File-level savings        → ctx_heatmap
```

## Multi-Agent Handoff

Claude Code supports multi-agent workflows via lean-ctx:

```
# Agent A records findings
ctx_knowledge(action="remember", category="insight", content="Config parsing uses TOML with JSONC fallback")

# Agent A hands off to Agent B
ctx_agent(action="handoff", target="agent-b", context="Continue implementing the config migration")

# Agent B receives context and continues
ctx_agent(action="sync")
```

The knowledge graph and session state persist across agents, so Agent B sees all of Agent A's discoveries and decisions.

## Knowledge Persistence

lean-ctx maintains a temporal knowledge graph that survives across sessions:

```
# Remember a decision
ctx_knowledge(action="remember", category="decision", content="Use connection pooling with max 10 connections")

# Recall later (even in a new session)
ctx_knowledge(action="recall", query="connection pooling")

# Search knowledge by time
ctx_knowledge(action="timeline", range="today")

# Full-text search across all knowledge
ctx_knowledge(action="search", query="database configuration")
```

Knowledge categories: `decision`, `discovery`, `blocker`, `progress`, `insight`.

## Advanced Configuration

### Project-Level Config

Create `.lean-ctx.toml` in your project root to override global settings:

```toml
# Project-specific lean-ctx configuration
shell_activation = "always"      # or "agents-only"
```

### Per-Project Rules

In addition to global rules (`~/.claude/rules/lean-ctx.md`), you can add project-specific rules in `CLAUDE.md` at your project root. lean-ctx will append its shared rules section if not already present.

### CLAUDE.md Integration

If you have a project-level `CLAUDE.md`, lean-ctx can inject its rules there too using the SharedMarkdown format:

```markdown
# Your existing project rules here
...

# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v10 -->
## Mode Selection
- Editing the file? → `full` first, then `diff` for re-reads
- Context only? → `map` or `signatures`
...
<!-- /lean-ctx -->
```

The section between `<!-- lean-ctx-rules-v10 -->` and `<!-- /lean-ctx -->` is managed by lean-ctx and auto-updated.

## Troubleshooting

### MCP server not connecting

```bash
# Check if lean-ctx is in PATH
which lean-ctx

# Verify MCP config
cat ~/.claude.json | python3 -m json.tool

# Test MCP server directly
echo '{"jsonrpc":"2.0","method":"initialize","params":{"capabilities":{}},"id":1}' | lean-ctx mcp

# Re-run setup
lean-ctx init --agent claude
```

### Rules not being applied

```bash
# Check rules file exists
cat ~/.claude/rules/lean-ctx.md

# Check rules version
grep "lean-ctx-rules-v" ~/.claude/rules/lean-ctx.md

# Update rules to latest version
lean-ctx setup
```

### Shell compression not working

```bash
# Check if shell hook is active
echo $LEAN_CTX_ACTIVE

# Re-install shell hook
lean-ctx init --global

# Restart your shell
exec $SHELL
```

### `claude mcp add-json` fails

This can happen if the Claude Code binary is in an untrusted path. Options:

```bash
# Trust the path explicitly
export LEAN_CTX_TRUST_CLAUDE_PATH=1
lean-ctx init --agent claude

# Or set up manually by editing ~/.claude.json directly
```

### High token usage despite lean-ctx

```bash
# Check if agent is using lean-ctx tools
lean-ctx gain --live

# Verify the agent sees the rules
# In Claude Code, check that ctx_read is being used instead of native Read
```

## CLI Integration

Claude Code also benefits from lean-ctx's CLI compression when running shell commands:

```bash
# These commands are automatically compressed when run through Claude Code:
git status                    # ~800 → ~120 tokens
git log --oneline -20         # ~600 → ~150 tokens
cargo test                    # ~2000 → ~300 tokens
npm install                   # ~1500 → ~200 tokens
docker ps                     # ~400 → ~80 tokens
```

The shell hook intercepts these commands transparently — no changes needed to how Claude Code invokes them.

## Further Reading

- [lean-ctx Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [Session Memory Guide](https://leanctx.com/docs/session-memory/)
- [Claude Code Documentation](https://docs.anthropic.com/en/docs/claude-code)
