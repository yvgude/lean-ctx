# Spec: VS Code Cache Observability  (refs #1253)

> SDD spec-anchored: this file is the source of truth for **intent**.
> Code + tests enforce it. When requirements change, update the spec first.

## Problem / Why
VS Code uses lean-ctx through MCP, but the extension and cache diagnostics expose only
`SessionCache` hits. Context Kernel content deduplication can independently collapse a
repeated `ctx_read` to an `already in context` stub, so the displayed hit rate can imply
that caching is unused even while effective re-read elimination is working.

Provider prompt-cache savings are a separate metric. Copilot model requests that do not
traverse the lean-ctx proxy cannot be observed or credited as provider cache reads.

## Goal
Expose MCP SessionCache and Content Dedup as separate, backward-compatible metrics in the
runtime diagnostics and VS Code extension.

## Acceptance Criteria (EARS)
- WHEN an MCP live snapshot is written, THE runtime SHALL persist content-dedup reads, hits, and tokens saved separately from SessionCache metrics.
- WHEN `lean-ctx stats json` is requested, THE CLI SHALL expose the MCP live snapshot under an additive `mcp_cache` object without removing or renaming existing fields.
- WHEN `lean-ctx cache stats` is requested, THE CLI SHALL display SessionCache and Content Dedup as separate layers.
- WHEN `ctx_cache status` is requested after deduplication activity, THE tool SHALL display content-dedup reads, hits, hit rate, and tokens saved.
- WHEN the VS Code dashboard receives cache metrics, THE extension SHALL display both hit rates and exact hit/read counts.
- IF an older runtime omits `mcp_cache` or its deduplication fields, THEN THE extension SHALL render zero or unavailable values without failing.
- THE implementation SHALL preserve existing SessionCache hit semantics and provider-cache accounting.

## Out of Scope
- Proxying GitHub Copilot model traffic through lean-ctx.
- Estimating provider prompt-cache hits when no provider turn was observed.
- Combining SessionCache and Content Dedup into one ambiguous hit rate.
- Changing cache eviction, invalidation, or compression behavior.

## Verification (deterministic first)
- `cargo test --lib mcp_cache_stats_distinguish_session_cache_and_content_dedup`
- `cargo test --lib stats_json_embeds_mcp_cache_snapshot`
- `cargo test --lib tools::ctx_read::dedup_hook::tests`
- `cargo fmt --check`
- `cargo clippy --all-features -- -D warnings`
- `npm ci && npm run compile`
- Extract the webview script and run `node --check`.
- `scripts/preflight.sh fast`

## Links
- Tracking issue: #1253
- Plan: ./plan.md · Tasks: ./tasks.md
- Contracts touched: additive fields in local `mcp-live.json` and `stats json`
