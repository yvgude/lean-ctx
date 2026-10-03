{RULES_MARKER} (Global)

You have the lean-ctx MCP server available. You MUST use it for ALL file reads, shell commands, and code searches. Using native tools wastes tokens — lean-ctx compresses everything.

## Tool Replacement Rules

| NEVER use | ALWAYS use instead |
|-----------|-------------------|
| `Read` / `View` / `cat` / `head` / `tail` | `ctx_read(path, mode)` — cached, 10 read modes, re-reads ~13 tokens |
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
