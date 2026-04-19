# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v9 -->

PREFER lean-ctx MCP tools over native equivalents for token savings:

## Tool preference:
| PREFER | OVER | Why |
|--------|------|-----|
| `ctx_read(path, mode)` | `Read` / `cat` | Cached, 10 read modes, re-reads ~13 tokens |
| `ctx_shell(command)` | `Shell` / `bash` | Pattern compression for git/npm/cargo output |
| `ctx_search(pattern, path)` | `Grep` / `rg` | Compact, token-efficient results |
| `ctx_tree(path, depth)` | `ls` / `find` | Compact directory maps |
| `ctx_edit(path, old_string, new_string)` | `Edit` (when Read unavailable) | Search-and-replace without native Read |

## ctx_read modes:
- `auto` — auto-select optimal mode (recommended default)
- `full` — cached read (files you edit)
- `map` — deps + exports (context-only files)
- `signatures` — API surface only
- `diff` — changed lines after edits
- `aggressive` — maximum compression (context only)
- `entropy` — highlight high-entropy fragments
- `task` — IB-filtered (task relevant)
- `reference` — quote-friendly minimal excerpts
- `lines:N-M` — specific range

## File editing:
Use native Edit/StrReplace if available. If Edit requires Read and Read is unavailable, use ctx_edit.
Write, Delete, Glob → use normally. NEVER loop on Edit failures — switch to ctx_edit immediately.

## Proactive (use without being asked):
- `ctx_overview(task)` at session start
- `ctx_compress` when context grows large
<!-- /lean-ctx -->