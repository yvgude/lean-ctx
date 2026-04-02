# Changelog

All notable changes to lean-ctx are documented here.

## [2.14.0] — 2026-04-02

### Intelligence Layer Architecture

lean-ctx transforms from a pure compressor into an **Intelligence Layer** between user, AI tool, and LLM. Based on empirical attention analysis (L-curve discovery), all output formats, filters, and encoding strategies are now calibrated to how LLMs actually process context — not how we assumed they would.

### Added
- **`ctx_preload` MCP tool** — Proactive context orchestration based on task description + import graph. Analyzes project files, extracts task-critical lines, key signatures, and imports, then delivers a compact L-curve-optimized context snapshot. Replaces 3-5 individual `ctx_read` calls with a single ~50 token summary
- **L-Curve Context Reorder Engine** (`core/neural/context_reorder.rs`) — Classifies lines into 7 categories (ErrorHandling, Import, TypeDefinition, FunctionSignature, Logic, ClosingBrace, Empty) and reorders output so the most task-relevant lines occupy position 0 (where LLMs allocate 20x more attention)

### Changed
- **Output-Format Reordering** — `ctx_read` output now places file content first, symbol tables second, and metadata/savings last. Task-critical information occupies position 0.0 in the context window instead of being buried after a header line
- **IB-Filter 2.0** — Information Bottleneck filter upgraded with empirical L-curve attention weights (via `LearnedAttention`), score-descending output order (most relevant first instead of original line order), error-handling prioritization, and task-keyword summary as first output line
- **LLM-Native Encoding** — Token optimizer expanded with 15+ new rules: generic type simplification (`Vec<String>` → `Vec`, `HashMap<K,V>` → `HashMap`, `Option<T>` → `Option`), lifetime elision (`&'a str` → `&str`), full path shortening (`std::collections::HashMap` → `HashMap`), and closing brace collapsing (`}\n}\n}` → `}}}`)
- **System-Prompt Cleanup** — Removed duplicate `decoder_block` insertion (~200 wasted tokens), consolidated 3 redundant "NEVER use native tools" warnings into 1, replaced all Unicode symbols (`λ§∂⊕⊖∆→⇒✓✗⚠`) with 1-token ASCII equivalents (`fn mod iface + - ~ -> ok fail`) — saves ~50 tokens AND improves LLM comprehension
- **CRP/TDD Response Shaping** — All instruction templates and output formats now use ASCII abbreviations instead of multi-token Unicode symbols. Dynamic response budgets by complexity: simple queries get `<=50 tokens, 1 line`, complex tasks get `<=500 tokens`
- **Diff/delta output** — Replaced Unicode `∅`/`∂` symbols with ASCII `(no changes)`/`diff` for consistent 1-token encoding

### Fixed
- **Shell hook compression broken** — `exec_buffered()` had a `piped && !force_compress` guard that bypassed compression entirely when stdout was piped (which is always the case for agent hook calls). All `lean-ctx -c` commands routed through agent hooks returned raw uncompressed output. Guard removed — compression now always applies in buffered mode
- **Shell hook stats lost** — `lean-ctx -c` path called `std::process::exit(code)` without flushing the stats buffer. Since Rust's `process::exit()` skips destructors and the buffer only auto-flushes every 30 seconds, all stats from CLI invocations were silently discarded. Added `stats::flush()` before exit. Dashboard "Shell Hook" section now shows actual data instead of permanent zero

## [2.13.1] — 2026-04-02

