# pi-lean-ctx

[Pi Coding Agent](https://github.com/badlogic/pi-mono) extension that routes all tool output through [lean-ctx](https://leanctx.com) for **60–90% token savings**.

## What it does

Overrides Pi's built-in tools to route them through `lean-ctx`:

| Tool | Compression |
|------|------------|
| `bash` | All shell commands compressed via lean-ctx's 90+ patterns |
| `read` | Smart mode selection (full/map/signatures) based on file type and size |
| `grep` | Results grouped and compressed via ripgrep + lean-ctx |
| `find` | File listings compressed and .gitignore-aware |
| `ls` | Directory output compressed |

## Install

```bash
# 1. Install lean-ctx (if not already installed)
cargo install lean-ctx
# or: brew tap yvgude/lean-ctx && brew install lean-ctx

# 2. Install the Pi package
pi install pi-lean-ctx
```

## Binary Resolution

The extension locates the `lean-ctx` binary in this order:

1. `LEAN_CTX_BIN` environment variable
2. `~/.cargo/bin/lean-ctx`
3. `~/.local/bin/lean-ctx` (Linux) or `%APPDATA%\Local\lean-ctx\lean-ctx.exe` (Windows)
4. `/usr/local/bin/lean-ctx` (macOS/Linux)
5. `lean-ctx` on PATH

## Smart Read Modes

The `read` tool automatically selects the optimal lean-ctx mode:

| File Type | Size | Mode |
|-----------|------|------|
| `.md`, `.json`, `.toml`, `.yaml`, etc. | Any | `full` |
| Code files (`.rs`, `.ts`, `.py`, etc.) | < 24 KB | `full` |
| Code files | 24–160 KB | `map` (deps + API signatures) |
| Code files | > 160 KB | `signatures` (AST extraction) |
| Other files | < 48 KB | `full` |
| Other files | > 48 KB | `map` |

## Slash Command

Use `/lean-ctx` in Pi to check which binary is being used.

## Links

- [lean-ctx](https://leanctx.com) — The Cognitive Filter for AI Engineering
- [GitHub](https://github.com/yvgude/lean-ctx)
- [Discord](https://discord.gg/pTHkG9Hew9)
