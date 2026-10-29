# Claude Code + lean-ctx Integration Guide

> **Status: Available first-class local setup path.** LeanCTX attaches to
> Claude Code through local Runtime wiring; its coverage is limited to observable
> context behavior. See the [canonical positioning](../POSITIONING_CANONICAL.md)
> for current product scope and claim boundaries.

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
> rules file unconditionally at session start, which duplicated instructions in each
> session. `lean-ctx setup` removes the legacy file and
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
      "args": [],
      "autoApprove": [
        "ctx_read",
        "ctx_shell",
        "shell",
        "ctx_search",
        "ctx_glob",
        "ctx_tree",
        "ctx_compose",
        "ctx_call",
        "ctx_patch",
        "…"
      ],
      "alwaysLoad": true
    }
  }
}
```

> **Note**: The `autoApprove` list (abbreviated above) covers the advertised lean-ctx tools so Claude Code doesn't prompt for confirmation on every call. The full list comes from `auto_approve_tools()` in `rust/src/core/editor_registry/writers/shared.rs` and is written automatically; it never names removed or non-advertised tools.

If `claude mcp add-json` is not available (older Claude Code versions), lean-ctx falls back to directly writing `~/.claude.json`.

### Step 2: Agent Instructions (CLAUDE.md block + skill)

lean-ctx maintains a marker-delimited block in `~/.claude/CLAUDE.md`:

```markdown
<!-- lean-ctx -->
<!-- lean-ctx-claude-v10 -->
## lean-ctx — Context Runtime

When the `ctx_*` MCP tools are listed in this session, prefer them over native equivalents:
- `ctx_read` instead of `Read` / `cat` for exploration (cached, 10 modes incl. map/signatures)
- `ctx_shell` instead of `bash` / `Shell` (pattern-based output compression)
- `ctx_search` instead of `Grep` / `rg` (compact results)
- `ctx_tree` instead of `ls` / `find` (compact directory maps)
- Edits: `ctx_read(mode="anchored")` → `ctx_patch` (line+hash anchors, never echo old text; `op=create` for new files). `ctx_edit` (str_replace) is the legacy power-profile fallback.

Native `Read` → `Edit`/`StrReplace` stays fully supported — the edit gate requires a
prior native Read of the same file path. Auto memory under
`~/.claude/projects/<slug>/memory/` also uses native Read/Edit (not MCP resources).
Write, Delete, Glob — use normally.
If no `ctx_*` tools are listed in this session, use the native tools throughout.

Read modes: anchored (edit), full (verbatim), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->
```

The block (shown for MCP mode; Replace mode writes a variant that denies native
Grep/Glob) routes edits to the anchored editor (`ctx_patch` is advertised in the
lazy core for Claude Code) while keeping the guard semantics: Claude Code enforces a
*path-keyed* read-before-write gate on Edit/Write, so a natively-edited file must have
been read with the **native** Read tool (lean-ctx's `read_redirect = auto` keeps that
gate intact, see [#637](https://github.com/yvgude/lean-ctx/issues/637)). And in sessions
where the lean-ctx MCP server is not connected, no `ctx_*` tools exist — the block says
explicitly to fall back to native tools instead of chasing unavailable ones.

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

This enables transparent handling for supported command patterns, including git, npm, cargo, docker, kubectl, and terraform.

### Read compression under the read-before-write gate

Two settings work together so Claude Code keeps its native edit safety while
unchanged reads can use the configured compact representation:

- **`read_redirect = auto`** (default): on guard hosts (Claude Code / CodeBuddy) the
  PreToolUse Read redirect stays **off**, so the native Read runs on the real path and
  the path-keyed read-before-write gate records it — native Edit/Write keep working
  ([#637](https://github.com/yvgude/lean-ctx/issues/637)).
- **`read_dedup = auto`** (default): a PostToolUse hook (`lean-ctx hook read-dedup`,
  matcher `Read` only) replaces the *result* of a **re-read of an unchanged file** with
  a compact `[unchanged]` stub via the documented `updatedToolOutput` channel. First
  reads stay byte-identical (edit safety: `old_string` always comes from real content),
  the file and the gate are untouched, and every failure path passes the original
  result through. Set `read_dedup = off` to disable, or `on` to dedup on every host.

### Step 4: SKILL.md (Optional)

lean-ctx installs a skill file at `~/.claude/skills/lean-ctx/SKILL.md` that provides Claude Code with detailed knowledge about all lean-ctx capabilities, modes, and best practices.

## Optimal Workflow

### Session Start

When Claude Code starts a new session, it should:

1. **Call `ctx_compose(task)` first** — ranked files and inline source for the task (replaces the removed `ctx_overview`)
2. **Use `ctx_read(path, "signatures")` or `"map"`** for context files — API surface, dependencies, exports
3. **Use `ctx_read(path, "anchored")`** for files it will edit, then `ctx_patch`

### During Development

```
Orient on a task         → ctx_compose(task="how does auth work?")
Read file for context    → ctx_read("src/auth.rs", "signatures")
Read file to edit        → ctx_read("src/auth.rs", "anchored") → ctx_patch
Re-read after editing    → ctx_delta("src/auth.rs")
Search for patterns      → ctx_search(pattern="fn authenticate", path="src/")
Find by meaning          → ctx_search(action="semantic", query="token refresh")
Exact symbol             → ctx_search(action="symbol", name="authenticate")
Run shell commands       → Uses shell hook automatically (or ctx_shell)
Callers of a function    → ctx_callgraph(action="callers", symbol="authenticate")
File-level impact        → ctx_graph(action="impact", path="src/auth.rs")
```

Removed tools from older prompts (`ctx_overview`, `ctx_semantic_search`,
`ctx_impact`, …) return their successor when called; see
[Removed in the focus release](../reference/appendix-mcp-tools.md#removed-in-the-focus-release).

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
When context grows large  → ctx_compress (compresses the read cache)
Check token savings       → ctx_metrics
Per-tool cost breakdown   → ctx_cost
File-level savings        → ctx_heatmap
```

## Multi-Agent Handoff

Claude Code supports multi-agent workflows via lean-ctx. `ctx_agent` is a local
compatibility tool: it is not advertised in `tools/list` but stays callable
(directly or via `ctx_call`).

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

# LeanCTX — Context Gateway for AI Systems
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
# Supported commands can return summarized output when run through the hook:
git status
git log --oneline -20
cargo test
npm install
docker ps
```

The shell hook handles supported commands without changing how Claude Code invokes them; the resulting output varies with command, repository state, and configured rules.

## Further Reading

- [lean-ctx Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [Session Memory Guide](https://leanctx.com/docs/session-memory/)
- [Claude Code Documentation](https://docs.anthropic.com/en/docs/claude-code)
