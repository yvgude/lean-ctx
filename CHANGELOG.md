# Changelog

All notable changes to lean-ctx are documented here.

## [2.3.3] — 2026-03-26

### Added

- **Pi Coding Agent integration** — New `pi-lean-ctx` npm package that overrides Pi's `bash`, `read`, `grep`, `find`, and `ls` tools to route all output through lean-ctx. Smart read mode selection based on file type and size (full/map/signatures). Includes compression stats footer and `/lean-ctx` slash command
- **`lean-ctx init --agent pi`** — One-command setup: auto-installs the `pi-lean-ctx` Pi Package and creates project-local `AGENTS.md` with lean-ctx instructions
- **Pi AGENTS.md template** — Skill file teaching Pi to leverage lean-ctx compression transparently

## [2.3.2] — 2026-03-26

### Fixed

- **Dashboard flicker-free live updates** — Replaced full DOM rebuild on each poll with incremental value patching. KPI values, charts, and tables now update in-place without page flicker. Charts update data arrays instead of being destroyed and recreated. Polling interval reduced from 10s to 3s for near-real-time feel

### Added

- **`lean-ctx gain --live`** — Live terminal dashboard mode. Refreshes in-place every 2s without scrolling. Press Ctrl+C to exit
- **Zed editor docs** — Full setup guide with `context_servers` configuration added to website getting-started page

## [2.3.1] — 2026-03-26

### Fixed

