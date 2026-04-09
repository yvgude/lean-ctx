# Changelog

All notable changes to lean-ctx are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/).

## [2.21.7] — 2026-04-09

### Cleanup + Website Redesign

#### Changed — Remove Hook E2E Test Suite
- **Removed `hook_e2e_tests.rs`** — The hook E2E test file and its corresponding CI workflow (`hook-integration`) have been removed. The pipe guard behavior is already covered by the integration tests in `integration_tests.rs` and the unit tests in `cli.rs`. This eliminates a redundant CI job that depended on `generate_rewrite_script`, simplifying the test matrix.

#### Changed — Website: LeanCTL Section Redesigned
- **Consistent page design** — The LeanCTL ecosystem section on the homepage now uses the same visual patterns (compare-cards, layer-cards, stats-grid) as the rest of the page, replacing the custom TUI terminal mockup with ~150 lines of dedicated CSS.
- **Real product facts** — Compare cards show concrete token savings from leanctl.com (4,200 → 48 tokens for file reads, 847 → 42 for test output, 4,200 → ~13 for re-reads).
- **Three feature cards** — "23 Built-in Tools", "Thinking Steering", "Bring Your Own Key" in the standard layer-card layout.
- **Stats grid** — "up to 90% savings", "23 tools", "8 compression modes", "0 data sent to us".

#### Changed — Navigation: Dedicated Ecosystem Dropdown
- **New top-level nav item** — "Ecosystem" mega dropdown with two columns: "AI Agents" (LeanCTL) and "Community" (GitHub, Discord, Blog).
- **Product dropdown cleaned** — Removed the ecosystem column from the Product mega dropdown (now 3 columns instead of 4).
- **Mobile menu updated** — Ecosystem section with LeanCTL, GitHub, Discord links.

#### i18n
- All 11 locale files updated with new ecosystem keys (en/de with translations, others with English fallbacks).

## [2.21.6] — 2026-04-08

### Shell Hook Pipe Guard — Fix `curl | sh` Broken by lean-ctx

#### Fixed — Piped commands corrupted by lean-ctx compression
- **Pipe guard for Bash/Zsh** — `_lc()` now checks `[ ! -t 1 ]` (stdout is not a terminal) before routing through lean-ctx. When piped (e.g. `curl -fsSL https://example.com/install.sh | sh`), commands run directly without compression. Previously, lean-ctx would buffer and compress the output, corrupting install scripts and other piped data.
- **Pipe guard for Fish** — `_lc` now checks `not isatty stdout` before routing through lean-ctx.
- **Pipe guard for PowerShell** — `_lc` now checks `[Console]::IsOutputRedirected` before routing through lean-ctx.

#### Important
After updating, run `lean-ctx init` to regenerate the shell hooks with the pipe guard. Or open a new terminal tab.

#### Testing
- 5 new E2E tests for pipe-guard behavior and piped output preservation.
- 3 new unit tests verifying pipe-guard presence in all shell hook variants (Bash, Fish, PowerShell).
- All 677 tests passing, zero clippy warnings.

## [2.21.5] — 2026-04-08

### Windows Updater Infinite Loop Fix (#69)

#### Fixed — Updater enters infinite loop with 100% CPU on Windows
- **Replaced `timeout /t` with `ping` delay** — The deferred update `.bat` script used `timeout /t 1 /nobreak` for delays. On Windows systems with GNU coreutils in PATH (Git Bash, Cygwin, MSYS2), the GNU `timeout` binary takes precedence over the Windows built-in, fails instantly with "invalid time interval '/t'", and causes a tight retry loop at 100% CPU. Now uses `ping 127.0.0.1 -n 2 >nul` which works on every Windows system regardless of PATH.
- **Added retry limit (60 attempts)** — The script now exits with an error message after 60 failed attempts (~60 seconds) instead of looping indefinitely. Cleans up the pending binary on timeout.
- **Extracted `generate_update_script()` as public function** for testability.

#### Testing
- 10 new unit tests covering: no `timeout` command usage, `ping` delay, retry limit, counter increment, timeout exit, pending file cleanup, path substitution (incl. spaces), batch syntax validity, rollback on failure.
- All 669 tests passing, zero clippy warnings.

## [2.21.4] — 2026-04-08

### Windows Shell Fix + Antigravity Support

