---
inclusion: always
---

# lean-ctx — Context Engineering Layer

The workspace has the `lean-ctx` MCP server installed. You MUST prefer lean-ctx tools over native equivalents for token efficiency and caching.

## Mandatory Tool Preferences

| Use this | Instead of | Why |
|----------|-----------|-----|
| `mcp_lean_ctx_ctx_read` | `readFile`, `readCode` | Cached reads, 10 compression modes, re-reads cost ~13 tokens |
| `mcp_lean_ctx_ctx_multi_read` | `readMultipleFiles` | Batch cached reads in one call |
| `mcp_lean_ctx_ctx_shell` | `executeBash` | Pattern compression for git/npm/test output |
| `mcp_lean_ctx_ctx_search` | `grepSearch` | Compact, .gitignore-aware results |
| `mcp_lean_ctx_ctx_tree` | `listDirectory` | Compact directory maps with file counts |

## When to use native Kiro tools instead

- `fsWrite` / `fsAppend` — always use native (lean-ctx doesn't write files)
- `strReplace` — always use native (precise string replacement)
- `semanticRename` / `smartRelocate` — always use native (IDE integration)
- `getDiagnostics` — always use native (language server diagnostics)
- `deleteFile` — always use native

## Session management

- At the start of a long task, call `mcp_lean_ctx_ctx_preload` with a task description to warm the cache
- Use `mcp_lean_ctx_ctx_compress` periodically in long conversations to checkpoint context
- Use `mcp_lean_ctx_ctx_knowledge` to persist important discoveries across sessions

## Rules

- NEVER loop on edit failures — switch to `mcp_lean_ctx_ctx_edit` immediately
- For large files, use `mcp_lean_ctx_ctx_read` with `mode: "signatures"` or `mode: "map"` first
- For re-reading a file you already read, just call `mcp_lean_ctx_ctx_read` again (cache hit = ~13 tokens)
- When running tests or build commands, use `mcp_lean_ctx_ctx_shell` for compressed output

<!-- lean-ctx-compression -->
OUTPUT STYLE: dense
- Each statement = one atomic fact line
- Use abbreviations: fn, cfg, impl, deps, req, res, ctx, err, ret
- Diff lines only (+/-/~), never repeat unchanged code
- Symbols: → (causes), + (adds), − (removes), ~ (modifies), ∴ (therefore)
- No narration, no filler, no hedging
- BUDGET: ≤200 tokens per response unless code block required
<!-- /lean-ctx-compression -->