### Fixed
- **Claude Code hooks: correct response format** — PreToolUse hooks now use the current `hookSpecificOutput` format with `permissionDecision` instead of the deprecated top-level `decision`/`reason` fields. This fixes Claude Code ignoring lean-ctx redirect hooks and continuing to use native Read/Grep/Bash tools.
- **Claude Code global CLAUDE.md** — `lean-ctx setup` and `lean-ctx init --agent claude --global` now install `~/.claude/CLAUDE.md` with instructions to prefer lean-ctx MCP tools. Previously, global mode skipped CLAUDE.md entirely, leaving Claude Code with no guidance to use MCP tools.
- **PowerShell .cmd resolution (Windows)** — Fixed issue #38 where npm/pnpm/yarn/eslint/prettier/tsc functions used hardcoded `.cmd` suffixes that failed in some Windows environments (e.g., Warp terminal). Functions now resolve the real executable path at profile load time via `Get-Command -CommandType Application`.
- **Portable binary paths in MCP configs** — `resolve_binary_path()` now returns `lean-ctx` (PATH-resolvable) instead of a hardcoded full path when lean-ctx is available in PATH. Prevents stale paths when switching between npm and cargo installations.

## [2.13.0] — 2026-04-01

### Added
- **Verdent IDE support**: Full integration for Verdent AI — MCP config at `~/.verdent/mcp.json`, rules injection at `~/.verdent/rules/lean-ctx.md`, auto-detection via `~/.verdent` directory, `lean-ctx init --agent verdent`, `lean-ctx setup` auto-detection, `lean-ctx doctor` diagnostics, and `lean-ctx uninstall` cleanup
- **21 total supported IDEs/agents** (up from 20): Cursor, Claude Code, GitHub Copilot, Windsurf, VS Code, Zed, Codex CLI, Gemini CLI, OpenCode, Pi, Qwen Code, Trae, Amazon Q Developer, JetBrains IDEs, Antigravity, Cline, Roo Code, Aider, Amp, AWS Kiro, Verdent
- **Scientific verification test suite**: 34 new tests mathematically validating Shannon Entropy, Normalized BPE Entropy, Kolmogorov Complexity proxy, Jaccard Similarity, LITM attention model, Information Bottleneck filter, Symbol Map ROI, Safeguard Ratio, and Cost Model
- **Normalized BPE token entropy** (`normalized_token_entropy`): Shannon entropy normalized to `[0, 1]` for more robust entropy-based content filtering — dual-gate mechanism requires both raw and normalized entropy to be below thresholds before removing a line

### Fixed
- **Token savings inflation in Terminal + Dashboard**: The headline "tokens saved" number in `lean-ctx gain` and the web dashboard incorrectly included speculative output token savings. Now shows only measured input token savings. Output savings remain in the USD cost breakdown where they are clearly labeled as estimates
- **VS Code uninstall**: Fixed wrong file path (`settings.json` → `mcp.json`) and added `servers` key removal alongside `mcpServers` for correct MCP config cleanup
- **Zed macOS path mismatch**: Setup now uses `~/Library/Application Support/Zed/` on macOS (matching uninstall), instead of `~/.config/zed/` which is the Linux path
- **Gemini uninstall path**: Added `~/.gemini/GEMINI.md` (the actual inject target) to the uninstall cleanup list, alongside the legacy `~/.gemini/rules/` path
- **Incomplete uninstall rules list**: Added 8 missing IDEs to `remove_rules_files`: Zed, Cline, Roo Code, OpenCode, Continue, Codex instructions.md, Verdent
- **Cursor MDC version marker**: Added `<!-- lean-ctx-rules-v5 -->` and `<!-- /lean-ctx -->` markers to the `lean-ctx.mdc` template, preventing `inject_all_rules` from overwriting the richer project-level template with the shorter embedded version

### Changed
- **LITM positional attention**: Upgraded from piecewise linear to quadratic U-curve, better matching empirical findings (Liu et al., 2023) on LLM attention distribution
- **Information Bottleneck score**: Replaced additive constants with weighted factors (relevance ×0.6, information ×0.25, attention ×0.15) plus a 0.05 floor, improving discrimination of task-relevant content
- **Entropy compression**: Dual-gate filtering — lines are only removed if both raw token entropy AND normalized token entropy fall below thresholds

## [2.12.9] — 2026-04-01

