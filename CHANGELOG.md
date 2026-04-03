# Changelog

All notable changes to lean-ctx are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/).

## [2.16.2] — 2026-04-03

### Codex MCP Compatibility

- **feat(mcp)**: Hybrid stdio transport that auto-detects `Content-Length` framing (Codex/LSP-style) vs JSONL (Cursor/Claude/etc.) and responds in the same protocol — contributed by [@JulienJBO](https://github.com/JulienJBO) ([#48](https://github.com/yvgude/lean-ctx/pull/48))
- **fix(hooks)**: Suppress Codex hook setup stdout noise during MCP server mode to keep the transport clean
- 3 new unit tests for JSONL decoding, Content-Length decoding, and framed response encoding
- New dependencies: `futures`, `tokio-util` (codec), `thiserror`

## [2.16.1] — 2026-04-03

### Patch Release

- **fix(report)**: `lean-ctx report-issue` now reliably finds the `gh` CLI binary by searching common install locations (`/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`) and falling back to `which gh`
- **fix(report)**: Graceful fallback when GitHub labels don't exist yet on the repository
- **fix(ci)**: Removed unused import causing CI failure with `RUSTFLAGS=-Dwarnings`
- **feat(gain)**: Added `report-issue` hint to rotating tips in `lean-ctx gain`
- **docs**: Added deploy branch security warnings to DEPLOY_CHECKLIST

## [2.16.0] — 2026-04-03

### Intelligence Layer & Bug Fixes

ctx_search hang fix, built-in issue reporting, graph impact analysis, and the intelligence layer that optimizes output tokens without affecting thinking quality.

### Added
- **`lean-ctx report-issue`** — One-command bug reporting with full diagnostics. Collects 9 sections (environment, config, MCP status, recent tool calls, session state, performance metrics, slow commands, tee logs, project context), anonymizes all paths and secrets, and creates a GitHub issue directly. Supports `--dry-run`, `--include-tee`, `--title`, and `--description` flags. Report is also saved locally to `~/.lean-ctx/last-report.md`.
- **Per-Tool Latency Tracking** — Every MCP tool call over 100ms is now logged to `~/.lean-ctx/tool-calls.log` with timestamp, duration, token counts, and mode. Calls exceeding 5 seconds are marked `**SLOW**`. Log is ring-buffered at 50 entries and included in `report-issue` output.
- **Intelligence Block (Output Efficiency)** — MCP server instructions now include output optimization hints: no-echo (don't repeat tool output), no-narration comments, delta-only code changes. These reduce output tokens by 15–40% without affecting thinking quality. Architecture tasks are explicitly protected: "architecture tasks need thorough analysis".
- **Task Briefing Pipeline** — Automatic task classification (9 types: Generate, FixBug, Refactor, Explore, Test, Debug, Config, Deploy, Review) with confidence scoring. Each classification carries an `OUTPUT-HINT` directive (CodeOnly, DiffOnly, ExplainConcise, Trace, StepList) that guides the LLM's response format. Injected automatically via the autonomy pipeline on session start.
- **Report Issue hint in CLI** — `lean-ctx` command box now shows `lean-ctx report-issue` alongside other commands.
- **Report Issue link in Dashboard** — Header now includes a "Report Issue" link that copies the CLI command to clipboard.

### Fixed
- **ctx_search hanging for minutes** — Root cause: synchronous regex search blocked the Tokio runtime, no file size limits, no max directory depth, incomplete binary file filter. Fix: `spawn_blocking` wrapper with 30-second timeout, 512KB file size limit (`MAX_FILE_SIZE`), max directory depth of 20 (`MAX_WALK_DEPTH`), extended binary extension list (43 types including `.map`, `.snap`, `.db`, `.sqlite`, `.parquet`), and generated file detection (`.min.js`, `.bundle.js`, `.d.ts`, `.js.map`, `.css.map`). Searches that previously hung now complete in <100ms.
- **`ctx_graph impact` always returning "No files depend on X"** — The graph stored import edges with Rust module paths (`lean_ctx::core::cache::SessionCache`) but `impact` compared against file paths (`src/core/cache.rs`). Added `file_path_to_module_prefixes()` converter and `edge_matches_file()` matcher that resolves `crate::`, `super::`, and crate-name prefixes. `ctx_graph impact cache.rs` now correctly reports 17 dependents.

### Changed
- **Security audit** — Removed all lab-specific references from tracked source code (Ollama `/no_think` directive, lab-only tool comments). Thinking budget instructions are now platform-neutral hints that work across all LLMs.
- **Test suite cleanup** — Removed 7 machine-dependent throughput benchmarks unsuitable for open-source CI. Fixed hardcoded developer paths in `savings_verification.rs` (now uses `env!("CARGO_MANIFEST_DIR")`). Marked environment-dependent test as `#[ignore]`.

### Performance
- **462/462** tests pass (362 unit + 100 integration/benchmark)
- **59%** average token savings in Cursor sessions
- **91%** compression on `git log --stat` output
- **98%** compression on `cargo test --no-run` output
- **<100ms** ctx_search response time (previously minutes/hang)
- **$0.05** estimated session savings (including thinking token reduction)

---

## [2.15.0] — 2026-04-03

### Scientific Compression Evolution

Six algorithms from information theory, graph theory, and statistical mechanics now power lean-ctx's compression pipeline — all automatic, all local, zero configuration.

### Added
- **Predictive Surprise Scoring** — Replaces static Shannon entropy with BPE cross-entropy. Measures how "surprising" each line is to the LLM's tokenizer. Boilerplate scores low and gets removed; complex logic scores high and stays. 15–30% better filtering than character-level entropy.
- **Spectral Relevance Propagation** — Heat diffusion + PageRank on the project dependency graph. Finds structurally important files even without keyword overlap. Seed files spread relevance along import edges with exponential decay.
- **Boltzmann Context Allocation** — Statistical mechanics-based token budget distribution. Specific tasks concentrate tokens on top files (low temperature); broad tasks spread evenly (high temperature). Automatically selects compression mode per file.
- **Semantic Chunking with Attention Bridges** — Restructures output to counter LLM "Lost in the Middle" attention bias. Promotes task-relevant chunks to high-attention positions, adds structural boundary markers and tail anchors.
- **MMR Deduplication** — Maximum Marginal Relevance removes redundant lines across files using bigram Jaccard similarity. 10–25% less noise in multi-file context loads.
- **BPE-Aligned Token Optimization** — Final-pass string replacements aligned to BPE token boundaries (`function `→`fn `, `" -> "`→`"->"`, lifetime elision). 3–8% additional savings.
- **Auto-Build Graph Index** — `load_or_build()` function automatically builds the project dependency graph on first use. No manual `ctx_graph build` required — the system is fully zero-config.
- **Fish Shell Doctor Check** — `lean-ctx doctor` now detects shell aliases in `~/.config/fish/config.fish` (previously only checked zsh/bash).
- **Codex Hook Refresh on Update** — `lean-ctx update` now refreshes Codex PreToolUse hook scripts alongside Claude, Cursor, and Gemini hooks.

### Changed
- Graph edge resolution now maps Rust module paths back to file paths, enabling correct heat diffusion and PageRank propagation across the codebase.
- Centralized graph index loading across `ctx_preload`, `ctx_overview`, `autonomy`, and `ctx_intent` — eliminates path mismatch bugs between relative and absolute project roots.

### Performance
- **85.7%** session-wide token savings (with CCP) in 30-min coding simulation
- **96%** compression in map/signatures mode with 94% quality preservation
- **99.3%** savings on cache re-reads (13 tokens)
- **95%** git command compression across all patterns
- **12/12** scientific verification checks passed
- **39/39** intensive benchmark tests passed

## [2.14.5] — 2026-04-02

### Changed
- **Internal cleanup** — Removed dead code (`format_type_short`, `instruction_encoding_savings`) and their orphaned test from the protocol module. Simplified cloud and help text messaging. No functional changes.

## [2.14.4] — 2026-04-02

### Fixed
- **LEAN_CTX_DISABLED kill-switch now works end-to-end** — The shell hook (bash/zsh/fish/powershell) previously ignored `LEAN_CTX_DISABLED` entirely. Setting it to `1` bypassed compression in the Rust code but the shell aliases were still loaded, spawning a `lean-ctx` process for every command. Now: the `_lc()` wrapper short-circuits to `command "$@"` when `LEAN_CTX_DISABLED` is set (zero overhead), the auto-start guard skips alias creation, and `lean-ctx -c` does an immediate passthrough. Closes #42.
- **`lean-ctx-status` shows DISABLED state** — `lean-ctx-status` now prints `DISABLED (LEAN_CTX_DISABLED is set)` when the kill-switch is active.
- **Help text documents both env vars** — `--help` now shows `LEAN_CTX_DISABLED=1` (full kill-switch) and `LEAN_CTX_ENABLED=0` (prevent auto-start, `lean-ctx-on` still works).

## [2.14.3] — 2026-04-02

### Added
- **Full Output Tee** — New `tee_mode` config (`always`/`failures`/`never`) replaces the old `tee_on_error` boolean. When set to `always`, full uncompressed output is saved to `~/.lean-ctx/tee/` and referenced in compressed output. Backward-compatible: `tee_on_error: true` maps to `failures`. Use `lean-ctx tee last` to view the most recent log. Closes #2021.
- **Raw Mode** — Skip compression entirely with `ctx_shell(command, raw=true)` in MCP or `lean-ctx -c --raw <command>` on CLI. New `lean-ctx-raw` shell function in all hooks (bash/zsh/fish/PowerShell). Use for small outputs or when full detail is critical. Closes #2022.
- **Truncation Warnings** — When output is truncated during compression, a transparent marker shows exactly how many lines were omitted and how to get full output (`raw=true`). Prevents silent data loss — the #1 reason users leave competing tools.
- **`LEAN_CTX_DISABLED` env var** — Master kill-switch that bypasses all compression in both shell hook and MCP server. Set `LEAN_CTX_DISABLED=1` to pass everything through unmodified.
- **ANSI Auto-Strip** — ANSI escape sequences are automatically stripped before compression, preventing wasted tokens on invisible formatting codes. Centralized `strip_ansi` implementation replaces 3 duplicated copies.
- **Passthrough URLs** — New `passthrough_urls` config option. Curl commands targeting listed URLs skip JSON schema compression and return full response bodies. Useful for local APIs where full JSON is needed.
- **Zero Telemetry Badge** — README and comparison table now explicitly highlight lean-ctx's privacy-first design: zero telemetry, zero network requests, zero PII exposure.
- **User TOML Filters** — Define custom compression rules in `~/.lean-ctx/filters/*.toml`. User filters are applied before builtin patterns. Supports regex pattern matching with replacement and keep-lines filtering. New CLI: `lean-ctx filter [list|validate|init]`. Closes #2023.
- **PreToolUse Hook for Codex** — Codex CLI now gets PreToolUse-style hook scripts alongside AGENTS.md, matching Claude and Cursor/Gemini behavior. Closes #2024.
- **New AI Tool Integrations** — Added `opencode`, `aider`, and `amp` as supported agents. Use `lean-ctx init --agent opencode|aider|amp`. Total supported agents: 19. Closes #2026.
- **Discover Enhancement** — `lean-ctx discover` now shows a formatted table with per-command token estimates, USD savings projection (daily and monthly), and uses real compression stats when available. Shared logic between CLI and MCP tool. Closes #2025.

### Changed
- `ctx_shell` MCP tool schema now accepts `raw` boolean parameter.
- Server instructions include raw mode and tee file hints.
- Help text updated for new commands (`filter`, `tee last`, `-c --raw`).

## [2.14.2] — 2026-04-02

### Fixed
- **Shell hook quoting** — `git commit -m "message with spaces"` now works correctly. The `_lc()` wrapper previously used `$*` which collapsed quoted arguments into a flat string; fixed to use `$@` (bash/zsh), unquoted `$argv` (fish), and splatted `@args` (PowerShell) to preserve argument boundaries. Closes #41.
- **Terminal colors preserved** — Commands run through the shell hook in a real terminal (outside AI agent context) now inherit stdout/stderr directly, preserving ANSI colors, interactive prompts, and pager behavior. Previously, output was piped through a streaming buffer which caused child processes to disable color output (`isatty()` returned false). Closes #40.

### Removed
- `exec_streaming` mode — replaced by `exec_inherit_tracked` which passes output through unmodified while still recording command usage for analytics.

## [2.14.1] — 2026-04-02

### Autonomous Intelligence Layer

lean-ctx now runs its optimization pipeline **autonomously** — no manual tool calls needed.
The system self-configures, pre-loads context, deduplicates files, and provides efficiency hints
without the user or AI agent triggering anything explicitly.

### Added
- **Session Lifecycle Manager** — Automatically triggers `ctx_overview` or `ctx_preload` on the first MCP tool call of each session, delivering immediate project context
- **Related Files Hints** — After every `ctx_read`, appends `[related: ...]` hints based on the import graph, guiding the AI to relevant files
- **Silent Background Preload** — Top-2 imported files are automatically cached after each `ctx_read`, eliminating cold-cache latency on follow-up reads
- **Auto-Dedup** — When the session cache reaches 8+ files, `ctx_dedup` runs automatically to eliminate cross-file redundancy (measured: -89.5% in real sessions)
- **Task Propagation** — Session task context automatically flows to all `ctx_read` and `ctx_multi_read` calls for better compression targeting
- **Shell Efficiency Hints** — When `grep`, `cat`, or `find` run through `ctx_shell`, lean-ctx suggests the more token-efficient MCP equivalent
- **`AutonomyConfig`** — Full configuration struct with per-feature toggles and environment variable overrides (`LEAN_CTX_AUTONOMY=false` to disable all)
- **PHP/Laravel Support** — Full PHP AST extraction, Laravel-specific compression (Eloquent models, Controllers, Migrations, Blade templates), and `php artisan` shell hook patterns
- **15 new integration tests** for the autonomy layer (`autonomy_tests.rs`)

### Changed
- **System Prompt** — Replaced verbose `PROACTIVE` + `OTHER TOOLS` blocks with a compact `AUTONOMY` block, reducing cognitive load on the AI agent (~20 tokens saved per session)
- **`ctx_multi_read`** — Now accepts and propagates session task for context-aware compression

### Fixed
- **Version command** — `lean-ctx --version` now uses `env!("CARGO_PKG_VERSION")` instead of a hardcoded string

### Performance
- **Net savings: ~1,739 tokens/session** (analytical measurement)
- Pre-hook wrapper overhead: 10 tokens (one-time)
- Related hints: ~10 tokens per `ctx_read` call
- Silent preload savings: ~974 tokens (eliminates 2 manual reads)
- Auto-dedup savings: ~750 tokens at 15% reduction on typical cache
- System prompt delta: -20 tokens

### Configuration
All autonomy features are **enabled by default**. Disable individually or globally:
```toml
# ~/.lean-ctx/config.toml
[autonomy]
enabled = true
auto_preload = true
auto_dedup = true
auto_related = true
silent_preload = true
dedup_threshold = 8
```
Or via environment: `LEAN_CTX_AUTONOMY=false`

## [2.14.0] — 2026-04-02

### Intelligence Layer Architecture

lean-ctx transforms from a pure compressor into an Intelligence Layer between user, AI tool, and LLM.

### Added
- `ctx_preload` MCP tool — proactive context orchestration based on task + import graph
- L-Curve Context Reorder Engine — classifies lines into 7 categories, reorders for optimal LLM attention

### Changed
- Output-format reordering: file content first, metadata last
- IB-Filter 2.0 with empirical L-curve attention weights
- LLM-native encoding with 15+ token optimization rules
- System prompt cleanup (~200 wasted tokens removed)

### Fixed
- Shell hook compression broken when stdout piped
- Shell hook stats lost due to early `process::exit()`