#### Fixed — Windows: `ctx_shell` fails with "& was unexpected at this time"
- **PowerShell always preferred** — On Windows, `find_real_shell()` now always attempts to locate PowerShell (`pwsh.exe` or `powershell.exe`) before falling back to `cmd.exe`. Previously, PowerShell was only used if `PSModulePath` was set — but when IDEs (VS Code, Codex, Antigravity) spawn the MCP server, this env var is often absent. Since AI agents send bash-like syntax (`&&`, pipes, subshells), `cmd.exe` cannot parse these commands. This was the root cause of "& was unexpected at this time" errors reported by Windows users.
- **`LEAN_CTX_SHELL` override** — Users can set `LEAN_CTX_SHELL=powershell.exe` (or any shell path) to force a specific shell, bypassing all detection logic.

#### Added — `antigravity` agent support
- **`lean-ctx init --agent antigravity`** — Now recognized as alias for `gemini`, creating the same hook scripts and settings under `~/.gemini/`. Previously, Antigravity users had to know to use `--agent gemini` or run `lean-ctx setup`.

#### Testing
- 19 new E2E tests covering shell detection, `LEAN_CTX_SHELL` override, shell command execution (pipes, `&&`, subshells, env vars), agent init (antigravity alias, unknown agent handling), Windows path handling in generated scripts, and bash script execution with Windows binary paths.
- 10 new unit tests for Windows shell flag detection and shell detection logic.
- All 659 tests passing, zero clippy warnings.

## [2.21.3] — 2026-04-08

### Robust Hook Escaping + Auto-Context Fix

#### Fixed — Commands with Embedded Quotes Truncated
- **JSON parser rewrite** — Hook scripts and Rust handler now correctly parse JSON values containing escaped quotes (e.g. `curl -H "Authorization: Bearer token"`). Previously, the naive `[^"]*` regex stopped at the first `\"` inside the value, truncating the command. Now uses `([^"\\]|\\.)*` pattern with proper unescape pass. Affects both bash scripts and Rust `extract_json_field`.
- **Double-escaping for rewrites** — Rewrite output now applies two escaping passes: shell-escape (for the `-c "..."` wrapper) then JSON-escape (for the hook protocol). Previously, only one pass was applied, causing inner quotes to break both shell and JSON parsing.

#### Fixed — Auto-Context Noise from Wrong Project (#62 Issue 4)
- **Project root guard** — `session_lifecycle_pre_hook` and `enrich_after_read` now require a known, non-trivial `project_root` before triggering auto-context. Previously, when `project_root` was `None` or `"."`, the autonomy system would run `ctx_overview` on the MCP server's working directory (often a completely different project), injecting irrelevant "AUTO CONTEXT" blocks into responses.

#### Improved — Cache Hit Message Clarity (#62 Issue 3)
- **Actionable stub** — Cache hit responses now include guidance: `"File already in context from previous read. Use fresh=true to re-read if content needed again."` Previously, the terse `F1=main.rs cached 2t 4L` stub left AI agents confused about what to do next.

#### Housekeeping
- Redirect scripts reduced to minimal `exit 0` (removed ~30 lines of dead `is_binary`/`FILE_PATH` parsing code that was never reached).
- 4 new unit tests for escaped-quote JSON parsing and double-escaping.
- 1 new integration test for auto-context project_root guard.
- All 611 tests passing, zero clippy warnings.

## [2.21.2] — 2026-04-08

### Critical Hook Fixes — Production Quality (Discussion #62)

#### Fixed — Pipe Commands Broken in Shell Hook
- **Pipe quoting fix** — Hook rewrite now properly quotes commands containing pipes. Previously `curl ... | python3 -m json.tool` was rewritten as `lean-ctx -c curl ... | python3 ...` (pipe interpreted by shell). Now correctly produces `lean-ctx -c "curl ... | python3 ..."`. This also fixes the `command not found: _lc` errors reported by users.

#### Fixed — Read/Grep/ListFiles Blocked by Hook (#62)
- **Removed tool blocking** — The redirect hook no longer denies native Read, Grep, or ListFiles tools. This was causing Claude Code's Edit tool to fail ("File has not been read yet") because Edit requires a prior native Read. Native tools now pass through freely. The MCP system instructions still guide the AI to prefer `ctx_read`/`ctx_search`/`ctx_tree`, but blocking is removed.

#### Fixed — `find` Command Glob Pattern Support
- **Glob patterns** — `lean-ctx find "*.toml"` now correctly uses glob matching instead of literal substring search. Added `glob` crate dependency.

