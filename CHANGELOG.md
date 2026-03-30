# Changelog

All notable changes to lean-ctx are documented here.

## [2.9.9] — 2026-03-30

### Added

- **Animated RGB logo in terminal** — `lean-ctx gain` and `lean-ctx setup` now display a cinematic ASCII art logo with animated RGB color cycling using ANSI TrueColor (24-bit). Gracefully falls back to static gradient in non-terminal environments (pipes, CI).
- **Animated logo on `lean-ctx update`** — After successful self-update, the RGB logo animation plays with the new version confirmation.
- **New `terminal_ui` module** — Centralized terminal UI toolkit with box-drawing, status icons (✓/○/⚠), spinner animations, and reusable step headers for consistent CLI appearance.
- **Redesigned `lean-ctx setup` flow** — Clean 5-step onboarding: Shell Hook → AI Tool Auto-Detection → Agent Instructions → Environment Check → Data Sharing. No interactive questions for shell or editor detection (fully automatic). Ends with animated logo + command reference box.
- **Command reference box** — After setup completion, displays all essential commands in a bordered box for quick reference.

## [2.9.8] — 2026-03-30

### Fixed

- **GLIBC compatibility for older Linux** (Issue #34) — The pre-built `x86_64-unknown-linux-gnu` binary was compiled on Ubuntu 24.04 (GLIBC 2.39), making it incompatible with Ubuntu 20.04/22.04 and other older distributions. Now compiled on Ubuntu 22.04 (GLIBC 2.35). Additionally, `x86_64-unknown-linux-musl` is now built **with all features** (including tree-sitter AST parsing) as a fully static binary that runs on any Linux distribution regardless of GLIBC version.
- **Auto-detect GLIBC for binary selection** — `lean-ctx update`, `install.sh`, and `npm postinstall` now detect the system's GLIBC version and automatically select the musl (static) binary for systems with GLIBC < 2.35, and the gnu (dynamic) binary otherwise. Previously, all Linux systems received the gnu binary which could fail on older distributions.

## [2.9.7] — 2026-03-30

### Fixed

- **Turbo monorepo TUI hang** (Issue #33) — Built-in passthrough list for 28 TUI/long-running commands: `turbo`, `next dev`, `vite dev`, `nuxt dev`, `astro dev`, `nodemon`, `concurrently`, `pm2`, `docker compose up`, `vim`, `nvim`, `htop`, `ssh`, `tail -f`, `less`, and more. These are detected automatically and run without output buffering or compression.
- **`LEAN_CTX_COMPRESS=1` env var** — Overrides pipe detection to force compression when stdout is piped. Used by pi-lean-ctx and other programmatic integrations that explicitly want compressed output.
- **pi-lean-ctx v1.0.8** — Sets `LEAN_CTX_COMPRESS=1` in all exec calls and spawnHook, ensuring compression works correctly with the v2.9.6 pipe detection fix.

## [2.9.6] — 2026-03-30

### Fixed

- **Shell hook pipe detection** — Shell aliases (`lean-ctx -c curl`, `lean-ctx -c git`, etc.) now detect when stdout is piped (to another command, file, or IDE tool) and pass output through uncompressed. Previously, piped output was compressed, breaking JSON parsing, SHA calculations, and programmatic processing. Terminal output (human use) remains compressed as before.
- **MCP server command isolation** — `execute_command()` in the MCP server now sets `LEAN_CTX_ACTIVE=1` to prevent shell alias interference in spawned subprocesses.
- **Website install counter** — Now counts all distribution channels: crates.io, npm (lean-ctx-bin + pi-lean-ctx), and GitHub Release binary downloads. Previously missed pi-lean-ctx (600+ installs) and used limited npm time range.
- **Turbo monorepo TUI hang** (Issue #33) — Built-in passthrough list for TUI/long-running commands (turbo, next dev, vite dev, nuxt dev, astro dev, nodemon, vim, htop, ssh, tail -f, docker compose up, etc.). These are detected automatically and run without output buffering.
- **`LEAN_CTX_COMPRESS=1` env var** — Overrides pipe detection to force compression even when stdout is piped. Used by pi-lean-ctx (v1.0.8) and other programmatic integrations that explicitly want compressed output.

### Changed

- **Website wording overhaul** — 53 changes across the website based on expert review: consolidated naming to "Context Engineering Layer", nuanced compression claims (60–99% with context), replaced "better reasoning" with "higher signal density", added protocol explanations (CEP/CCP/TDD), sharpened Open Source vs Pro messaging, integrated thought-leadership core message.
- **pi-lean-ctx v1.0.8** — Sets `LEAN_CTX_COMPRESS=1` in all exec calls and spawnHook, ensuring compression works correctly with pipe detection.

## [2.9.5] — 2026-03-30

### Added

- **Smart Auto-Unified Mode** — MCP tool overhead reduced from ~18K to ~2.3K tokens per session. lean-ctx now detects the connecting IDE/agent during MCP initialization and automatically switches to Unified Mode (5 tools instead of 24) for all known clients: Cursor, Claude Code, Windsurf, VS Code Copilot, Cline, Roo Code, OpenCode, Gemini CLI, Codex, Zed, JetBrains, Amazon Q, Goose, AmpCode, and 19 more. Unknown clients safely fall back to full 24 tools.
- **Enhanced `ctx()` meta-tool** — Description now includes all 21 sub-tools with parameter signatures, so LLMs can call sub-tools without reading instructions.
- **`LEAN_CTX_FULL_TOOLS=1`** — New env var to force full 24-tool mode (overrides auto-detection). `LEAN_CTX_UNIFIED=1` kept for backward compatibility.
- **4 new unit tests** for client detection logic and tool count verification.

## [2.9.4] — 2026-03-30

### Fixed

- **Dashboard vs CLI token mismatch** — `lean-ctx gain`, `lean-ctx gain --daily`, and `lean-ctx gain --graph` now include both input and output token savings (CEP/TDD estimate) in their totals, consistent with the web dashboard. Previously, the CLI only showed input compression savings while the dashboard included output savings, causing confusing discrepancies.

### Changed

- **Consistent savings calculation** — All views (CLI gain, daily table, graph, web dashboard) now use the same formula: `total_saved = input_compressed + commands × 330` (output reduction from 450→120 tokens per call via CEP/TDD protocols).
- **Comprehensive project documentation** — Added detailed `PROJECT.md` with complete architecture, all 24 MCP tools, compression pipeline data flow, algorithm descriptions, shell pattern routing, and storage locations.

## [2.9.3] — 2026-03-30

### Fixed

- **Inflated token savings in `ctx_search`** — Token counting now only considers files with actual matches, not all scanned files. Previously, searching a large project could report millions of "saved" tokens from a single call because every file's content was counted as "input", even files without matches.
- **Inflated savings in `ctx_tree`** — Raw tree comparison now respects `.gitignore` (same as compact output), preventing artificial 100% compression rates from including `node_modules/`, `.git/`, etc. in the baseline.

## [2.9.2] — 2026-03-30

### Fixed

- **Homebrew sha256 mismatch** (#30) — Formula sha256 now matches actual GitHub tarball. GitHub regenerates tarballs dynamically; hash was stale after tag push.
- **git push loses pipeline URLs** (#31) — Shell hook now preserves `remote:` lines containing URLs (GitLab pipeline links, merge request URLs, GitHub PR links) instead of discarding them.
- **git commit loses pre-commit hook output** (#31) — Pre-commit hook output (linter results, test output) is now shown before the commit summary instead of being silently dropped. Up to 10 hook lines preserved.
- **CI formatting** — Fixed `cargo fmt` diff that caused CI failure.

## [2.9.1] — 2026-03-29

### Fixed

- **git log compression** — Shell hook now compresses `git log` output by 50-88% (was 1%). Fixed overly conservative `min_output_tokens` safeguard that rejected effective compression. Added truncation to 20 entries for both `--oneline` and standard format logs.
- **ctx_search SymbolMap** — SymbolMap compression now applies to all search results when ROI >= 5%, not just in TDD mode. Consistent with ctx_read behavior.

### Added

- **Dormant compression activation** — SymbolMap applied universally with ROI >= 5% gate (was TDD-only). `lightweight_cleanup()` as fallback for unmatched shell commands. Feedback loop active: `FeedbackStore` records compression outcomes for adaptive learning.
- **Scientific safeguards** — 7 mathematical safeguards: Shannon entropy floor, Kolmogorov gate (K>0.7), Symbol-Map ROI guarantee (>=5%), token ratio bounds [0.15, 1.0], edit integrity (fresh=true), feedback dampening (EWMA + min 5 samples), benchmark monotonicity.
- **Thinking token optimization** — SNR metric in protocol instructions, adaptive CEP output budget (mechanical=50, standard=150, architectural=unlimited), token budget hints in checkpoint instructions.
- **Output token optimization** — Auto filler removal on checkpoint outputs via `ctx_response`, CRP defaults to TDD mode for maximum compression.
- **Codebook deduplication** — Cross-file TF-IDF codebook applied during auto-checkpoints for deduplication.

## [2.9.0] — 2026-03-29

### Added

- **Pro Adaptive Intelligence** — `lean-ctx upgrade` command for one-step Pro subscription ($9/mo). Pro users get adaptive compression models trained on community data, auto-sync, and cloud dashboard. Everything happens automatically after upgrade.
- **BM25 Semantic Code Search** — New `ctx_semantic_search` and `ctx_intent` tools with pure-Rust BM25 ranking and AST-based code chunking for intent-driven context retrieval.
- **Information Bottleneck Filter** — Scientific compression based on Tishby et al. (2000). Maximizes task relevance while minimizing token usage in `task_relevance.rs`.
- **MCP Unified Mode** — Set `LEAN_CTX_UNIFIED=1` to consolidate 24 tools into 4 core tools + 1 `ctx()` meta-tool, saving ~18,000 tokens per session in tool definitions.
- **Cloud Backend** — Axum API server with SQLite for analytics, Pro model delivery, Stripe checkout, and team context sync. Deployed on Hetzner via GitLab CI.
- **Website Pro page** — New `/pro` page with benefit-oriented messaging, replacing the old pricing page. New `/checkout` and `/docs/cloud` pages.
- **mypy Compression Pattern** — New shell hook pattern for Python type checking output.
- **pytest Direct Routing** — Direct recognition of `pytest` and `python -m pytest` in shell hook.

### Fixed

- **Config overwrite bug** (#29) — `lean-ctx setup` no longer overwrites existing editor configs (OpenCode, Cursor, Zed, VS Code) when JSON parsing fails. Shows manual instructions instead.
- **UTF-8 file reading** (#28) — `lean-ctx read` now uses lossy UTF-8 decoding for files with non-UTF-8 bytes instead of erroring.
- **Windows self-update** — Deferred binary replacement via background `.bat` script when the binary is locked by the MCP server.
- **Compression regression** — Reintroduced static rule-based prediction fallback (`predict_from_defaults`) for all users, ensuring good compression even without Pro models.
- **Cost constants** — All dashboard and CLI displays now consistently use $2.50/M input + $10/M output (was inconsistently $3/$15 in some places).

### Changed

- **Website rebranding** — "The Cognitive Filter" → "The Intelligence Layer for AI Coding". "MCP tools" → "intelligent tools" / "Context Server" throughout the website. Navigation updated from "Pricing" to "Pro".
- **GitHub/GitLab separation** — Three-layer protection (`.gitignore`, pre-push hook, CI guardrail) ensures proprietary code stays off GitHub.

---

## [2.8.2] — 2026-03-29

### Fixed

- **Windows self-update** — Binary replacement now uses rename-before-replace strategy, preventing "Access is denied" errors when MCP server is running
- **Search modal** — Complete redesign with custom Pagefind API integration, keyboard navigation (↑↓ to navigate, ↵ to open), cleaner dark-theme styling

---

## [2.8.1] — 2026-03-29

### Added

- **Global search (Pagefind)** — Website navigation with Cmd+K shortcut
- **CLI Reference docs** — `/docs/cli-reference` with all commands, flags, and examples
- **Analytics & Dashboards docs** — `/docs/analytics` with gain, wrapped, and dashboard guide
- **Editor support** — Qwen Code, Trae, Amazon Q Developer, JetBrains IDEs (`setup.rs`, `hooks.rs`, `uninstall.rs`, `doctor.rs`)
- **Cline and Roo Code** — Separate auto-detected setup targets

### Changed

- **Compatibility page and landing page** — Updated with all 18 supported AI tools
- **Getting started prompt generator** — New editors
- **Docs navigation** — CLI Reference and Analytics links

---

## [2.8.0] — 2026-03-29

### Added

- **`lean-ctx uninstall` command** — Clean removal of all lean-ctx configuration: shell hooks (zsh/bash/fish/PowerShell), MCP configs (Cursor, Claude Code, Windsurf, Gemini CLI, Antigravity, Codex CLI, VS Code, OpenCode), and data directory (`~/.lean-ctx/`). Prints instructions for removing the binary itself based on the installation method (cargo/brew/manual)
- **Website: Uninstallation documentation** — New docs page explaining how to uninstall lean-ctx completely

### Changed

- **Cargo.toml description** — Updated MCP tool count from 23 to 24

---

## [2.7.1] — 2026-03-28

### Fixed

- **Shell hook: `git status` compressed to `?`** — When `compress_status` failed to parse output (e.g. non-English locale, custom git config), the entire output was replaced with a single `?` character. Now falls back to compact line truncation instead of destroying output ([#24](https://github.com/yvgude/lean-ctx/issues/24))
- **Shell hook: compression safety floor** — Added minimum output token floor (10% of original, minimum 5 tokens). Compression can never reduce output below this threshold, preventing data destruction
- **Shell hook: passthrough recursion** — `passthrough()` now sets `LEAN_CTX_ACTIVE=1` to prevent infinite recursion when aliases are defined in `.zshenv` instead of `.zshrc` ([#24](https://github.com/yvgude/lean-ctx/issues/24))
- **Help text: 21→24 MCP tools** — Updated `--help` output to reflect actual tool count

### Added

- **Workflow & Cheat Sheet documentation** — New `/docs/workflow` page with 5-session command cheat sheet, read mode decision tree, protocol summary, and quick reference cards ([#23](https://github.com/yvgude/lean-ctx/issues/23))
- **Website: ctx_knowledge, ctx_agent, ctx_overview** — Full documentation on tools page with action tables, examples, and storage paths
- **Website: CEP output token budgets** — Documented Mechanical/Standard/Architectural task complexity tiers
- **Website: CCP cross-session feature comparison** — CCP vs ctx_knowledge vs ctx_agent disambiguation section
- **Website: CLI docs** — Added `lean-ctx update`, `slow-log`, `gain --live`, `lean-ctx-on/off/status` toggle docs
- **Sidebar: Memory & Agents section** — New navigation group for ctx_knowledge, ctx_agent, ctx_overview
- **pi-lean-ctx: lower read thresholds** — Code files now use `map` mode from 8KB (was 24KB), `signatures` from 96KB (was 160KB). Fixes 0% compression on medium-sized code files (PR [#22](https://github.com/yvgude/lean-ctx/pull/22) by @amiano4)

### Changed

- **Website: 21→24 MCP tools** — Updated tool count across all pages (tools, features, getting-started, index, tdd, manifest, BaseLayout, FeatureCards, PowerFeatures)

---

## [2.7.0] — 2026-03-28

### Added

- **Persistent AI Memory (`ctx_knowledge`)** — Cross-session project knowledge store. Store facts with categories, confidence scores, and automatic timestamps. Recall by text search or category filter. Record project patterns (naming conventions, architecture decisions). Consolidate session findings into permanent knowledge. Knowledge persists per project in `~/.lean-ctx/knowledge/`
- **Multi-Agent Context Sharing (`ctx_agent`)** — Agent registry with scratchpad messaging system. Multiple AI agents (Cursor, Claude, Copilot, etc.) can register, share findings via broadcast or targeted messages, and coordinate work on the same project. Includes automatic heartbeat, stale agent cleanup, and file-based locking for safe concurrent access
- **Antigravity editor support** — Added Antigravity (Gemini-based IDE) as a supported editor in `lean-ctx setup` auto-detection, `lean-ctx doctor` diagnostics, and the website prompt generator
- **Dashboard: Active Agents panel** — Real-time view of all registered AI agents, their roles, status, and recent scratchpad messages
- **Dashboard: Project Knowledge panel** — Shows all stored project facts and patterns with category grouping

### Fixed

- **Dashboard CEP score calculation** — Now uses real read-mode diversity from `cep.modes` (full, map, signatures, diff, aggressive, entropy, auto) instead of counting distinct tool names. Combined compression rate uses the higher of shell and MCP compression. Score improved from misleading 28/100 to accurate 53/100
- **Dashboard project root detection** — Knowledge API now walks up to `.git` directory for correct project identification instead of using `cwd` (which could be a subdirectory)
- **Clippy warnings** — Fixed collapsible if, unwrap_or_default, trim before split_whitespace

---

## [2.6.1] — 2026-03-27

### Fixed

- **Dashboard cost model** — Replaced flat $2.50/M pricing with realistic tiered model ($3/M input, $15/M output) matching Claude/GPT API pricing. Dashboard now shows separate input and output cost breakdowns
- **Dashboard redesign** — Hero section with total savings, visual cost comparison (with vs without lean-ctx), daily savings rate chart, filterable command table, sticky header, and fade-up animations
- **Dashboard CEP values** — Fixed stale CEP metrics by prioritizing computed stats from `stats.json` over potentially stale `mcp-live.json` data (was showing 0% cache hit rate)
- **`lean-ctx gain` cost model** — Terminal gain command now uses the same tiered pricing model with input/output breakdown and estimated output token savings via CEP/TDD

### Changed

- **README** — Updated cost model references from $2.50/M to $3/M input + $15/M output, added Scientific Compression Engine section documenting v2.6 features
- **Website** — Updated all version references to v2.6.1, added 6 new Scientific Features to features page (Adaptive Entropy, Attention Model, TF-IDF Codebook, Feedback Loop, Information Bottleneck, ctx_overview)

---

## [2.6.0] — 2026-03-27

### Added

- **Adaptive per-language entropy thresholds** — Entropy compression now uses language-specific BPE entropy thresholds (e.g. Rust 0.85, Python 1.2, JSON 0.6) with Kolmogorov complexity adjustment. Header files (`.h`, `.hpp`) and config files get the most aggressive compression
- **Task-conditioned compression** — New `task_relevance` module computes relevance scores for project files using BFS through the dependency graph + keyword matching. Files are ranked and assigned optimal read modes (full/signatures/map/reference)
- **`ctx_overview` MCP tool** — Multi-resolution project overview that shows files grouped by task relevance. Provides a compact project map at session start, recommending which files to read at which detail level
- **Heuristic attention prediction model** — Position-based U-curve (alpha/beta/gamma) combined with structural importance scoring (definitions > errors > control flow > imports > comments > braces). Predicts which lines receive the most transformer attention
- **Cross-file semantic dedup via TF-IDF** — Codebook system identifies patterns appearing in 3+ files and creates short `§N` references. TF-IDF cosine similarity detects semantically duplicate files in `ctx_dedup`
- **Information Bottleneck filter** — Approximates the IB method using line-level relevance scoring + positional U-weighting to select the most informative subset within a token budget
- **Feedback loop** — Tracks compression outcomes (thresholds used, tokens saved, turns taken, task completion) in `~/.lean-ctx/feedback.json`. After 5+ sessions per language, adaptively learns optimal entropy/jaccard thresholds
- **Output token budget in CEP** — System prompt now guides LLMs on response length by task complexity: Mechanical (max 50 tokens), Standard (max 200), Architectural (full reasoning)
- **Prefix-cache aligned system prompt** — Static instructions placed before variable session state for optimal KV-cache reuse by API providers

### Changed

- **Entropy compression** — `entropy_compress_adaptive()` now accepts path for per-language threshold selection. Existing `entropy_compress()` preserved for backward compatibility
- **`ctx_dedup` analysis** — Now includes TF-IDF cosine similarity analysis for semantic duplicate detection alongside existing block-based dedup
- **LITM module** — New `content_attention_efficiency()` function combines positional U-curve with structural importance analysis for content-aware attention prediction

---

## [2.5.3] — 2026-03-27

### Added

- **VS Code / GitHub Copilot MCP support** — `lean-ctx init --agent copilot` now creates `.vscode/mcp.json` with lean-ctx as MCP server instead of incorrectly installing a Claude Code hook. Copilot agents gain access to `ctx_read`, `ctx_shell`, `ctx_search`, `ctx_tree` as direct tools
- **`lean-ctx setup` detects VS Code** — Auto-setup now detects VS Code installations on macOS, Linux, and Windows and configures `mcp.json` in the VS Code user directory
- **`lean-ctx doctor` checks VS Code MCP** — Diagnostics now include VS Code / Copilot MCP configuration status

### Changed

- **Landing page particle animation** — Reduced mouse attraction force for subtler, smoother cursor interaction

---

## [2.5.2] — 2026-03-27

### Fixed

- **MCP instructions: Write/StrReplace confusion** — Restructured MCP system prompt to clearly separate "REPLACE" tools (Read → ctx_read, Shell → ctx_shell, Grep → ctx_search) from "KEEP" tools (Write, StrReplace, Delete, Glob). Agents no longer think they must ctx_read before creating files with Write (#20)
- **`lean-ctx doctor` on Windows** — Fixed OS Error 193 ("not a valid Win32 application") when doctor tried to run `lean-ctx --version` via the npm `.cmd` shim. Now prefers `.exe` binaries from `where.exe` output and falls back to alternative candidates
- **`stats.json missing` false alarm** — `lean-ctx doctor` no longer shows a red error for missing `stats.json` on fresh installs. Now shows yellow "not yet created (will appear after first use)" and counts as passed
- **`lean-ctx gain` missing MCP hint** — When MCP/CEP shows 0% savings (shell hook only, no MCP server configured), gain now displays a clear hint to run `lean-ctx setup` for full token savings

### Added

- **GitHub Discussions** — Enabled Discussions tab for questions and community support (#19)

---

## [2.5.1] — 2026-03-27

### Fixed

- **`ctx_read` cache bypass** — Added `start_line` parameter and `lines:N-M` mode to MCP schema. When an LLM requests specific lines from a cached file, lean-ctx now returns actual content instead of the compact `cached Nt NL` stub. Fixes issue where LLMs fell back to native Read tools after wasting 3-5 minutes (#17)
- **`pi install` registry resolution** — Fixed `lean-ctx init --agent pi` to use `npm:pi-lean-ctx` prefix so Pi resolves the package from npm registry instead of treating it as a local path
- **Improved MCP instructions** — System prompt now explicitly guides LLMs to use `fresh=true`, `start_line`, or `lines:N-M` mode when they encounter a cache stub, preventing fallback to native tools
- **pi-lean-ctx v1.0.2** — Added 40+ file extensions to code detection (`.vue`, `.svelte`, `.astro`, `.html`, `.css`, `.scss`, `.lua`, `.zig`, `.dart`, `.scala`, `.sql`, `.graphql`, `.proto`, `.tf`, `.sh`, and more). Partial reads with `offset`/`limit` now route through lean-ctx `lines:N-M` mode instead of bypassing compression (#18)

---

## [2.5.0] — 2026-03-27

### Added

- **`lean-ctx setup`** — One-command setup that installs shell aliases, auto-detects installed AI editors (Cursor, Claude Code, Windsurf, Codex CLI, Gemini CLI, Zed), creates MCP config files, installs agent instructions, and runs diagnostics. Replaces the multi-step manual installation process
- **`lean-ctx doctor` improvements** — Enhanced diagnostic output with better detection of editor configurations and more actionable error messages

### Changed

- **Website documentation restructured** — Getting Started page now follows a clean chronological flow: Quick Install → Step 1: Install → Step 2: Setup → Step 3: Editor Setup → Step 4: Verify. Docs sidebar reorganized into logical groups for better navigation
- **Installation Prompt Generator redesigned** — Integrated into documentation style with consistent headings, labels, and layout. Now generates prompts referencing `lean-ctx setup` for simplified installation

---

## [2.4.1] — 2026-03-27

### Added

- **Persistent Project Graph** — New `ctx_graph` with 5 actions (`build`, `related`, `symbol`, `impact`, `status`). Incrementally scans project files, persists index to `~/.lean-ctx/graphs/`, and enables symbol-level reads at up to 93% token savings over full file reads
- **`install.sh`** — Universal installer with `--download` mode (no Rust required), SHA256 checksum verification, and one-liner support: `curl -fsSL .../install.sh | bash -s -- --download`
- **`lean-ctx-bin` npm package** — Pre-built binary distribution via npm for users without Rust: `npm install -g lean-ctx-bin`
- **`lctx` launcher** — Multi-agent launcher script supporting Claude Code, Cursor, Gemini CLI, Codex, Windsurf, and Cline. Auto-detects agent, builds project graph, and configures lean-ctx in one command
- **Graph-based intent** — `ctx_intent` now uses the persistent project graph for more precise file selection when a graph index is available

### Fixed

- **Self-update Linux target mismatch** — `updater.rs` now matches the `gnu` targets produced by CI instead of expecting `musl`. Release CI also builds `musl` targets for maximum portability

---

## [2.4.0] — 2026-03-27

### Fixed

- **`excluded_commands` now enforced** — Commands listed in `~/.lean-ctx/config.toml` under `excluded_commands` now actually bypass compression and return raw output. Previously the config option was parsed but never checked ([#10](https://github.com/yvgude/lean-ctx/issues/10))
- **Windows Git Bash shell flag** — `shell_and_flag()` now correctly assigns `-c` for POSIX-style shells (bash/sh/zsh/fish) on Windows, instead of `/C` (cmd.exe only). Fixes the `/C: Is a directory` error and exit code 126 when using lean-ctx with Git Bash ([#7](https://github.com/yvgude/lean-ctx/issues/7), [#11](https://github.com/yvgude/lean-ctx/issues/11), via PR [#8](https://github.com/yvgude/lean-ctx/pull/8))

### Added

- **`lean-ctx-on` / `lean-ctx-off` / `lean-ctx-status`** — Shell toggle functions installed by `lean-ctx init --global`. Switch between compressed AI mode and human-readable output without restarting the shell. `LEAN_CTX_ENABLED=0` disables by default ([#13](https://github.com/yvgude/lean-ctx/issues/13))
- **Slow query log** — Commands exceeding `slow_command_threshold_ms` (default: 5000ms) are automatically logged to `~/.lean-ctx/slow-commands.log`. New `lean-ctx slow-log [list|clear]` command to inspect or clear the log. Configure threshold in `config.toml` ([#14](https://github.com/yvgude/lean-ctx/issues/14))
- **`lean-ctx update`** — Built-in self-update command. Fetches the latest release from the GitHub API, downloads the appropriate binary archive for the current platform (macOS arm64/x86\_64, Linux x86\_64/aarch64 musl, Windows x86\_64), and safely replaces the running binary. `lean-ctx update --check` checks for updates without installing ([#15](https://github.com/yvgude/lean-ctx/issues/15))
- **`slow_command_threshold_ms` config option** — New field in `~/.lean-ctx/config.toml` (default: 5000). Set to `0` to disable slow logging

### Docs

- **README** updated with `lean-ctx-on/off/status` usage and `lean-ctx update` examples (via PR [#9](https://github.com/yvgude/lean-ctx/pull/9))
- **`lean-ctx-session-metrics.mdc`** example added to `rust/examples/` showing how to surface live MCP session token savings in agent transcripts

---

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
