{RULES_MARKER} (Global)

LeanCTX (lean-ctx) is the Context Gateway for AI Systems. Use its MCP tools for file reads, shell commands and code searches: select task-relevant views, reuse cached context and recover detail from authorized sources or archives. Your host owns the task, agent loop and model.

## Tool Replacement Rules

| NEVER use | ALWAYS use instead |
|-----------|-------------------|
| `Read` / `View` / `cat` / `head` / `tail` | `ctx_read(path, mode)` — cached read modes and compact references for eligible re-reads |
| `Bash` (any shell command) | `ctx_shell(command)` — pattern compression for git/npm/cargo/docker |
| `Grep` / `Search` / `rg` | `ctx_search(pattern, path)` — compact, token-efficient results |
| `ListFiles` / `ListDirectory` / `ls` / `find` | `ctx_tree(path, depth)` — compact directory maps |

## How to Use

```
ctx_read("src/main.rs")              # instead of Read("src/main.rs")
ctx_read("src/lib.rs", mode="map")   # deps + API signatures
ctx_read("src/lib.rs", mode="signatures") # API surface only
ctx_shell("git status")              # instead of Bash("git status")
ctx_search("pub fn", "src/")         # instead of Grep("pub fn", "src/")
ctx_tree(".", 2)                     # instead of ListFiles(".")
```

Edits: `ctx_read(path, mode="anchored")` → `ctx_patch(path, op, line, hash, new_text)` — patch by anchor,
never echo old text (`op=create` for new files). Native Write/Edit, where the host provides them, stay fine.

CRITICAL: Every time you reach for Read, Bash, Grep, or ListFiles — stop and use the lean-ctx MCP equivalent instead. This is not optional.
