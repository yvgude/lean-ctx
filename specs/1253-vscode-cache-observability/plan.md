# Plan: VS Code Cache Observability  (refs #1253)

> Implementation plan for `./spec.md`.

**Goal:** Expose MCP SessionCache and Content Dedup as separate, backward-compatible metrics in runtime diagnostics and VS Code.
**Architecture:** Sample the existing process-local dedup summary when the MCP live snapshot is written. Preserve existing fields and add three dedup fields; embed that snapshot under `mcp_cache` in `stats json`. Keep rendering consumers tolerant of missing fields.
**Tech Stack:** Rust, serde_json, TypeScript, VS Code webview HTML/JavaScript.

## Global Constraints
- Do not change SessionCache, Content Dedup, compression, or provider-cache behavior.
- Keep all JSON additions optional and additive for older runtime/extension combinations.
- Never merge the two cache rates; their denominators and semantics differ.
- No mock data, no placeholders, no stubs.
- Output determinism: no new timestamps, counters, or randomness in cacheable tool bodies (#498).

## File Structure
| File | Responsibility | New/Modify |
|------|----------------|------------|
| `rust/src/tools/server_metrics.rs` | Persist dedup counters in `mcp-live.json` | modify |
| `rust/src/cli/config_cmd.rs` | Embed snapshot in `stats json`; render separate CLI sections; unit tests | modify |
| `rust/src/tools/registered/ctx_cache.rs` | Show process-local dedup summary in cache status | modify |
| `vscode-extension/src/leanctx.ts` | Parse optional cache metrics with zero fallback | modify |
| `vscode-extension/src/sidebar/panel.html` | Display both rates and exact counts | modify |
| `vscode-extension/src/statusbar.ts` | Add both cache layers to tooltip | modify |
| `specs/1253-vscode-cache-observability/*` | Intent, plan, and task traceability | new |

## Impact (run impact analysis first)
- `server_metrics.rs`: 8 dependents through server modules, updater, library/main, and `ctx_session`.
- `config_cmd.rs`: 2 dependents through CLI dispatch and library.
- `leanctx.ts`: 9 dependents, including commands, extension activation, sidebar provider, status bar, editor signals, dashboard, and URI handling.
- Affected tests: CLI cache rendering, stats JSON embedding, existing ctx_read dedup hook tests, TypeScript compilation, webview script syntax.
- Affected modules: MCP live metrics, CLI stats/cache diagnostics, registered cache tool, VS Code data adapter and stats UI.

## Self-Review (fill before implementing)
- Spec coverage: every EARS criterion maps to T1-T4 in `tasks.md`.
- Placeholder scan: no TODO, TBD, mock, or guessed provider metrics.
- Determinism: snapshot fields are numeric state; no new dynamic data enters cached read bodies.
- Cleanup: generated `node_modules` remains ignored; no scratch files committed.
