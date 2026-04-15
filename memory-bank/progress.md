# Progress

## Fertig
- **SSOT Manifest**: Generator + CI Gate, `website/generated/mcp-tools.json`.
- **42 Tools / 10 Read Modes**: Runtime + Website aligned.
- **Rails**: Workflow State Machine + Gatekeeper + Evidence + `ctx_workflow`.
- **Observability**: `ctx_cost`, `ctx_heatmap` wired + default-on (local-first).
- **Phase 3**:
  - `lean_ctx::http_server` (Streamable HTTP)
  - CLI `lean-ctx serve`
  - Library-first `lean_ctx::engine::ContextEngine`
  - Tests für Streamable HTTP + Engine Call

## Ausstehend (Prozess)
- GitLab Issues/Epics finalisieren (Status/AC/DoD, Referenzen auf Evidence).
- Finaler Cleanup/Commit-Strategie über beide Worktrees (main + deploy).

# Progress

## Done
- 42 granular MCP Tools wired + Tool defs aktualisiert
- Workflow Runtime (core + store) implementiert
- Tool Gatekeeper (list_tools + call_tool enforcement) implementiert
- Evidence receipts (tool-call receipts + manual evidence) implementiert
- Cost Attribution + Heatmap Instrumentation wired
- SSOT Manifest Generator + Drift Test + generiertes `website/generated/mcp-tools.json`
- `cargo test` grün

## In Progress
- GitLab Ticket-Sync (Issue-States/AC/DoD final abgleichen)
- repo cleanup vor dem finalen Commit

## Pending (Phase 3, bewusst später)
- Library-first API Surface + HTTP server mode (`lean-ctx serve`)

# Progress — lean-ctx

## What Works

### Core (Rust Binary)
- [x] MCP Server (stdio) with 21 tools
- [x] Session caching with MD5 hashing (re-reads = 13 tokens)
- [x] 6 read modes: full, map, signatures, diff, aggressive, entropy
- [x] Line range support (`lines:N-M`)
- [x] Tree-sitter AST extraction for 14 languages
- [x] 90+ shell compression patterns across 34 categories (47 pattern modules)
- [x] Token Dense Dialect (TDD) with CRP mode
- [x] Persistent stats (`~/.lean-ctx/stats.json`)
- [x] Visual terminal dashboard (`lean-ctx gain` with --graph, --daily, --json)
- [x] Web dashboard (`lean-ctx dashboard` at localhost:3333)
- [x] 23 auto-installed shell aliases/functions
- [x] Cross-platform: macOS, Linux, Windows (native binaries)
- [x] PowerShell shell hook support
- [x] Doctor command with 8 diagnostic checks
- [x] Discover command for finding missed savings
- [x] Session adoption statistics
- [x] Config management (`config set/init/show`)
- [x] Tee log management (`tee list/clear/show`) with redaction
- [x] Auto-checkpoint at configurable intervals
- [x] Smart read with automatic mode selection (ctx_smart_read)
- [x] Myers diff for incremental updates (ctx_delta)
- [x] Cross-file deduplication analysis (ctx_dedup)
- [x] Priority-based context filling with token budget (ctx_fill)
- [x] Semantic intent detection with auto-file selection (ctx_intent)
- [x] Bi-directional response compression (ctx_response)
- [x] Multi-turn context manager (ctx_context)
- [x] Project intelligence graph (ctx_graph)
- [x] UTF-8 safe string handling throughout
- [x] Context Continuity Protocol (CCP) — cross-session memory
- [x] LITM-aware positioning for optimal attention placement
- [x] ctx_session MCP tool (status/load/save/task/finding/decision/reset/list/cleanup)
- [x] ctx_wrapped MCP tool (shareable savings report)
- [x] Wrapped CLI command with period filtering
- [x] Sessions CLI command (list/show/cleanup)
- [x] Benchmark CLI command with real project measurements (run/report/--json)
- [x] Session state JSON persistence (~/.lean-ctx/sessions/)
- [x] Information preservation scoring (core/preservation.rs) — AST-based quality verification
- [x] Project-wide benchmark engine — scans files, measures tokens/latency/quality per mode
- [x] Benchmark session simulation with real numbers
- [x] Shareable benchmark reports (terminal/markdown/JSON)
- [x] MCP ctx_benchmark project mode (action=project)

### Website (leanctx.com)
- [x] Homepage with hero, feature cards, problem cards, terminal showcase
- [x] Features page with all 21 MCP tools + 5 CLI commands + methodology section
- [x] Full documentation (15+ pages)
- [x] Changelog with all versions through v2.1.0
- [x] Manifest page (vision/philosophy)
- [x] Comparison table (vs RTK, Orbital)
- [x] Editor compatibility cards (Cursor, Claude Code, Copilot, Windsurf, Zed, Cline, Continue, Aider)
- [x] Quick start guide
- [x] Responsive/mobile optimized
- [x] Transparent savings methodology ("How We Measure Savings" section)
- [x] Real session data labels on all demo outputs
- [x] Windsurf troubleshooting documentation

### Distribution
- [x] crates.io (v2.1.0)
- [x] Homebrew tap (yvgude/lean-ctx)
- [x] AUR (lean-ctx + lean-ctx-bin, v2.1.0)
- [x] GitHub Releases with CI binaries (5 targets)
- [x] GitHub Actions CI/CD

## What's Left to Build

### Short-term
- [ ] Centralize version string (env! macro or const)

### Medium-term
- [ ] Context Autopilot — automatic context window management
- [ ] GitHub PR Badge action
- [ ] Cloud Dashboard (SaaS)
- [ ] Team statistics / multi-user analytics
- [ ] More tree-sitter grammars

### Long-term
- [ ] Semantic Router (model selection based on query intent)
- [ ] Multi-Agent Memory (shared context across parallel agents)
- [ ] Output Verification (quality guardrail)
- [ ] Adaptive Compression (ML-driven entropy thresholds)

## Known Issues

1. **Version hardcoded in 7+ places** — easy to forget during bumps
2. **Website not in git** — must be deployed manually via rsync
3. **Node.js version** — server has Node 20, need 22+ for Astro; build locally
4. **GitLab push** — sometimes shows exit code 1 despite successful push
5. **AUR API caching** — may show old version for hours after push
