---
name: lean-ctx
description: Context Engineering for AI Agents — 78 MCP tools, 10 read modes, 95+ shell patterns, tree-sitter AST for 18 languages. Compresses LLM context by up to 99%. Use when reading files, running shell commands, searching code, or exploring directories. Auto-installs if not present.
---

# lean-ctx — Context Engineering for AI Agents

## Setup

```bash
which lean-ctx || curl -fsSL https://raw.githubusercontent.com/yvgude/lean-ctx/main/skills/lean-ctx/scripts/install.sh | bash
lean-ctx setup
```

## Core Tools (10 always visible)

| Tool | Purpose |
|------|---------|
| `ctx_read(path, mode)` | Read file with compression and caching |
| `ctx_search(pattern, path)` | Search code with compressed results |
| `ctx_shell(command)` | Run shell with compressed output |
| `ctx_tree(path, depth)` | Directory listing |
| `ctx_edit(path, old, new)` | Search-and-replace editing |
| `ctx_session(action)` | Session state and persistence |
| `ctx_knowledge(action)` | Project knowledge across sessions |
| `ctx_overview(task)` | Task-relevant project map |
| `ctx_graph(action)` | Code relationships and impact |
| `ctx_call(name, args)` | Invoke any tool by name |

## Shell Hook (use instead of raw exec)

```bash
lean-ctx -c "git status"
lean-ctx -c "cargo test"
lean-ctx -c "npm install"
lean-ctx ls src/
```

## ctx_read Modes

| Mode | When |
|------|------|
| `full` | Files you will edit |
| `map` | Context-only (deps + exports) |
| `signatures` | API surface only |
| `diff` | After edits (changed lines) |
| `aggressive` | Large files, syntax-stripped |
| `entropy` | Shannon filtering |
| `task` | Task-relevant lines |
| `lines:N-M` | Specific range |
| `auto` | System selects optimal |

Re-reads cost ~13 tokens. fresh=true bypasses cache.

## File Editing

Use native Edit/StrReplace. If unavailable, use `ctx_edit` immediately.

## More Tools (via ctx_call or ctx_load_tools)

Architecture: ctx_symbol, ctx_callgraph, ctx_impact, ctx_architecture, ctx_routes, ctx_smells
  ↳ "What breaks if I change this file/class/type?" → ctx_impact (file-level blast radius; resolves same-package/namespace type usage with no import for C#, Java, Go and Kotlin). "Who calls this function?" → ctx_callgraph (symbol-level).
Multi-agent: ctx_agent, ctx_share, ctx_task, ctx_handoff, ctx_workflow
Verify: ctx_benchmark, ctx_verify, ctx_proof, ctx_review
Batch: ctx_fill, ctx_execute, ctx_expand, ctx_pack

Full docs: https://leanctx.com/docs