### Fixed
- **ctx_tree inflated savings**: Raw tree comparison now uses the same depth and hidden-file settings as the compact output. Previously, the raw tree walked the *entire* directory (no depth limit), inflating savings by 10–100x. A project with 50K files would report ~500K "saved tokens" per `ctx_tree` call
- **Cache hit tracking**: `session.record_cache_hit()` is now only called for actual `ctx_read` cache re-reads, not for every tool call with any savings
- **Output savings model**: Reduced the per-call output token bonus from 330 to 60 tokens (180 vs 120 estimated verbose/concise output), preventing inflated USD savings estimates
- **`count_files_in_dir`**: Now respects `show_hidden` setting and has a depth limit of 5, preventing unbounded recursive walks into deep directory trees

### Added
- **`lean-ctx gain --reset`**: New command to clear all stats data. Recommended for users upgrading from versions with inflated savings
- **`ctx_tree` regression test**: Asserts that raw tree tokens stay under 5000 and savings ratio under 90% for same-depth comparison

## [2.12.8] — 2026-04-01

### Added
- **AWS Kiro support**: Full integration for Amazon's agentic IDE — MCP config at `~/.kiro/settings/mcp.json`, rules injection at `~/.kiro/rules/lean-ctx.md`, auto-detection via `~/.kiro` directory, `lean-ctx init --agent kiro`, and `lean-ctx setup` auto-detection
- **20 total supported IDEs/agents** (up from 19): Cursor, Claude Code, GitHub Copilot, Windsurf, VS Code, Zed, Codex CLI, Gemini CLI, OpenCode, Pi, Qwen Code, Trae, Amazon Q Developer, JetBrains IDEs, Antigravity, Cline, Roo Code, Aider, Amp, AWS Kiro
- **Prompt Generator**: Kiro added to the interactive prompt generator on the Getting Started docs page — select "Kiro" and get a copy-paste installation prompt for any AI assistant
- Website: Kiro added to compatibility page, getting-started docs with dedicated setup section, and homepage IDE count

## [2.12.7] — 2026-04-01

### Added
- **MCP Tool Enforcement — 3-Layer Strategy**: LLMs now reliably use lean-ctx MCP tools instead of native tools across all supported IDEs
  - **Layer 1: PreToolUse Hooks** — Soft-redirect hooks for Claude Code, Cursor, and Gemini CLI that intercept native `Read`, `Grep`, `ListFiles`, and `ListDirectory` calls, returning a block decision with guidance to use `ctx_read`/`ctx_search`/`ctx_tree` instead. Safety fallback: if lean-ctx MCP server is unreachable, native tools are allowed through
  - **Layer 2: LITM-Optimized Rules v5** — Rules injection now places critical "NEVER use native tools" instructions at both the beginning AND end of every rules block (exploiting LLM attention patterns). Updated from v4 to v5 with stronger "FORBIDDEN / USE INSTEAD" tool mapping tables
  - **Layer 3: Project-Level Reinforcement** — `lean-ctx init --agent` now auto-creates `AGENTS.md` and `.cursorrules` in the project root with lean-ctx tool mappings
- **6 new IDE/agent rules targets**: Qwen Code (`~/.qwen/rules/`), Trae (`~/.trae/rules/`), Amazon Q Developer (`~/.aws/amazonq/rules/`), JetBrains IDEs (`~/.jb-rules/`), Antigravity (`~/.gemini/antigravity/rules/`), Pi Coding Agent (`~/.pi/rules/`) — all with auto-detection and dedicated markdown rules injection
- **19 total supported IDEs/agents** with full rules coverage (up from 13)
- **Auto-refresh on update**: `lean-ctx update` now automatically refreshes all rules files and hook scripts after installing a new binary via `post_update_refresh()`. MCP server startup also triggers `refresh_installed_hooks()` to ensure hooks match the current binary
- **Complete uninstall cleanup**: `lean-ctx uninstall` now removes rules files (13 locations), hook scripts (7 files), and Cursor `hooks.json` in addition to MCP configs and data directory

