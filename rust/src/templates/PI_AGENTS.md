# lean-ctx — Token Optimization for Pi

This project uses the **pi-lean-ctx** extension. It exposes `ctx_*` tools backed by **lean-ctx**,
and runs an embedded MCP bridge (on by default) that holds a **persistent session cache**.

## What to do (as Pi agent)

Prefer the `ctx_*` tools over Pi's built-ins — only the `ctx_*` tools are compressed and cached;
the native `read`/`bash`/`grep`/`find`/`ls` are **not** routed through lean-ctx in additive mode.

| Prefer | Over (native) | Why |
|--------|---------------|-----|
| `ctx_read` | `read`, `cat`/`head`/`tail` | Cached + compressed; unchanged re-reads cost ~13 tokens |
| `ctx_shell` | `bash` | Shell output compressed via 95+ patterns |
| `ctx_search` | `grep` | Compact, ranked matches |
| `ctx_glob` | `find` | Compressed, .gitignore-aware file matching |
| `ctx_tree` | `ls` | Compact directory maps |

- Use `ctx_shell` for commands with side effects (build/test/git/etc.); set `raw=true` when exact
  output matters.
- Use `ctx_read` with `mode=full` for files you will edit. For line ranges pass `offset`/`limit`
  (aliases of `start_line`) or `mode=lines:N-M` — all cached through the bridge, so repeated reads
  stay cheap.

## Advanced lean-ctx commands

Prefer the `lean_ctx` tool (installed by the extension) to run `lean-ctx` directly:

- `lean-ctx overview`
- `lean-ctx session …`
- `lean-ctx knowledge …`
- `lean-ctx gain` / `lean-ctx stats`
- `lean-ctx index …`

## MCP bridge

The embedded bridge is on by default and shows up in `/lean-ctx` (it reports `connected` plus a
tool count). To force the one-shot CLI path (no cross-call cache), set `LEAN_CTX_PI_ENABLE_MCP=0`.
