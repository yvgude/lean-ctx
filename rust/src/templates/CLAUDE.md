{RULES_MARKER}

PREFER lean-ctx MCP tools over native equivalents for token savings:

| PREFER | OVER | Why |
|--------|------|-----|
| `ctx_read(path, mode)` | Read / cat / head / tail | Session caching, 10 read modes, re-reads cost ~13 tokens |
| `ctx_shell(command)` | Bash (shell commands) | Pattern-based compression for git, npm, cargo, docker, tsc |
| `ctx_search(pattern, path)` | Grep / rg | Compact context, token-efficient results |
| `ctx_tree(path, depth)` | ls / find | Compact directory maps with file counts |

## ctx_read Modes

- `auto` — auto-select optimal mode (recommended default)
- `anchored` — full text + `N:hh|` anchors (use for files you will edit via ctx_patch)
- `full` — cached verbatim read
- `map` — deps + API signatures (use for context-only files)
- `signatures` — API surface only
- `diff` — changed lines only (after edits)
- `task` — task-relevant filtering
- `reference` — quote-friendly minimal excerpts
- `aggressive` — syntax stripped
- `entropy` — Shannon + Jaccard filtering
- `lines:N-M` — specific range

## File Editing

Anchored editing saves output tokens: `ctx_read(path, mode="anchored")` → `ctx_patch(path, op, line, hash, new_text)`.
Patch by `N:hh|` anchor — never reproduce old text byte-for-byte. Batch several edits via `ops:[…]`
(all-or-nothing); `op=create` writes new files; a stale anchor returns CONFLICT with fresh anchors — retry once.
Native Edit/StrReplace stay fine when the host provides them. `ctx_edit(path, old_string, new_string)`
is the legacy str-replace fallback (power profile / ctx_call).
Write, Delete have no lean-ctx equivalent — use them normally.

## Session Documentation

After significant work, document progress for session continuity:
- `ctx_knowledge(action="remember", category="decision", content="<what and why>")`
- `ctx_session(action="task", value="<task description> [progress%]")`
- If blocked: `ctx_knowledge(action="remember", category="blocker", content="...")`

When you see `[CHECKPOINT]` in tool output → document current task status immediately.
