# Tasks: VS Code Cache Observability  (refs #1253)

> Atomic, individually testable. Each task pairs a change with its verification.
> Commit reference: `fix(vscode): expose effective cache usage refs specs/1253-vscode-cache-observability`.

- [x] **T1 — Persist effective MCP cache metrics**
  - Files: `rust/src/tools/server_metrics.rs`
  - Do: add content-dedup reads, hits, and tokens saved to the additive MCP live snapshot.
  - Verify: focused Rust compilation and `cargo test --lib tools::ctx_read::dedup_hook::tests`.
- [x] **T2 — Expose separate runtime diagnostics**
  - Files: `rust/src/cli/config_cmd.rs`, `rust/src/tools/registered/ctx_cache.rs`
  - Do: embed `mcp_cache` in stats JSON and render SessionCache/Content Dedup separately.
  - Verify: `cargo test --lib mcp_cache_stats_distinguish_session_cache_and_content_dedup` and `cargo test --lib stats_json_embeds_mcp_cache_snapshot`.
- [x] **T3 — Surface metrics in VS Code**
  - Files: `vscode-extension/src/leanctx.ts`, `vscode-extension/src/sidebar/panel.html`, `vscode-extension/src/statusbar.ts`
  - Do: parse optional metrics, show both rates, and expose exact counts in tooltips.
  - Verify: `npm run compile` and extracted webview script through `node --check`.
- [x] **T4 — Preserve compatibility and semantics**
  - Files: all touched runtime and extension files.
  - Do: retain existing fields, zero-fallback absent fields, and avoid provider-cache estimation.
  - Verify: legacy snapshot assertions, existing dedup tests, and diff review.

## Done gate
- [x] All EARS criteria covered by a task.
- [x] `cargo fmt --check` and `cargo clippy --all-targets --all-features -- -D warnings` green.
- [x] Fast preflight-equivalent gates green; Windows cross-check skipped because target is not installed.
- [ ] Full local lib suite: 9,044 passed; 15 unrelated HOME/env-lock failures in unchanged modules.
- [x] Tracking issue #1253 updated; spec referenced in commit and PR #1290.
