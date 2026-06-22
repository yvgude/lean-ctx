# Claude Code + lean-ctx Integration Guide

Complete guide to setting up and optimally using lean-ctx with Claude Code (Anthropic's CLI coding agent).

## Overview

| Property | Value |
|----------|-------|
| Integration mode | **Hybrid** (MCP reads + shell hooks) |
| Config file | `~/.claude.json` |
| Instructions | `<!-- lean-ctx -->` block in `~/.claude/CLAUDE.md` |
| Skill file | `~/.claude/skills/lean-ctx/SKILL.md` (loads on demand) |
| Setup command | `lean-ctx init --agent claude` |

> **Since 3.8:** there is no `~/.claude/rules/lean-ctx.md` anymore. Claude Code loads every
> rules file unconditionally at session start, which duplicated the instructions in each
> session (12k+ token memory footprints). `lean-ctx setup` removes the legacy file and
> maintains a compact block in `~/.claude/CLAUDE.md` instead; detail docs live in the
> on-demand skill.

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

> **Note**: The `autoApprove` list includes all read-only and safe tools so Claude Code doesn't prompt for confirmation on every call. lean-ctx supports 77 tools total — the full list is auto-configured.

If `claude mcp add-json` is not available (older Claude Code versions), lean-ctx falls back to directly writing `~/.claude.json`.

### Step 2: Agent Instructions (CLAUDE.md block + skill)

lean-ctx maintains a marker-delimited block in `~/.claude/CLAUDE.md`:

```markdown
<!-- lean-ctx -->
<!-- lean-ctx-claude-v3 -->
## lean-ctx — Context Runtime

Always prefer lean-ctx MCP tools over native equivalents:
- `ctx_read` instead of `Read` / `cat` (cached, 10 modes, re-reads ~13 tokens)
- `ctx_shell` instead of `bash` / `Shell` (95+ compression patterns)
- `ctx_search` instead of `Grep` / `rg` (compact results)
- `ctx_tree` instead of `ls` / `find` (compact directory maps)
- Native Edit/StrReplace stay unchanged. If Edit requires Read and Read is unavailable, use `ctx_edit(path, old_string, new_string)` instead.
- Write, Delete, Glob — use normally.

Read modes: full (edit), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->
```

Detail documentation (mode selection, session memory, proactive tools) lives in the
skill at `~/.claude/skills/lean-ctx/SKILL.md`, which Claude loads only when needed.

Both are written automatically:

```bash
lean-ctx setup
```

If a legacy `~/.claude/rules/lean-ctx.md` from an older install still exists, `setup`
removes it (it would be loaded in *every* session on top of the CLAUDE.md block).

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

In addition to the global block in `~/.claude/CLAUDE.md`, you can add project-specific rules in `CLAUDE.md` at your project root. lean-ctx will append its shared rules section if not already present.

### CLAUDE.md Integration

If you have a project-level `CLAUDE.md`, lean-ctx can inject its rules there too using the SharedMarkdown format:

```markdown
# Your existing project rules here
...

# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules -->
## Mode Selection
- Editing the file? → `full` first, then `diff` for re-reads
- Context only? → `map` or `signatures`
...
<!-- /lean-ctx -->
```

The section between `<!-- lean-ctx-rules -->` and `<!-- /lean-ctx -->` is managed by lean-ctx and auto-updated.

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

### Instructions not being applied

```bash
# Check the CLAUDE.md block exists
grep -A2 'lean-ctx' ~/.claude/CLAUDE.md

# Check the on-demand skill exists
ls ~/.claude/skills/lean-ctx/SKILL.md

# Reinstall block + skill
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