#### Changed — README
- **RTK** — Corrected "RTK" references to full name "Rust Token Killer" throughout README and FAQ section.

#### Housekeeping
- Removed ~180 lines of dead code from `hook_handlers.rs` (unused glob matching, binary detection, path exclusion functions that were orphaned by the redirect removal).
- Added 3 new unit tests for hook rewrite quoting behavior.
- All 504 tests passing, zero clippy warnings.

## [2.21.1] — 2026-04-08

### CLI File Caching

#### Added — Persistent CLI Read Cache (#65)
- **File-based CLI caching** — `lean-ctx read <file>` now caches file content to `~/.lean-ctx/cli-cache/cache.json`. Second and subsequent reads of unchanged files return a compact ~13-token cache-hit response instead of the full file content. This directly addresses Issue #65 (pi-lean-ctx zero cache hits) by enabling caching for CLI-mode integrations that don't use the MCP server.
- **Cache management** — New `lean-ctx cache` subcommand with `stats`, `clear`, and `invalidate <path>` actions.
- **`--fresh` / `--no-cache` flag** — Bypass the CLI cache for a single read when needed.
- **5-minute TTL** — Cache entries expire after 300 seconds, matching the MCP server cache behavior.
- **MD5 change detection** — Files are re-read when their content changes, even within the TTL window.
- **Max 200 entries** — Oldest entries are evicted when the cache exceeds capacity.
- 6 new unit tests including integration test for full cache lifecycle.

#### Fixed — Missing Module Registrations
- Registered `sandbox` and `loop_detection` modules that were present on disk but missing from `core/mod.rs`.

## [2.21.0] — 2026-04-08

### Binary File Passthrough, Disabled Tools, Community Contributions

#### Fixed — Hook Blocks Image Viewing (#67)
- **Binary file passthrough** — Hook redirect now detects binary files (images, PDFs, archives, fonts, videos, compiled files) by extension and passes them through to the native Read tool. Previously, the hook would deny all `read_file` calls when lean-ctx was running, which blocked AI agents from viewing screenshots and images.
- Updated both Rust `handle_redirect()` and all bash hook scripts (Claude, Cursor, Gemini CLI) with the same binary extension check.

#### Added — Disabled Tools Config (#66, @DustinReynoldsPE)
- **`disabled_tools`** config field — Exclude unused tools from the MCP tool list to reduce token overhead from tool definitions. Configure via `~/.lean-ctx/config.toml` or `LEAN_CTX_DISABLED_TOOLS` env var (comma-separated).
- Example: `disabled_tools = ["ctx_benchmark", "ctx_metrics", "ctx_analyze", "ctx_wrapped"]`
- 10 new tests covering parsing, TOML deserialization, and filtering logic.

#### Closed — Cache Hits Documentation (#65)
- Clarified that file caching requires MCP server mode (`ctx_read`), not shell hook mode (`lean-ctx -c`). Shell hooks compress command output only; the MCP server provides file caching with ~13 token re-reads.

## [2.20.0] — 2026-04-07

### Sandbox Execution, Progressive Throttling, Compaction Recovery

#### Added — Sandbox Code Execution
- **`ctx_execute`** — New MCP tool that runs code in 11 languages (JavaScript, TypeScript, Python, Shell, Ruby, Go, Rust, PHP, Perl, R, Elixir) in an isolated subprocess. Only stdout enters the context window — raw data never leaves the sandbox. Supports `action=batch` for multiple scripts in one call, and `action=file` to process files in sandbox with auto-detected language.
- **Smart truncation** — Large outputs (>32 KB) are truncated with head (60%) + tail (40%) preservation, keeping both setup context and error messages visible.
- **`LEAN_CTX_SANDBOX=1` env** — Set in all sandbox processes for detection by user code.
- **Timeout support** — Default 30s, configurable per-call.

#### Added — Progressive Throttling (Loop Detection)
- **Automatic agent loop detection** — Tracks tool call fingerprints within a 5-minute sliding window. Calls 1-3: normal. Calls 4-8: reduced results + warning. Calls 9-12: stronger warning. Calls 13+: blocked with suggestion to use `ctx_batch_execute` or vary approach.
- **Deterministic fingerprinting** — JSON args are canonicalized (key-sorted) before hashing, so `{path: "a", mode: "b"}` and `{mode: "b", path: "a"}` are treated as the same call.
- **Per-tool tracking** — Different tools with different args are tracked independently.