### Changed
- **Token Optimization Deep Dive** (Phase 1–4):
  - Removed double savings footer on `ctx_read` (server.rs duplicate eliminated)
  - Config caching with `OnceLock` — no more disk read on every call
  - Entropy mode header compressed to single line (saves 20-40 tokens/read)
  - Stale note compressed to single line (saves ~15 tokens)
  - Stats batching with in-memory accumulator + periodic flush (reduces disk I/O)
  - ModePredictor/FeedbackStore deferred writes (no longer synchronous on hot path)
  - `count_tokens` caching for repeated strings
  - Added compression patterns for `cargo doc/tree/fmt/update`, `git show/rebase/submodule`, `docker system df/info/version`, `uv/conda/pipx/poetry`
  - Fixed npm test heuristic (proper Jest/Vitest summary parsing instead of substring matching)
  - Strengthened CRP/TDD output constraints with token budget hints
  - Trimmed tool descriptions to minimum viable length
- System instructions now include redundant LITM-end reminder block for maximum LLM attention
- Hook installation refactored into separate script-generation and config-update functions for better reusability

### Fixed
- JetBrains IDE detection now checks platform-specific paths (`Library/Application Support/JetBrains` on macOS, `.config/JetBrains` on Linux)

## [2.12.6] — 2026-04-01

### Added
- **Bulletproof shell aliases**: All aliases now use a `_lc` wrapper function that silently falls back to the original command if the lean-ctx binary is missing (exit 127) or not executable (exit 126). If lean-ctx is removed, commands like `ls`, `git`, `grep` continue working as if lean-ctx was never installed
- **Binary existence guard**: Shell hook activation checks `command -v lean-ctx` before setting up aliases — prevents broken commands if binary is not in PATH
- **Shell config backup**: `.lean-ctx.bak` backup created before every modification to `.zshrc`, `.bashrc`, `config.fish`, or PowerShell profile
- **Panic handler**: Human-friendly error message with recovery instructions (`lean-ctx-off`, `lean-ctx uninstall`) instead of cryptic Rust backtraces
- **`lean-ctx init --dry-run`**: Preview exactly what `init` would modify without changing anything
- **Troubleshooting section**: Added to `--help` output and README with quick-fix commands
- **Improved post-install message**: Shows disable/enable/uninstall/doctor commands and backup path

### Changed
- Shell hook aliases route through `_lc` wrapper instead of directly calling binary path — zero performance difference, full safety net
- Post-install output is more structured with clear recovery options

## [2.12.5] — 2026-04-01

### Fixed
- **Shell hook output buffering**: Commands routed through `lean-ctx -c` (via shell aliases) now stream output in real-time instead of buffering until completion. Progress bars, build output, and streaming logs appear immediately as they happen
- **Excluded command overhead**: Interactive tools (vim, htop, ssh, etc.) now use `Stdio::inherit()` directly for zero-overhead passthrough — no piping or buffering at all

### Changed
- Shell exec refactored into 3 distinct modes: `exec_inherit` (passthrough), `exec_streaming` (terminal, real-time), `exec_buffered` (AI agent, compressed) — each optimized for its use case
- Streaming mode captures output in parallel threads (4KB chunks) for stats recording while forwarding to terminal with immediate flush
- Savings summary shown on stderr after streaming commands complete (only when >50 tokens and >10% savings)

## [2.12.4] — 2026-04-01

