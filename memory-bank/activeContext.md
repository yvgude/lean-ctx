# Active Context

## Fokus (jetzt)
- Phase 3 ist implementiert: `lean-ctx serve` (Streamable HTTP) + Library-API (`ContextEngine`).
- Website-Deploy-Worktree ist manifest-driven (keine Hardcodes für Tool-/Read-Mode-Counts), i18n Gate grün.

## Offene Punkte
- Repo-Cleanup: `website/generated/mcp-tools.json` tracken (main worktree), Deploy-Worktree muss `node_modules` removals committen.
- GitLab Sync: Epic/Subtickets Status für Phase 3 + Website-SSOT final nachziehen.

# Active Context

## Fokus (jetzt)
- Phase 2 „Agent Runtime“ (Workflow + Gatekeeper + Evidence) fertig verdrahtet und getestet
- SSOT Manifest Generator + Drift-Test im Main-Worktree ergänzt

## Aktuelle Änderungen (Worktree)
- Workflow Core + `ctx_workflow` Tool
- `server.rs`: Gatekeeper + Evidence receipts + Cost/Heatmap Hooks + 6 Tool-Dispatch-Arme
- SSOT: `core/mcp_manifest` + `gen_mcp_manifest` + `website/generated/mcp-tools.json` + Drift-Test

## Nächste Schritte
- GitLab Tickets/Status synchronisieren (Epics/Subtickets updaten/abschließen)
- finaler Cleanup (nur intended Änderungen, keine Artefakte)
- erst dann committen (kein Push zu GitHub; GitLab optional)

# Active Context — lean-ctx

## Current State

**Version**: v2.1.0 (March 25, 2026)
**Status**: Stable, deployed on all platforms

## Recent Changes

### v2.1.0 — Real Benchmark Engine (March 25, 2026)
- Replaced estimation-based benchmarks with real project-file measurements
- New core/preservation.rs: AST-based information preservation scoring (tree-sitter)
- Rewritten core/benchmark.rs: scans up to 50 files, measures tokens/latency/quality per mode
- Session simulation with real numbers (15 reads, 10 cache hits, 8 shell commands)
- Three output formats: Terminal (ANSI), Markdown (shareable), JSON
- MCP ctx_benchmark extended with action=project for project-wide benchmarks
- All 21 MCP tools verified functional
- Published to crates.io, GitHub Releases, AUR PKGBUILDs updated

### v2.0.0 — Context Continuity Protocol (March 25, 2026)
- Context Continuity Protocol (CCP): cross-session memory that persists across chats, context compactions, and IDE restarts
- LITM-aware positioning: critical context placed in high-attention zones (beginning/end) based on LLM attention research
- 2 new MCP tools: ctx_session, ctx_wrapped (21 total)
- 3 new CLI commands: wrapped, sessions, benchmark
- Reproducible benchmark engine vs. raw/cursorrules baselines
- Session state stored as JSON in ~/.lean-ctx/sessions/
- Auto-checkpoint integration saves session state
- Idle cache expiry preserves session before clearing

### v1.9.0 — Intelligence Engine (March 25, 2026)
- 9 new MCP tools: ctx_smart_read, ctx_delta, ctx_dedup, ctx_fill, ctx_intent, ctx_response, ctx_context, ctx_graph, ctx_discover
- 19 MCP tools total (up from 10)
- 90+ shell compression patterns across 34 categories (47 pattern modules)
- New pattern modules: ansible, aws, bazel, bun, cmake, composer, deno, flutter, helm, mix, mysql, prisma, psql, swift, systemd, zig
- Myers diff algorithm for ctx_delta
- Language-aware aggressive compression (tree-sitter)
- UTF-8 safety fix for multi-byte characters (GitHub Issue #4)
- AI tool hook integration (Cursor, Claude Code, Windsurf)
- Website transparency audit: all % claims now qualified with methodology
- Dashboard redesign matching leanctx.com design system

### v1.8.2 — Security Fix (March 25, 2026)
- Fixed tee log privacy issue (GitHub Issue #3)
- `tee_on_error` default changed to `false` (opt-in)
- 7-pattern regex redaction for sensitive data in tee logs
- Auto-cleanup of tee logs after 24h

### v1.8.0 — Windows Support (March 25, 2026)
- Full Windows support with native binaries
- PowerShell shell hook via `lean-ctx init --global`
- GitHub Actions CI for Windows binaries

## Active Decisions

- **No env!("CARGO_PKG_VERSION")**: Version is hardcoded in 7+ files. Known maintenance burden but avoids build-time macro complexity.
- **Website not in git**: Deployed via rsync to server. Build locally with Node.js 22+.
- **Node.js version**: Use `/opt/homebrew/opt/node@22/bin` for Astro builds.
- **Savings transparency**: All % claims on website are per-operation, with methodology section on features page.
- **CCP session storage**: JSON files in ~/.lean-ctx/sessions/ with latest.json pointer. Max 20 findings, 10 decisions (FIFO).
- **LITM positioning**: Critical context (task, decisions) at beginning; findings and progress at end of context window.

## Next Steps / Ideas

- [ ] Consider using `env!("CARGO_PKG_VERSION")` to centralize version management
- [ ] Context Autopilot: automatic context window management
- [ ] GitHub PR Badge action for token savings visibility
- [ ] Cloud Dashboard / SaaS features for monetization
- [ ] Tree-sitter grammar expansion (more languages)
- [ ] Multi-Agent Memory (shared context across parallel agents)
- [ ] Semantic Router (model selection based on query intent)