#### Added — Compaction Recovery
- **`ctx_session(action=snapshot)`** — Builds a priority-tiered XML snapshot (~2 KB max) of the current session state including task, modified files, decisions, findings, progress, test results, and stats. Saved to `~/.lean-ctx/sessions/{id}_snapshot.txt`.
- **`ctx_session(action=restore)`** — Rebuilds session state from the most recent compaction snapshot. When the context window fills up and the agent compacts, the snapshot allows seamless continuation.
- **Priority tiers** — Task and files (P1) are always included. Decisions and findings (P2) next. Tests, next steps, and stats (P3/P4) are dropped first if the 2 KB budget is tight.

## [2.19.2] — 2026-04-07

### Fixed
- **Gemini CLI hook schema** — Fixed "Discarding invalid hook definition for BeforeTool" error. Hook definitions now include the required `"type": "command"` field and nested `"hooks"` array structure expected by the Gemini CLI validator. Existing configs without `"type"` are automatically migrated. (#63)
- **Remote dashboard auth** — Fixed dashboard returning `{"error":"unauthorized"}` when accessed remotely via browser. Auth is now only enforced on `/api/*` endpoints. HTML pages load freely, with the bearer token automatically injected into API calls. Browser URL with `?token=` query parameter is printed on startup for easy remote access. (#64)

## [2.19.1] — 2026-04-07

### Fixed
- **Cursor hooks.json format** — Fixed invalid hooks.json that caused "Config version must be a number; Config hooks must be an object" error in Cursor. Now generates correct format with `"version": 1` and hooks as an object with `preToolUse` key instead of array. Existing broken configs are automatically migrated on next `lean-ctx install cursor` or MCP server start.
- **cargo publish workflow** — Added `--allow-dirty` to release pipeline to prevent publish failures from checkout artifacts

## [2.19.0] — 2026-04-07

### Temporal Knowledge, Contradiction Detection, Agent Diaries & Cross-Session Search

#### Added — Knowledge Intelligence
- **Temporal facts** — All facts now track `valid_from`/`valid_until` timestamps. When a high-confidence fact changes, the old value is archived (not deleted) with full history
- **Contradiction detection** — `ctx_knowledge(action=remember)` automatically detects when a new fact conflicts with an existing high-confidence fact, reporting severity (low/medium/high) and resolution
- **Confirmation tracking** — Facts that are re-asserted gain increasing `confirmation_count`, boosting their reliability score
- **Knowledge rooms** — `ctx_knowledge(action=rooms)` lists all knowledge categories (rooms) with fact counts, providing a MemPalace-like structured overview
- **Timeline view** — `ctx_knowledge(action=timeline, category="...")` shows the full version history of facts in a category, including archived values with validity ranges
- **Cross-session search** — `ctx_knowledge(action=search, query="...")` searches across ALL projects and ALL past sessions for matching facts, findings, and decisions
- **Wake-up briefing** — `ctx_knowledge(action=wakeup)` returns a compact AAAK-formatted briefing of the most important project facts
- **AAAK format** — Compact knowledge representation (`CATEGORY:key=value★★★|key2=value2★★`) used in LLM instructions instead of verbose prose, saving ~60% tokens

#### Added — Agent Diaries
- **Persistent agent diaries** — `ctx_agent(action=diary, category=discovery|decision|blocker|progress|insight)` logs structured entries that persist across sessions at `~/.lean-ctx/agents/diaries/`
- **Diary recall** — `ctx_agent(action=recall_diary)` shows the 10 most recent diary entries for an agent with timestamps and context
- **Diary listing** — `ctx_agent(action=diaries)` lists all agent diaries across the system with entry counts and last-updated times

#### Added — Wake-Up Context
- **ctx_overview wake-up briefing** — `ctx_overview` now automatically includes a compact briefing at session start: top project facts (AAAK), last task, recent decisions, and active agents — zero configuration needed

#### Changed
- **Knowledge block in LLM instructions** now uses AAAK compact format instead of verbose prose, reducing knowledge injection tokens by ~60%
- **MCP tool descriptions** updated for `ctx_knowledge` (12 actions) and `ctx_agent` (11 actions) to document all new capabilities

## [2.18.1] — 2026-04-07

### Code Quality & Security Hardening

#### Fixed
- **Shell injection in CLI** — `lean-ctx grep` and `lean-ctx find` no longer shell-interpolate user input; replaced with pure Rust implementation using `ignore::WalkBuilder` + `regex`
- **Panic in `report_gotcha`** — `unwrap()` after `add_or_merge` could panic when gotcha store exceeds capacity (100 entries) and the new entry gets evicted; now returns `Option<&Gotcha>` safely
- **Broken `FilterEngine` cache** — Removed dead `get_or_load()` method that stored empty rules in a `Mutex` and was never called; `CACHED_ENGINE` static removed
- **`unwrap()` after `is_some()` pattern** — Replaced fragile double-lookup + `unwrap()` with idiomatic `if let Some()` / `match` in `ctx_read`, `ctx_smart_read`, and `ctx_delta`
- **`graph` CLI argument parsing** — `lean-ctx graph build /path` now correctly separates action from path argument

#### Added
- **`lean-ctx graph` CLI command** — Build the project dependency graph from the command line (`lean-ctx graph [build] [path]`); previously only available via MCP `ctx_graph` tool
- **Consolidated `detect_project_root`** — Single implementation in `core::protocol` replacing 3 duplicate copies across `server.rs`, `ctx_read.rs`, and `dashboard/mod.rs`

#### Changed
- **Tokio features trimmed** — `features = ["full"]` replaced with 8 specific features (`rt`, `rt-multi-thread`, `macros`, `io-std`, `io-util`, `net`, `sync`, `time`), reducing compile time and binary size
- **Security workflow updated** — `security-check.yml` now correctly documents `ureq` as the allowed HTTP client (for opt-in cloud sync, updates, error reports) instead of claiming "no network"

## [2.18.0] — 2026-04-07

### Multi-Agent Context Sharing, Semantic Caching, Dashboard & Editor Integrations

#### Added — Multi-Agent
- **`ctx_share` tool** (28th MCP tool) — Share cached file contexts between agents. Actions: `push`, `pull`, `list`, `clear`
- **`ctx_agent` handoff action** — Transfer a task to another agent with a summary message, automatically marks the handing-off agent as finished
- **`ctx_agent` sync action** — Combined overview of active agents, pending messages, and shared contexts
- **`lctx --agents` flag** — Launch multiple agents in parallel: `lctx --agents claude,gemini` starts both in the background with shared context
- **Dashboard `/api/agents` enhancement** — Returns structured JSON with active agents, pending messages, and shared context count

#### Added — Intent & Semantic Intelligence
- **Multi-intent detection** — `ctx_intent` now detects compound queries ("fix X and then test Y") and splits them into sub-intents with individual classifications
- **Complexity classification** — `ctx_intent` returns task complexity (mechanical/standard/architectural) based on query analysis, target count, and cross-cutting keywords
- **Heat-ranked file strategy** — `ctx_intent` file discovery ranks results by heat score (token density + graph connectivity)
- **Semantic cache** — TF-IDF + cosine similarity index for finding semantically similar files across reads. Persistent at `~/.lean-ctx/semantic_cache/`. Cache warming suggestions based on access patterns. Hints shown on `ctx_read` cache misses

#### Added — Dashboard & CLI
- **`lean-ctx heatmap`** — New CLI command for context heat map visualization with color-coded token counts and graph connections
- **Dashboard authentication** — Bearer token auth for `/api/*` endpoints, token generated on first launch at `~/.lean-ctx/dashboard_token`
- **Heatmap API** — `GET /api/heatmap` returns project-wide file heat scores as JSON

#### Added — Editor Integrations
- **VS Code Extension** (`packages/vscode-lean-ctx`) — Status bar token savings, one-click setup, MCP auto-config for GitHub Copilot, command palette (setup, doctor, gain, dashboard, heatmap)
- **Chrome Extension** (`packages/chrome-lean-ctx`) — Manifest V3, auto-compress pastes in ChatGPT, Claude, Gemini. Native messaging bridge for full compression, fallback for comment/whitespace removal

#### Changed
- MCP tool count: 25 → 28 across all documentation, READMEs, SKILL.md, and 11 website locales


## [2.17.6] — 2026-04-07

### Feature: Crush Support (#61)

#### Added
- **Crush integration** — `lean-ctx init --agent crush` configures MCP in `~/.config/crush/crush.json` with the Crush-specific `"mcp"` key format (instead of `"mcpServers"`)
- **Auto-detection** — `lean-ctx setup` and `lean-ctx doctor` now detect Crush installations
- **Rules injection** — `lean-ctx rules` creates `~/.config/crush/rules/lean-ctx.md` when Crush is installed
- **Prompt generator** — Website getting-started page includes Crush with manual config instructions
- **Compatibility page** — Crush listed in all compatibility matrices across 11 languages

## [2.17.5] — 2026-04-06

### Fix: ctx_shell Input Validation (#50)

#### Added
- **File-write command blocking** — `ctx_shell` now detects and rejects shell redirects (`>`, `>>`), heredocs (`<< EOF`), and `tee` commands. Returns a clear error redirecting to the native Write tool
- **Command size limit** — Rejects commands over 8KB, preventing oversized heredocs from corrupting the MCP protocol stream
- **Quote-aware redirect parsing** — Redirect detection respects single/double quotes, ignores `2>` (stderr) and `> /dev/null`

This prevents the cascading failure reported in #50:
Oversized `ctx_shell` → API Error 400 → MCP stream corruption → "path is required" → MCP stops

## [2.17.4] — 2026-04-06

### Feature: Hook Redirect Path Exclusion + Automated Publishing

#### Added
- **Path exclusion for hook redirect** (#60) — Exclude specific paths from PreToolUse redirect hook. Paths matching patterns bypass the redirect and allow native Read/Grep/ListFiles to proceed
  - Config: `redirect_exclude = [".wolf/**", ".claude/**", "*.json"]` in `~/.lean-ctx/config.toml`
  - Env var: `LEAN_CTX_HOOK_EXCLUDE=".wolf/**,.claude/**"` (takes precedence)
  - Glob patterns support `*`, `?`, and `**` (recursive directory match)
- **Automated crates.io publishing** — `cargo publish` runs automatically after GitHub Release
- **Automated npm publishing** — `lean-ctx-bin` and `pi-lean-ctx` published automatically

## [2.17.3] — 2026-04-06

### Fix: MCP Stdout Pollution on Windows

#### Fixed
- **Windows MCP "not valid JSON" error** — `println!("Installed...")` messages in `install_claude/cursor/gemini_hook_config` polluted stdout during MCP server initialization, breaking JSON-RPC protocol. Now suppressed via `mcp_server_quiet_mode()` guard. (Fixes Lorenzo Rossi's report on Discord)

#### Changed
- **LanguageSwitcher position** — Moved to the right of the "Get Started" button in the header
- **Token Guardian Buddy** — Now shown inline in `lean-ctx gain` output when enabled
- **Bug Memory stats** — Active gotchas and prevention stats shown in `lean-ctx gain`
- **Helpful footer** — `lean-ctx gain` now shows links to `report-issue`, `contribute`, and `gotchas`

## [2.17.2] — 2026-04-06

### Fix: Cross-Platform Hook Handlers

#### Fixed
- **Windows: PreToolUse hook errors** — Agent hooks (Claude Code, Cursor, Gemini) no longer require Bash. Hook logic is now implemented natively in the lean-ctx binary via `lean-ctx hook rewrite` and `lean-ctx hook redirect` (#49)
- **"Stuck in file reading"** — Fixed hook redirect loop where denied Read/Grep tools caused repeated retries when the MCP server wasn't properly connected
- **Hook auto-migration** — Existing `.sh`-based hook configs are automatically upgraded to native binary commands on next MCP server start

#### Changed
- Hook configs now point to `lean-ctx hook rewrite` / `lean-ctx hook redirect` instead of `.sh` scripts
- `refresh_installed_hooks()` also updates hook configs (not just scripts) to ensure migration

## [2.17.1] — 2026-04-05

### Token Guardian Buddy — Data-Driven ASCII Companion

#### Added
- **Token Guardian Buddy** — Tamagotchi-style companion that evolves based on real usage metrics (tokens saved, commands, bugs prevented)
- **Procedural ASCII avatar generation** — Over 69 million unique creature combinations from 8 modular body parts (head, eyes, mouth, ears, body, legs, tail, markings)
- **Deterministic identity** — Each user gets a unique, persistent buddy based on their system seed
- **XP & leveling system** — XP calculated from tokens saved, commands issued, and bugs prevented; level derived via `sqrt(xp / 50)`
- **Rarity tiers** — Egg → Common → Uncommon → Rare → Epic → Legendary, based on lifetime tokens saved
- **Mood system** — Dynamic mood (Happy, Focused, Tired, Excited, Zen) derived from compression rate, errors, bugs prevented, and streak
- **RPG stats** — Compression, Vigilance, Endurance, Wisdom, Experience (0-100 scale)
- **Name generator** — Deterministic adjective + noun combinations (~900 combos, e.g. "Cosmic Orbit")
- **CLI commands** — `lean-ctx buddy` with `show`, `stats`, `ascii`, `json` actions; `pet` alias
- **Dashboard Buddy card** — Glasmorphism UI with rarity-dependent gradients/animations, animated XP bar, SVG radial gauges, styled speech bubble, mood indicator
- **API endpoint** — `/api/buddy` serving full `BuddyState` JSON including `ascii_art` and `xp_next_level`

## [2.17.0] — 2026-04-04

### Premium Experience Upgrade — Architecture, Performance & Polish

Major internal refactoring for long-term maintainability, performance improvements for async I/O, unified error handling, and premium polish across CLI, dashboard, and CI pipeline.

#### Architecture
- **server.rs split** — Monolithic `server.rs` (1918 lines) split into 4 focused modules: `tool_defs.rs` (620L), `instructions.rs` (159L), `cloud_sync.rs` (136L), `server.rs` (1001L). Each module has a single responsibility.
- **Centralized error handling** — New `LeanCtxError` enum in `core/error.rs` with `thiserror` derive. `From` impls for `io::Error`, `toml::de::Error`, `serde_json::Error`. `Config::save()` migrated as first consumer.

#### Performance
- **Async I/O for ctx_shell** — `execute_command` wrapped in `tokio::task::spawn_blocking` to prevent blocking the Tokio runtime during shell command execution.

#### CLI
- **Dynamic version** — All hardcoded version strings replaced with `env!("CARGO_PKG_VERSION")`. Version is now single-sourced from `Cargo.toml`.
- **report-issue exit code** — Empty title now exits with status 1 for proper script error detection.
- **Theme migration** — `print_command_box()` migrated from hardcoded ANSI to the `core::theme` system.
- **upgrade → update** — `lean-ctx upgrade` now prints deprecation notice and delegates to `lean-ctx update`.

#### Dashboard
- **Offline fonts** — Removed Google Fonts CDN dependency, switched to system font stacks.
- **Dynamic version** — Version display fetched from `/api/version` instead of hardcoded.
- **Empty state UX** — "No data yet" message links to Getting Started guide.
- **Connection retry** — Auto-retry with clear user message when dashboard API is unavailable.

#### Setup
- **Compact doctor** — New `doctor::run_compact()` provides concise diagnostics during `lean-ctx setup`, reducing noise for new users.

#### Tool Robustness
- **ctx_search** — Reports count of files skipped due to encoding/permission errors.
- **ctx_read** — Warns on unknown mode (falls back to `full`). Shows message when cached content is used after file read failure.
- **ctx_analyze / ctx_benchmark** — `.unwrap()` on `min_by_key` replaced with `if let Some(...)` to prevent potential panics.

#### CI
- **Deduplicated audit** — Removed redundant `cargo audit` job (handled in `security-check.yml`).
- **Release tests** — `cargo test --all-features` now runs before release builds in `release.yml`.

## [2.16.6] — 2026-04-04

### ctx_edit — MCP-native file editing with Windows CRLF support

Agents in Windsurf + Claude Code extension loop when Edit requires unavailable Read.
`ctx_edit` provides search-and-replace as an MCP tool — no native Read/Edit dependency.

#### Added
- **`ctx_edit` MCP tool** — reads, replaces, and writes files in one call. Parameters: `path`, `old_string`, `new_string`, `replace_all`, `create`.

#### Fixed
- **CRLF/LF auto-normalization** — Windows files with `\r\n` now match when agents send `\n` strings (and vice versa). Line endings are preserved.
- **Trailing whitespace tolerance** — retries with trimmed trailing whitespace per line if exact match fails.
- **Edit loop prevention** — instructions say "NEVER loop on Edit failures — use ctx_edit immediately".
- **PREFER over NEVER** — all injected rules use "PREFER lean-ctx tools" instead of "NEVER use native tools".
- **9 unit tests** covering CRLF, LF, trailing whitespace, and combined scenarios.

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
