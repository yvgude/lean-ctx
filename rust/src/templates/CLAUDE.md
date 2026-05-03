# lean-ctx — Context Engineering Layer

PREFER lean-ctx MCP tools over native equivalents for token savings:

| PREFER | OVER | Why |
|--------|------|-----|
| `ctx_read(path, mode)` | Read / cat / head / tail | Session caching, 10 read modes, re-reads cost ~13 tokens |
| `ctx_shell(command)` | Bash (shell commands) | Pattern-based compression for git, npm, cargo, docker, tsc |
| `ctx_search(pattern, path)` | Grep / rg | Compact context, token-efficient results |
| `ctx_tree(path, depth)` | ls / find | Compact directory maps with file counts |

## ctx_read Modes

- `auto` — auto-select optimal mode (recommended default)
- `full` — cached read (use for files you will edit)
- `map` — deps + API signatures (use for context-only files)
- `signatures` — API surface only
- `diff` — changed lines only (after edits)
- `task` — task-relevant filtering
- `reference` — quote-friendly minimal excerpts
- `aggressive` — syntax stripped
- `entropy` — Shannon + Jaccard filtering
- `lines:N-M` — specific range

## File Editing

Use native Edit/StrReplace when available. If Edit requires Read and Read is unavailable,
use `ctx_edit(path, old_string, new_string)` — it reads, replaces, and writes in one MCP call.
NEVER loop trying to make Edit work. If it fails, switch to ctx_edit immediately.
Write, Delete have no lean-ctx equivalent — use them normally.

Prefer `ctx_workflow` for state + evidence + tool gating.