### Fixed
- **Shell exec `-c` bug**: `lean-ctx -c "command"` now works correctly — single command strings are no longer double-quoted by `shell_join()`, which caused zsh to treat the entire string as one command name ("command not found")
- **Git log compression**: `git log -p` and `git log --stat` now filter diff/stat content, achieving 95-99% token savings (was 2.6%)
- **Git commit compression**: Pre-commit hook output is now summarized ("N hooks passed"), failed hooks are preserved. Branch name regex supports `/`, `.`, `-` (e.g. `feature/my-branch`)
- **CEP over-counting**: Session stats now use delta tracking with PID-based detection — prevents exponential inflation of token counts when recording snapshots
- **ctx_read savings accuracy**: Token count is now calculated before appending the `[saved N tokens]` note, giving accurate savings figures

### Added
- `lean-ctx stats` CLI command with `stats reset-cep` to clear inflated CEP data while preserving shell hook stats
- 12 new verification tests covering git compression, CEP delta tracking, and E2E with real git data

## [2.12.3] — 2026-04-01

### Fixed
- **PowerShell npm bug** (fixes #37): Shell hook now uses `.cmd` extensions for Node.js tools (`npm.cmd`, `pnpm.cmd`, `yarn.cmd`, `tsc.cmd`, `eslint.cmd`, `prettier.cmd`) to avoid `cmd.exe` resolution failures on Windows
- **PowerShell shell detection**: `lean-ctx -c` now detects PowerShell context via `PSModulePath` env var and uses `pwsh.exe`/`powershell.exe` instead of falling back to `cmd.exe`
- **Gain dashboard alignment**: All KPI values, labels, cost breakdown, top commands, and recent days now use ANSI-aware `pad_right()` for pixel-perfect terminal alignment regardless of color codes
- **Gain dashboard spacing**: More vertical whitespace above logo and below tips for a cleaner look

### Added
- Theme-aware contextual tips in `lean-ctx gain`: suggests theme commands for users on default theme, shows current theme for users who already customized

## [2.12.2] — 2026-04-01

### Added
- `autoApprove` array in MCP config during `lean-ctx init` — enables auto-approval of tool calls in clients that support it (Cline, Roo Code, and future Antigravity support)
- Dashboard `--host=0.0.0.0` now shows actual bind address instead of misleading "localhost"

### Changed
- Gain dashboard redesign: wider layout (62 cols), more whitespace between sections, better column spacing, more elegant typography

## [2.12.1] — 2026-03-31

### Added
- Dashboard `--host=` flag and `LEAN_CTX_HOST` env var to bind to a custom IP address (fixes #36)
- Enables remote access for headless development (e.g. `lean-ctx dashboard --host=0.0.0.0`)
- Security warning when binding to non-localhost addresses (no auth)
- Auto-open browser only when binding to localhost

### Changed
- Logo animation now uses active theme colors (primary↔secondary sine wave) instead of hardcoded rainbow HSL

## [2.12.0] — 2026-03-31

### Changed
- **Granular tools as default**: All clients now get 25 individual `ctx_*` tools instead of the unified `ctx()` meta-tool. Unified mode is still available via `LEAN_CTX_UNIFIED=1` env var. This improves model reliability in calling MCP tools.
- **Slimmer MCP instructions**: Reduced instruction text by ~40%. Removed verbose CEP examples, Output Budget rules, and redundant enforcement prose that caused instruction overload.
- **Slimmer rules injection**: Reduced injected rules (CLAUDE.md, .cursor/rules, etc.) by ~60%. Eliminated duplication with MCP instructions. Rules now focus on the tool mapping table only.
- **Unified mode opt-in only**: `LEAN_CTX_UNIFIED=1` explicitly enables unified mode. `LEAN_CTX_FULL_TOOLS=1` still forces granular mode.

## [2.11.1] — 2026-03-31

### Changed
- Brand title "lean-ctx" now renders in theme primary/secondary colors across all dashboards (gain, graph, daily, CEP, theme preview)

## [2.11.0] — 2026-03-31

### Added
- **Theme System**: 6 built-in color themes (default, neon, ocean, sunset, monochrome, cyberpunk)
- **`lean-ctx theme` subcommand**: `list`, `set`, `export`, `import`, `preview`
- **Gradient bars**: All progress bars use 24-bit RGB color gradients (bar_start → bar_end)
- **Gradient sparklines**: Sparkline charts use theme-aware color transitions
- **Animated countup**: Token savings animate from 0 on `lean-ctx gain` (TTY only)
- **Box-frame layout**: Dashboard sections use Unicode box-drawing characters (╭─╮│╰─╯)
- **Custom themes via TOML**: Create `~/.lean-ctx/theme.toml` with 11 configurable color slots
- **Theme export/import**: Share themes as `.toml` files
- **`config set theme`**: Quick theme switching via config command
- **NO_COLOR support**: Respects `NO_COLOR` env var and non-TTY environments

### Changed
- `format_gain()`, `format_gain_graph()`, `format_gain_daily()`, `format_cep_report()` all use Theme system
- Logo gradient adapts to active theme colors (primary → secondary)
- Removed hardcoded ANSI color constants from stats.rs

## [2.10.0] — 2026-03-29

### Added

- **Task-Conditioned Compression** — New `task` mode in `ctx_read` uses the Information Bottleneck filter to return only task-relevant lines. A 2000-line file shrinks to ~600 lines when a task is set, yielding **70% token savings** on large code files
- **Auto-Task-Enhancement** — When reading large code files (>1000 tokens) in `full` mode with a session task set, automatically applies IB filtering at 50% budget ratio. Output is marked with `[task-enhanced]`
- **Smart Default Mode** — `ctx_read` without an explicit mode now auto-selects the optimal compression (diff for changed files, task for large code with task, signatures for huge files) instead of always using `full`
- **`reference` Mode** — One-line metadata output (`path: N lines, M tok (ext)`) for files that are almost certainly irrelevant. Near-100% token savings
- **tree-sitter Dart** — Full AST-based signature extraction for Dart (classes, enums, mixins, typedefs)
- **tree-sitter Bash/Shell** — Function definitions in `.sh`/`.bash` files
- **tree-sitter Scala** — Classes, objects, traits, enums, functions, type definitions
- **tree-sitter Elixir** — Modules (`defmodule`), functions (`def`/`defp`), macros (`defmacro`)
- **tree-sitter Zig** — Function declarations with pub/private visibility
- **Svelte/Vue SFC Support** — Extracts `<script>` blocks from `.svelte`/`.vue` files and parses them as TypeScript/JavaScript for proper signature extraction

### Changed

- Task context from session state is now passed through to `ctx_read` for task-aware compression
- `select_mode` in `ctx_smart_read` now considers task context when choosing the default mode
- Total supported tree-sitter languages: **19** (was 14)

## [2.9.16] — 2026-03-31

### Added

- **LITM-optimized Instructions** — Enforcement directive placed at first AND last position in MCP server instructions to avoid Lost-In-The-Middle attention drop
- **Action-first Tool Descriptions** — Tool descriptions now lead with the action ("Read files...", "Run shell commands...") instead of meta-info ("REPLACES...") for better agent tool matching
- **IDE-specific Tool Call Examples** — Injected rules now include concrete syntax examples for Cursor (`CallMcpTool`), Claude Code (`mcp__lean-ctx__ctx_read`), and other IDEs
- **Token Savings Feedback** — Every tool response now shows `[saved X tokens vs native Read/Shell/Grep/ls]` as positive reinforcement for the agent
- **Rules v4** — Updated injected agent rules with all of the above, auto-replaces v3 rules on next MCP connect

## [2.9.15] — 2026-03-31

### Fixed

- **Tips in English** — Contextual tips in `lean-ctx gain` are now in English and user-friendly

## [2.9.14] — 2026-03-31

### Added

- **Auto-Consolidate** — Session findings and decisions are automatically persisted into project knowledge at every auto-checkpoint (every 15 tool calls). No manual `ctx_knowledge consolidate` needed
- **Cache-TTL-aware compression** — When the prompt cache has expired (>60 min idle), `ctx_read` automatically upgrades to more aggressive compression modes (`full` → `aggressive`, `map` → `signatures`), saving tokens when the cache would be rebuilt anyway
- **Contextual tips in `lean-ctx gain`** — Rotating daily tips based on your actual usage stats. Suggests unused modes, features, and optimization strategies
- **macOS codesign fix** — `lean-ctx update` now automatically re-signs the binary after replacement on Apple Silicon, preventing SIGKILL from Gatekeeper that could make the binary hang

### Fixed

- **Binary hang after copy on macOS Apple Silicon** — Copying the lean-ctx binary without re-signing caused macOS to kill it with SIGKILL. The updater now runs `codesign --force -s -` automatically after every binary replacement

## [2.9.13] — 2026-03-31

### Improved

- **Clear IDE restart guidance** — `lean-ctx setup`, `lean-ctx update`, and `npm install lean-ctx-bin` now show explicit numbered next-step instructions, prominently highlighting that the IDE must be restarted for changes to take effect
- **Auto-inject agent rules on MCP connect** — When any IDE connects to lean-ctx's MCP server, agent rules are silently updated in the background. No manual `lean-ctx setup` needed after updates

## [2.9.12] — 2026-03-31

### Added

- **Automatic agent rules injection** — `lean-ctx setup` now injects tool-preference rules into every detected AI tool's global configuration, ensuring agents actually use lean-ctx MCP tools instead of falling back to native equivalents. Supported:
  - Claude Code (`~/.claude/CLAUDE.md`)
  - Codex CLI (`~/.codex/instructions.md`)
  - Cursor (`~/.cursor/rules/lean-ctx.mdc`)
  - Windsurf (`~/.codeium/windsurf/rules/lean-ctx.md`)
  - Gemini CLI (`~/.gemini/GEMINI.md`)
  - VS Code / Copilot (`github-copilot-instructions.md`)
  - Zed, Cline, Roo Code, OpenCode (dedicated rules files)
- Rules are **append-only** — existing user rules are never modified or deleted
- Idempotent: marker-based detection prevents duplicate injection

## [2.9.11] — 2026-03-30

### Fixed

- **Massively expanded passthrough list** (28 → 68 commands) — Commands that stream, run interactively, or use TUI are now correctly passed through without buffering. Previously, commands like `docker logs`, `kubectl logs`, `psql`, `ping`, or `tmux` would hang because lean-ctx tried to capture and compress their output. New categories:
  - **Docker**: `docker logs`, `docker attach`, `docker exec -it`, `docker run -it`, `docker stats`, `docker events`, `docker compose run`
  - **Kubernetes**: `kubectl logs`, `kubectl exec -it`, `kubectl attach`, `kubectl port-forward`, `kubectl proxy`
  - **Database REPLs**: `psql`, `mysql`, `sqlite3`, `redis-cli`, `mongosh`
  - **System streaming**: `journalctl -f`, `dmesg -w`, `ping`, `strace`, `tcpdump`, `tail -F`
  - **Dev servers**: `gatsby develop`, `ng serve`, `remix dev`, `wrangler dev`, `hugo server`, `jekyll serve`, `bun dev`, `expo start`
  - **Editors**: `vi`, `micro`, `helix`, `emacs`, `more`
  - **Terminal multiplexers**: `tmux`, `screen`
  - **Network**: `telnet`, `nc`, `ncat`
  - **Language REPLs**: `python -i`, `irb`, `rails console`, `iex`
  - **Rust**: `cargo watch`

## [2.9.10] — 2026-03-30

### Fixed

- **Codex CLI auto-detection in `lean-ctx setup`** (Issue #35) — Setup now detects OpenAI Codex CLI via `which codex` in addition to checking for `~/.codex/`. Previously, if Codex was installed but hadn't created its config directory yet, setup would skip it entirely. Reported by @Jain2098.

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