- **Dashboard live update** — Added `Cache-Control: no-cache, no-store, must-revalidate` headers to API responses, preventing browser caching of stale data. `mcp-live.json` now updates on every MCP tool call instead of only during auto-checkpoint (every 15 calls)
- **ctx_search respects .gitignore** — Replaced `walkdir` with the `ignore` crate (same library ripgrep uses) in `ctx_search`, `ctx_tree`, `ctx_graph`, and `ctx_intent`. Next.js projects no longer scan 50k+ files in `node_modules`/`.next`. Added `ignore_gitignore` parameter to `ctx_search` for opt-out ([#6](https://github.com/yvgude/lean-ctx/issues/6))

### Added

- **Zed editor configuration** — Added Zed MCP setup instructions to README with `context_servers` configuration example ([#5](https://github.com/yvgude/lean-ctx/issues/5))
- **`ignore` crate dependency** — Provides automatic `.gitignore`, `.git/info/exclude`, and global gitignore support for all file-walking operations

## [2.3.0] — 2026-03-26

### Scientific Compression Engine (10 Information-Theoretic Optimizations)

Major release adding a scientifically-grounded compression engine — 10 optimizations derived from Shannon entropy, Kolmogorov complexity, Bayesian inference, and rate-distortion theory.

### Added

- **I1: BPE Token-Aware Entropy** — Shannon entropy calculated on BPE token distributions instead of character frequencies, precisely matching LLM tokenizer behavior. Low-entropy threshold calibrated for real code
- **I2: N-Gram Jaccard Similarity** — Bigram-based Jaccard replaces word-set Jaccard for order-sensitive deduplication. Includes Minhash approximation (128 hashes, error < 0.01) for O(1) comparisons
- **I3: Cross-File Dedup v2** — Detects shared 5-line blocks across cached files and replaces duplicates with `[= Fn:L1-L2]` references. `ctx_dedup` now supports `analyze` and `apply` actions
- **I4: Bayesian Mode Predictor** — Learns optimal read mode (full/map/signatures/aggressive/entropy) per file signature (extension × size bucket) from historical outcomes. Persists to `~/.lean-ctx/mode_stats.json`
- **I5: Adaptive LITM Profiles** — Model-specific Lost-In-The-Middle weights (Claude α=0.50, GPT α=0.45, Gemini α=0.40) for optimal context positioning. Configurable via `LEAN_CTX_LITM_PROFILE` env var
- **I6: Boltzmann Cache Eviction** — Thermodynamic-inspired eviction scoring: `score = frequency × exp(-age/τ)`. Respects configurable token budget (`LEAN_CTX_CACHE_MAX_TOKENS`, default 500K)
- **I7: Information Density Metric** — Measures semantic tokens per output token. Integrated into `QualityScore` with adaptive thresholds. Dense code (>0.15 density) gets lighter compression
- **I8: Auto-Delta Encoding** — Automatically detects file changes on `ctx_read(mode="full")` and sends compact diffs when delta < 60% of full content. Typical savings: 98.9% for 1-line edits
- **I9: Huffman Instruction Templates** — Short codes (ACT1, BRIEF, FULL, DELTA, etc.) replace verbose task complexity instructions. 52-60% shorter per instruction, break-even at 24 calls, saves 286 tokens per 50-call session
- **I10: Kolmogorov Complexity Proxy** — Gzip-ratio approximation of Kolmogorov complexity classifies files as High/Medium/Low compressibility. Guides mode selection in `ctx_analyze`

### Changed

- **Crate restructure** — Added `lib.rs` for public API exposure, enabling integration testing. Binary `main.rs` now imports from library crate
- **Entropy filter** uses BPE token entropy (threshold 1.0) instead of character entropy
- **Pattern grouping** uses N-gram Jaccard (n=2) instead of word-set Jaccard
- **`ctx_smart_read`** consults Bayesian mode predictor before falling back to heuristics
- **`ctx_analyze`** reports Kolmogorov proxy and compressibility class
- **Server instructions** include LITM profile name and instruction decoder block

### Dependencies

- Added `flate2 = "1"` for gzip-based Kolmogorov complexity proxy

### Benchmarks (on lean-ctx's own 14 source files, 33,737 tokens)

| Scenario | Savings |
|---|---|
| Cache re-read | **99%** (~8 tokens vs thousands) |
| Map mode (server.rs) | **97.6%** (8,684 → 206 tokens) |
| Auto-delta (1-line edit) | **98.9%** (3,325 → 38 tokens) |
| Typical 40-read session | **69.0%** (149,695 → 46,332 tokens) |
| Entropy mode (dense code) | 0.8% (already optimal) |
| Aggressive mode | 3.9% |

## [2.2.0] — 2026-03-26

### Cognitive Efficiency Protocol (CEP v1)

Major release introducing the Cognitive Efficiency Protocol — a holistic approach to LLM communication optimization that leverages the model's mathematical processing strengths.

### Added

- **CEP Compliance Scoring** in `ctx_metrics` — tracks Cache utilization, Mode diversity, Compression rate, and an overall CEP Score (0-100)
- **Adaptive Instructions Engine** (`adaptive.rs`) — classifies task complexity (Mechanical / Standard / Architectural) based on session context and dynamically adjusts LLM reasoning guidance
- **Smart Context Prefill Hints** in `ctx_context` — suggests optimal read modes for large or infrequently-used files
- **Quality Scorer** (`quality.rs`) — measures AST, identifier, and line preservation to ensure compression quality stays above 95%
- **Auto-Validation Pipeline** (`validator.rs`) — syntax checks for Rust, JS/TS, Python, JSON, TOML after file changes
- **CEP A/B Benchmark** in `benchmark.rs` — compare token counts with and without CEP overhead
- **MCP Live Stats** (`~/.lean-ctx/mcp-live.json`) — real-time CEP metrics for dashboard integration
- **Dashboard CEP Intelligence Card** — shows CEP Score, Cache Hit Rate, Mode Diversity, Compression, and Task Complexity in the web dashboard
- **TOON-Inspired Output Format** — indentation-based headers replacing bracket-label format for ~15% fewer tokens per header
- **Extended Filler Detection v2** — 60+ patterns across Hedging, Meta-Commentary, Closings, Transitions, and Acknowledgments
- **Dynamic MAP Threshold** — ROI-based identifier registration replaces fixed 12-char minimum
- **Formal Action Vocabulary (TDD v2)** — Unicode symbols for code changes (`⊕⊖∆→⇒✓✗⚠`) and structural elements (`λ§∂τε`)

### Fixed

- **`--global --agent` no longer overwrites project files** — running `lean-ctx init --global --agent claude` now installs global hooks without creating `CLAUDE.md` in the current directory
- **Multiple `--agent` flags** — `lean-ctx init --global --agent claude --agent codex` now processes all agents, not just the first

---

## [2.1.3] — 2026-03-26

### Bug Fix: Shell Hook Idempotent Updates

Fixes a critical UX issue where `lean-ctx init --global` refused to update existing shell aliases, leaving users stuck with broken (bare `lean-ctx`) aliases from older versions even after upgrading the binary.

### Fixed

- **`init --global` now auto-replaces old aliases** — running `lean-ctx init --global` detects and removes the previous lean-ctx block from `.bashrc`/`.zshrc`/`config.fish`/PowerShell profile, then writes fresh aliases with the correct absolute binary path
- **No manual cleanup required** — users no longer need to manually delete old alias blocks before re-running init
- **PowerShell profile update** — `init_powershell` also auto-replaces the old function block

### Added

- `remove_lean_ctx_block()` helper to cleanly strip old POSIX/fish hook blocks from shell config files
- `remove_lean_ctx_block_ps()` helper for PowerShell profile block removal (brace-depth aware)
- 4 unit tests for block removal covering bash, fish, PowerShell, and no-op cases

### Note for existing users

Simply run `lean-ctx init --global` — the old aliases will be automatically replaced with the correct absolute-path versions. No manual `.bashrc` editing needed.

---

## [2.1.2] — 2026-03-26

### Bug Fix: Shell Hook PATH Resolution

Fixes a critical bug where `lean-ctx init --global` and `lean-ctx init --agent <tool>` generated shell aliases and hook scripts using bare `lean-ctx` instead of the absolute binary path. This caused all rewritten commands to fail with exit code 126 when `lean-ctx` was not in the shell's PATH.

### Fixed

- **Shell aliases (bash/zsh/fish)** now use the absolute binary path from `std::env::current_exe()` instead of hardcoded `lean-ctx`
- **Editor hook scripts (Claude, Cursor, Gemini)** embed `LEAN_CTX_BIN="/full/path/lean-ctx"` at the top and use `$LEAN_CTX_BIN` throughout
- **Codex and Cline instruction files** reference the full binary path
- **Windows + Git Bash compatibility**: Windows paths (`C:\Users\...`) are automatically converted to Git Bash paths (`/c/Users/...`) in bash hook scripts, fixing the `/C: Is a directory` error

### Added

- `to_bash_compatible_path()` helper for cross-platform path conversion (Windows drive letters to POSIX format)
- `resolve_binary_path_for_bash()` for bash-specific path resolution
- 6 unit tests for path conversion covering Unix paths, Windows drive letters, and edge cases

### Note for existing users

After updating, re-run `lean-ctx init --global` and/or `lean-ctx init --agent <tool>` to regenerate the aliases/hooks with the absolute path. Remove the old shell hook block from your `.zshrc`/`.bashrc` first (between `# lean-ctx shell hook` and `fi`).

---

## [2.1.1] — 2026-03-25

### Tool Enforcement + Editor Hook Improvements + Security & Trust

This release ensures AI coding tools reliably use lean-ctx MCP tools, and establishes a comprehensive security posture.

### Changed

- **MCP tool descriptions** now start with "REPLACES built-in X tool — ALWAYS use this instead of X"
- **Server instructions** include a LITM-optimized REMINDER at the end
- **`lean-ctx init --agent cursor`** now auto-creates `.cursor/rules/lean-ctx.mdc` in the project directory
- **`lean-ctx init --agent claude`** now auto-creates `CLAUDE.md` in the project directory
- **`lean-ctx init --agent windsurf`** now uses bundled template
- Example files now embedded via `include_str!` for consistent deployment

### Added

- **SECURITY.md** — Comprehensive security policy: vulnerability reporting, dependency audit, VirusTotal false positive explanation, build reproducibility
- **CI workflow** (`ci.yml`) — Automated tests, clippy lints (warnings=errors), rustfmt check, cargo audit on every push/PR
- **Security Check workflow** (`security-check.yml`) — Dangerous pattern scan (network ops, unsafe blocks, shell injection, hardcoded secrets), critical file change alerts, dependency audit
- **72 unit + integration tests** — Cache operations, entropy compression, LITM efficiency, shell pattern compression (git, cargo), CLI commands, pattern dispatch routing
- **README badges** — CI status, Security Check status, crates.io version, downloads, license
- **Security section** in README with VirusTotal false positive explanation

---

## [2.1.0] — 2026-03-25

### Real Benchmark Engine + Information Preservation

This release replaces the estimation-based benchmark with a **real measurement engine** that scans project files and produces verifiable, shareable results.

### Added

- **`core/preservation.rs`** — AST-based information preservation scoring using tree-sitter. Measures how many functions, exports, and imports survive each compression mode.
- **Project-wide benchmark** (`lean-ctx benchmark run [path]`):
  - Scans up to 50 representative files across all languages
  - Measures real token counts per compression mode (map, signatures, aggressive, entropy, cache_hit)
  - Tracks wall-clock latency per operation
  - Computes preservation quality scores per mode
  - Session simulation: models a 30-min coding session with real numbers
- **Three output formats**:
  - `lean-ctx benchmark run` — ANSI terminal table
  - `lean-ctx benchmark run --json` — machine-readable JSON
  - `lean-ctx benchmark report` — shareable Markdown for GitHub/LinkedIn
- **MCP `ctx_benchmark` extended** — new `action=project` parameter for project-wide benchmarks via MCP, with `format` parameter (terminal/markdown/json)

### Changed

- `lean-ctx benchmark` CLI now uses subcommands (`run`, `report`) instead of scenario names
- Benchmark engine uses real file measurements instead of estimates from stats.json

---

## [2.0.0] — 2026-03-25

### Major: Context Continuity Protocol (CCP) + LITM-Aware Positioning

This release introduces the **Context Continuity Protocol** — cross-session memory that persists task context, findings, and decisions across chat sessions and context compactions. Combined with **LITM-aware positioning** (based on Liu et al., 2023), CCP eliminates 99.2% of cold-start tokens and improves information recall by +42%.

### Added

- **2 new MCP tools** (19 → 21 total):
  - `ctx_session` — Session state manager with actions: status, load, save, task, finding, decision, reset, list, cleanup. Persists to `~/.lean-ctx/sessions/`. Load previous sessions in ~400 tokens (vs ~50K cold start)
  - `ctx_wrapped` — Generate savings report cards showing tokens saved, costs avoided, top commands, and cache efficiency

- **3 new CLI commands**:
  - `lean-ctx wrapped [--week|--month|--all]` — Shareable savings report card
  - `lean-ctx sessions [list|show|cleanup]` — Manage CCP sessions
  - `lean-ctx benchmark run [path]` — Real project benchmark (superseded by v2.1.0 project benchmarks)

- **LITM-Aware Positioning Engine** (`core/litm.rs`):
  - Places session state at context begin position (attention α=0.9)
  - Places findings/test results at end position (attention γ=0.85)
  - Eliminates lossy middle (attention β=0.55 → 0.0)
  - Quantified: +42% relative LITM efficiency improvement

- **Session State Persistence**:
  - Automatic session state tracking across all tool calls
  - Batch save every 5 tool calls
  - Auto-save before idle cache clear
  - Session state embedded in auto-checkpoints
  - Session state embedded in MCP server instructions (LITM P1 position)
  - 7-day session archival with cleanup

- **Benchmark Engine** (`core/benchmark.rs`):
  - Project-wide benchmark scanning up to 50 representative files
  - Per-mode token measurement using tiktoken (o200k_base)
  - Session simulation with real file data
  - Superseded by v2.1.0 project benchmarks with latency and preservation scoring

### Improved

- Auto-checkpoint now includes session state summary
- MCP server instructions now include CCP usage hints and session load prompt
- Idle cache expiry now auto-saves session before clearing

---

## [1.9.0] — 2026-03-25

### Major: Context Intelligence Engine

This release transforms lean-ctx from a compression tool into a **Context Intelligence Engine** — 9 new MCP tools, 15 new shell patterns, AI tool hooks, and a complete intent-detection system.

### Added

- **9 new MCP tools** (10 → 19 total):
  - `ctx_smart_read` — Adaptive file reading: automatically selects the optimal compression mode based on file size, type, cache state, and token count
  - `ctx_delta` — Incremental file updates via Myers diff. Only sends changed hunks instead of full content
  - `ctx_dedup` — Cross-file deduplication analysis: finds shared imports and boilerplate across cached files
  - `ctx_fill` — Priority-based context filling with a token budget. Automatically maximizes information density
  - `ctx_intent` — Semantic intent detection: classifies queries (fix, add, refactor, understand, test, config, deploy) and auto-loads relevant files
  - `ctx_response` — Bi-directional response compression with filler removal and TDD shortcuts
  - `ctx_context` — Multi-turn context manager: shows cached files, read counts, and session state
  - `ctx_graph` — Project intelligence graph: analyzes file dependencies, imports/exports, and finds related files
  - `ctx_discover` — Analyzes shell history to find missed compression opportunities with estimated savings

- **15 new shell pattern modules** (32 → 47 total):
  - `aws` (S3, EC2, Lambda, CloudFormation, ECS, CloudWatch Logs)
  - `psql` (table output, describe, DML)
  - `mysql` (table output, SHOW, queries)
  - `prisma` (generate, migrate, db push/pull, format, validate)
  - `helm` (list, install, upgrade, status, template, repo)
  - `bun` (test, install, build)
  - `deno` (test, lint, check, fmt)
  - `swift` (test, build, package resolve)
  - `zig` (test, build)
  - `cmake` (configure, build, ctest)
  - `ansible` (playbook recap, task summary)
  - `composer` (install, update, outdated)
  - `mix` (test, deps, compile, credo/dialyzer)
  - `bazel` (test, build, query)
  - `systemd` (systemctl status/list, journalctl log deduplication)

- **AI tool hook integration** via `lean-ctx init --agent <tool>`:
  - Claude Code (PreToolUse hook)
  - Cursor (hooks.json)
  - Gemini CLI (BeforeTool hook)
  - Codex (AGENTS.md)
  - Windsurf (.windsurfrules)
  - Cline/Roo (.clinerules)
  - Copilot (PreToolUse hook)

### Improved

- **Myers diff algorithm** in `compressor.rs`: Replaced naive line-index comparison with LCS-based diff using the `similar` crate. Insertions/deletions are now correctly tracked instead of producing mass-deltas
- **Language-aware aggressive compression**: `aggressive` mode now correctly handles Python `#` comments, SQL `--` comments, Shell `#` comments, HTML `<!-- -->` blocks, and multi-line `/* */` blocks
- **Indentation normalization**: Detects tab-based indentation and preserves it correctly

### Fixed

- **UTF-8 panic in `grep.rs`** (fixes [#4](https://github.com/yvgude/lean-ctx/issues/4)): String truncation now uses `.chars().take(n)` instead of byte-based slicing `[..n]`, preventing panics on multi-byte characters (em dash, CJK, emoji)
- Applied the same UTF-8 safety fix to `env_filter.rs`, `typescript.rs`, and `ctx_context.rs`

### Dependencies

- Added `similar = "2"` for Myers diff algorithm

---

## [1.8.2] — 2026-03-23

### Added
- Tee logging for full output recovery
- Poetry/uv shell pattern support
- Flutter/Dart shell pattern support
- .NET (dotnet) shell pattern support

### Fixed
- AUR source build: force GNU BFD linker via RUSTFLAGS to work around lld/tree-sitter symbol resolution

---

## [1.8.0] — 2026-03-22

### Added
- Web dashboard at localhost:3333
- Visual terminal dashboard with ANSI colors, Unicode bars, sparklines
- `lean-ctx discover` command
- `lean-ctx session` command
- `lean-ctx doctor` diagnostics
- `lean-ctx config` management

---

## [1.7.0] — 2026-03-21

### Added
- Token Dense Dialect (TDD) mode with symbol shorthand
- `ctx_cache` tool for cache management
- `ctx_analyze` tool for entropy analysis
- `ctx_benchmark` tool for compression comparison
- Fish shell support
- PowerShell support

---

## [1.5.0] — 2026-03-18

### Added
- tree-sitter AST parsing for 14 languages
- `ctx_compress` context checkpoints
- `ctx_multi_read` batch file reads

---

## [1.0.0] — 2026-03-15

### Initial Release
- Shell hook with 20+ patterns
- MCP server with ctx_read, ctx_tree, ctx_shell, ctx_search
- Session caching with MD5 hashing
- 6 compression modes (full, map, signatures, diff, aggressive, entropy)
