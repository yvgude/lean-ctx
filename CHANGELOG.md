# Changelog

All notable changes to lean-ctx are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added
- **PowerShell-native cmdlets route through lean-ctx (#561).** Follow-up to #556:
  shadow/harden mode already recognised the Windows `powershell` shell tool, covering
  the Unix-style PS *aliases* (`cat`/`ls`/`rg`). The command-rewrite layer now also
  maps the PowerShell-**native** cmdlets and their short aliases — `Get-Content`/`gc`
  → `lean-ctx read` (honoring `-Path`, `-TotalCount`/`-Head`/`-First` and
  `-Tail`/`-Last`), `Select-String`/`sls` → `lean-ctx grep` (`-Pattern`, `-Path`),
  and `Get-ChildItem`/`gci` → `lean-ctx ls` (`-Path`). Parameter names are matched
  case-insensitively; anything with an unrecognised flag, a pipeline, multiple
  operands, or an out-of-project path passes through untouched (same conservative
  contract as the Unix rewrites), so determinism and redaction guarantees are
  inherited. The PowerShell cmdlets are detected only in the rewrite path and are
  deliberately kept out of the POSIX shell-alias surface.
- **Addon security hardening — trust, policy, signing, sandbox, audit (#863).**
  Because an addon is executable trust (a stdio addon runs code on your machine;
  an http addon receives your context; its output enters the model), the
  ecosystem ships with defense-in-depth across three tiers:
  - **Trust tier + risk review.** A registry-controlled `addon.verified` flag
    splits the catalog into *verified* (maintainer-audited) and *community*, shown
    in `addon list`/`info` and the install preview. `core::addons::trust::assess`
    statically reviews the `[mcp]` wiring (remote endpoint, non-HTTPS, inline
    shell, fetch-and-exec, unpinned upstream, secret-bearing env) at info/warn/
    danger severity. The same logic backs a **registry CI validator**
    (`registry::validate_entries`, run by `cargo test`): unique slugs, required
    provenance for installable entries, no shell/fetch/non-HTTPS/unpinned wiring,
    and zero findings for verified entries.
  - **Install policy floor — `[addons]`.** A global-only config block (never
    merged from a project-local file): `policy` (`open`/`verified_only`/
    `allowlist`/`locked`), `allowlist`, `require_signature`, `sandbox`,
    `block_risky`. `policy::gate` enforces it in `install` before any gateway
    mutation. Fully permissive by default; distribute via MDM or pin through the
    signed org-policy floor.
  - **Registry signing.** A user-override registry can shadow trusted names; with
    `require_signature = true` it is honoured only if a sidecar
    `addon_registry.json.sig` carries a valid Ed25519 signature by a trusted org
    key (same anchor as `policy org trust`).
  - **Opt-in OS sandbox.** `addons.sandbox = auto|strict` wraps spawned stdio
    servers in `sandbox-exec` (macOS) / `bwrap` (Linux) at the single spawn point
    — outbound-network isolation in `auto`, read-only fs + refuse-if-no-launcher
    in `strict`. Off by default.
  - **Runtime redaction + audit.** Downstream tool output is run through the
    shell-layer secret redaction and audit-tagged as untrusted before it reaches
    the model (`runtime::scrub_output`).
  New small, unit-tested modules `core::addons::{trust,policy,signing,sandbox,
  runtime}`; binding registry-review checklist in `CONTRIBUTING.md`.

### Changed
- **Leaderboard — no top-50 cap, real pagination, everyone findable.** The
  community leaderboard previously truncated to the top 50 accounts, so most
  contributors never appeared and the headline community energy could silently
  drop when the cut-off shifted. `GET /api/leaderboard` now paginates
  (`?page`, `?per_page`, default 50 / max 200) and supports case-insensitive
  name search (`?q=`), while two new fields — `total_tokens_saved` and
  `total_cost_avoided_usd` — report the **uncapped** community totals across all
  opted-in accounts, independent of the displayed page or any filter. The
  server-rendered `/leaderboard` page and the website `/metrics` page gained
  matching search + pagination controls; the landing-page hero energy stat and
  the in-app cockpit now read the uncapped totals so headline numbers stay
  stable. Global ranks are preserved across pages. Pagination, ranking, totals
  and search are pure, unit-tested functions (`paginate`, `all_ranked_cards`).
  (gitlab #868–#871)

### Fixed
- **Shadow-mode hook reads dropped ~75% of the MCP read side effects (#550).** When
  shadow/harden mode intercepts a native `view`/`grep` call it spawns `lean-ctx read`
  as a single-shot subprocess. That CLI path recorded only a fraction of what the MCP
  `ctx_read` pipeline does, and — crucially — never *flushed* its buffered telemetry
  before the process exited, so `lean-ctx heatmap` stayed empty and `lean-ctx gain`
  reported nothing for compressed reads. Three fixes:
  - **One flush set, no drift.** A new `tool_lifecycle::flush_all()` is the single
    source of truth for the buffered-telemetry flush (stats, heatmap, path-mode
    memory, auto-mode resolver, edit-quality, mode predictor, feedback, threshold
    learning, LiTM calibration). The daemon shutdown, the parent watchdog and every
    CLI tool arm (`read`/`grep`/`ls`/`find`/`deps`/`diff`/`-c`/`-t`) now call it — the
    hand-rolled per-arm copies had drifted (the `read` arm flushed only `stats`), which
    is exactly how the gap went unnoticed.
  - **CLI read learning parity.** `record_file_read`/`record_search` now run the same
    disk-backed learning sinks the MCP background thread does — mode-predictor training,
    the per-language compression feedback outcome, and the per-call anomaly metric — so
    auto-mode selection, the feedback loop and dashboard signals improve from
    shadow-mode reads too (not just direct MCP calls).
  - **Mode predictor actually persists now.** `ModePredictor` stored its history in a
    struct-keyed `HashMap<FileSignature, _>`, which `serde_json` cannot serialize
    ("key must be a string") — so `mode_stats.json` was *never* written and the
    predictor relearned from zero every process. The history now serializes as an entry
    list (round-trip tested). The in-memory-only loop/correction detectors and the
    bounce/adaptive signals that need routing through `ctx_read::handle` are tracked as
    a follow-up (they cannot be honored from a single-shot subprocess without
    cross-process state).
- **Windows PowerShell profile path hardcoded to `~\Documents` — broke under OneDrive
  redirection (#558).** `proxy enable` and the shell-hook install resolved the
  PowerShell profile by hardcoding `home\Documents\PowerShell\…`. Windows OneDrive
  folder backup (on by default on most installs) redirects *Documents* to e.g.
  `…\OneDrive\Documents\…`, so lean-ctx wrote to a file PowerShell never reads — the
  active `$PROFILE` was never updated and the proxy received no traffic in new
  terminals. A new `resolve_powershell_profile_path` asks PowerShell itself for
  `$PROFILE.CurrentUserCurrentHost` (authoritative under any folder redirection,
  preferring `pwsh` then Windows PowerShell, UTF-8 output) and falls back to the
  documented default only when no PowerShell host can be launched. Non-Windows hosts
  keep the static `~/.config/powershell` path and never spawn a process (#356).
- **Copilot CLI `view` (read) and `rg` (search) tool calls passed through uncompressed (#562).**
  `handle_redirect` dispatched on the tool name but only matched `Read`/`read`/
  `read_file` and `Grep`/`grep`/`search`/`ripgrep`, so two documented GitHub Copilot
  CLI tool names — `view` (its read tool) and `rg` (its search alias) — slipped
  through without compression in shadow/harden mode. The dispatch is now a tested
  `classify_redirect` helper that includes `view` (→ read) and `rg` (→ grep); the
  Claude/Cursor/CodeBuddy matchers are unchanged because those hosts never emit
  those names and Copilot CLI fires the hook for every tool call.
- **Copilot/VS Code Claude models ignored lean-ctx — no `.github/copilot-instructions.md` (#555).**
  `lean-ctx init --agent copilot` installed the MCP server plus a deliberately
  weak `AGENTS.md` pointer but never wrote `.github/copilot-instructions.md`, the
  repo-level file VS Code Copilot Chat auto-applies to every request. Claude-
  family models (Sonnet/Opus) therefore ignored the tool mapping while GPT-5.x
  followed it ~95% of the time. `init` now writes the strong dedicated ruleset
  into `.github/copilot-instructions.md` as an idempotent `<!-- lean-ctx-rules -->`
  block (user content is preserved, never clobbered) and pins
  `github.copilot.chat.codeGeneration.useInstructionFiles: true` in the project
  `.vscode/settings.json` as a safety net (an explicit user value is honoured);
  uninstall removes the block.
- **Shadow mode ignored `glob` and Windows `powershell` tool calls (#556).**
  Shadow/harden mode silently passed two documented Copilot CLI tools straight
  through: the `glob` tool ("find files matching patterns") had no arm in the
  redirect hook, and the `powershell` shell tool (paired with `bash` on Windows)
  was not recognised as a shell, so command rewrites never fired there.
  `handle_redirect` now intercepts `Glob`/`glob` — warming the shared `ctx_glob`
  core via a new `lean-ctx glob` subcommand and recording the intercept in
  `shadow.log`, then letting the native path-list result through — `is_shell_tool`
  (now shared by both hook entry points) covers `PowerShell`/`powershell`/`pwsh`,
  and the Claude/Cursor/CodeBuddy redirect matchers include `Glob` so the hook
  fires for it. Copilot CLI already dispatches every tool, so its `glob`/
  `powershell` calls are covered automatically.
- **Codex proxy never compressed — ChatGPT login bypasses it; the API-key config
  was a no-op (#554).** `lean-ctx proxy enable` reported success for Codex yet
  `Requests/Compressed/Tokens saved` stayed at `0`, for two reasons. (1) A Codex
  **ChatGPT login** (the default) authenticates via OAuth directly against
  `chatgpt.com/backend-api`, so a custom `openai_base_url` is ignored and the
  proxy never sees the traffic — the Claude Pro/Max situation, but with no
  warning. lean-ctx now detects a ChatGPT login (`~/.codex/auth.json`
  `auth_mode = "chatgpt"`, overridable by an explicit `OPENAI_API_KEY`) and
  prints an honest skip notice pointing at the MCP tools instead of writing dead
  config. (2) In **API-key** mode lean-ctx wrote `[env] OPENAI_BASE_URL` into
  `~/.codex/config.toml`, which Codex does not read; it now writes the documented
  top-level `openai_base_url` key (openai/codex#12031), migrates the dead legacy
  entry, and preserves any custom remote endpoint. Uninstall/cleanup/preview
  handle both forms.
- **`lean-ctx index build-semantic` cold-starts the embedding model again
  (#545).** On a machine without the model cached, the build dead-ended with
  *"embedding model not downloaded — auto-download … failed"* even though no
  download was ever attempted: the build path checked `is_available()` (a pure
  file-existence check) and bailed before the download could run — a regression
  from the #519 ORT-teardown guard. `build_or_update` now downloads the model
  first via a new `EmbeddingEngine::ensure_downloaded()` (pure network/file IO,
  no ORT init) and only loads the ONNX Runtime once the files are present, so the
  cold bootstrap works again and the #519 teardown safety is preserved. The
  passive search path is unchanged.

## [3.8.12] — 2026-06-24

### Added
- **Addon ecosystem — `lean-ctx addon` (#858).** A package manager for community
  extensions: an *addon* wraps an external MCP server behind a small
  `lean-ctx-addon.toml` manifest and plugs into the MCP gateway with one
  `lean-ctx addon add` — no fork, no recompile. `list` / `search` / `info` browse
  a curated registry (bundled `rust/data/addon_registry.json`, overridable per
  entry via `<data_dir>/addon_registry.json`); `add` resolves a registry name **or**
  a local manifest path, discloses the exact transport/command/args/env it will
  run, then — after confirmation (`--yes` to skip; refuses non-interactively
  without it) — wires a `[[gateway.servers]]` entry via the safe global-only
  `Config::update_global` path and records it in `<data_dir>/addons/installed.json`;
  `remove` unwinds exactly what it wired. Registry entries without a runnable
  `[mcp]` block are *listed* (directory + homepage link), never installed with
  fabricated wiring. Reuses the gateway trust model (global-only, opt-in) and the
  `cli::prompt` confirmation gate; no new config section, so schema parity is
  untouched. Manifest, registry and install logic live in small, unit-tested
  `core::addons::{manifest,registry,store,install}` modules. Spec:
  [`addon-manifest-v1`](docs/contracts/addon-manifest-v1.md) · guide:
  [`docs/guides/addons.md`](docs/guides/addons.md).
- **Repo-stack-aware profile recommendation — `lean-ctx profile suggest` (#851).**
  Scans the current repo for deterministic, local signals (languages + source-file
  count, monorepo layout via `pathutil::has_multi_repo_children` + workspace
  markers, build/CI markers, configured LLM providers) and recommends a context
  profile plus key settings (`profile`, `output_density`, `proxy.history_mode`;
  `proxy.effort` is left off — it is never inferred from a repo). Prints the exact
  `export` / `config set` commands to apply it, plus task-oriented alternatives
  (`ci-debug` first when CI is detected, then `hotfix`/`bugfix`/`review`). Strictly
  **read-only** — it never writes config. `--json` for scripting. The mapping
  (`core::profile_suggest::suggest`) is a pure, unit-tested function separated from
  the gitignore-aware scan, so the suggestion is a deterministic function of the
  repo + environment (no network, no telemetry).
- **Review-before-overwrite for consequential CLI writes (#852).** State-mutating
  writes that could clobber existing state now print a before→after diff plus a
  risk note and require confirmation (or `--yes`) — mirroring the `yolo` / `secure`
  pattern, and refusing to run non-interactively without `--yes`. Covers
  `lean-ctx config set` for security/egress-relevant keys (`path_jail`,
  `shell_security`, `sandbox_level`, `secret_detection.*`, `boundary_policy`,
  `proxy.*_upstream`) and `lean-ctx knowledge remember` when it would overwrite an
  existing fact with a materially different value (the prior value is archived).
  The knowledge gate reuses the exact overwrite predicate the write path applies
  (`check_contradiction`), so additive, identical, near-identical (>0.8 similarity)
  and no-op writes stay frictionless. The shared prompt/confirm helper is now a
  single `cli::prompt` module (extracted from `security_cmd`), and config-key risk
  classification lives in `core::config::risk` — deterministic and local-only. The
  MCP `ctx_knowledge` tool path is unchanged (agent writes stay versioned and
  contradiction-warned without an interactive gate).
- **Tool & rule budget — `lean-ctx tools health` (#848).** A deterministic,
  local-only "rot" report answering whether every always-on token earns its
  place. Cross-references the *fixed cost* of each advertised MCP tool schema,
  the MCP instructions, and every auto-loaded rules file with *recorded usage*
  (the post-dispatch cost ledger) to flag: tools that cost schema tokens every
  session but are never called (`unused`), heavy-schema tools used for <1% of
  calls (`low-use`), rules files that bill the same guidance to a client more
  than once, and stale knowledge facts (>30d, never retrieved). Reuses existing
  telemetry and adds **no** new hot-path cost — per-tool `last_used` rides the
  cost-attribution write that already happens. Text (rot candidates only; `--all`
  for the full list), `--json` for scripting, and a **Tool Budget** panel in the
  dashboard health view (`/api/tools-health`). Never auto-applies: every finding
  is a suggestion (`lean-ctx tools lean`, `lean-ctx rules dedup --apply`).
- **Cache-safe cross-provider reasoning-effort control — `proxy.effort` (#834).**
  One opt-in setting (`off` | `minimal` | `low` | `medium` | `high`) pins a single
  reasoning-effort level across **all three providers** without breaking the provider
  prompt cache. lean-ctx translates the constant level to each provider's native
  parameter — OpenAI `reasoning_effort` / `reasoning.effort`, Anthropic
  `output_config.effort`, and Gemini `thinkingConfig` (`thinkingLevel` on 3.x,
  `thinkingBudget` on 2.5 pro/flash) — only on models that accept it and only when the
  client didn't set its own value. Unlike per-turn "effort routing" (which flips
  effort between turns and invalidates the cache — OpenAI lists effort changes as a
  cache-invalidation cause; Anthropic breaks its message-cache breakpoints), the level
  is a *constant*, so the cached prefix stays byte-stable (#448/#498) and only the
  model's reasoning depth changes. Conservative by design: `off` is a strict no-op, it
  never overrides a client value, never enables reasoning the client didn't ask for
  (Anthropic adaptive-only; Gemini skips 2.5 flash-lite and never sends both thinking
  fields), is model-gated (never turns a working `200` into a `400`) and deterministic.
  `lean-ctx proxy status` surfaces the active level plus per-provider steer counts. Set
  via `proxy.effort` or the `LEAN_CTX_PROXY_EFFORT` env (env wins).
- **Unified security posture + `lean-ctx yolo` / `secure` master switches (#507).**
  Decouples lean-ctx's two independent security planes and makes them discoverable:
  **containment** (path jail + shell gating — protects the machine from the agent)
  vs **secret defense** (`.env`/credential redaction — protects secrets from the LLM
  provider). `lean-ctx security status` prints a posture board (and a coarse
  STRICT / RELAXED / OPEN label) reused by `lean-ctx doctor`, which now also shows a
  dedicated **Secret redaction** line. `lean-ctx yolo` (alias `security open`) drops
  containment in one step — writes `path_jail = false` + `shell_security = "off"`,
  takes effect immediately, and **deliberately keeps secret redaction on**;
  `lean-ctx secure` (alias `security strict` / `lockdown`) restores the secure
  defaults. The standalone `.env` switch is `lean-ctx security secrets <on|off>`.
  `path_jail` is now a first-class, schema-documented config key (the blanket
  "any path" opt-out, equivalent to `allow_paths = ["/"]`), so granular re-enabling
  via `lean-ctx config set …` / `lean-ctx allow <cmd>` composes cleanly after a
  `yolo`. Disabling either plane requires a confirmation (or `--yes`) and refuses to
  run non-interactively, so an agent can never silently weaken security.
- **Observation tier — synthesized, recall-prioritized entity summaries (GL #802).**
  A 9th cognition-loop step distils clusters of related facts into compact,
  per-entity *observations* (Hindsight-inspired). Synthesis is **deterministic by
  default** — facts are grouped by an entity anchor (file path in key/value, else
  category) and each cluster of ≥ `cognition_synthesis_min_cluster` (default 3)
  facts is written through the normal `remember()` path, so versioning, persistence
  and idempotency come for free and the value stays byte-stable (#498). An optional
  LLM refinement sits behind `llm.enabled` with the deterministic digest as
  fallback. Recall gives a *balanced* boost to relevant synthesized observations
  (above incidental matches, below an exact key hit). Facts are now **epistemically
  typed** on write (evidence vs. inference) via `infer_from_category`, feeding
  salience and — opt-in via `archetype_aware_decay` — slower decay for structural
  evidence. Gated by `cognition_loop_max_steps >= 9` (the new default; set 8 to
  disable); visible as `observation_synthesis` in `lean-ctx introspect cognition`.
- **Configurable shell-security mode — `enforce` | `warn` | `off` (GL #788).** One
  switch now governs *all* command gating (the allowlist **and** the hard blocks:
  `eval`/`exec`/`source`, `$()`/backticks at command position, interpreter `-c`),
  applied at a single chokepoint so MCP `ctx_shell` and the CLI (`lean-ctx -c`/`-t`)
  behave identically. `enforce` stays the secure default; `warn` runs every check
  but only logs violations; `off` is a deliberate opt-out that skips gating entirely
  while **compression stays fully active**. Set via `shell_security` in config or
  the `LEAN_CTX_SHELL_SECURITY` env (env wins; unknown values fall back to
  `enforce`, never fail open). `off` does not lift the read-only-output doctrine
  (no `>`/`tee`/heredoc writes via shell). `lean-ctx doctor` surfaces the active
  mode whenever it is not `enforce`. Supersedes the CLI-only
  `LEAN_CTX_ALLOWLIST_WARN_ONLY` (kept for backward compatibility).
- **`/v1` contract clients published under one name — `lean-ctx-client`.** The thin,
  engine-independent clients now ship on every registry under a single consistent
  name: [PyPI](https://pypi.org/project/lean-ctx-client/) (import module stays
  `leanctx`), [npm](https://www.npmjs.com/package/lean-ctx-client), and
  [crates.io](https://crates.io/crates/lean-ctx-client). Replaces docs that pointed
  at an unrelated third-party `leanctx` / unpublished `@leanctx/sdk` (GL #783). A
  dedicated, idempotent `publish-clients.yml` workflow ships the family independently
  of the engine.
- **Cognition v2 — science-grounded context engineering, deterministic by default,
  provably active.** Ten neuroscience/physics-motivated mechanisms are wired to
  real hot-path call sites and made inspectable via `lean-ctx introspect cognition`
  (each subsystem reports wired/active/last-run/count; also surfaced in `lean-ctx
  doctor`). All decision layers are deterministic by default (Rule #498 / prompt
  cache intact); stochastic exploration is gated behind `LEAN_CTX_STOCHASTIC`.
  - **Time-variant Φ (attention).** Context salience is recomputed and EMA-blended
    on every re-read instead of being frozen on first sight (`context_ledger`).
  - **Ebbinghaus forgetting + spacing effect.** Knowledge confidence decays as
    `R = exp(-Δt/S)` with stability `S` growing per retrieval, replacing linear
    decay. Configurable via `forgetting_model` (`ebbinghaus`|`linear`),
    `base_stability_days`, and `LEAN_CTX_LIFECYCLE_FORGETTING` (`memory_lifecycle`).
  - **Hebbian eviction.** Co-accessed cache entries protect each other from
    eviction ("fire together, wire together") via a deterministic association bonus
    (`cache`, `hebbian_cache`).
  - **Complementary-learning-systems consolidation.** Idle/loop replay lifts the
    confidence of related, frequently-retrieved facts (`cognition_loop`).
  - **Integration-aware Φ (IIT non-redundancy / MMR).** The context compiler now
    selects via greedy Maximal-Marginal-Relevance and deduplicates on **content**
    (fixes a bug that compared file *paths*), so near-duplicate items collapse to
    one (`context_compiler`, `context_field`).
  - **Global-workspace ignition.** High-salience Φ-outliers (z-score > θ, default
    `LEAN_CTX_GWT_IGNITION_Z`) are broadcast/pinned and resist reinjection
    downgrades (`context_ledger`, `context_gate`).
  - **Learned field weights (bandit).** Φ field weights are chosen by a Thompson
    bandit — deterministic argmax-of-posterior-mean by default, sampling only under
    `LEAN_CTX_STOCHASTIC` (`bandit`, `context_field`, `adaptive_thresholds`).
  - **Sharp-wave-ripple idle replay.** A quiet gap (default 300 s,
    `LEAN_CTX_COGNITION_IDLE_SECS`) triggers a deeper replay-consolidation pass in
    the background (`cognition_scheduler`, `cognition_loop`).
  - **FEP prefetch (active inference).** After a read, likely-next files from the
    co-access graph are surfaced as a deterministic warmup hint — never an automatic
    read (`fep_prefetch`, `context_gate`).
  - **Immune detector (artificial immune system).** External provider data is
    screened for prompt-injection/poisoning before it can become a fact, edge or
    cache entry; untrusted workspaces get a stricter screen (coupled to Workspace
    Trust) (`immune_detector`, `consolidation`, `ctx_provider`).
- **`lean-ctx introspect cognition` / `introspect qubo`.** New CLI to prove which
  cognition subsystems are wired and active, and to run the experimental
  QUBO-vs-greedy selection benchmark.
- **QUBO selection spike (research only).** A deterministic simulated-annealing
  QUBO solver and benchmark harness for redundancy-aware context selection, gated
  behind `LEAN_CTX_EXPERIMENTAL_QUBO`. On clean problems it reaches parity with the
  greedy knapsack (no measurable win), so **greedy remains the default**; promotion
  is conditional on a future measurable gain (`qubo_select`).
- **Opt-in debug log — `LEAN_CTX_DEBUG_LOG` / `lean-ctx debug-log` (#520).** A
  human-readable, off-by-default trace of every MCP tool call (tool, arguments,
  outcome) and every shell-hook routing decision (compress / track / pass-through
  and why), for diagnosing "why did lean-ctx do X?" without attaching a debugger.
  Enable via the `LEAN_CTX_DEBUG_LOG` env (truthy) or `lean-ctx config set
  debug_log true`; read or clear it with `lean-ctx debug-log` (`--clear`). Writes
  to a single rolling file under the state dir; never on the hot path when
  disabled, and the body carries no secrets (arguments are redaction-screened).
- **In-band remote-proxy expansion marker — `<lc_expand:HASH>` (#493).** Lets the
  cold-prefix/CCR retrieval layer work through a **remote** proxy with no shared
  filesystem: the model can emit a `<lc_expand:HASH>` marker in its output and the
  proxy splices the referenced content back in band, across all three providers
  (OpenAI chat + responses, Anthropic, Gemini). Opt-in and cache-safe by
  construction (the marker is deterministic), follow-up to #482.

### Security
- **Shell allowlist now enforced on the `-t` / track path (external audit, finding 1).**
  `exec_argv` (used by the default shell hook `_lc() { lean-ctx -t "$@" }` for
  multi-arg commands) never called `check_shell_allowlist`, so every aliased
  invocation like `_lc git status` bypassed the restriction that `lean-ctx -c`
  enforces. Both paths now share a single `allowlist_gate`, so the track path
  blocks non-allowlisted commands (exit 126) exactly like the compress path.
- **Agent API keys are no longer captured or forwarded to `ctx_shell` children
  (external audit, finding 2).** The agent-runtime-env bridge forwarded every
  `CODEX_*`/`CLAUDE_*`/`OPENCODE_*`/`GEMINI_*`… var — including `*_API_KEY`,
  `*_TOKEN`, `*_SECRET`, `*_PASSWORD` — into the env of every command the agent
  ran, where output redaction can't stop network exfiltration. `is_forwardable`
  now excludes credential-shaped names (only session/thread identifiers cross the
  bridge), and `load` retroactively scrubs such vars from any capture file
  written by an older build, removing the plaintext secret at rest.
- **Path-jail relaxations are now surfaced loudly (external audit, finding 3).**
  `path_jail = false`, the `no-jail` build feature and the env channels
  (`LEAN_CTX_ALLOW_PATH`, `LEAN_CTX_EXTRA_ROOTS`, `LEAN_CTX_ALLOW_IDE_DIRS`) that
  widen or disable the jail are inherited from the IDE/launchd env and previously
  loosened the boundary with no in-band signal. The MCP and HTTP servers now emit
  a `[SECURITY]` warning at startup for each active relaxation, and `lean-ctx
  doctor` reports env-channel relaxations alongside the config-level ones.
- **Workspace Trust for project-local `.lean-ctx.toml` overrides (external audit,
  finding 4).** A cloned repo's `.lean-ctx.toml` is merged over the global config
  and could raise security-sensitive settings — replace the shell allowlist, widen
  the path jail (`allow_paths`/`extra_roots`), repoint the proxy upstream, define
  command aliases, change `rules_scope`/`rules_injection`. For an untrusted
  workspace those overrides are now **withheld** (comfort knobs like
  `compression`/`theme` still apply) with a `[SECURITY]` warning; `lean-ctx doctor`
  shows the state. Grant trust with `lean-ctx trust` (and `lean-ctx untrust` /
  `lean-ctx trust status` / `--list`); trust is pinned to the workspace path **and**
  a content hash of `.lean-ctx.toml`, so editing the file re-gates it. Headless use
  can opt in via `LEAN_CTX_TRUST_WORKSPACE=1` or `LEAN_CTX_TRUSTED_ROOTS`.

### Changed
- **Change-aware pre-push gate + no-test advisory (#850/#849).** `scripts/preflight.sh`
  now classifies the diff against `origin/main`: a docs-only push (README, CHANGELOG,
  `*.md`, website, scripts) skips the Rust gates (fmt/clippy/rustdoc/Windows
  cross-compile) and the pre-push hook finishes in ~0.1 s instead of ~140 s.
  `gen_docs --check` still runs whenever Rust **or** a committed file under
  `docs/reference/generated/**` changed. CI is unchanged and remains the source of
  truth (a docs-only diff cannot turn a Rust gate red, so the local skip can never
  cause a local-green / CI-red split); `make preflight` forces the full gate. A change
  to contract code (`proxy/`, `tools/`, `config/schema/`) with no test signal in the
  diff prints a no-test advisory — blocking under `LEAN_CTX_PREFLIGHT_STRICT_TESTS=1`.
- **Faster semantic search on a native ONNX Runtime (#497).** The
  embedding/index stack moves from the pure-Rust `rten` backend to native `ort`
  (ONNX Runtime 2.0), with a rebuilt indexing pipeline (int8-quantized vectors,
  tighter HNSW, a compact postcard on-disk format). ONNX Runtime is loaded at
  runtime (ort's `load-dynamic`), resolved across platforms from `ORT_DYLIB_PATH`,
  Nix profiles, and well-known system locations — so it is provided once by the
  platform `onnxruntime` package (declared as a dependency in the Arch/Homebrew
  packages), `pip install onnxruntime`, or a manual `ORT_DYLIB_PATH`. The `ort`
  crate is exact-pinned (`=2.0.0-rc.12`) until a stable 2.0 ships. **One-time
  re-index:** the new index format is not backward-compatible; the first semantic
  search after upgrade rebuilds the index automatically (a load-time version guard
  removes any stale index rather than risk mis-decoding it). The `jina-code-v2`
  built-in (pre-existing broken) is removed; code-specialized embeddings remain
  available through the `hf:org/repo[@rev]` custom scheme
  (`hf:jinaai/jina-embeddings-v2-base-code`), which auto-probes the model's
  ONNX I/O signature. Thanks to @omar-mohamed-khallaf for the optimization work.
- **`lean-ctx bypass` renamed to `lean-ctx raw` (external audit, finding 5).**
  The "bypass" wording read to a model like a *security* bypass, but it only
  skips output compression — the shell allowlist and path jail still apply.
  `lean-ctx bypass` stays as a back-compat alias; model-visible hints now use
  `raw` and state that the allowlist still holds.
- **Fewer, less-duplicated MCP read tools (#509 Phase 1+2 / #527, #528, #532).**
  The read-variant cluster (`ctx_smart_read`, `ctx_multi_read`) folds into a
  single `ctx_read` (multi-path + auto mode); the former tool names stay as
  **deprecated aliases** that still work but no longer cost schema tokens in
  `tools/list`, shrinking the always-on surface. Internally, `ctx_read` modes are
  now a type-safe `ReadMode` vocabulary (parsed once, `FromStr`/`Display`) instead
  of ad-hoc strings, with behavioural-equivalence tests and the eval A/B gate
  guarding zero output regression. `SessionCache` is retained (the decoupling
  thesis was evaluated and rejected as net-negative).
- **Configurable `ctx_shell` timeouts + opt-in writes (#526 / #523, #529).** The
  hard-coded 2-min / 10-min shell ceilings are now tunable via `shell_timeout_secs`
  and `shell_heavy_timeout_secs` (env `LEAN_CTX_SHELL_TIMEOUT*`), and the read-only
  output doctrine can be relaxed deliberately with `shell_allow_writes` (env
  `LEAN_CTX_SHELL_ALLOW_WRITES`) so a trusted operator can permit `>`/`tee`/heredoc
  writes through `ctx_shell` — off by default, part of making prohibitive security
  opt-in rather than absolute (#526).
- **Leaner always-on tool & rules schema (#510/#517, #505/#508).** Power-tier tool
  descriptions are reworked workflow-first and de-duplicated, and the optimized
  tools schema + canonical rules consolidation land, trimming the fixed
  per-session token cost of advertised tools and auto-loaded rules without
  changing behaviour (eval-gated).

### Fixed
- **`config set` now accepts every valid `Option` config key (`persona`,
  `bypass_hints`) instead of rejecting them as "Unknown config key" (#856).**
  `config set` resolves keys via the hand-written schema (`ConfigSchema::lookup`)
  only. An `Option<_>` scalar field defaults to `None`, so serde omits it from
  `Config::default()` and it never appears in `config_derived_keys()` (which feeds
  only `config validate`/`apply`). Any such field that wasn't hand-registered was
  therefore accepted from `config.toml` but rejected by `config set` and flagged
  "unknown" by `config validate` — the class behind the `path_jail` report (fixed
  earlier in #507). Auditing all 17 root-level `Option` fields found two more:
  `persona` and `bypass_hints` are now registered in the root schema (`persona`
  as an open `string` so custom `<name>.toml` personas stay valid; `bypass_hints`
  as `enum(on|off|aggressive)` so `config set` validates the value). A new
  regression test (`option_scalar_keys_are_cli_settable`) asserts the
  `Option`-scalar knobs resolve via schema lookup, guarding the whole class.
- **`lean-ctx -c` no longer kills hook-running `git commit`/`git push` at the
  2-minute default (#854).** The shell wrapper enforces `DEFAULT_TIMEOUT` (2 min)
  on ordinary commands and `HEAVY_TIMEOUT` (10 min) on build/test commands, but
  `git commit`/`git push` were treated as ordinary — even though, in a repo with
  hooks, `git commit` fans out into `cargo clippy` (pre-commit) and `git push`
  into the full `scripts/preflight.sh` (pre-push), each of which routinely runs
  3–10 min. The wrapper SIGKILLed git mid-hook, leaving the tree
  staged-but-uncommitted or the push half-done. `is_heavy_command` now classifies
  `git commit` and `git push` as heavy (10-min ceiling, 32 MB buffer); read-only
  verbs (`git status`/`log`/`diff`) stay on the default ceiling because matching
  is on the full `git <verb>` prefix.
- **Hybrid/dense cold-start no longer re-embeds the whole corpus inline (#512).**
  On a large repo, the *first* `ctx_semantic_search mode=hybrid` (or `dense`) call on
  an MCP server that started *before* the on-disk dense index existed would embed the
  entire corpus under the 120s per-request watchdog. The watchdog abandons the response
  but cannot cancel the spawned compute, so the embed kept running — observed as a 500%+
  CPU child for >10 min after the call "returned". A new cold-start guard counts the
  chunks a re-embed would touch (`EmbeddingIndex::pending_chunk_count`) and, above a
  budget (default 2000 chunks, tunable via `LEAN_CTX_HYBRID_INLINE_EMBED_MAX`; `0`
  disables — the pre-#512 behavior), refuses the inline embed across **all four search
  entry points** (hybrid/dense × the MCP tool and the CLI/editor `search_hits` path):
  **hybrid** degrades to the coherent BM25(+graph) ranking (the same fallback used when
  dense is disabled) and **dense** fails fast — both with a one-line hint to build the
  index once, out of band (`lean-ctx index build-semantic`). Warm and incremental paths
  (a few changed chunks on an existing index) are untouched and still embed inline.
- **Shell-output compression can no longer inflate token counts (Windows CI
  flake).** The VCS branch of `compress_output` (git/jj/gh/glab/hg) returned its
  authoritative compressor's result even when it was not strictly shorter — so a
  compact `git log --oneline` stays verbatim — but it skipped the token guard the
  other paths use. A tiny adversarial `git status` body could reshape into a
  one-token-larger summary, breaking the `compress_output_never_inflates_tokens`
  property on Windows. The VCS path now allows *equal* (verbatim) output but
  rejects any growth, restoring the never-inflate invariant deterministically.
  Pinned with a regression unit test for the exact failing input.
- **Cold-prefix repack is now sticky, persistent, and marker-stable (#499).** Three
  fixes to the opt-in big-gap repack (#480): (1) once a resumed conversation is judged
  cold and repacked, the decision **latches** so every warm follow-up keeps the same
  deterministic prefix compression and hits the cache written at the cold turn — the
  previous one-shot repack re-sent the *uncompressed* prefix on the very next turn and
  busted its own fresh cache (net-negative for the common resume-then-continue case);
  (2) per-conversation baselines now **persist to disk** (`cold_prefix_touch.json`,
  atomic write) and reload on proxy startup, so a long idle gap that straddles a daemon
  restart is still detected; (3) the conversation key **ignores the volatile
  `cache_control` marker**, so a moving cache breakpoint no longer flips the key into a
  permanent first-sighting that never repacks. All three are cache-safe by construction
  (deterministic re-compression) and covered by new N→N+1, restart, and marker-stability
  tests. Thanks to @phawrylak for the precise analysis.
- **`gain` no longer reports `0` saved when MCP tools wrote to a different data
  dir (#500).** The savings headline, gain score, cost view and net-of-injection
  line now **sum stats across every auto-resolved data dir** that holds a
  `stats.json`. When an agent host launches the lean-ctx MCP server with a
  different `HOME`/`XDG_*` than the user's shell (e.g. a containerised Hermes
  Agent) the savings landed in a sibling tree while the CLI read an empty primary
  dir and showed a false zero. Aggregation is a **no-op without a split** and is
  skipped entirely when `LEAN_CTX_DATA_DIR` pins one dir, so non-split users are
  unaffected. The empty-state screen now also cross-checks the tamper-evident
  savings ledger and, when it holds events that `stats.json` does not, names the
  data-dir split outright (`lean-ctx savings` / `lean-ctx doctor`). Finally, the
  proxy "bridge OFF — savings cannot be measured" caveat is suppressed whenever
  there are real (MCP-measured) savings to show, since `gain` measures MCP-tool
  savings directly and needs no proxy. Thanks to the reporter for the detailed
  Hermes + OpenRouter writeup.
- **Billing edge no longer downgrades a paying account on a billing-service blip
  (GL #785).** Entitlement resolution at the cloud edge now caches each user's
  last known plan (in-memory, short TTL) and, when the upstream billing service
  is unreachable or returns a bad response, serves that cached plan instead of
  silently falling back to Free. Successful lookups refresh the cache; only
  never-seen accounts fall to Free. A transient upstream outage can no longer
  lock a Pro subscriber out of paid features mid-session.
- **Windows PowerShell/cmd no longer rewrites the `lean-ctx` path (#518 / #521).**
  The terminal-integration shell hook used a Unix-style `/c/...` path that
  PowerShell and cmd can't execute, so `lean-ctx` invocations failed on Windows.
  The hook now emits the native binary path on PowerShell/cmd, restoring terminal
  integration there.
- **No more flaky ORT SIGSEGV on process exit (#519 / #522).** Short-lived
  processes that loaded the ONNX Runtime model could crash with a ~30% flaky
  `SIGSEGV`/`EXC_BAD_ACCESS` during static `OpSchema` teardown at exit (arm64
  macOS). lean-ctx now skips the detached ORT model load in short-lived processes
  that won't use it, removing the teardown crash without affecting real search.
- **Inherited `LEAN_CTX_ACTIVE` no longer silently disables compression (#533 /
  #537).** `LEAN_CTX_ACTIVE` served double duty as both a shell-hook re-entrancy
  guard and a compression bypass; when an agent (e.g. Codex) inherited it into the
  MCP server's environment, every tool output came back uncompressed. Re-entrancy
  ownership now rides a dedicated `LEAN_CTX_WRAPPED` marker, so an inherited
  `LEAN_CTX_ACTIVE` no longer turns compression off.
- **`ctx_read raw:true` / `mode=raw` now honored and documented (#513 / #514).**
  The verbatim escape hatch was silently ignored on the `raw:true` argument and
  undocumented for `mode=raw`, so non-Opus models (GLM 5.2 report) fought the
  compression by retrying reads. Both forms now reliably return uncompressed,
  un-elided bytes and are documented as the way to get exact file content.
- **`allow_paths` / `shell_allowlist_extra` failures are no longer silent (#540 /
  #541, #542).** Two invisible-over-MCP failure modes are surfaced at the point of
  the block: (1) a project-local `.lean-ctx.toml` whose security-sensitive
  overrides are **withheld because the workspace is untrusted** now names the
  ignored keys and the `lean-ctx trust` / global-config remedies; (2) when the
  runtime resolves a global `config.toml` that **doesn't exist** (an edit that
  landed in a different dir — XDG vs legacy `~/.lean-ctx`, or a sandboxed/container
  `$HOME`), both the allowlist and path-jail block messages now say so and name
  the path actually read. The stderr-only `tracing::warn` was invisible to MCP
  clients (OpenCode), making these read as "the setting does nothing".

## [3.8.11] — 2026-06-20

### Fixed
- **#478 — JetBrains plugin now writes its backend port file to `XDG_DATA_HOME`,
  matching the Rust `data_dir`.** After the #408 path refactor, `LeanCtxPaths`
  treated `config.toml` as a data marker and fell back to `XDG_CONFIG_HOME`, so
  the plugin wrote the port file under `~/.config/lean-ctx` while the Rust reader
  looks under `~/.local/share/lean-ctx`. The file was never found
  (`BACKEND_REQUIRED`), disabling every IDE-side `ctx_*` action. Data-dir
  resolution now mirrors the Rust implementation (single-dir override, layout
  pin, data-only markers with `config.toml` excluded), with regression tests for
  fresh installs, mixed configs and XDG pins. Thanks @dasTholo.
- **`lean-ctx uninstall` now also removes the auto-update agent and every XDG
  data directory.** A full uninstall left the 6-hourly self-update LaunchAgent
  (`com.leanctx.autoupdate`) running and never deleted the real runtime dirs
  (`~/.local/share`, `~/.local/state`, `~/.cache` — >150 MB), because
  `remove_data_dir` resolved through `dirs::data_dir()`, which collapses onto
  `~/Library/Application Support` on macOS. Uninstall now calls
  `update_scheduler::remove_schedule()` and resolves every XDG category through
  `core::paths` (honoring `LEAN_CTX_*_DIR` / `XDG_*`), with a regression test that
  asserts every canonical directory is covered exactly once.
- **Onboarding command box now shows `LEAN_CTX_DISABLED=1` instead of the
  non-existent `lean-ctx off` / `on` toggle.** The box advertised subcommands
  that don't exist (they fail with "unknown command"); the real global switch is
  the `LEAN_CTX_DISABLED=1` environment variable.

## [3.8.10] — 2026-06-20

### Fixed
- **#462 / #474 — restricted shell mode no longer rejects `for`/`while`/`if`
  loops, `case` blocks and subshells.** The allowlist checker now expands a
  compound command down to its leaf command segments and validates each segment
  against the allowlist, so legitimate constructs (`for f in *.rs; do cat $f;
  done`, `if test -f x; then ls; fi`, `( ls && pwd )`) run under restricted mode
  while injection attempts smuggled through the same constructs stay blocked.
- **#476 / #477 — `lean-ctx uninstall --help` no longer performs a real
  uninstall.** The `--help`/`-h` flag fell through to the uninstaller, which
  removed the installation instead of printing usage. The CLI now short-circuits
  `uninstall --help`/`-h` to print the usage text and exit without touching
  processes, configs, data or the binary.
- **#356 — the "lean-ctx wants to access your Documents folder" prompt is now
  closed even for `brew upgrade`-only installs.** The path guards + LaunchAgent
  Seatbelt wrapper already made daemon/proxy boot promptless, and `lean-ctx
  update`/`dev-install` regenerate the plists with that wrapper. The remaining
  hole was a user who *only* runs `brew upgrade` (which bypasses lean-ctx's
  updater), so their pre-Seatbelt plists were never regenerated. New belt-and-
  suspenders: a launchd-standalone process (`ppid 1`) now **re-execs itself
  under the deny-`~/Documents` Seatbelt at startup** if it is not already
  wrapped (`reexec_under_seatbelt_if_needed`, called first thing in `main`). A
  sentinel env var baked into the plists (`LEAN_CTX_SEATBELT`) prevents any
  double-wrap for current-code plists; terminal/editor children (host TCC grant)
  and non-macOS are unaffected. Verified by the existing `tcc_sandbox.sh`
  SIGKILL-on-access boot test. This makes the daemon/proxy promptless
  independent of code signature, so no Apple Developer ID is required.
- **#451 — `ctx_shell` / `lean-ctx -c` no longer run agent commands in a
  non-POSIX interactive shell.** `$SHELL` is the user's *interactive* shell; when
  it is Nushell, Fish, Elvish, xonsh or PowerShell, an agent's bash/POSIX command
  silently mis-executes. `detect_shell` now honors `$SHELL` only when it is
  POSIX-compatible (bash/zsh/sh/dash/ash/ksh/mksh) and otherwise falls back to a
  real POSIX shell. zsh/bash users are unaffected; `LEAN_CTX_SHELL` still forces a
  specific shell regardless of the gate.
- **Shell gotcha auto-learning now correlates fail→fix in CLI (`lean-ctx -c`)
  mode.** `pending_errors` were `#[serde(skip)]` and cleared on load, so a fix
  spanning two separate `lean-ctx -c` processes never correlated (only the
  long-lived daemon could). They are now persisted (bounded by `MAX_PENDING` + a
  15-min TTL, pruned on load), so a later process loads the pending error and
  correlates the fix — the gotcha loop now works in the hybrid CLI-shell setup.

### Added
- **#668 — FinOps showback: readable project names.** The savings ledger stores
  only a truncated repo hash (never a path), so the `finops export` `project`
  column was opaque. An opt-in `<config_dir>/finops-aliases.toml` (`[projects]`
  `<repo_hash> = "Team"`, also `--aliases=FILE` / `$LEAN_CTX_FINOPS_ALIASES`) now
  maps hashes to human-readable names **at export time only** — the ledger, the
  signed batch and the hash chain are never touched, so privacy guarantees and
  signatures stay intact. Unmapped hashes fall back to the hash, so an incomplete
  map never drops rows. New `core/finops_export/aliases.rs`; applies to all
  targets (FOCUS / CBF / Vantage).
- **#674 — central, signed org policy distribution + admin.** `lean-ctx policy
  org sign <pack.toml> --org <name>` wraps a policy pack in an **Ed25519-signed**
  artifact; endpoints `policy org trust <pubkey>` (pin once, out-of-band) and
  `policy org install <artifact>`, after which the runtime folds the org pack in
  as an **un-bypassable floor** beneath the local `.lean-ctx/policy.toml`. The
  local pack can only ever *tighten* it: `deny_tools` union, `allow_tools`
  intersect, `redaction` union (org patterns win clashes), the stricter filter
  action, the tighter egress/`max_context_tokens` caps, the longer
  `audit_retention_days`. Two independent checks gate enforcement — the signature
  must verify **and** the signer key must be pinned — so a forged or untrusted
  artifact is ignored, never enforced, and never bricks the agent (fail-open);
  with no key pinned nothing is enforced (opt-in). `--advisory` distributes a
  policy for preview without enforcing it; `policy org status` shows the
  effective floor and `policy org verify` checks an artifact offline. Pluggable
  source (`LEANCTX_ORG_POLICY` / `LEANCTX_ORG_TRUST_KEY` for MDM). New
  `core::policy::org` + `core::policy::floor`; contract
  `docs/contracts/org-policy-v1.md`.
- **#677 — signed CISO compliance report.** `lean-ctx compliance report --from
  <rfc3339> --to <rfc3339> [--framework eu-ai-act|iso42001|soc2]... [--pack
  <name|path>] [--format json|csv|pdf|text]` composes the engine's evidence
  surfaces into one **Ed25519-signed** artifact for a date range: OWASP
  Top-10-for-Agents alignment, framework coverage (verified live against the
  resolved pack), what enforcement **blocked** (`ToolDenied`) and **redacted**
  (`SecretDetected`) over the period (folded from the append-only audit chain,
  with the segment's `head_hash` bound into the signed payload), and the
  retention posture (pack `audit_retention_days` intent vs. plan entitlement).
  The signed JSON is always written and is offline-verifiable with `lean-ctx
  compliance verify <report.json>` (no audit trail, no LeanCTX needed); `--format
  csv|pdf` additionally emits that human rendering — the PDF is a real,
  dependency-free PDF 1.7. Honest by construction: a quiet period reports zero
  blocks, and a broken local chain is reported (`chain_valid = false`), never
  hidden. New `core::compliance_report` module; contract
  `docs/contracts/compliance-report-v1.md`.
- **#676 — egress / output DLP on agent writes & actions.** A new `[egress]`
  policy-pack section governs what the agent *emits* (the output side of the
  Great Filter), checked **before dispatch** of `ctx_edit` writes and
  `ctx_shell`/`ctx_execute` actions — so a blocked write never touches disk and a
  blocked command never runs. **`forbidden_patterns`** are regexes that refuse a
  write/action on match (e.g. a prod-DB DSN or a destructive query);
  **`block_secrets`** refuses content carrying detected secrets (the pack's
  `[redaction]` patterns) or PII (the #675 checksum-validated detectors);
  **`max_writes_per_min`** is a per-process sliding-window rate limit on agent
  writes/actions. Blocked egress returns `[POLICY BLOCKED]` and is audited
  (`ToolDenied`) with a non-sensitive reason (`forbidden-pattern:…`, `secret`,
  `pii:…`, `rate-limit`) — never the matched content. Egress obeys the same
  opt-in / fail-open / Local-Free guarantees; `forbidden_patterns` accumulate and
  the scalars override down the `extends` chain. New `core::egress` module.
- **#675 — inbound content filters (PII / classification / prompt-injection).**
  A new `[filters]` policy-pack section adds net-new detectors that run inside the
  enforcement pipeline *before* tool output reaches the agent (the input side of
  the Great Filter). Each detector takes an action — `off` / `warn` / `redact` /
  `block`: **`pii`** finds Swiss AHV (EAN-13), IBAN (mod-97), payment cards
  (Luhn) and email, each checksum-validated to keep false positives low;
  **`classification`** gates files *marked* confidential/secret (banner lines or
  a `Classification:` field, not prose mentions; `blocked_labels` is
  configurable); **`injection`** masks/blocks OWASP-LLM01 prompt-injection lines
  (reusing `output_sanitizer::detect_injection`). Decisions are audit-logged
  privacy-preservingly — only `(class, count)` pairs (e.g. `pii:iban×2`), never
  the matched value. Filters obey the same opt-in / fail-open / Local-Free
  guarantees as the rest of the pack; actions override and `blocked_labels`
  accumulate down the `extends` chain. New `core::input_filters` module.
- **#673 — context policy packs are now enforced at runtime.** A project pack
  (`.lean-ctx/policy.toml`, authored from any built-in via `lean-ctx policy show
  <name> --toml`) is applied at the MCP hot path: `deny_tools`/`allow_tools`
  gate which tools the agent may call (denied calls return `[POLICY DENIED]` and
  are audited as `ToolDenied`), `[redaction]` patterns strip matches
  (`[REDACTED:<name>]`) from tool output before it reaches the model,
  `default_read_mode` sets the `ctx_read` fallback when the caller omits `mode`,
  and `max_context_tokens` tightens (never loosens) the session token ceiling.
  Enforcement is opt-in (no pack → unchanged behavior), fail-open on an invalid
  pack, and Local-Free — only the agent pipeline is constrained, never a human's
  own reads. The `ctx`/`ctx_session`/`ctx_policy` meta tools are never gated, so
  a pack can never lock the operator out.
- **#454 — `prefer_native_editor` config to opt out of lean-ctx edit operations.**
  Set `prefer_native_editor = true` (or `LEAN_CTX_PREFER_NATIVE_EDITOR=1`) so the
  lean-ctx edit tool (`ctx_edit`) is neither advertised in `list_tools` nor
  dispatchable (direct *or* via `ctx_call`); the agent falls back to the host's
  built-in editor UI. Read / search / shell / memory tools are unaffected.
  Colorized diffs are intentionally left to host extensions rather than the MCP
  tool output, which must stay byte-stable for prompt caching (#498).

## [3.8.9] — 2026-06-18

### Added
- **Hermes context-engine plugin + `ctx_transcript_compact` core tool** — lean-ctx
  can now be Hermes Agent's *active context engine*, not just an MCP server it
  might call. The new `integrations/hermes-lean-ctx` plugin is a thin Python
  `ContextEngine` that replaces Hermes' built-in `ContextCompressor`: it keeps the
  system preamble + a fresh tail verbatim, replaces older turns with a recoverable
  summary, and injects lean-ctx's recall tools (`ctx_search`, `ctx_semantic_search`,
  `ctx_read`, `ctx_expand`, `ctx_knowledge`, `ctx_summary`) natively into the agent.
  Compaction itself lives in a new daemon tool, `ctx_transcript_compact` (the 77th
  MCP tool): deterministic, prompt-cache-friendly compaction of OpenAI-format
  message arrays that never splits a `tool_call`/`tool_result` pair and offloads the
  raw turns into session memory. The plugin prefers this core tool over `/v1` and
  falls back to local Python compaction when the daemon is unreachable, so the agent
  loop never breaks. Includes session-lifecycle persistence (`resume` on start,
  `ctx_summary` + `ctx_handoff` on end), model-window presets, a runnable
  head-to-head benchmark harness (vs. import-guarded `ContextCompressor`/`hermes-lcm`),
  and a dedicated CI job (pytest + offline benchmark smoke). `lean-ctx init --agent
  hermes` now also points to the engine plugin.
- **ACE-inspired auto-learning loop — gotchas now learn themselves, distil, and
  surface** (study of `kayba-ai/agentic-context-engine`). Previously the
  `GotchaStore` could *correlate* an error with its later fix but nothing ever fed
  it real shell outcomes, so it stayed empty in production. The loop is now wired
  end to end:
  - **Live capture** — `shell::exec` hands every finished command to
    `gotcha_tracker::record_shell_outcome`, gated by a cheap `is_correlatable_command`
    filter (cargo/npm/pytest/go/docker/git…) so only build/test/run output is
    inspected. A process-global in-memory store keyed by project hash keeps the
    `pending_errors` (which are `#[serde(skip)]`) alive across commands inside a
    long-lived daemon, mirroring `diagnostics_store`; durable gotchas are persisted
    when a fix is correlated.
  - **Reflector** — a deterministic `reflect()` distils the store into Playbook
    deltas: recurring fixes (≥2 occurrences with a resolution) become *proven
    strategies*, error signatures that recur across ≥2 distinct sessions with no
    recorded fix become *recurring pitfalls*. It folds into the session Playbook
    during `ctx_compress` via the existing dedup/stable-ID `add_delta`.
  - **Offline mining** — `lean-ctx learn --mine <dir>` scans a directory of
    `.jsonl` transcripts/logs for high-precision error markers (Rust E-codes,
    tsc/pytest/npm signatures), aggregating recurring signatures across files
    read-only — it never mutates stored state.
  - **Learning Ledger** — `lean-ctx gotchas ledger` renders a human-readable
    summary (errors observed, fixes correlated, repeats avoided, promoted to
    knowledge) plus the distilled strategies/pitfalls, making the learning visible.
- **Semantic near-duplicate detection on `ctx_knowledge remember`** — the lexical
  similarity check only caught facts sharing tokens, so paraphrases of the same
  decision silently accumulated. `remember` now also runs an embedding cosine
  pass (threshold 0.86) *before* upsert and appends a non-destructive "SEMANTIC
  NEAR-DUPLICATES" advisory listing paraphrases the lexical pass missed, so the
  agent can `judge`/merge them. Self-matches and already-judged pairs are
  excluded; the embedding path is behind the default `embeddings` feature.

### Fixed
- **High idle CPU when no session is running (#453)** — on v3.8.8 (macOS, Claude
  Code & OpenCode) a connected-but-idle agent pegged a whole CPU core in the
  `lean-ctx` process. A `sample` of the live process showed the `leanctx-index`
  thread burning ~100% while every other thread (tokio workers, `memory-guard`,
  main) sat parked in `cond_wait`/`nanosleep` — a CPU-bound worker, not a busy
  timer loop (the screenshot's "2 idle wake-ups" at 97.5% CPU confirmed it). Root
  cause: `LeanCtxServer::new()` ran an **eager full index build** (graph + BM25 +
  line-search) on *every* server start whenever a project root was detected. A
  warm cache still burned ~1 core for 6–9 s per start; multiplied across two
  agents and stdio respawns it never settled. Fixed comprehensively:
  - **No eager startup build (primary fix)** — the startup scan is removed; the
    server falls back to the demand-driven lazy warming it already documents
    (#152). A session that sits idle or only uses `ctx_read`/`ctx_shell`/
    `ctx_tree` now pays **zero** indexing cost (measured: idle CPU stays at 0.0%);
    graph/search tools still warm their index on first use. The eager call was an
    unrelated regression slipped in via #294.
  - **Long-lived HTTP `serve` keeps a one-time background warm-up** — only the
    persistent `lean-ctx serve` process (never the per-respawn stdio path) kicks
    off a single deduped background index build at startup. Without it the first
    heavy/search tool call on a large project root raced a cold scan against the
    per-request timeout and, on CPU-constrained CI runners, starved the request
    handlers into `504 request_timeout` (the SDK-conformance regression). Idle CPU
    still settles flat once the build completes, so #453 idle hygiene is preserved.
  - **stdio transport no longer respawns on a single bad frame** — the codec
    mapped *any* decode error to the same `None` as a true EOF, so one malformed
    JSON-RPC message tore down the server (rmcp `QuitReason::Closed`), the agent
    respawned it, and the fresh process paid another index build — a CPU churn
    loop. Malformed frames are now skipped (the bad frame is already consumed) and
    the stream resyncs onto the next message; only a real stream end closes the
    transport.
  - **No duplicate daemons** — concurrent MCP servers launching at once could all
    pass the `is_daemon_running()` check in a TOCTOU window and each spawn a
    daemon. `start_daemon()` now serializes that critical section with an
    exclusive, bounded-wait file lock.
  - **Leaner proxy reload** — the #449 upstream-reload loop's default interval is
    relaxed from 2 s to 5 s; `Config::load()`'s internal content-hash cache
    already skips re-parsing an unchanged `config.toml`, so each idle tick is just
    a small file read.
  - **memory-guard idle backoff** — RSS sampling stretches from every 3 s to
    every 15 s once memory has been stably calm, and snaps back instantly under
    any pressure (OOM reaction time during real work is unchanged).
- **Quick settings that "keep resetting" are now diagnosable and stable (#450)** —
  a value saved in the dashboard could be silently shadowed so it appeared to
  revert to defaults (lite/off), and `lean-ctx config validate` only said
  "no config" without telling you *where* it looked. There are four mechanisms
  and none of them was visible: an env var (`LEAN_CTX_*`), a project-local
  `.lean-ctx.toml` override (`compression_level`/`terse_agent`/`tool_profile`), a
  divergent resolved config dir (dashboard writes path X, runtime reads path Y),
  or an unparseable `config.toml` falling back to defaults. Fixed by making the
  provenance explicit and the path stable:
  - **`config validate` shows the source** — it now always prints the resolved
    `config.toml` path (even when missing), the layout-pin state, any parse
    error, and the active env / project-local overrides, with a one-line
    explanation of why a value can appear to "reset".
  - **Dashboard surfaces provenance** — `/api/settings` returns `config_path`,
    `config_exists`, `parse_error` and a per-setting `local_override`; the Quick
    Settings panel shows which `config.toml` is read and warns (and disables the
    toggle) when an env var or a project-local `.lean-ctx.toml` is winning.
  - **Dashboard pins the layout** — `lean-ctx dashboard` now runs the same
    `layout_pin::heal()` as the daemon/server start paths, so it can no longer
    write `config.toml` into a divergent dir the runtime never reads.
- **Dashboard no longer times out on load; heavy index/graph routes never block (#452)** —
  opening the dashboard mounted ~22 `<cockpit-*>` components that each fired
  `loadData()` from `connectedCallback()` at once — a thundering herd of
  `/api/graph`, `/api/call-graph`, `/api/symbols`, `/api/search-index` and
  `/api/tree` requests that ran synchronous, file-count-scaling index/graph
  builds and starved the trivial `/api/settings` handler until the client
  aborted after 8 s ("Settings timeout"). Fixed on two layers:
  - **Frontend lazy-load (primary fix)** — components no longer load in
    `connectedCallback()`; the router's view-loader fetches only the active
    view, so `#context/settings` issues a single `/api/settings` request instead
    of triggering every panel's data load at once.
  - **Backend single-flight + non-blocking (hardening)** — `graph_index` and
    `bm25_index` gained a `get_or_start_build` coordinator (one background build
    per root, concurrent callers deduplicated) modeled on `call_graph`. Heavy
    routes (`/api/tree`, `/api/symbols`, `/api/call-graph`, `/api/search-index`,
    `/api/search`) now return `202 {status:"building"}` with progress instead of
    blocking on a full scan; the affected panels poll and show an
    "index building…" state until the build completes.
- **`ctx_shell` is clearly labelled and runs profile-free (#451)** —
  - **Pi renderer** — the Pi extension rendered shell calls with a bare `$`
    prefix (inherited from Pi's bash renderer), making `ctx_shell` look like a
    native interactive bash shell. It now renders an explicit `ctx_shell` label.
  - **Profile-free shell** — `ctx_shell` (MCP `execute_command_with_env`) and the
    CLI `lean-ctx -c` paths now neutralize inherited `BASH_ENV`/`ENV` so a
    non-interactive `sh -c`/`bash -c` can no longer be hijacked into sourcing a
    profile/rc file (e.g. an `exec nu` snippet silently replacing the shell).
    Shell behavior is now deterministic and independent of user shell config.
  - **Sharper description** — the tool description (MCP and Pi) states it runs
    the system shell (`$SHELL`) profile-free, so agents stop treating it as a
    config-loaded interactive bash.
- **Proxy upstream is now live from `config.toml` — no more stale upstream on a long-lived proxy (#449)** —
  the proxy froze its provider upstreams in `ProxyState` at startup and never
  re-read them, so a later `lean-ctx config set proxy.openai_upstream …` (or any
  `config.toml` edit) had no effect until a manual restart — and a shell
  `export LEAN_CTX_OPENAI_UPSTREAM=…` could never reach an already-running,
  service-managed proxy at all (the env simply does not propagate into a running
  process). Now:
  - **Live reload** — a background task re-resolves the upstreams from
    `config.toml` every ~5s (`LEAN_CTX_PROXY_RELOAD_SECS` to tune) and publishes
    any change through a `tokio::sync::watch` channel that every provider handler
    reads per request, so `config set` takes effect on the running proxy within
    seconds, without a restart. An invalid value keeps the last good upstream
    instead of silently dropping to the provider default.
  - **`config.toml` is the source of truth for long-lived proxies**; a
    `LEAN_CTX_*_UPSTREAM` env var remains a *start-time* override only (it cannot
    reach a process that is already running). MCP hosts make this acute: Codex
    (and others) launch the lean-ctx MCP server with a stripped, allowlisted
    environment that omits `LEAN_CTX_*_UPSTREAM`, so the proxy it spawns never
    sees it — `config.toml` is the only mechanism that reaches every proxy.
  - **Root cause for service/MCP-managed proxies — directory pinning** — a
    launchd-spawned proxy inherits only launchd's minimal environment (no `HOME`,
    no XDG vars) and so resolved a *different* config/data dir than the CLI: it
    never read the user's `config.toml` (live reload had nothing to read) and
    derived a mismatched session token (its `/status` 401'd). The proxy/daemon
    LaunchAgent plists now bake in the exact `HOME` + `LEAN_CTX_{CONFIG,DATA,STATE,CACHE}_DIR`
    the installing CLI resolves, so a managed process always agrees with the CLI.
  - **Observability** — `/status` and `lean-ctx proxy status` now report the
    active upstreams; `proxy status` derives liveness from the public `/health`
    endpoint (so a running proxy is never misreported as down) and warns in two
    cases: a `LEAN_CTX_*_UPSTREAM` set in the shell that never reached the proxy
    (with the exact `config set` command to persist it), and a proxy started with
    an env override now masking a later `config.toml` edit. `doctor` carries the
    same drift check.
  - **`lean-ctx proxy restart`** — new subcommand that cleanly restarts the
    managed service (re-reads `config.toml`, drops any start-time env override).
- **`ctx_impact` resolves C# extension-method hosts and disambiguates types by namespace (GH #398 follow-ups, #640–#643)** —
  the two deferred #398 follow-ups are now closed:
  - **Extension methods (#642)** — a call `value.WordCount()` to a C# extension
    method (`static int WordCount(this string s)`) names neither the defining
    static class nor any of its types, so it produced no edge and left the host
    a false-negative leaf. A new `deep_queries::ext_methods` extractor collects
    `this`-parameter methods, and `ctx_impact` links each `value.Foo()` call to
    the defining file (file + symbol `TypeRef` edge), self-filtered and capped.
  - **Namespace-aware resolution (#641)** — `TypeDef` now carries its C#
    namespace (block-, file-scoped and nested), and `type_ref_targets` resolves
    hybridly: a definer in the consumer's *visible* namespace (own namespace +
    enclosing namespaces + `using`s) always links — even past the cap — and its
    homonyms in other namespaces are dropped, so same-named types are no longer
    conflated. With no namespace match the global fallback still links, with the
    too-generic cap raised 3 → 5. Java (no namespaces) keeps the fallback path.
  Both capabilities are wired into the embeddings **and** minimal builder paths;
  all new regressions are gated on `tree-sitter` so they exercise both. Outputs
  stay deterministic (sorted/deduped, bounded indexes; #498).
- **`ctx_impact` now sees C# types used only in expression position (GH #398 follow-up)** —
  the v3.8.3 fix linked same-namespace C# consumers to definers for types in
  *declaration* positions (fields, parameters, return types, `base_list`,
  generics, casts, `typeof`), but a type referenced **only in expression
  position** still produced no `TypeRef` edge, so `ctx_impact` reported the
  defining file as a false-negative leaf. Now covered in
  `deep_queries::type_uses`: static calls/fields and enum values via a
  member-access receiver (`Engine.Create()`, `Engine.Default`, `Status.Active`)
  and attributes (`[ApiController]`, which additionally resolves to the
  `…Attribute` class name). Only PascalCase receivers are collected and the
  existing def-index resolution discards any name that is not a real project
  type, so precision is unchanged. The new end-to-end regression is gated on
  `tree-sitter` rather than `embeddings`, so it also exercises the
  `index_graph_file_minimal` builder path that the earlier #398 e2e tests never
  reached. (Extension-method hosts and namespace-aware resolution were the
  remaining follow-ups, now closed above.)
- **`lean-ctx update` / `config init --full` no longer reset or leak config values (#443)** —
  persisting a single setting could silently rewrite *other* customized keys in the
  global `config.toml` (e.g. `compression_level` → `lite`, `max_ram_percent` → 5).
  Three root causes, now closed by construction:
  - **(A) default-seed clobber** — `config init --full` historically wrote
    `Config::default()`, and `save()` overwrites every key present in both the
    incoming document and the file (`config_io::merge_table`), resetting customized
    values. (Already mitigated via `config_for_full_init`; now superseded.)
  - **(B) project-local leak** — `Config::load()` folds project-local
    `.lean-ctx.toml` overrides into the in-memory struct, so the common
    `load() → mutate → save()` pattern (18 call sites across 10 files) wrote those
    per-project values back into the *global* file.
  - **(C) corrupt-file clobber** — `write_toml_preserving_minimal` wrote a fresh
    document when the existing file failed to parse, discarding a hand-broken config.
  The fix introduces a leak-free persistence API — `Config::load_global()` (reads
  the global file only, never merging project-local overrides) and
  `Config::update_global()` (read global-only → mutate → minimal save, and *refuses*
  to touch an unparseable file) — and migrates every persist site to it. The runtime
  read path (`Config::load()`, with project-local merge) is unchanged. In addition,
  `write_toml_preserving_minimal` now refuses to overwrite an unparseable config
  instead of clobbering it, and `config init --full` emits a fully annotated
  reference document seeded with the user's current values (lossless round-trip,
  independent of schema completeness).
- **XDG layout no longer flips back to `~/.lean-ctx` (GL #623)** — once an install
  resolved to the XDG four-dir layout, a single stray marker appearing in
  `~/.lean-ctx` (a legacy residue, a restored backup, a concurrent older binary,
  even an empty `sessions/`) silently re-collapsed config/data/state/cache onto
  that one directory via `single_dir_override`, after which `config.toml` was no
  longer found and the dashboard graph disappeared (data had moved to
  `$XDG_DATA_HOME/lean-ctx/graphs`). A new **layout pin**
  (`$XDG_CONFIG_HOME/lean-ctx/layout.toml`, `mode = "xdg"`) records the
  commitment: the resolver reads it *before* the legacy/mixed heuristic and never
  re-adopts `~/.lean-ctx` for a pinned install. The pin is written (and a
  residual `~/.lean-ctx` auto-drained) by every independent long-running writer
  and repair path — `setup`, the MCP server start, the daemon
  (`init_foreground_daemon`, incl. the launchd/systemd autostart), and
  `doctor --fix` (after it migrates + reclaims). Marker detection was hardened so an empty
  `sessions/`/`graphs/` directory (or a zero-byte `stats.json`) no longer counts
  as data, and the Docker self-heal shell hook no longer touches `~/.lean-ctx`
  (heal timestamp → `$XDG_STATE_HOME`, lock count → `$XDG_DATA_HOME`). `doctor`
  now reports the active layout mode (`xdg-pinned` / `single-dir / legacy`).
- **Re-reads stop blowing up to full content (cache hit-rate regression)** — with
  `mode` omitted (the recommended usage), a file first read in a compressed mode
  (`map`/`signatures`) was resolved to `full` on its *second* read by the
  `cache_hit` shortcut, even though full content had never been delivered
  (`full_content_delivered=false`). The 2nd read therefore re-delivered the
  *entire file* — more tokens than the first read — a compression bounce that
  also meant stub hits only began at the 3rd read, which agents rarely reach.
  Measured lifetime cache hit-rate had collapsed to ~5% (down from ~90%). The
  resolver now only short-circuits to `full` once full content was actually
  delivered; otherwise it falls through to the predictor, which reproduces the
  cached compressed mode and serves it from the compressed-output cache as a
  cheap, consistent hit. Explicit `mode="full"` reads (for editing) are
  unchanged.
- **Cache-aware pruning no longer churns the cached prompt prefix (#448)** — on
  cache-metered rails (Anthropic), the default `cache-aware` history pruner
  rewrote already-cached history every time the prune boundary advanced a
  `STRIDE` (~every 16 messages), invalidating the provider prompt-cache prefix
  from the first changed message and re-billing cheap reads (0.1x) as writes
  (1.25x). Pruning now skips the client's `cache_control`-marked prefix and only
  ever rewrites not-yet-cached content, so a growing conversation keeps hitting
  the cache. Per-message tool-result compression is unchanged (it is
  content-deterministic and prefix-stable), and requests without `cache_control`
  (e.g. OpenAI) are byte-for-byte unaffected.
- **`ctx_retrieve` / `ctx_share` no longer serve stale cached content** — both
  paths returned the *cached* full content for a file (`get_full_content`) with
  **no staleness check**, so an agent that retrieved a file — or received one via
  a cross-agent `ctx_share` handover — could be handed a version that no longer
  matched disk if the file had been edited since it was first read. This is the
  classic handover failure: agent A edits a handover file, agent B reads the
  pre-edit cached copy and "does not see the changes". `ctx_read` was already
  safe (it revalidates by mtime **and** content hash and re-reads on any
  mismatch); the two retrieve/share accessors bypassed that guard. Both now go
  through a new staleness-safe accessor (`SessionCache::current_full_content`)
  that validates the cached entry against disk (mtime + hash) and transparently
  re-reads the current bytes when the cache is behind the file, so a retrieve or
  handover always reflects the latest content.

## [3.8.8] — 2026-06-17

### Added
- **`lean-ctx update <version>` pins a specific release (#447)** — `update` now
  takes an optional version (`lean-ctx update 3.8.5`, `v`-prefix optional) and
  installs that exact tagged GitHub release instead of the latest, so you can
  roll back or A/B an older build. It reuses the normal update path —
  SHA256-verified download, atomic binary swap, `post_update_rewire` — so the
  same checksum guarantee applies and **no data, config or logs are touched**
  (only the binary is swapped; downgrades read your existing data as-is).
  Invalid versions are rejected before any network call; `--check` reports
  whether the pinned version differs. The auto-update scheduler is unchanged
  (still tracks latest).
- **R2 benchmark faithful-arm preflight (#361)** — `bench/agent-task/r2/preflight.mjs`
  proves, before any priced run, that the pi arm routes shell through `ctx_shell`
  (native `bash` suppressed) and actually compresses it — the "green preflight =
  running as designed" gate the tokbench reviewer asked for, ruling out R1's
  `102 native bash / 0 ctx_shell`. The shell-suppression decision is now the
  single, unit-tested invariant `resolveSuppressedBuiltins`
  (`packages/pi-lean-ctx`), so the routing fix can never silently regress.
- **Proxy accepts a trusted non-loopback HTTP upstream behind an opt-in (#440)** —
  Codex and other clients that sit in front of the proxy need to point it at an
  upstream like `http://host.docker.internal:2455`, but `validate_upstream_url`
  rejected every non-loopback `http://` URL with a misleading "must use HTTPS"
  error and no escape hatch. A trusted plaintext upstream is now allowed via
  `LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM=1` or
  `[proxy] allow_insecure_http_upstream = true`; the startup banner and `doctor`
  flag the plaintext hop so it stays a conscious choice. Documented end-to-end in
  `docs/reference/05-advanced.md`, including the `supports_websockets = false`
  Codex HTTP/SSE setup as an alternative to the native WebSocket transport below.
- **Native WebSocket `/responses` transport for Codex (#440)** — Codex CLI and the
  OpenAI SDK default to a persistent WebSocket connection (`ws://…/responses`,
  one `response.create` event per turn), so the HTTP-only proxy forced clients to
  set `supports_websockets = false`. The proxy now speaks the Responses WebSocket
  protocol natively: `GET /responses` (and `/v1/responses`) upgrades to a
  WebSocket, each `response.create` turn is bridged to the configured HTTP/SSE
  upstream with lean-ctx's tool-output compression applied, and every upstream
  SSE event is relayed back verbatim as a WebSocket frame. Method routing keeps
  `POST` on the HTTP/SSE forwarder, so both transports share one upstream, auth
  path and compression logic (`proxy::openai_responses_ws`). Codex works as a
  drop-in now without disabling WebSockets.

### Changed
- **Rust crate migrated to edition 2024 (#438)** — `cargo fix --edition` plus
  manual fixes for `#[cfg(windows)]` FFI (`unsafe extern "system"`) and
  feature-gated paths `cargo fix` cannot reach on a single host. Newly-`unsafe`
  `std::env::set_var` / `remove_var` calls are fully documented: the 13 production
  sites carry exact `// SAFETY:` justifications (all single-threaded CLI/startup
  paths), while the ~390 test sites route through one audited helper,
  `crate::test_env`, instead of repeating the same comment at every call. Profile
  switching no longer mutates the environment from the multi-threaded MCP server —
  `set_active_profile` records the active profile in a thread-safe in-process cell,
  removing a latent `env::set_var` data race. Nested `if` / `if let` collapsed to
  edition-2024 let-chains tree-wide. No behavioural change. Thanks @dasTholo for
  the original migration PR (#438).
- **OpenCode plugin no longer double-registers the built-in overrides (#441)** —
  the plugin exposed `ctx_read`/`ctx_search`/`ctx_glob`/`ctx_edit`/`ctx_shell`
  both as static replacements of the native `read`/`grep`/`glob`/`edit`/`bash`
  tools and again under their `ctx_*` names via dynamic MCP registration, so the
  model saw two copies of each and paid for the duplicate schemas. The five
  already-overridden tools are now filtered out of the dynamic set; every other
  `ctx_*` tool is still registered dynamically. Thanks @omar-mohamed-khallaf.
- **Default shell allowlist now includes the C/C++ compilers (#361)** — under
  `mode=replace`, `ctx_shell` enforces the allowlist, but `gcc`/`cc`/`clang`/
  `g++`/`c++`/`clang++` were missing even though `rustc`/`go`/`javac` were, so a
  coding agent could not compile an ad-hoc reproducer (`gcc repro.c`) without an
  explicit opt-in (reported by the tokbench review, which set
  `LEAN_CTX_ALLOWLIST_WARN_ONLY=1` to work around it). They are compile-only —
  executing the produced binary stays gated like any other path — so the security
  boundary is unchanged.

### Fixed
- **`gain` dashboard shows the per-day lean-ctx version again (#307)** — the
  "richer theme rendering" pass replaced the per-day version column in the
  RECENT DAYS section with a gradient bar, so `lean-ctx gain` and `gain --deep`
  silently stopped attributing each day's compression rate to a release
  (regressing the feature added in v3.7.1). The version is still recorded on
  every day's stats and the `gain --daily` table still showed it — only the
  dashboard renderer dropped it. The bar is kept (now padded to a fixed width so
  the column lines up) and the version is re-appended (`v{x.y.z}`, `—` for
  pre-tracking days).
- **Secret redaction stops corrupting type annotations and drops its duplicate rules (#430)** —
  `ctx_edit` carried a second copy of the redaction regex set that never got the
  non-secret-literal guard added for #430; worse, its generic-long-secret branch
  kept the matched value before the `[REDACTED]` tag, so a real key could leak into
  diff evidence. Diff masking now goes through the single `core::redaction` source
  of truth. That guard is also widened: right-hand sides that are type expressions —
  `password: Promise<string>`, `apiKey: Record<string, unknown>`, `token: string[]` —
  are recognized as non-secrets (real keys never contain `<>|()[]{}`), so reading
  TypeScript through `ctx_read` no longer masks `password: undefined`-style
  annotations as API keys.
- **`ctx_read` exposes the same schema in Pi as in Codex / MCP (#432)** — the Pi
  adapter hand-wrote a `ctx_read` schema with only `path` / `offset` / `limit` /
  `mode`, so an agent running in Pi never saw `fresh` or `start_line` even though
  the canonical MCP schema (and the Pi handler internally) already supported them —
  making cross-harness instructions like `ctx_read(mode="full", fresh=true)` look
  invalid in Pi only. The Pi schema now matches the registry: `start_line` (with
  `offset` kept as a back-compat alias) and `fresh` are exposed and wired through
  both the MCP-bridge and CLI read paths.
- **`proxy enable` now also routes Pi / forge through the proxy (#361)** — Pi and
  forge resolve their endpoint from `~/.pi/agent/models.json`
  (`providers.<name>.baseUrl`) + OAuth, not from `ANTHROPIC_BASE_URL` /
  `OPENAI_BASE_URL`, so the shell and Claude/Codex env wiring silently bypassed
  them (the tokbench review had to hand-edit `models.json`). `proxy enable` /
  `disable` now wire Pi's `anthropic` (bare origin) and `openai` (`/v1`-suffixed)
  providers when `~/.pi/agent` exists, preserving any custom remote endpoint
  unless `--force` and reverting only the endpoints it set. Pi's OAuth keeps
  working because the proxy forwards the credential verbatim to the real upstream.
- **`config init --full` no longer resets the existing config to defaults (#443)** —
  the command rebuilt the file from `Config::default()` and saved that over the
  user's `config.toml`. Because the TOML merge writes every default value, this
  silently reverted custom settings (proxy port, compression level, provider
  setup, …) on every `init --full`. The command now loads the existing config and
  re-serializes *that* (falling back to defaults only when no file exists),
  preserving user values while still materializing the fully-commented template;
  an unparseable file aborts with a clear message instead of being overwritten.
- **OpenCode (and 18 other agents) now get the `ctx_*` usage rules injected (#442)** —
  rule injection was gated on `rules_already_present()`, a hand-maintained list
  that only knew about five agents. For everyone else it returned `false`, so with
  `auto_inject_rules` unset the setup skipped injection and the model never saw
  the "prefer `ctx_*` tools" guidance — defeating the whole point of MCP-only
  mode. Detection is now derived from the single `build_rules_targets` catalog
  (`rules_inject::any_rules_marker_present`), so every supported agent is covered
  and can never drift from the writer again. The OpenCode hook additionally
  injects the rules into `AGENTS.md` when running MCP-only (shadow mode off) and
  MCP is registered, so the guidance lands even without the interception plugin.
- **Impact graph self-heals after an upgrade so C# same-namespace edges apply (#398)** —
  the v3.8.3 fix added `type_ref` edges for C#/Java types consumed without a
  `using`/import (same-namespace/package visibility), but those edges only exist
  in a freshly built graph. `ctx_impact` rebuilt the property graph only when it
  was *completely empty*, so after upgrading, an existing graph (built before the
  edges existed) was served unchanged — leaving the consumed class a
  false-negative leaf that reported "no impact". The property graph now records
  the engine generation that produced it (`engine_version` + `built_with` in
  `graph.meta.json`), and `ctx_impact analyze`/`diff`/`chain` detect a graph
  built by an older engine and transparently rebuild it once before querying.
  Combined with the XDG resolver fixes (#436/#439) — which keep the graph and
  `config.toml` in a single stable location — a stale or misplaced graph can no
  longer mask the real blast radius. Thanks @nigeldun.
- **Direct writers stop re-creating `~/.lean-ctx` after migration (#439)** — the
  resolver fix (#436) flips the *data tree* to XDG, but several feature-specific
  writers still hard-coded `~/.lean-ctx` and re-created it post-split regardless
  of where the resolver pointed: multi-agent `shared_knowledge.json`
  (`core::agents`), Jira OAuth credentials (`core::providers::jira_oauth`), the
  personal-cloud cache/knowledge readers (`cloud_client` / `cloud_sync`), the
  LaunchAgent proxy logs and scheduled-update logs (`proxy_autostart` /
  `update_scheduler`), the A2A task store (`core::a2a::task`), the cloud
  `mode_stats` reader (`cli::cloud`) and the ctxpkg publisher signing key
  (`core::context_package::keys`). All now route through the typed
  `data_dir()` / `state_dir()` resolvers — the same categories `doctor --fix`
  migrates them to — so a post-migration session reads and writes the XDG dirs,
  while legacy single-dir installs still resolve in place. The source-level
  legacy-path firewall (`rust/tests`) was tightened to catch both the multi-line
  `dirs::home_dir()…join(".lean-ctx")` chains and the `join(".lean-ctx/…")`
  subpath form it previously missed, so the tracked-debt allowlist can only shrink.
- **`doctor` shows `~` instead of the absolute home path (#437)** — dozens of
  checks printed the full `/Users/<name>/…` (or `/home/<name>/…`) path, leaking
  the username and adding noise. Two chokepoint helpers in `doctor/common.rs`
  (`tildify_home` for formatted lines, `display_user_path` for raw paths, with
  component-boundary safety so a sibling like `…/<name>-backup` is never mangled)
  collapse the home dir back to `~` at the central output sinks, so `doctor` and
  `doctor integrations` no longer print an absolute home path.
- **Data dir no longer re-adopts a marker-free `~/.lean-ctx` (#436)** — the data
  resolver returned the legacy `~/.lean-ctx` whenever that directory merely
  *existed*, even after `doctor --fix` had moved every data marker to the XDG
  dirs. Config/state/cache had already flipped to `$XDG_*` in that case, so data
  silently diverged from its siblings and editor sessions kept writing
  `active_transcript.json` / `context_radar.jsonl` back into `~/.lean-ctx`. The
  legacy/mixed decision now lives in a single source of truth
  (`paths::single_dir_override`): a legacy dir wins only while it still holds data
  markers, so once split, data flips to `$XDG_DATA_HOME/lean-ctx` like the rest.
  A cross-category contract test plus a source-level legacy-path firewall
  (`rust/tests`) lock the invariant in so it can never silently regress.
- **`doctor --fix` now empties a residual `~/.lean-ctx` (#434)** — after the data
  moved to XDG, leftover reports (`doctor/`, `setup/`, `status/`) and the empty
  directory lingered, so the next run re-detected the old location and the fix
  report itself was written back into the legacy dir. `--fix` now drains any
  remaining non-runtime entries into the typed XDG dirs and removes the empty
  directory (`xdg_migrate::reclaim_legacy`), and the report lands in XDG.
- **`doctor` reports the real `config.toml` location after a split (#435)** — the
  `config.toml` check and the path-jail hint were hardcoded to `~/.lean-ctx`, so
  after the XDG split `doctor` pointed users at a stale path. Both now resolve
  through `Config::path()` / `config_dir()` and show where the file actually lives.
- **`doctor` score matches the checks it prints (#433)** — `passed`/`total` were
  two hand-maintained counters that drifted: rendered ✗ checks ("XDG layout",
  "data dir split") were shown but never counted, so the summary overstated
  health. Every check now flows through one accumulator that counts exactly what
  it renders; advisory lines (LSP, providers, MCP bridges) are rendered but
  explicitly excluded from the score, so display and tally can no longer diverge.
- **Secret redaction no longer mangles source files read via `ctx_read` (#430)** —
  the key/value secret pattern matched TypeScript type annotations and language
  literals such as `password: undefined`, `secret: string` and `token: null`,
  replacing the value with `[REDACTED:API key param]` and corrupting files read
  through `ctx_read`. The redactor now skips a denylist of obvious non-secret
  literals (undefined/null/none/true/false/string/number/boolean/…). The same
  pass fixed two latent **under**-redaction bugs: AWS keys and generic long
  secrets were annotated in place (the secret kept, `[REDACTED]` merely
  appended) instead of removed. The shell tee redactor and the `ctx_read`
  redactor now share one implementation (`core::redaction`), so the two layers
  can never drift apart again.
- **Dashboard tool profile "Lean" no longer reverts to "Power" (#431)** —
  selecting Lean persisted `tool_profile = "lean"`, but the config loader didn't
  recognise it (logging `Unknown tool_profile 'lean'` and falling back to Power)
  and the settings API reported the *effective* profile (Power) rather than the
  unpinned state. `lean`/`lazy`/`reset` are now understood everywhere as the
  unpin sentinel (centralised in `tool_profiles::is_unpinned_alias`), the loader
  self-heals silently, and the dashboard reports — and round-trips — Lean (the
  toggle is labelled "Lean (default)").
- **Dashboard settings page no longer times out on load (#431)** — route
  handlers ran synchronously on the small async worker pool, so one slow
  endpoint (e.g. a graph/index build) could starve a trivial `GET /api/settings`
  for minutes on few-core machines. Handlers now run on the blocking thread
  pool, keeping light endpoints responsive, and any handler crossing 1s is
  logged for diagnosis.
- **`ctx_read` accepts `offset`/`limit` aliases (#432)** — agents trained on the
  native Read tool send `offset`/`limit`, but the schema only documented
  `start_line`, so those range reads were silently ignored. `offset` is now an
  alias for `start_line` and `limit` bounds the window (`lines:N-M`); the aliases
  are advertised in the tool schema and the generated manifest/reference docs,
  with `PI_AGENTS.md` aligned.
- **macOS "access your Documents" prompt eliminated structurally (#356)** — the
  daemon, proxy and auto-updater run as LaunchAgents (their own TCC identity,
  `ppid 1`), so any access they make under `~/Documents`, `~/Desktop` or
  `~/Downloads` pops the privacy prompt in lean-ctx's name — and because every
  release re-signs the binary, the grant is voided on each update, re-prompting
  forever. The earlier opt-out path guards (v3.8.0–v3.8.7) were per-call-site
  and fragile, and the stable code-signing identity only made *one* "Allow"
  stick — neither satisfied users who refuse Documents access outright. The
  three LaunchAgents are now wrapped in `sandbox-exec` with a minimal Seatbelt
  profile (`allow default`; `deny file-read*/file-write*` under the three
  protected home dirs — `rust/src/core/tcc_guard_sandbox.rs`), so the kernel
  refuses any such access silently with `EPERM`: TCC is never consulted and the
  prompt can no longer appear, with no "Allow" required. Everything else stays
  permitted, so functionality is intact; the path guards and stable signing
  remain as defense-in-depth. The profile is smoke-tested before use (no
  `KeepAlive` crash-loop on a malformed profile), existing installs adopt the
  wrapper automatically on the next `lean-ctx update`, and a new regression
  (`rust/tests/tcc_sandbox.sh`) boots the daemon under the production wrapper.

## [3.8.7] — 2026-06-15

### Added
- **Dashboard: sort the live call feed by per-call cost (#426)** — the Live
  Activity feed already showed per-call detail (tool, file/query, tokens in →
  out, tokens saved, read mode); it now has a **Sort** selector — Recent / Top
  saved / Largest / Slowest — so you can rank tool calls by cost and instantly
  see which reads/searches/shell calls were expensive vs cheap. Read-only,
  reuses the existing `/api/events` journal data; no new routes.
- **Dashboard: Quick Settings — flip core switches from the UI (#427)** — a new
  **Settings** tab (Context area) flips the four high-impact, mid-session
  switches without dropping to the terminal: compression level
  (off/lite/standard/max), tool profile (minimal/standard/power/lean),
  `structure_first` (on/off) and terse agent (off/lite/full/ultra). Writes go
  through a new `/api/settings` endpoint that inherits the dashboard's
  Bearer-token auth and CSRF-`Origin` check, validates every value against the
  config schema **and** a fixed four-key allow-list (no arbitrary config keys
  are writable), and persists to `config.toml` exactly like the matching CLI
  commands. Settings pinned by a `LEAN_CTX_*` environment variable are flagged
  in the UI so a toggle never silently no-ops.
- **Dashboard: `--open=browser|none|vscode` reveal control (#424)** — `lean-ctx
  dashboard` always launched the system browser, which is jarring inside an
  editor or behind a reverse proxy. A new `--open=<mode>` flag (or `--no-open`),
  resolved as `--open` > `LEAN_CTX_DASHBOARD_OPEN` > the browser default, picks
  the reveal behaviour: `browser` (launch the system browser, unchanged default),
  `none` (start silently and just print the URL) or `vscode` (suppress the
  external browser and print the VS Code Simple Browser steps). Flag parsing is
  case-insensitive and falls back to `browser` on an unknown value.

### Fixed
- **macOS: the "lean-ctx wants to access your Documents folder" prompt no longer
  returns after every update (#356)** — lean-ctx binaries are *ad-hoc* signed, so
  their cdhash changes on every build. macOS TCC anchors an ad-hoc binary's
  privacy grant to that cdhash, so each update looked like a brand-new program and
  re-popped the prompt — clicking "Allow" only lasted until the next build. New
  `lean-ctx codesign-setup` (macOS) creates a dedicated keychain with a persistent
  self-signed code-signing identity and trusts it once (a single Touch ID / login
  password confirmation). `dev-install` and the self-updater now sign every build
  with that identity, giving TCC a stable Designated Requirement
  (`identifier "com.leanctx.cli" and certificate leaf = H"…"`) instead of a
  per-build cdhash. Result: a single "Allow" survives all future updates. Falls
  back to ad-hoc signing when the identity isn't set up, so the binary always runs.
- **`doctor --fix` now fully empties `~/.lean-ctx` instead of leaving items behind
  (#429)** — the XDG split migration skipped any entry whose destination already
  existed and *left the source in place*. On Windows (and after any partial
  earlier run or a parallel data dir) the targets routinely pre-existed, so ~30
  legacy items lingered and `doctor` warned about the single-dir install forever,
  no matter how often you ran `--fix`. Collisions are now **reconciled instead of
  skipped**: directories are merged child-by-child, a source file byte-identical
  to the destination is dropped as a duplicate, and a genuinely different source
  is moved aside next to the winner under a `*.legacy` name. The destination is
  never overwritten and nothing is lost, so the legacy directory empties out and
  the warning clears. `doctor --fix` now reports `N moved/merged, N duplicate(s)
  dropped, N kept as *.legacy`.
- **macOS TCC "Documents" prompt — definitive structural fix (#356)** — the
  privacy prompt asking to access your *Documents* folder, which kept returning
  after every `lean-ctx update` despite earlier patches (v3.8.0, v3.8.2), is now
  fixed at the root. The TCC guard (`may_probe_path`) was *opt-in per call site*,
  so every new or forgotten heuristic filesystem probe re-introduced the prompt
  (whack-a-mole). The model is inverted to a **choke-point / opt-out** design:
  - `safe_canonicalize` — the sink that ~8 heuristic call sites funnel through —
    returns the path lexically (no `stat`/`realpath`) when the process is
    launchd-standalone and the path is under `~/Documents`, `~/Desktop` or
    `~/Downloads`.
  - every duplicated project-marker probe (`config`, `graph_index`, `setup`,
    `dashboard`, `knowledge_bootstrap`, `graph_provider`) now routes through the
    single guarded `pathutil::has_project_marker`, with one marker set.
  - `is_safe_scan_root` refuses launchd-standalone scans under the protected dirs
    before any marker probe or `read_dir`; `has_multi_repo_children` now also
    refuses *nested* protected paths (e.g. `~/Documents/proj`), not just the bare
    magic dirs. The project-local `.lean-ctx.toml` read and the `git rev-parse` /
    cwd-fallback in project-root detection are guarded too.

  Why it kept coming back: `lean-ctx update` run from a terminal makes the daemon
  inherit the terminal's TCC grant, masking the bug; end users run the daemon and
  proxy as LaunchAgents (ppid 1, *standalone*), where the unguarded probes hit
  `~/Documents` and prompt — and every update changes the binary's code signature,
  invalidating any prior grant. A new macOS `sandbox-exec` regression test
  (`rust/tests/tcc_sandbox.sh`) boots the daemon as a standalone process under a
  profile that SIGKILLs on any `~/Documents` access, reproducing the real
  end-user condition that terminal testing hid, alongside standalone unit tests
  in `pathutil` / `graph_index` / `session`.

  Note: installing the update that *contains* this fix may show the prompt one
  last time (the old, still-running binary's signature changes as it is
  replaced); after that it stays quiet.
- **`auto_update_mcp = false` now suppresses MCP writes on every registration
  path (#281)** — earlier fixes only gated the shared JSON-config writer and
  `configure_agent_mcp`; the per-agent hook writers (Claude, JetBrains, OpenClaw,
  Crush, OpenCode) and the editor-registry registration in interactive setup,
  non-interactive setup and `doctor --fix` still wrote MCP server entries
  unconditionally. The check is now centralized in `hooks::should_register_mcp()`
  and applied on every path: hooks, rules and skills still install, only the MCP
  server entry is withheld. A subprocess regression test guards it.
- **`ctx_read` map/signatures no longer serve pre-rebuild output after
  `ctx_index build-full` (#420)** — the CLI `build-full` path cleared the daemon
  read cache, but the MCP tool runs in the process that owns the `SessionCache`,
  so a forced rebuild left `ctx_read map`/`signatures` returning stale output.
  The MCP tool now invalidates the in-process graph cache and clears the
  `SessionCache` in-process, matching the CLI guarantee.
- **Dashboard auto-refreshes the active view on data change and tab focus
  (#425)** — the 10s poll only refreshed the status bar and flagged the manual
  refresh button; the main panels listen to `lctx:refresh`, which only the manual
  button dispatched, so stats/metrics stayed static until a reload. The poll now
  dispatches `lctx:refresh` on a content-hash change while the tab is visible
  (panels reload in place, preserving UI state), and a `visibilitychange` handler
  catches up immediately when the tab regains focus.
- **`lean-ctx watch` backfills recent events on start (#560)** — `watch` set the
  tail offset to EOF on startup, so an idle launch showed a blank screen even
  when `events.jsonl` was already populated. It now seeds the view with the last
  20 events (bounded, O(n) memory) and advances the offset to EOF, so the live
  poll stream continues without re-emitting them.
- **Homebrew installs no longer run a stale shadowed binary (#559)** — a
  brew-managed shim (`/opt/homebrew/bin/lean-ctx` → `../Cellar/lean-ctx/<old>`)
  could shadow the freshly built `~/.local/bin/lean-ctx` on `PATH`, so the daemon
  and CLI ran different builds (md5 drift). After installing, lean-ctx repoints
  any Cellar/linuxbrew shim at the just-installed binary and warns about any other
  `PATH` entry that still resolves before it. (The drift helper is correctly
  gated to unix so the Windows cross-compile stays warning-clean.)
- **JetBrains plugin ships under a discoverable release-asset name (#418)** —
  `buildPlugin` emitted `lean-ctx-<version>.zip`, indistinguishable from a source
  archive in the GitHub Release asset list, so the plugin looked "missing" even
  though it was attached. The artifact is renamed to
  `lean-ctx-jetbrains-plugin-<version>.zip` before upload, and the release job
  now fails loudly if `buildPlugin` produced no zip.

### Security
- **PathJail keeps resolving symlinks under TCC-protected dirs (#356 follow-up)**
  — the #356 choke-point accidentally routed PathJail's canonicalization through
  the same TCC guard (`canonicalize_or_self` → `safe_canonicalize_bounded` →
  `safe_canonicalize`), so a launchd-standalone daemon validating a path under
  `~/Documents` got a *lexical* (unresolved) path and could miss a symlink jail
  escape. Security canonicalization is now split from heuristic canonicalization:
  PathJail (jail root, candidate ancestor, extra-roots, TOCTOU re-check, and the
  allow-list) uses a new unguarded `pathutil::canonicalize_secure[_bounded]` that
  always resolves symlinks; only self-initiated heuristic probes keep the guard.
  The jail only ever runs on a path the client explicitly asked for, so a
  one-time prompt there is legitimate, while #356's self-initiated boot prompts
  stay suppressed (verified by the `sandbox-exec` boot test plus a new
  `canonicalize_secure_bypasses_tcc_guard_for_pathjail` unit test).
- **Cookbook dev-dependency upgrade — Vite 6 → 8 (#595)** — the example apps now
  build on Vite `^8.0.16` with `@vitejs/plugin-react` `^6` (peer `vite ^8`),
  pulling a patched esbuild and clearing the esbuild dev-server advisory
  (GHSA-67mh-4wv8-2f99). `npm audit` reports 0 vulnerabilities; the
  knowledge-graph-explorer example builds and typechecks unchanged. Node engine
  floor raised to `>=20.19.0` to match Vite 8's requirement.

## [3.8.6] — 2026-06-15

### Added
- **CodeBuddy AI platform support (#423)** — CodeBuddy joins Claude Code / Codex
  as a first-class agent: detection, `init` / `setup` / `uninstall`, MCP wiring
  at `~/.codebuddy/mcp.json`, dedicated rules injection, and the same path-jail
  protection as `.claude` / `.codex` (`~/.codebuddy` in `IDE_CONFIG_DIRS`, the
  broad-root guard, and the home/agent-dir checks). Thanks @studyzy.
- **Structure-first cold reads (`structure_first`, #361)** — an opt-in bias (off
  by default; env `LEAN_CTX_STRUCTURE_FIRST`) for `auto` to prefer `map` on a
  cold read of a medium-sized source file. It is the one read saving that
  survives a phase-isolated harness (no warm-session re-read to amortise a full
  read) and is capability-safe: the active-diagnostic / edit-fail / small-file
  guards still force `full`.
- **`gain` now reports net-of-injection bill impact (#361)** — `lean-ctx gain`
  (and `gain --json`) surface the observed proxy turns, the total injected
  overhead (per-turn tax × turns) and `net_tokens_saved` (which can go negative
  and says so), so the meter reconciles to the provider bill instead of a
  tool-local ratio. The proxy persists its request count to make this honest.
- **Faithful benchmark arm config (#361)** — `bench/agent-task/r2/` ships a
  zero-injection, capability-safe lean-ctx arm (`rules_injection=off`, minimal
  tool profile, `structure_first`, proxy on with cache-aware pruning) plus the pi
  extension config and proxy env wiring, so an independent benchmark runs
  lean-ctx "installed = running as designed".

### Changed
- **Suspect files are never compressed away on a fix task (#361)** — when the
  task text explicitly names a file (e.g. "fix the sort in versioncmp.c"), `auto`
  now forces `full` for that file ahead of any compression-favouring intent, so
  the agent always gets the body it needs to localise and edit the defect.
- **The proxy protects build/test fidelity and foreign tools (#361)** — a
  generic/foreign shell `tool_result` that looks like a build failure or test run
  is preserved verbatim at the wire (compiler errors, panics and test summaries
  kept intact), and vendor-prefixed tools (`forge_read`, `pi.shell`, …) are now
  classified by name segment so a foreign source read is protected and a foreign
  shell log is compressed. Request-body compression is deterministic, keeping the
  provider prompt-cache prefix byte-stable.
- **The pi extension can route shell through `ctx_shell` (#361)** — a new
  `routeShell` opt-in (env `LEAN_CTX_PI_ROUTE_SHELL`, implied by `replace` mode)
  suppresses the native `bash` builtin so build/test/log output is compressed and
  metered (lossless for signal), while the read/list/search builtins stay
  available alongside `ctx_*`.

### Fixed
- **`[archive]` could exhaust host RAM and force a reboot (#417)** — archived
  tool outputs (`.txt` + `.meta.json` + SQLite FTS) were written on every large
  call, but the configured `max_disk_mb` / `max_age_hours` limits were never
  enforced: `archive::cleanup()` had no production caller and the FTS cap deleted
  only DB rows, orphaning the (much larger) `.txt` blobs. The store therefore
  grew unbounded on disk and starved the host of RAM via the page cache.
  `cleanup()` now enforces both the age TTL and the on-disk size budget, prunes
  the content files and FTS index together (no more orphans), runs at MCP start
  and periodically off the hot path, and `lean-ctx cache prune` reclaims the
  archive too.
- **`doctor` reported the proxy as broken on Windows (#416)** — proxy autostart
  has no backend on Windows, so `doctor` treated its absence as a hard failure
  (a permanent 27/28). The proxy check is now platform-aware: a reachable proxy
  is green, an unreachable proxy on a platform without autostart is a warning
  (run `lean-ctx proxy start`), and "running but autostart not installed" is a
  warning rather than a failure on macOS/Linux.
- **`setup` reported compression settings it never saved (#415)** — the wizard
  printed "✓ Compression: …" before writing and swallowed the write error, so a
  failed save still looked successful. Success (and the rules-prompt injection)
  is now reported only after the config is actually persisted. `doctor` also
  displayed "power" for an unpinned install; it now correctly reports
  "lean (default)".
- **A data dir split across two trees could not be merged (#414)** — when both a
  legacy (`~/.lean-ctx`) and an XDG tree held a `stats.json`, the old migration
  bailed and `doctor` pointed at `lean-ctx setup` instead of `doctor --fix`.
  `doctor --fix` now consolidates every non-canonical data tree into the
  canonical one (newer file wins, never clobbering a newer copy) before the XDG
  split, the hint points to the right command, and `$XDG_DATA_HOME/lean-ctx` is
  included in split detection.
- **JetBrains plugin now ships as a downloadable GitHub Release asset (#418)** —
  the plugin `.zip` is built and attached to every release. It was missing from
  v3.8.5 because the plugin's `Release Asset` job only ran on `release` events,
  which a `GITHUB_TOKEN`-created release never triggers. The plugin version is now
  single-sourced in `gradle.properties` and mirrors the engine release via
  `-Pversion=<tag>`, so it can no longer drift (it had been stuck at 3.8.3).
- **The wake-up briefing listed dead and foreign agents (#419)** — `ctx_overview`
  read the raw `AgentRegistry`, so it showed peers from crashed or exited MCP
  processes (and from other projects). It now prunes stale entries
  (`cleanup_stale`) and scopes the list to the current project root, matching
  what `ctx_agent list` and the dashboard already do.
- **`ctx_read` map/signatures served pre-rebuild output (#420)** — `lean-ctx graph
  build --force` and `lean-ctx index build-full` only dropped the in-process graph
  cache, but a running daemon kept serving stale `map`/`signatures` from its
  long-lived `SessionCache` in another process. Both commands now also flush the
  daemon's read cache over IPC (never auto-starting one), so derivations
  re-derive on the next read.
- **`ctx_multi_read` ignored `auto` mode (#421)** — batch reads forced
  `auto`→`full`, so every file came back fully expanded regardless of the active
  profile. `ctx_multi_read` now honours `auto` like a single `ctx_read`, resolving
  the optimal mode per file. Tool descriptions, schemas and the injected rules
  (bumped to v12) now steer agents to omit `mode` (= `auto`) and reserve `full`
  for the read immediately before an edit.
- **`ctx_semantic_search` was hidden in the default profile (#422)** — the
  meaning-based search tool was categorised under `Memory` and absent from the
  lean core set, so it never appeared in the default ("lean") gate. It is now a
  Core tool and part of the advertised core surface; the setup/doctor tool counts
  are derived dynamically instead of a hard-coded "13".
- **A cold read could cost more tokens than the raw file (#361)** — an
  independent benchmark measured `ctx_read` auto-mode payloads up to +21.6%
  larger than the source on a small codebase: on a tiny file the one-line
  framing header (file ref + deps/exports summary) is net overhead that only
  amortises across re-reads, and the CLI one-shot path used a divergent resolver
  that lacked the small-file guard. `ctx_read` now enforces a hard anti-inflation
  invariant — a read **never** returns more tokens than the raw file. When
  framing would exceed the bare content (auto-resolved or `full` reads) the file
  is shipped verbatim, so a read is break-even at worst and a win whenever a
  compressed mode or cached re-read applies; an explicitly requested view
  (`map`/`signatures`/`lines:`) is always honoured untouched. The same guarantee
  now covers the additive one-shot CLI path, which also routes through the
  unified auto-mode resolver. Re-reads are unaffected (the cache keys on path and
  re-derives the file ref). Follow-up: `map` mode no longer repeats exports the
  `API:` section already lists with full signatures — the same symbols were
  emitted twice (once as a bare `exports:` line, once in `API:`). A shared
  `exports_not_in_signatures` helper now drives the MCP, CLI **and** benchmark map
  renderers, so every export is shown exactly once (re-exports/const aliases the
  API can't capture still surface) and the scorecard measures the deduped output
  agents actually receive.
- **A knowledge store could grow to 2× its fact cap on import (#417)** —
  `remember()` hard-caps a project's facts at `max_facts`, but the bulk
  `import_facts()` path still used the old `max_facts * 2` guard, so a
  merge/import could inflate a store to twice its budget before any eviction
  fired (observed live as a `doctor` capacity `CRIT`, e.g. facts 232/200). The
  import path now runs the memory lifecycle as soon as it exceeds `max_facts`,
  draining the excess by importance (archived, not lost). The eviction invariant
  now holds on every write path (`remember`, `import`, persist-merge).
- **Knowledge stores for deleted projects accumulated forever (#615)** — a store
  at `knowledge/<hash>/` is keyed to a `project_root`; when that root is deleted
  (a removed git worktree, a thrown-away project) the store can never be written
  again, so its eviction cap can never self-heal and it lingers as pure disk
  bloat (one such store surfaced live as a permanent `doctor` capacity `CRIT`).
  `lean-ctx doctor` now reports orphaned stores and the reclaimable size,
  `lean-ctx cache prune` reclaims them (alongside BM25/graph/archive), and
  `doctor --fix` prunes them as part of a repair. Detection is conservative — a
  store with an empty (legacy/global) root or a still-existing root is never
  touched, and only the explicit prune commands delete (never the background
  lifecycle), so a temporarily-unmounted drive can't trigger data loss.
- **`auto_update_mcp = false` was still ignored on several MCP registration
  paths (#281)** — earlier fixes gated the shared JSON-config writer and the
  editor-target helper (`configure_agent_mcp`), but the per-agent hook writers
  (Claude, JetBrains, OpenClaw, Crush, OpenCode) and the editor-registry
  registration in interactive `setup`, non-interactive `setup` and `doctor --fix`
  still wrote MCP server entries unconditionally. Every registration path now
  honours the flag: hooks, rules and skills still install, only the MCP *server*
  entry is withheld, so a locked-down environment stays MCP-free after
  `setup`/`onboard`/`init`/`doctor --fix`.

## [3.8.5] — 2026-06-14

### Added
- **JetBrains / IntelliJ IDE plugin (#413)** — a native plugin (community
  contribution by @dasTholo) that drives lean-ctx from inside JetBrains IDEs:
  PSI-backed navigation, a refactoring engine (rename / move / inline / safe
  delete), symbolic body edits and an in-IDE tool window. The Rust engine gains a
  matching `ctx_refactor` surface and an LSP layer (`lsp::backend`,
  `jetbrains_backend`, `edit_apply`, `port_discovery`) that talks to the IDE over
  a **localhost-only, token-authenticated** HTTP channel and **re-validates every
  plugin-reported path against the project PathJail** (BLAKE3 TOCTOU guard, atomic
  writes). It also works **headless** (tree-sitter range edits without a running
  IDE). Kotlin / Kotlin-Script (`.kt` / `.kts`) are now recognised for indexing.
- **First-class Lua / Luau graph indexing (#360)** — symbols, `require` edges and
  the call graph are now extracted for Lua and Luau sources.
- **`lean-ctx dashboard --auth-token` (#377)** — a fixed dashboard auth token via
  flag or env (env takes precedence) for reverse-proxy deployments, with
  token-aware connection reuse.
- **`lean-ctx doctor --fix` splits a legacy/mixed install into the XDG dirs
  (#408)**: moves data/state/cache out of the config dir on demand. The migration
  is all-or-nothing, idempotent/resumable (existing files are never clobbered) and
  crash-safe (atomic `rename` with a copy+remove fallback across filesystems).
  Read-only `lean-ctx doctor` reports a pending split. New per-category overrides
  `LEAN_CTX_CONFIG_DIR`, `LEAN_CTX_STATE_DIR`, `LEAN_CTX_CACHE_DIR`.
- **Multilingual intent routing (#591)** — intent detection now handles
  non-English queries.

### Changed
- **XDG Base Directory compliance (#408)** — lean-ctx now separates its files
  into the standard XDG categories so the config dir can be mounted **read-only**:
  - **Config** (`config.toml`, shell hooks, `env.sh`) → `$XDG_CONFIG_HOME/lean-ctx`.
  - **Data** (sessions, vectors, graphs, knowledge, archives, memory, `stats.json`)
    → `$XDG_DATA_HOME/lean-ctx` — the fresh-install default flipped here from the
    old config dir.
  - **State** (events, journals, logs, ledgers, `agent_runtime_env.json`) →
    `$XDG_STATE_HOME/lean-ctx`.
  - **Cache** (semantic cache, models, learned patterns) →
    `$XDG_CACHE_HOME/lean-ctx`.

  Existing legacy (`~/.lean-ctx`) and mixed (`$XDG_CONFIG_HOME/lean-ctx`) installs
  keep working unchanged in single-dir mode; an explicit `LEAN_CTX_DATA_DIR` still
  forces one directory and is never auto-split.
- **pi-lean-ctx bridge tool parity (#409)** — `ctx_search`, `ctx_tree` and
  `ctx_multi_read` are now exposed through the Pi bridge, guarded by a Node CI gate.

### Fixed
- **Embedding index clobbered by parallel `remember` (#412)** — embedding-index
  writes are now serialized under the per-project lock, fixing degraded recall
  when multiple `remember` calls raced.
- **`auto_update_mcp = false` ignored during setup/onboard/init (#281)** — the
  first fix gated only the editor-target registration (`configure_agent_mcp`);
  the hooks-layer MCP writers still wrote server entries unconditionally — the
  shared JSON-config writer behind Aider/Continue/Qwen/Zed/Amazon Q/Trae/Neovim/…,
  plus Copilot CLI, Gemini/Antigravity and Hermes. The flag is now honored on
  every registration path: hooks, rules and skills still install, only the MCP
  *server* entry is withheld, so a locked-down environment stays MCP-free after
  `setup`/`onboard`/`init`/`doctor --fix`.
- **Session `extra_roots` not honored in path resolution (#403)** — extra roots
  are propagated at init and respected by the resolver.
- **Verbatim reads compressed on the CLI path (#404)** — verbatim reads are now
  exempt from terse compression on the CLI.
- **`Config::load` served stale config (#406, #407)** — the load cache is now
  invalidated by content hash so live edits apply immediately.
- **pi-lean-ctx MCP bridge did not shut down cleanly (#405)**.

### Security
- **Captured agent API keys now stored in the state dir at `0o600` (#408)** — keys
  such as `GEMINI_API_KEY` no longer sit alongside config files.
- **esbuild forced to ≥0.28.1 in the cookbook (#595)** — closes
  GHSA-gv7w-rqvm-qjhr (dev-scope: missing binary integrity verification) by
  deduping the whole cookbook tree onto a patched esbuild.

### Internal
- **`make preflight` CI-parity gate** — a local fmt / clippy / doc / doc-drift /
  Windows-cross-compile / test gate wired into a pre-push hook, so the
  deterministic CI jobs can no longer go red only after the full CI matrix.

## [3.8.4] — 2026-06-13

### Fixed
- **`ctx_tree`/`ctx_search`/`ctx_glob` ignored an out-of-scope `path` and
  scanned the whole project instead (#401)**: when an explicit `path` (or
  `paths`) argument pointed outside the project root — or was otherwise
  unresolvable — the dispatcher's PathJail rejection was swallowed and the tools
  silently fell back to the project root, returning the entire repository tree
  for an unrelated path. The resolution error is now surfaced
  (`ERROR: path escapes project root: … (root: …)`) instead of a misleading
  full-tree result. Non-existent paths *inside* the project keep their clear
  "does not exist" message.

### Added
- **`lean-ctx doctor overhead` (#572)**: per-client fixed-cost report — how many
  tokens your editor pays *every session* for tool schemas, instructions and
  rules files, with duplicate detection across CLAUDE.md/.cursorrules/AGENTS.md.
- **`lean-ctx rules dedup [--apply]` (#578)**: finds and removes lean-ctx-owned
  duplicate rule files and stale marked blocks across editors. The
  `.cursorrules` template is now a pointer to the canonical rules, and the
  compression block is no longer double-injected for Cursor.

### Changed
- **Token-efficiency epic, phase 1 (#571)** — fixed per-session overhead cut
  from ~13.7K to ~6.0K tokens on a typical setup:
  - **Lean default tool surface (#575)**: setup no longer pins a
    `tool_profile`; the default surface is 13 lazy-core tools instead of 61.
    `lean-ctx tools lean`/`reset` manage it explicitly.
  - **Schema diet (#576)**: core tool descriptions and schemas trimmed
    3031→1935 tokens (−36%); large action enums folded into pipe-delimited
    descriptions; a budget regression test keeps it from creeping back.
  - **Instructions cap (#579)**: the static instruction skeleton stays ≤400
    tokens (Off/Compact CRP) / ≤500 (TDD); the decoder block is mode-aware and
    canonical rule blocks were condensed.
  - **Honest metrics (#573)**: dashboard, footer and ledger report observed
    tokens only — the modeled 2.5× grep baseline moves to the *estimated*
    series; `ctx_cost` splits cached vs uncached input at cache-read pricing;
    the benchmark measures the real CCP resume payload.
  - **Self-describing outputs (#580)**: plain notation uses real language
    keywords (`struct`/`trait`/`pub`), and TDD symbol outputs carry a minimal
    inline legend (≤15 tokens) so agents never guess the notation.
- **Codex hook: native rewrite instead of block-and-retry (#399, community
  contribution)**: on Codex ≥ 0.20 the `PreToolUse` hook now returns
  `updatedInput` to rewrite shell commands through lean-ctx in place — no more
  deny + model-retry round-trip per command.

### Security
- Bumped the postgres crate family past three fresh RUSTSEC advisories
  (unbounded SCRAM iteration DoS, `hstore`/`DataRow` decode panics) — found by
  `cargo-deny` the moment they were published; lean-ctx never exposed the
  vulnerable paths to untrusted servers (#399).

### Fixed
- **`lean-ctx overview` flooded the terminal with thousands of `node_modules`
  entries on projects without a top-level `.git` (#400)**: the `ignore` crate
  only applies `.gitignore` files *inside* git repositories — in a monorepo
  whose subprojects carry their own `.gitignore` but whose root is not a git
  repo, every scanner walked `node_modules` wholesale (74k+ files in the
  report). Two-part fix, applied to **all 15 directory walkers** (graph/BM25/
  trigram index builders, `ctx_impact`, `ctx_search`/`ctx_tree`/`ctx_glob`,
  CLI scans): a shared `walk_filter` now prunes unambiguous vendor dirs
  (`node_modules`, `__pycache__`, `bower_components`, virtualenvs with a
  `pyvenv.cfg`) regardless of git state, and `require_git(false)` makes
  `.gitignore` files effective without a `.git` directory. Explicit roots
  stay reachable (`ctx_tree node_modules/react` works), and
  `respect_gitignore=false` remains the escape hatch for searching inside
  vendor dirs.
- **macOS privacy prompts ("lean-ctx would like to access …") fired repeatedly
  while the MCP server was running (#356 follow-up)**: editors spawn the
  user-level MCP server with `cwd == $HOME`. A `ctx_search`/`ctx_tree`/
  `ctx_glob` call whose `path` fell back to `"."` then walked the **entire
  home directory** — every `stat` under `~/Library`, `~/Desktop`, `~/Pictures`
  trips a TCC prompt (Calendar/Reminders/AddressBook/Photos), and the walk
  burned 10–20 s per call. The index builders already refused broad roots;
  the direct walk fallbacks did not. All three walk tools now share that same
  root policy (new `walk_guard`): relative paths are absolutized against the
  process cwd first — so `lean-ctx grep`/`ls` inside a real project keep
  working — and broad or privacy-protected roots (`$HOME`, `/`, `~/Library`,
  TCC dirs without project markers) return an actionable error telling the
  agent to pass an explicit project `path` instead of silently scanning.
- **`ctx_impact` reported C# classes as leaf nodes when consumers had no
  `using` directive (#398)**: C# resolves types in the same namespace without
  any import, and DI-style code never `new`s its dependencies — so a class
  consumed only as a *type* (constructor parameter, field, property, base
  class, generic argument) produced **zero** graph edges and a false-negative
  "no files depend on X". The property-graph builder now extracts **type
  usages** from the AST (fields, parameters, returns, base lists, generics,
  casts, `typeof`) for C# and Java — the two supported languages with implicit
  same-namespace/package visibility — and links consumer files to defining
  files with `type_ref` edges, which `impact_analysis` already traverses.
  Names defined in more than 3 files are skipped as too generic to attribute.
- **Same root cause, second symptom**: classes consumed only as a type were
  flagged by the `dead_code` smell — its SQL already exempted `type_ref`
  targets, but nothing ever *created* those edges. The builder now also emits
  symbol-level `type_ref` edges, so DI-consumed classes no longer show up as
  dead code while genuinely unreferenced ones still do.
- Both property-graph builder paths (default and minimal) now share one
  analysis pass and definition index, so the fix applies regardless of build
  features.

## [3.8.2] — 2026-06-12

### Fixed
- **Codex PreToolUse shell compression no longer blocks with a manual re-run
  prompt**: Codex now supports native `updatedInput` rewrites for `PreToolUse`
  hooks, so `hook codex-pretooluse` emits the documented allow+rewrite JSON on
  stdout instead of exiting 2 with "Re-run with ..." feedback. Rewritable Bash
  commands are transparently replaced with the `lean-ctx -c ...` command while
  preserving normal tool execution.
- **Linux: `ctx_*` tools broke for projects under `/c/…` and other
  single-letter roots (#397)**: the MSYS2/Git-Bash drive mapping
  (`/c/Users/…` → `C:/Users/…`) in the MCP path normalizer ran
  **unconditionally** — on Linux/macOS, where `/c/…` is a literal directory,
  every file-addressing tool then failed with `file not found` on a
  nonexistent `C:/…` path (and absolute arguments were re-joined under the
  already-translated root, doubling it). The mapping is now gated on Windows
  hosts (`cfg!(windows)`) — that is the only platform where MSYS2/Git-Bash
  clients hand POSIX drive paths to a native Windows binary. On other hosts,
  `/c/…` passes through untouched; regression tests cover both sides.
- **`lean-ctx doctor` reported "no rules file found" right after `lean-ctx setup`
  (#396)**: the 3.8 layout (GL #555) intentionally replaced the always-loaded
  `~/.claude/rules/lean-ctx.md` with a CLAUDE.md block + on-demand skill — setup
  even *removes* the legacy file — but the doctor check still demanded it, so a
  clean install could never reach a full pass and the suggested fix
  (`init --agent claude`) couldn't recreate the file either. Both doctor views
  (`doctor` and `doctor integrations`) now share one layout detector
  (`claude_instructions_state`) that accepts every state setup can produce:
  CLAUDE.md block (+ skill), dedicated injection (SessionStart hook + skill),
  legacy rules file, project scope, and `rules_injection=off`. Docs that still
  described the retired rules file were updated as well.
- **macOS still prompted "lean-ctx would like to access files in your Documents
  folder" on every upgrade (#356, reopened)**: the first fix (3.8.0) removed the
  *scan-heuristic* probes, but the prompt actually came from the **launchd
  daemon's boot path** — a process that is its own TCC identity, and whose
  grant is invalidated by every update (binary swap → new cdhash → re-prompt).
  Traced empirically with a deny-sandbox + crash-stack bisection; two
  independent boot-time offenders fixed:
  1. `serve` booted with cwd `/` and walked **every stored session**, stat-ing
     each session's `project_root`/`shell_cwd` (project-marker probes +
     `canonicalize`) — paths that usually live under `~/Documents`. Broad
     roots ("/", HOME, agent sandboxes) now bail out *before* the scan — they
     can never own a session (this also stops `shell_cwd.starts_with("/")`
     from leaking an arbitrary project's session into the daemon default).
  2. `ContextLedger::load → prune` ran `realpath` over every persisted ledger
     entry at boot for its dedupe key; the key is now lexical-only.
  Defense in depth: launchd-owned processes (ppid 1) are detected as
  *TCC-standalone* and never stat/canonicalize paths under
  `~/Documents`/`Desktop`/`Downloads` in heuristics (`has_project_marker`,
  session-root matching, `normalize_tool_path`); editor/CLI children inherit
  their host's TCC grant and keep full behavior. Verified with a
  SIGKILL-on-Documents-access sandbox: daemon boot (30 s soak), proxy boot,
  and the full `lean-ctx update` rewire now run clean against a real data dir
  with 600+ sessions rooted under `~/Documents`.
- **Pi: `ctx_grep`/`ctx_find`/`ctx_ls` silently searched the wrong directory
  (#395)**: `path` was optional and fell back to the extension's cwd, so an
  agent working elsewhere got results from the wrong tree and was derailed;
  the calls also rendered without their arguments. `path` is now **required**
  (schema + description make the scope explicit), and the three tools reuse
  Pi's native call renderers so every invocation shows its pattern and
  directory in the transcript.
- **OpenCode × ChatGPT-OAuth broke behind the proxy (#366)**: `proxy enable`
  exported `OPENAI_BASE_URL` without the `/v1` suffix the OpenAI SDK convention
  expects (default is `https://api.openai.com/v1`). OpenCode therefore sent
  Responses-API calls to `…:4444/responses` — a path its ChatGPT-OAuth plugin
  does not recognize (it matches `/v1/responses`), so subscription traffic
  leaked through the proxy to the platform API with the wrong credential:
  *"Missing scopes: api.responses.write"*. The shell exports and the Codex CLI
  config now advertise `http://127.0.0.1:<port>/v1`; with that base, OpenCode's
  OAuth plugin correctly routes ChatGPT-subscription requests directly to
  `chatgpt.com` (analogous to the Claude Pro/Max guard), while API-key traffic
  keeps flowing through the proxy. Stale `/v1`-less entries in Codex
  `config.toml` are migrated on the next `proxy enable`; the proxy also
  collapses accidental `/v1/v1/…` double prefixes from clients that append
  `/v1` themselves. Verified end-to-end against OpenCode 1.2.15.
- **Dashboard token race**: `lean-ctx dashboard` persisted its fresh auth token
  *before* binding the port. Two racing starts both wrote `dashboard.token`;
  the bind loser exited, leaving a token on disk the surviving server never
  accepted — every "already running" browser open then hit silent 401s. The
  token is now saved only after a successful bind.
- **Live Activity feed masked errors as "No events recorded yet"**: a failed
  `/api/events` poll (daemon restart, expired token, timeout) was rendered as
  an empty feed. The dashboard now keeps the last known events and shows the
  actual error with a recovery hint instead.
- **Status bar showed "No session" while agents were active**: `/api/session`
  matched sessions against the dashboard process's own cwd (usually HOME — a
  broad root that rightly matches nothing). It now falls back to the most
  recently updated session rooted in a real project.

### Performance
- **`/api/events` no longer re-parses the event log on every poll**: the
  file-backed event load is cached on (path, mtime, length) — the 3-second
  dashboard poll now costs a `stat()` instead of reading and parsing up to
  10k JSONL lines.

## [3.8.1] — 2026-06-12

> **The Field-Report Patch.** Five issues straight from users' terminals, fixed
> the same week v3.8.0 shipped: `daemon enable --help` no longer *installs the
> service it was asked to explain* (#393), `allow_paths` finally expands `~`
> and `$VAR` instead of matching them literally (#392), and `ctx_shell` closes
> the download-to-file, xargs-delegation and "strict mode that only warned"
> gaps from the #391 security report. Plus: service file paths are printed
> where you need them with a new `daemon restart` (#394), and `/reopen` works
> anywhere in a comment (#388).

### Added
- **`lean-ctx daemon restart`** (GH #394): stops the supervised service and/or a
  manually started daemon, then starts it again through whichever channel was
  active before.
- **Service file paths are printed** on `daemon enable`/`disable`, shown in
  `daemon status` and `lean-ctx doctor` (GH #394): the exact LaunchAgent plist /
  systemd user unit path plus the unit name, so `systemctl --user` /
  `launchctl` targets are obvious without searching.
- **`lean-ctx doctor` Path-jail check** (GH #392): reports the effective jail
  state (active / `path_jail = false` / compile-time `no-jail`), flags
  `allow_paths` entries that can never match (unset `$VAR`, missing directory)
  and the `allow_paths = ["/"]` pattern.
- **Consolidated filesystem-boundary reference** (GH #392):
  `docs/reference/appendix-paths-and-config.md` §5 documents `path_jail` vs
  `allow_paths` vs `extra_roots`, the `no-jail` cargo feature and the removed
  `LEAN_CTX_NO_JAIL` env var; SECURITY.md cross-links it.

### Fixed
- **`daemon enable --help` executed instead of showing help** (GH #393):
  `--help`/`-h`/`help` anywhere in `lean-ctx daemon …`, `lean-ctx proxy …` or
  `lean-ctx allow …` now prints usage and never executes the verb (an agent in
  read-only plan mode installed the systemd service by asking for help).
- **`allow_paths` / `extra_roots` entries with `~`, `$VAR` or `${VAR}` were
  matched literally** (GH #392): config files see no shell, so
  `"$HOME/code"` silently never matched and PathJail kept rejecting paths the
  user had explicitly allowed. Entries (and the `LEAN_CTX_ALLOW_PATH` /
  `LEAN_CTX_EXTRA_ROOTS` env lists, which MCP hosts pass shell-less too) are
  now expanded; unset variables warn and are reported by doctor.

### Security
- **`ctx_shell` hardening** (GH #391): download-to-file flags are now treated
  as file writes (`curl -o/-O/--output/--remote-name`, `wget`'s default
  file-download mode — `wget -qO-`/`--spider` stay allowed, `dd of=` except
  `/dev/null`); `xargs`/`nohup` join the delegation-aware checks so
  `… | xargs bash -c '…'` cannot smuggle inline code past the interpreter
  block in either allowlist or blocklist-only mode; `shell_strict_mode = true`
  now actually **blocks** command substitution in arguments and
  pipe-to-bare-interpreter (both previously only logged a warning while
  claiming to block); substitution detection now also covers double-quoted
  `"$(…)"` (single quotes still exempt — the shell doesn't expand there).
  SECURITY.md states the ctx_shell threat model explicitly: defense in depth
  for agent mistakes, **not** an OS sandbox — kernel-grade isolation belongs
  to containers/seccomp and the agent's own permission model.

### Changed
- **`/reopen` matches anywhere in a comment** (GH #388): "Please /reopen"
  works now; previously the comment had to *start* with the command.

## [3.8.0] — 2026-06-12

> **The Governance & Proof release.** Agents become accountable identities,
> context gets enforceable policy, and savings become auditable evidence:
> Ed25519-bound agent registry, deterministic evidence bundles with an
> offline verifier, EU AI Act / ISO 42001 / SOC 2 coverage reports, context
> policy packs, org SSO (OIDC) + org audit log, and a FinOps surface that
> exports the signed ledger to Datadog, CloudZero, Vantage and FOCUS.
> The Context OS opens up — WASM extensions, personas, plugin tools,
> Python/TS/Rust SDKs with a lockstep conformance matrix — while the
> dashboard reorganizes around the four jobs (decides · remembers · guards ·
> proves). Underneath: a P0 security hardening series, attribute-safe
> dashboard escaping, MCP failures that finally set `isError` (#389),
> a cache-aware proxy that stops defeating provider prompt caching (#534),
> and a long tail of field-reported crash and correctness fixes.

### Added
- **First-class agent identities** (GL #433, H3 Epic D):
  `core/agent_registry.rs` + `lean-ctx agent
  register/list/show/heartbeat/suspend/resume/decommission/offboard-owner/check`.
  Agents become registered identities with a mandatory human owner
  (accountability principle), Ed25519 key binding, lifecycle states
  (decommission is final and audit-closed), best-effort attestation
  (binary + role-config hash, drift surfaces on heartbeat with exit 3)
  and SPIFFE-compatible workload ids
  (`spiffe://<domain>/agent/<role>/<id>`). Owner offboarding suspends all
  of an owner's active agents in one locked transaction (SCIM hook for
  ENT-2); every transition writes tamper-evident audit entries via four
  new additive OCP Part 4 event types. Registry is cross-process safe
  (advisory file lock). Docs: `docs/enterprise/agent-identity.md` with an
  honest attestation threat model.
- **Evidence Bundle v1 + standalone offline verifier** (GL #425, H3
  Epic A): `lean-ctx audit evidence --from --to [--framework]` exports a
  deterministic ZIP (`evidence-bundle-v1` contract) — audit-chain segment,
  resolved policy pack, CGB + framework coverage reports, Ed25519-signed
  manifest; identical inputs produce byte-identical bundles. New
  independent verifier `packages/leanctx-verify` (no engine code, no
  network, 4 deps) replays the hash chain and validates signatures in
  five auditor-readable PASS/FAIL steps; mutation tests prove 1-byte
  flips, truncation and wrong keys are detected. Auditor guide:
  `docs/enterprise/reading-evidence.md`.
- **Framework compliance reports — EU AI Act, ISO 42001, SOC 2**
  (GL #424, H3 Epic A): machine-readable mapping matrices under
  `compliance/mappings/*.toml` (framework-edition pinned, semi-annual
  review cycle, explicit residual gaps) and three new builtin policy packs
  implementing the enforceable slice of each framework
  (`eu-ai-act-deployer`, `iso42001-aligned`, `soc2-context`). New
  `lean-ctx policy coverage --framework <id> [pack]` renders the
  audit-conversation artifact: every control as
  ENFORCED (live-verified against the resolved pack) / ENGINE (CI-proven
  guarantee) / GAP (documented organisational duty) — for the EU AI Act
  reference setup that is 11 of 14 controls technically enforced. Honesty
  is mechanized: every `full` claim must name a CI test
  (`tests/compliance_frameworks.rs` proves enforcement AND that violations
  are detectable — tampered logs fail verification, weak packs downgrade
  to NOT-ENFORCED), and a drift test fails the build when claims and tests
  diverge. Not legal advice; aligned ≠ certified.
- **Business plan — $149/mo flat, self-serve governance** (GL #533,
  contract `billing-plane-v3`): new tier between Team and Enterprise with
  50 flat seats, 20 GB hosted index, 10 managed connectors, private
  registry, **org SSO via OIDC** (new `sso_oidc` entitlement key, additive
  on every plan) and 365-day audit retention. Self-serve via
  `lean-ctx cloud upgrade --plan business`; existing subscribers are
  switched in place (prorated) instead of double-billed. SAML/SCIM
  (`sso_scim`) stays Enterprise. `billing-plane-v1` remains frozen — v3 is
  a purely additive catalog delta.
- **Datadog/Prometheus FinOps export — metrics contract + scrape token**
  (GL #401): `/metrics` now exposes verified ledger savings
  (`lean_ctx_ledger_tokens_saved_total`, `lean_ctx_cost_saved_usd_total`,
  30 s cache over the hash-chained ledger) and a `lean_ctx_info` series
  carrying `project`/`profile`/`agent_role`/`model`/`version` tags
  (kube-state-metrics `_info` idiom — one series per process, no
  cardinality explosion). New `LEAN_CTX_SCRAPE_TOKEN` env: a read-only
  Bearer token valid **only** for `GET /metrics`, so monitoring agents
  never hold the dashboard credential. The exposition surface is frozen in
  `docs/reference/metrics-contract.json`, enforced by
  `rust/tests/metrics_contract.rs` (update via
  `LEANCTX_UPDATE_METRICS_CONTRACT=1`). Ready-to-import Datadog assets:
  `integrations/datadog/` (OpenMetrics `conf.yaml`, Token-Economy
  dashboard, savings-drop + SLO-violation monitors), guide:
  `docs/integrations/datadog.md`.
- **`lean-ctx finops export` — CloudZero, Vantage & FOCUS cost export**
  (GL #402): turns the hash-chained savings ledger into daily showback rows
  (day × project × agent × model × tool) with the model price pinned per
  event — no pricing table to maintain, reproducible forever. Targets:
  `--target=focus` (FOCUS 1.2 CSV, all 21 Mandatory columns + 1.0 compat
  set, **passes the official FinOps Foundation `focus-validator`**),
  `--target=cbf` (CloudZero AnyCost; `--upload` posts per-month Stream
  drops with `replace_drop` = idempotent re-runs), `--target=vantage`
  (custom-provider CSV; `--upload` posts multipart, additive semantics
  documented). Savings are emitted as `Credit`/`Discount` rows with
  negative cost — Usage spend stays clean for budgets. Guide:
  `docs/integrations/finops.md`.
- **Agentless Datadog push** (GL #401): opt-in direct submit to the Datadog
  Metrics API v2 — `LEAN_CTX_DATADOG_PUSH=1` **and** `DD_API_KEY` required
  (a stray API key alone never enables egress), `DD_SITE` +
  `LEAN_CTX_DATADOG_INTERVAL_SECS` optional. Counters go out as
  per-interval deltas (baseline cycle first — lifetime totals never spike
  a graph), gauges every cycle, all series tagged
  `project/profile/agent_role/model/version`. Runs as a background loop in
  `lean-ctx dashboard`.
- **Quality loop v1 — edit failures teach mode selection** (GL #494):
  `ctx_edit` outcomes are now correlated with the last read mode of the
  file. An `old_string` miss after a compressed read (a) escalates the
  next auto read of that file to `full` (one-shot, 1 h TTL) and (b) feeds
  a per-(extension × mode) failure rate; pairs crossing the documented
  risky threshold (≥2 fails and ≥25 % fail rate, hysteresis exit <15 %)
  resolve to `full` until they recover. New resolver sources
  `edit_fail_escalation` / `edit_quality_penalty`, persisted in
  `~/.lean-ctx/edit_quality.json` (bounded, 30 d decay), surfaced in
  `ctx_metrics` under "Edit quality". Contract:
  `docs/contracts/quality-loop-v1.md`. Golden test:
  `rust/tests/quality_loop_golden.rs`.
- **ctxpkg hosted registry — client side** (GL #406): `lean-ctx pack
  publish` is real — preflight (parse, ed25519 signature, scoped-name
  check) then `PUT` to the registry at ctxpkg.com with a `ctxp_…` token
  (`--token`/`CTXPKG_TOKEN`). `lean-ctx pack install ns/name[@version]`
  resolves, downloads, verifies the artifact SHA-256 against the index,
  runs the standard import gates, re-verifies the signature locally and
  pins the result in `.lean-ctx/ctxpkg.lock`. `lean-ctx pack export
  --sign` signs bundles with an auto-managed ed25519 key
  (`~/.lean-ctx/keys/ctxpkg-ed25519.key`, 0600). Edge: account routes for
  namespace claim + publish-token lifecycle. Contract:
  `docs/contracts/ctxpkg-registry-v1.md`.
- **`lean-ctx policy coverage` — automated partial CGB assessment**
  (GL #426): statically grades a resolved policy pack against the Context
  Governance Benchmark v1.0-draft — credential fixtures vs. redaction
  patterns, regulated-identifier classes, budget cap, retention, tool
  posture, egress restriction. PASS/FAIL/INCONCLUSIVE per aspect, `--json`
  for CI gating (exit 1 on FAIL), and an explicit honesty line instead of a
  maturity grade: 7 of 32 controls are statically checkable, the rest need
  the manual assessment.
- **Context Governance Benchmark — spec + self-assessment** (GL #426): CGB
  v1.0-draft published as its own tool-neutral spec repo
  (`context-governance-benchmark`): 32 measurable controls in 6 domains
  (sensitivity/redaction, provenance, budget, audit/evidence, access
  scoping, lifecycle/retention), three levels (Basic/Hardened/Audited),
  maturity grades C1–C4, CC-BY-4.0, RFC-light governance and a CI wordlist
  lint that bans product names from normative text. LeanCTX's own honest
  self-assessment lands in `docs/compliance/cgb-self-assessment.md`:
  **C2 — Managed** (Basic 96%, Hardened 80%, Audited 50%), with declared
  gaps incl. no independent redaction verification and no one-step egress
  inventory — graded down where claims couldn't be hard-verified.
- **Dashboard: one tabbed page per job area** (GL #487, Redesign P2): the
  sidebar now carries six destinations — Home plus one entry per four-jobs
  area (Context, Memory, Protection, Proof, Project Map) — and each area is a
  single page whose views are tabs with canonical `#area/tab` deep links
  (`#context/triage`, `#proof/roi`, …). Every pre-#487 hash (`#live`,
  `#health`, `#graph`, …) still resolves and is rewritten to its canonical
  form; the last-used tab per area is remembered. New Protection area: the
  Guards tab hosts the existing reliability view, the new Risk & Policies tab
  shows live session-risk warnings (`/api/context-risk`) and the OWASP
  agentic-risk coverage map served by the new `/api/owasp` endpoint (same data
  as `lean-ctx audit`). The in-component Project-Map tab bar was removed in
  favour of the area strip.
- **Dashboard: four-jobs language pass** (GL #488, Redesign P3): onboarding
  modal tells the four-jobs story (decides · remembers · guards · proves)
  with token savings framed as the receipt, includes Protection, and the
  status bar links the estimated figure to the signed ledger in Proof.
- **Agent-task benchmark v1 harness** (GL #493): outcome evidence instead of
  token arithmetic — does lean-ctx change task success rate and cost per
  solved task? `bench/agent-task/` runs two identical Claude-Code-headless
  arms (native vs. lean-ctx MCP, fresh HOME per run, hard-pinned MCP surface
  via `--strict-mcp-config`) over a deterministic SWE-bench-Verified subset
  (sorted round-robin by repo, frozen as `tasks.lock.json`), judged by the
  official SWE-bench evaluation; usage/cost come from the runtime's own final
  report — nothing is estimated. Pre-registered protocol with numbered
  amendments (`PROTOCOL.md`), self-hashing result artifact ready for
  `ssh-keygen -Y sign`; negative results publish unchanged.
- **LoCoMo memory benchmark harness** (#291): a model-free, deterministic
  retrieval-recall benchmark over LoCoMo-style long conversations — every
  turn is stored as a memory, every question recalls top-k and is scored
  against the gold answers (answer containment, token-F1, exact match,
  recalled-context vs. full-transcript tokens). Ships a committed
  `reference-suite` with publishable numbers (`benchmark/locomo/LOCOMO.md`:
  100% containment@5 at 29.4% token reduction), a `locomo_bench` binary for
  full-dataset runs, and a CI smoke test.
- **Context policy packs** (GL #489): governance presets as code. A pack pins
  a team's context-governance expectations in reviewable TOML — default read
  mode, allowed/denied tools, named redaction regexes, audit-retention
  expectation, context-budget cap — with single inheritance (`extends`) whose
  semantics are security-first: denies and redaction accumulate down the
  chain, scalars override, allowlists replace deliberately. Five curated
  built-ins ship embedded (`baseline`, `strict-redaction`, `finance-eu`,
  `healthcare`, `open-source`); `lean-ctx policy list|show|validate` lists,
  resolves and lints packs (project pack: `.lean-ctx/policy.toml`). v1 is the
  format + tooling; runtime enforcement follows. Contract:
  `docs/contracts/context-policy-packs-v1.md`; guide:
  `docs/guides/policy-packs.md`.
- **Org audit log + retention** (GL #484): a unified, append-only governance
  audit log for orgs, surfaced to the owner at `/account/audit` with a
  filterable table and CSV export. Every governance path now writes
  best-effort events (SSO config/verify/enforce/remove/login, invite
  create/redeem/revoke) into one `org_audit_log`; the retired SSO-only table
  is migrated and dropped by an idempotent boot migration. Retention is the
  owner-plan window from the `billing-plane-v1` SSOT (Team 90 days, Enterprise
  ~10 years) and is enforced server-side both by a daily fleet sweep and on
  read, so an owner never sees a row older than they're entitled to keep. Reads
  are owner-only, cursor-paginated, and bounded. Contract:
  `docs/contracts/org-audit-log-v1.md`.
- **Org SSO (OIDC)** (GL #482): self-serve single sign-on for Team and
  Enterprise orgs. Owners configure an OIDC provider (Okta, Entra ID, Google
  Workspace, any compliant OP) under Account → Billing, prove domain ownership
  via a DNS-TXT record (checked over DNS-over-HTTPS), and optionally require
  SSO for everyone — the owner stays password-exempt (break-glass). Members
  click *Continue with SSO*, authenticate at the IdP, and land in a normal
  session with just-in-time user + org-membership provisioning. Edge runs the
  Relying Party (Authorization Code + PKCE, discovery/JWKS cache, ID-token
  verification with nonce binding and HS*/none rejection); the control plane
  is the system of record (AEAD-sealed client secret, append-only
  `billing_sso_audit`). API keys never touch URLs — a single-use 60-second
  handoff code carries the session to the browser. Contract:
  `docs/contracts/org-sso-oidc-v1.md`; setup guide:
  `docs/guides/org-sso-setup.md`.
- **Team invite links** (GL #385): owners mint one-time links
  (`leanctx.com/join/?code=…`) instead of copy-pasting tokens. Codes are
  256-bit, stored hashed, expire after 7 days, and redeem exactly once
  (atomic claim; a failed seat check releases the claim for retry). The
  public join page issues the member token once, with prefilled CLI + MCP
  setup snippets; pending invites are revocable from the dashboard like
  member tokens. Redeem endpoint is rate-limited per IP and answers every
  dead code with one neutral 404. Contract:
  `docs/contracts/team-invite-links-v1.md`.
- **Device overview** (GL #387): every authenticated Personal-Cloud push now
  carries an `X-Device-Label` header (the machine's hostname), tracked
  server-side as fire-and-forget display metadata — never auth, quota, or
  billing input. `/account/cloud` lists each machine with last sync, last
  surface and push count, plus a per-row Forget control
  (`GET/DELETE /api/account/devices`). Contract:
  `docs/contracts/device-overview-v1.md`.
- **Supporters wall + dashboard badge** (GL #393): the public supporters wall
  is live end-to-end — Stripe checkout fields (display name, message, opt-in)
  are captured idempotently by the billing webhook, clamped to 60/140 chars,
  profanity-gated and served via the public `GET /api/supporters` edge;
  `leanctx.com/support/` renders the wall client-side (plaintext-only,
  tier pills, newest first). Cancelling the subscription hides the entry on
  the next `subscription.deleted` webhook, and an internal-key moderation API
  (`GET …/supporters/moderation`, `PATCH …/supporters/{id}`) provides an
  audited kill-switch. Locally, the dashboard's support bar now swaps its ask
  for a thank-you when the machine is linked to a supporting account — served
  by the new `/api/billing-badge` endpoint from the cached plan only (no
  network, purely cosmetic, never gates a local capability).
- **Email digests** (GL #386): the cloud server now sends a monthly Pro digest
  (tokens saved, agent actions, sessions, CEP score — from synced snapshots)
  and a weekly Team digest (net tokens, USD, actions, top model/tool — from
  the hosted server's savings summary). Idempotent per period with automatic
  catch-up and SMTP retry; silent when a period has no real data. Every email
  carries a one-click, login-free unsubscribe (hashed, rotating tokens);
  `GET/PUT /api/account/digest` exposes the preference to the dashboard.
  Contract: `docs/contracts/email-digest-v1.md`. Cloud-server CORS now allows
  `PUT`/`PATCH` (digest toggle + team settings).
- **Weekly team-ROI webhook** (GL #388): team servers post a weekly savings
  summary (net tokens, USD, measured actions, 7-day window, top mover, top
  model/tool) to Slack, Discord, or any JSON webhook. Configured via
  `roiWebhookUrl` in `team.json` (https-only, validated at boot) or self-serve
  through the team dashboard's new Integrations card
  (`PUT /api/account/team/settings` → control plane re-renders the config).
  Posts once per ISO week with retry-on-failure; weeks without reported data
  stay silent — no synthetic numbers. Payload shape auto-detects the vendor
  (Slack `text`, Discord `content`, generic both).
- **Per-member savings drilldown** (GL #389): new audit-scoped team-server
  endpoint `GET /v1/savings/member/{signer}` — one member's latest totals,
  model/tool breakdowns and a member-only 90-day cumulative series (carry-
  forward replay of that signer's snapshot history). Signer ids are validated
  against `[A-Za-z0-9_-]{1,64}` before any filesystem access; unknown signers
  are a clean 404. Proxied through the control plane
  (`/api/billing/team/{id}/savings/member/{signer}`) and the account edge
  (`/api/account/team/savings/member/{signer}`); the team dashboard's member
  rows are now clickable and open an inline drilldown panel (own series chart,
  top models, top tools). Contract: `docs/contracts/billing-plane-v2.md`.
- **model2vec static-embedding support** (GL #452): the embedding engine now
  drives EmbeddingBag-topology ONNX graphs (model2vec exports like
  `hf:minishlab/potion-base-8M`) next to classic transformers. Topology is
  detected from the graph's input signature (`input_ids` + `offsets`) at load
  time; the adapter feeds flat ids + batch offsets, skips mean-pooling (the
  graph pools internally) and probes dimensions off the rank-2 output. ~500x
  faster inference at ~30 MB — built for initial indexing of large repos and
  semantic search on weak hardware. Live-verified end-to-end (256d, L2-normed,
  semantic sanity); guide section in `docs/guides/custom-embeddings.md`.
- **Minimal org model on the cloud plane** (GL #468): team checkouts now
  create an organization with the buyer as owner; memberships inherit the
  owners' best active plan at the entitlements edge (never downgrading a
  personal plan) and `/api/account/entitlements` carries the org
  `{id, name, role}` for the dashboard's new organization section.
- **Zero-knowledge Personal Cloud vaults** (GL #467): knowledge *and* gotchas
  now sync as client-side-encrypted blobs (XChaCha20-Poly1305, domain-separated
  HKDF keys `knowledge-vault-v1` / `gotcha-vault-v1` derived from the account
  API key the server only stores hashed). The first vault push purges the
  account's legacy plaintext rows; dashboards read the client-declared
  `entry_count` from blob metadata. Contract:
  `docs/contracts/personal-cloud-encryption-v1.md`.
- **Team server billing-plane endpoints** (GL #463): `GET /v1/storage` reports
  the hosted workspace footprint (allocated-blocks sizing, hard links counted
  once, symlinks never followed, 60 s cache; `camelCase` per
  `billing-plane-v2`) and `GET /v1/usage` serves the unified snapshot —
  signed-ledger savings roll-up, measured `toolCalls`, and a `snake_case`
  `storage` block. Both audit-scope-gated like `/v1/metrics`; quota via
  `LEANCTX_TEAM_STORAGE_QUOTA_BYTES`. Unblocks the control plane's hourly
  Stripe metering job and threshold mails against real team servers.
- **`lean-ctx doctor --migrate-check`** (GL #396): v1.0 migration-readiness
  audit — config.toml keys validated against the schema (free-form sections
  like `ide_paths` respected), active deprecations, data-layout writability,
  frozen-contract set. `--json` for fleet rollouts; exit 0 = "ready for 1.0".
  Plus the launch program docs: `docs/releases/v1.0-runbook.md` (RC/freeze/
  bug-bash/rollback/launch-day plan), `docs/releases/migration-1.0.md`
  (zero-breaking-changes guide) and `marketing/launch-v1/` (Show HN + Product
  Hunt drafts with tokbench-informed Q&A prep).
- **Custom embedding models** (GL #397, upstream #328): `ctx_semantic_search` can now
  load any HuggingFace repo with an ONNX export via `model = "hf:org/repo[@revision]"`
  (`[embedding]` in `config.toml` or `LEAN_CTX_EMBEDDING_MODEL`). Includes revision
  pinning with an unpinned-warning, automatic dimension probing from the ONNX graph
  (`[embedding].dimensions` as declared fallback), per-repo+revision storage isolation,
  and SHA-256 lockfiles (`model.lock.json`, trust-on-first-use) that reject silent
  upstream content swaps. Model or revision changes trigger the established one-shot
  re-index. New guide: `docs/guides/custom-embeddings.md`.
- **SDK conformance matrix** (GL #395): all three first-party SDKs (`leanctx`
  on PyPI, `@leanctx/sdk` on npm, `lean-ctx-client` on crates.io) now cover the
  **entire** public `/v1` surface — added `context_summary`, `search_events`,
  `event_lineage` and `metrics` to every client. The shared conformance kit
  grows from 4 to 14 lockstep checks, including two drift gates:
  `route_coverage` (a server route without an SDK method fails within one CI
  run) and `engine_compat` (SDK declares its supported `http_mcp` contract
  versions). New CI job `sdk-conformance` runs all three kits against a real
  `lean-ctx serve` build via `scripts/sdk-conformance.sh` and publishes
  `docs/reference/sdk-conformance-matrix.md` (current state: 3/3 SDKs,
  14/14 checks PASS). SDK majors follow the engine contract major.
  Completing the audit: live adapter smoke tests (OpenAI/LangChain/
  LlamaIndex/CrewAI run one real tool round trip each against the live
  server, optional frameworks skip cleanly) and a release gate
  (`scripts/check-sdk-versions.py`, first job of the release workflow):
  an engine release fails hard when an SDK cannot speak the shipped
  `http_mcp` contract version, and warns on >1 minor SDK-family drift.
- **Contract freeze & SemVer/deprecation policy** (GL #394): all 29 contract
  docs are now classified `frozen` / `stable` / `experimental` in a stability
  matrix (CONTRACTS.md, SSOT `core/contracts.rs::contract_docs()`). Two new CI
  gates enforce the freeze: `tests/contracts_frozen.rs` (every doc classified;
  frozen docs content-hashed against `docs/contracts/frozen-hashes.json` —
  semantic changes must land as a new `-v2.md` file) and
  `tests/openapi_stability.rs` (public `/v1` surface vs.
  `docs/reference/openapi-v1.snapshot.json`; additive diffs pass, removed or
  mutated routes fail). `GET /v1/capabilities` additionally returns a
  `contract_status` map so clients can verify stability guarantees at runtime.
  The deprecation register `DEPRECATIONS.toml` (compiled into the binary,
  ≥ 2 minor releases between announcement and removal) feeds a new
  `lean-ctx doctor` check that warns about every deprecation shipping in the
  installed build.
- **Personal-Cloud auto-push** (GL #384): opt-in `lean-ctx cloud autosync on`
  pushes the Pro surfaces (knowledge, commands, CEP, gotchas, buddy, feedback)
  silently once per day from the background task — offline keeps the day's
  slot open for retry, a Pro gate (402) consumes it quietly (no error spam on
  Free accounts).
- **Hosted Personal Index for Pro** (GL #392): `lean-ctx sync index
  push|pull|status` syncs the project's retrieval index (BM25 + embeddings)
  across devices — a fresh machine gets working `ctx_semantic_search` without
  a local re-index. Bundles are encrypted client-side (XChaCha20-Poly1305;
  key HKDF-derived from the account API key, which the backend stores only as
  a hash): the server holds ciphertext it cannot read. Per-account quota from
  the plan's `hosted_index_mb` (Pro: 1 GB; open self-hosted deployments:
  1 GB default), display-first — an over-quota push warns and blocks, it
  never bills. New backend routes `PUT/GET/DELETE /api/sync/index/{project}`
  + `GET /api/sync/index`; the Personal-Cloud dashboard payload gains a
  `hosted_index` block (projects, used bytes, quota). The local index is
  never gated (Local-Free Invariant; `tests/local_free_invariant.rs`).
  Contract: `docs/contracts/hosted-personal-index-v1.md`.
- **Hosted-index SLO gate** (GL #391): the team server now measures every
  `/v1` request in an outermost middleware and derives the three GA-gate
  signals — rolling p50/p95/p99 latency, availability (non-5xx share over the
  last 4096 requests) and index freshness (seconds since the last successful
  Index-scoped tool call). Exposed via `/v1/metrics` (new `slo` block) and
  `/v1/metrics?format=prometheus` (`leanctx_team_*` series for Datadog/
  Prometheus scrape agents). New CLI: `lean-ctx team slo-report --server
  <url> --token <token> [--json]` renders the gate and exits non-zero on
  violation (CI-friendly). SLO definitions ship in
  `docs/examples/team-slos.toml`; the SLO engine understands the new metrics
  `team_query_p95_ms`, `team_availability_pct`, `team_index_lag_seconds`.
  Runbook: `docs/guides/hosted-index-slo.md`.
- **Accuracy conformance checks for lossy read modes** (P1, GL #441):
  `lean-ctx conformance` now verifies structural invariants of `map`,
  `signatures`, `aggressive` and `entropy` against a fixed Rust fixture —
  determinism, symbol retention, body stripping, and real compression. CI
  gates on regressions in the modes agents rely on for correctness.
- **Honest metering on phase-isolated / non-caching workloads** (#361): `lean-ctx
  gain` now states its denominator — savings are compression on
  *lean-ctx-touched traffic*, not the full provider bill — via a **Methodology**
  line and a new `injected_overhead_tokens_per_turn` field in `gain --json`
  (`net bill impact = tokens_saved − injected_overhead_tokens_per_turn × turns`).
  New `core::context_overhead` measures the fixed per-turn prefix lean-ctx
  injects (tool schemas + server instructions + rules block). A new
  `rules_injection = "off"` (also `none`/`disabled`) writes **no** rules file —
  for hosts that supply their own steering, or phase-isolated/non-caching
  harnesses where the injected prefix is pure re-billed overhead. The
  performance-tuning journey gains a "workload fit" section documenting the proxy
  as the way to reach tool output the `ctx_*` tools can't wrap. Prompted by an
  independent, reproducible external benchmark.
- **Team RBAC roles** (Commercial Plane, EPIC 13.2): a `TeamRole`
  (`viewer`/`member`/`admin`/`owner`) layer over the existing fine-grained
  `TeamScope`s. A token's effective scopes are `scopes ∪ role.scopes()`, enforced
  by the unchanged team middleware (zero new enforcement paths). Roles are
  monotonic (`viewer ⊆ member ⊆ admin = owner`). New CLI:
  `lean-ctx team token create --role <role>` (still supports `--scopes`, or both).
  Additive / Team-Cloud only — never gates local. SSO/SCIM, org-shared knowledge
  graph, and audit-retention dashboards build on this and remain tracked on the
  commercial plane. Contract updated: `docs/contracts/team-server-contract-v1.md`.
- **Billing plane: real plans + usage metering** (Commercial Plane, EPIC 13.6):
  new `core::billing` turns the upgrade flow into real plans (`free`/`team`/
  `enterprise`) with explicit `Entitlements`, plus usage-based metering derived
  **read-only** from the Ed25519-signed savings ledger (EPIC 12.20). `Usage` is
  privacy-preserving and only billable on a signed + intact chain. Crucially,
  `entitlement_allows` upholds the Local-Free Invariant — every local feature is
  allowed on **every** plan (incl. Free); the local binary has **no entitlement
  checks** (enforced by `tests/local_free_invariant.rs`). New CLI:
  `lean-ctx billing <plans|entitlements|usage> [--json]` (informational only).
  Quota semantics disambiguated: `0` = none, `UNBOUNDED` = unlimited. Checkout/
  provisioning are documented as a hosted control-plane concern (no fakes).
  Contract: `docs/contracts/billing-plane-v1.md`.
- **WASM extension runtime** (Context OS, EPIC 12.8 + 12.10): a sandboxed,
  language-independent way to contribute **compressors** and **context providers**
  as plain `.wasm` modules — no recompile of lean-ctx. Behind the off-by-default
  `wasm` Cargo feature (`features.wasm_runtime` in `/v1/capabilities`), upholding
  the Local-Free Invariant (free, compile-optional). Uniform ABI v1 (`memory` +
  `alloc(i32)->i32` + `entry(i32,i32,i32)->i64` packed `ptr/len`); guests run
  against an **empty linker** (no syscalls/network/fs/clock — sandboxed by the
  runtime itself) with a fresh `Store` per call for thread-safety + determinism.
  `WasmCompressor` registers as a first-class compressor (host-enforced byte
  budget, graceful fallback on traps, conformance-checked); `WasmProvider`
  registers as a first-class `ContextProvider` (lenient result-JSON mapping).
  Opt-in discovery from `LEAN_CTX_WASM_DIR` (`*.wasm` compressors; `*.wasm` +
  `<stem>.provider.json` sidecar providers). Contract: `docs/contracts/wasm-abi-v1.md`.
- **Context OS guide + non-coding cookbook** (Context OS, EPIC 12.18): `docs/context-os/guide.md` maps the whole platform — principles (Local-Free Invariant), architecture, capability discovery, the four ways to build your own tool (SDK / plugin tool / hook / extension), ingestion+extractors+personas, the savings→ROI substrate, and the plane model. `docs/context-os/cookbook-non-coding.md` adds four runnable, verified recipes (lead-gen, research, support, data-analysis) plus a custom-vertical template, all using real personas/extractors/SDKs/adapters.
- **Framework adapters** (Context OS, EPIC 12.6): `leanctx.adapters` exposes the lean-ctx tool surface to popular agent frameworks — OpenAI function calling (`to_openai_tools` / `run_openai_tool_call`, a pure transform with no extra dep), LangChain (`to_langchain_tools`), LlamaIndex (`to_llamaindex_tools`), and CrewAI (`to_crewai_tools`). Each framework is an optional, lazily-imported dependency (`leanctx[langchain|llamaindex|crewai]`); all adapters share one tool normalizer and the same `call_tool_text` path so they behave identically. Tested with/without each framework installed.
- **Python SDK** (Context OS, EPIC 12.4): new `leanctx` package (`clients/python/`) — a thin, **standard-library-only** client (`urllib`, zero runtime deps) for the HTTP `/v1` contract, mirroring the TS/Rust SDKs: `health`, `manifest`, `capabilities`, `openapi`, `list_tools`, `call_tool`/`call_tool_text`, and `subscribe_events` (SSE). Structured errors (`LeanCtxConfigError`/`TransportError`/`HTTPError`) and the shared `run_conformance` kit (lockstep with the TS SDK). Ships a README, `pyproject.toml`, in-process HTTP-server tests, and a `python-sdk` CI job.
- **TypeScript SDK GA + shared conformance kit** (Context OS, EPIC 12.5): `@leanctx/sdk` gains `capabilities()` and `openapi()` for full `/v1` discovery parity, a typed `CapabilitiesV1`, and a new `runConformance(client)` kit that returns a client-side scorecard (health, capabilities shape, OpenAPI shape, tools listing). The kit mirrors the server-side `lean-ctx conformance` and is kept in lockstep with the Python SDK so every client proves the same contract. Adds a README and tests.
- **Non-code compression tuning** (Context OS, EPIC 12.14): two new compressors tuned for non-code corpora, registered in `extension-registry-v1` — `prose` (collapse blank-line runs, strip/collapse intra-line whitespace, drop adjacent duplicate lines) and `markdown` (everything `prose` does plus strip HTML comments, drop image/badge syntax, and rewrite `[text](url)` links to their visible text). Both are deterministic and honor a hard byte budget (conformance-checked). Non-coding personas now default to them: `research` → `markdown`; `lead-gen`/`support` → `prose`.
- **Format extractors & chunkers** (Context OS, EPIC 12.13): new `core::extractors` (`extractors-v1`) turns non-code documents/data into clean LLM text + structure-aware chunks. JSON (per array element / object entry), CSV/TSV (RFC-4180-aware, header-prefixed row groups), EML (salient headers + body, `text/plain` from multipart), HTML (rendered Markdown paragraphs, reusing `web::html_to_text`), and PDF (reusing `web::pdf`) — with a verbatim paragraph fallback for plain text. Every extractor is total/graceful (never panics, non-empty input always yields ≥1 non-empty chunk, deterministic). The text chunkers (csv/json/eml/html) register into `extension-registry-v1`, so they surface in `/v1/capabilities` and are conformance-checked.
- **Conformance & reproducibility scorecard** (Context OS, EPIC 12.17): new `core::conformance` + `lean-ctx conformance [--json]` produce a `Scorecard` proving an instance honors its own contracts. Checks span three categories — `contracts` (all machine-verified versions present), `reproducibility` (`/v1/capabilities` and `/v1/openapi.json` are byte-deterministic), and `extensions` (every registered compressor/chunker/read-mode satisfies determinism, byte-budget, UTF-8, and coverage invariants — built-in *and* extension-provided). Exits non-zero on failure; gated in CI via `tests/conformance_suite.rs`. Contract: `conformance-v1`.
- **Extension trust & sandbox model** (Context OS, EPIC 12.3): every plugin subprocess (hooks + manifest tools) now runs under a `SandboxPolicy` derived from a new `[trust]` manifest section (`extension-trust-v1`). **Least privilege by default**: the child runs with a scrubbed environment (fixed allowlist — host secrets in env never leak) and a working-directory jail, on top of the existing per-call timeout. Plugins declare capabilities (`network`, `fs_write` = consent surface, surfaced in `/v1/capabilities`; `env_passthrough` = opt out of env scrubbing). Unknown permissions are a fail-closed manifest error. Declared permissions appear per plugin under `extensions.plugins[].permissions`.
- **ROI / metering substrate** (Context OS, EPIC 12.20): new `core::savings_ledger::roi` derives a `RoiReport` strictly from the **signed** savings batch (`BatchTotals` + committed chain head + Ed25519 signature) — adding derived metering metrics (net tokens, USD, per-event averages, top models/tools) and provenance (`chain_valid`, `signed`, signer key). This is the minimal, privacy-preserving aggregate the Cloud plane meters on: no raw events, paths, prompts, or code — only numbers and hashes — and it is read-only w.r.t. the local ledger. Exposed via `lean-ctx savings roi [--json]`.
- **Plane separation + Local-Free-Invariant CI gate** (Context OS, EPIC 12.19): the Personal (local) plane is now a documented, machine-checked boundary. `core::server_capabilities` classifies every feature flag as `LOCAL_ALWAYS_ON`, `LOCAL_OPTIONAL` (compile-only), or `COMMERCIAL_PLANE` (additive team/cloud). A CI conformance test (`tests/local_free_invariant.rs`) fails the build if the default plane isn't `personal`, any local capability isn't unconditionally free, the planes overlap, or a local capability reacts to a `LEAN_CTX_LICENSE`/`LEAN_CTX_PLAN`/`LEAN_CTX_ACCOUNT` env var; a unit test fails if a new feature flag is added without classification. Contract: `docs/contracts/local-free-invariant-v1.md`.
- **Built-in personas + persona-aware intent/terse** (Context OS, EPIC 12.16): ships four non-coding presets alongside `coding` — `research`, `lead-gen` (alias `sales`), `support`, `data-analysis` — each with its own tool surface, read-mode/compressor/chunker defaults, intent taxonomy, and sensitivity floor. The terse agent prompt is now persona-parametrized: non-coding personas append a domain vocabulary block + their intent list, while the `coding` persona leaves the prompt byte-for-byte unchanged (no regression). Available presets surface under `presets` in `GET /v1/capabilities`.
- **Context persona model** (Context OS, EPIC 12.15): new `core::persona` (`persona-spec-v1`) — a declarative bundle that shapes the entire context surface for a domain (tool surface, default read-mode, compressor/chunker, intent taxonomy, sensitivity floor), not just coding. Personas are selectable via `LEAN_CTX_PERSONA` or `persona = "…"` in config, resolved against built-in presets then `<personas_dir>/<name>.toml` (override `LEAN_CTX_PERSONAS_DIR`). The built-in `coding` persona reproduces today's defaults — the tool surface still resolves to `power` when nothing is pinned (no regression), and explicit tool-profile settings always win. The active persona surfaces at `GET /v1/capabilities` under `server.persona`, with available presets under `presets`. Contract: `docs/contracts/persona-spec-v1.md`.
- **Native tool registration without forking** (Context OS, EPIC 12.11): a plugin can declare `[[tools]]` in its manifest (name, description, command, timeout, JSON input schema). Enabled plugins' tools are discovered (`PluginManager::tool_specs`), adapted into native MCP tools (`registered::plugin_tool::PluginTool`), and registered dynamically in `build_registry()` — no fork, no code edit. They surface in `GET /v1/capabilities` under `extensions.tools` and in the agent's tool list, and run sandboxed through the shared subprocess runner (piped stdio, `LEAN_CTX_PLUGIN_DIR`/`LEAN_CTX_TOOL` env, bounded per-tool timeout). A plugin tool whose name collides with a native tool is skipped (native wins, so a plugin can never shadow core behavior). The hook executor and tool invocation now share one `run_subprocess` runner; an end-to-end test proves discover → register → invoke.
- **Pluggable read-modes / compressors / chunkers** (Context OS, EPIC 12.9): new `core::extension_registry` (`extension-registry-v1`) exposes stable, object-safe traits — `ReadMode`, `Compressor`, `Chunker` — backed by a process-global registry seeded with real built-ins (`full` read-mode; `identity`/`whitespace` compressors; `lines`/`paragraph` chunkers) registered through the exact same public API extensions use (no special-casing). Extensions register custom transforms by name; the live registry contents now surface under `extensions.{read_modes,compressors,chunkers}` in `GET /v1/capabilities`.
- **Generic ingestion front-door** (Context OS, EPIC 12.12): intake is no longer gated by `is_code_file`. A new `core::ingestion` front-door (`ingestion-spec-v1`) classifies every path by content *kind* — `Code` / `Document` / `Data` / `Text` / `Binary` — via an extension fast-path plus a bounded binary sniff (NUL/control-byte ratio over the first 8 KB). So any text corpus (markdown, csv, json, yaml, html, email, logs, transcripts, even unknown-but-textual files) now reaches BM25/semantic/knowledge — not just source code. Genuine binaries (images, media, archives, compiled artifacts, and binary documents like PDF/DOCX whose extractors arrive in 12.13) are excluded. The duplicate `is_code_file` in the CLI indexer is removed; `bm25_index::is_code_file` remains the single canonical code detector, now one input to the front-door. Code repositories are fully backward-compatible — everything that indexed before still indexes.
- **`lean-ctx-client` Rust crate — the embedding boundary** (Context OS, EPIC 12.2): a thin, stable HTTP client for the `/v1` contract so any program (an agent harness, a lead-gen worker, a research bot) can integrate lean-ctx **over the process boundary without linking the engine**. It is the Rust counterpart of the TypeScript SDK (`cookbook/sdk`) and speaks the same versioned contract: `health`, `manifest`, `capabilities`, `openapi.json`, paginated `tools`, `tools/call` (raw result + flattened text), and `events` as a blocking SSE iterator. Open-ended documents are returned as `serde_json::Value` so new server keys never break a client build; errors carry the stable `error_code` (not the human message) for branching. The crate is deliberately decoupled — it does **not** depend on the engine crate, re-exports no internals, and documents its non-goals (full-crate linking stays unsupported; integration = process boundary). One small dependency (`ureq`), blocking by design, `#![forbid(unsafe_code)]`, and covered by a dedicated CI job (fmt + clippy `-D warnings` + tests against a real localhost HTTP server + docs). Lives at `clients/rust/lean-ctx-client`.
- **Plugin hooks are now live in the core pipeline** (Context OS, EPIC 12.7): the plugin seam that previously only existed in `PluginManager` is wired into the running server, so a third-party plugin can finally *observe the engine without forking it*. `pre_read` and `post_compress` fire around the central `ctx_read` choke point (carrying the path and the realized `original → compressed` token counts), and `on_session_start` fires once per server process (stdio + HTTP + daemon). Every firing goes through a **zero-cost guard** (`PluginManager::has_listener` / `notify`): with no plugin declaring a hook — the default — the hot read path allocates nothing and spawns no thread, so users without plugins pay exactly zero. Hooks run in the background with per-plugin error isolation and a per-hook timeout (a failing or slow plugin can never block or corrupt a read). The registry is initialized exactly once per process via the existing idempotent `init()`. An end-to-end test proves a real `ctx_read` triggers an installed plugin's `pre_read` hook, and a new `LEAN_CTX_PLUGINS_DIR` override lets containers/CI/tests point the registry at an isolated plugins root (distinct from the per-hook `LEAN_CTX_PLUGIN_DIR` the executor exports to a plugin's own child process). All five hook points are now live: `on_session_start`/`on_session_end` bracket each server process (the end hook fires **synchronously** at shutdown so it always runs before exit), `pre_read`/`post_compress` wrap reads, and `on_knowledge_update` fires when `ctx_knowledge(action="remember")` writes a fact (carrying `category:key`).
- **OpenAPI spec — `GET /v1/openapi.json`** (Context OS, EPIC 12.1): the public `/v1` surface is now described by an OpenAPI 3.0.3 document generated from a single in-code endpoint inventory (`core::openapi`), so SDK/codegen tooling in any language can consume it. A drift test (`openapi_contract_up_to_date`) binds the inventory to the Endpoints table in `http-mcp-contract-v1.md`, so a new public route must update both — code and docs can't diverge. Internal/experimental routes (agent registry, A2A, `.well-known`, shutdown) are intentionally excluded from the published spec.
- **Capabilities discovery — `GET /v1/capabilities`** (Context OS, EPIC 12.1): a runtime discovery document so any client — in any language — can learn what a lean-ctx instance supports and branch on *real* features instead of making trial calls. Reports the contract version, server name/version, deployment `plane` (`personal`/`team`/`cloud`), wire `transports`, built-in `presets` (personas), `read_modes`, the `tools` surface, a `features` map (always-on capabilities plus compiled Cargo features like `semantic_search`/`team_server`/`cloud_server`), runtime-discovered `extensions` (plugins), and all machine-verified `contracts` versions in one place — no secrets ever included. Versioned by `capabilities-contract-v1` (`CAPABILITIES_CONTRACT_VERSION`); the documented key set is bound to the code SSOT (`core::server_capabilities`) by a drift test, and a formal `/v1` deprecation policy is documented alongside the contract.
- **MCP Tool-Catalog Gateway — `ctx_tools`** (the answer to "more tools → less adoption"): lean-ctx can now sit in front of any number of **downstream MCP servers** and expose them through a single meta-tool instead of injecting every downstream schema into the system prompt. The agent calls `ctx_tools find` with a natural-language need; the gateway aggregates the downstream catalogs (TTL-cached), ranks them with the same BM25 engine as `ctx_search`, and returns a top-N **ChoiceCard** shortlist (`server::tool` + one-line description + key params). `ctx_tools call` then **proxies** the real call to the owning server and returns its (firewall- and sensitivity-filtered) result. Net effect: *unlimited* downstream tools at roughly constant context cost. Transports: local **stdio** (spawns the server as a child process) and remote **streamable HTTP** (with custom headers / bearer auth) — built on the official `rmcp` client, no bespoke JSON-RPC. **Global-only** config and **off by default** (`[gateway]` / `[[gateway.servers]]`); spawning downstream processes can never be enabled by an untrusted project. Granular tool surface → 72.
- **Per-item sensitivity policy floor** (`[sensitivity]`): classify every context item as `public < internal < confidential < secret` (path heuristics + secret/PII detection incl. Luhn-validated cards and ISO-7064 IBANs) and enforce a uniform **floor** before content ever reaches the model — `redact` (mask the spans) or `drop` (withhold the item). Applied uniformly to tool outputs and knowledge facts. Global-only and **off by default**.
- **Reproducible scorecard — `lean-ctx benchmark scorecard`**: a deterministic, machine-independent report of compression savings, retrieval recall/MRR, and latency over a synthetic, byte-reproducible corpus. The JSON and human output embed a `determinism_digest`, so two runs of the same code anywhere produce the same fingerprint — the artifact is self-verifying. Wired into CI as an uploaded artifact.

### Changed
- **Parallel dashboard tracks consolidated** (GL #476–#479, #486, #490): the
  four-jobs IA from the redesign epic and the incremental UX/data passes that
  shipped in parallel now live on one branch. The epic layout wins (slim Home,
  Proof group with ROI & Plan + Trends, Simple = Home only); the data passes
  win correctness and language — relative search scores (top hit = 100%),
  the verified-bridge line in the Home hero (estimated ⇄ signed ledger),
  Context Triage / Context Contents / Episodes labels, estimate-methodology
  tooltips, per-task episode metrics, the dead Symbols signature column
  removed and vendor noise filtered from the Compression Lab. Search keeps
  the inline ±12-line preview and gains an "Open in Lab →" handoff. On the
  Rust side `ctx_search` now returns a `SearchOutcome` that separates the
  modeled native-grep baseline (estimated stats) from raw observed tokens
  (verified ledger), so the two series can never cross-contaminate.
- **Four-jobs cockpit navigation + slim Home** (GL #470/#486, phase 1): the
  sidebar now tells the same story as the website — Context *(decides what
  agents read)*, Memory *(remembers what agents learn)*, Proof *(proves what
  you save)* and Project Map *(understands your codebase)* — instead of 17
  flat entries. Simple mode is the 5-second answer: Home only. Home itself
  slimmed down to status strip + receipt + gauge/triage + one trend + top-3
  commands (expandable); the cost-analysis card moved to ROI & Plan (labelled
  as the estimated, all-time view next to the verified-ledger methodology)
  and the MCP-vs-shell / task-breakdown doughnuts moved to Trends. Every view
  stays reachable via Advanced mode, deep links and the command palette.
- **Large modules split by domain** (P1, GL #439, #440):
  `cli/dispatch/analytics.rs` (1685 LOC) → `analytics/{gain,savings,billing,graph}`,
  `core/stats/format.rs` (1532) → `format/{util,cep,dashboard,views}`,
  `rules_inject.rs` (1542) → `rules_inject/{content,targets,detect,write,skills}`.
  No behavior change; entry-point visibility narrowed to the dispatch layer.

### Fixed
- **Scorecard determinism restored** (#211 contract): benchmark `entropy`
  numbers fed the scorecard's reproducibility digest through the regular
  compression path, whose opportunistic semantic redundancy filter (#544)
  kicks in as soon as the shared embedding engine finishes loading — two
  runs in the same process could disagree (e.g. `entropy=0.00` vs `57.29`
  on the small corpus). Benchmarks now pin the filter off via the new
  `entropy_compress_deterministic`, keeping the digest machine-independent
  (and cutting the determinism test from 25 min to 4 s).
- **Signed artifacts always embed the key that actually signed them**: every
  signer that embeds its public key next to the signature (handoff transfer
  bundles, evidence bundles, `wrapped publish`) previously resolved the
  keypair twice — once to sign, once to read the public key. If the key store
  moved or the key was regenerated between the two reads (concurrent
  data-dir changes, parallel processes), the artifact carried a public key
  that could never verify its own signature. New atomic
  `agent_identity::sign_with_public_key` / `sign_bytes_with` APIs resolve the
  keypair exactly once; all three call sites migrated.
  (pipeline red since the #551 efficiency program landed): the
  `try_shared_engine_returns_none_when_not_initialized` unit test asserted
  on the process-global `SHARED_ENGINE` `OnceLock` while the new #551
  background activation (triggered by any sibling test touching entropy
  compression) could load — and in CI even *download* — the model
  mid-suite. The test now lives in its own integration-test binary
  (`tests/embeddings_shared_engine.rs`, fresh process = deterministic),
  `ensure_engine_background()` is a no-op under `cfg!(test)`, and CI
  exports `LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD=0` so the suite is hermetic.
  Also un-sticks the Coverage job: the silent engine load made
  `run_project_benchmark("src")` exceed tarpaulin's 180 s timeout.
- **`ctx_shell`/`ctx_execute` failures now set MCP `isError` +
  `structuredContent`** (GitHub #389): every tool call returned
  `CallToolResult::success` regardless of the shell exit code — MCP clients
  (OpenCode guards, Claude Code, Cursor) had no programmatic way to detect
  failures and were forced to regex-parse the `[exit:N]` text footer. A new
  `ShellOutcome` (Exit(code) | Blocked) now flows from the shell tools
  through dispatch into the MCP result: non-zero exit sets
  `isError: true` + `structuredContent: {"exitCode": N}`, allowlist/
  validation rejections set `isError: true` + `{"blocked": true}`.
  Covered end-to-end: the degraded session-lock path (which previously
  even dropped the exit footer), the auto-checkpoint early return, the
  reference-store substitution, `ctx_call` chaining, and `ctx_execute`
  (single/batch — first failing task fails the batch — and file
  preconditions). Exit 0 stays byte-identical (no metadata churn).
- **OpenClaw: `setup --auto` re-injected the legacy `mcpServers` key and
  broke 2026.6.1+ hot-reload** (GitHub #390): OpenClaw moved to a nested
  `mcp.servers` schema with strict validation; the editor-registry writer
  still wrote top-level camelCase `mcpServers`, so every watchdog tick
  produced `config reload skipped (invalid config): Unrecognized key` —
  with gateway-down risk on restart if the stale block won. OpenClaw now
  has a dedicated `ConfigType::OpenClaw` writer: it detects the version
  via `meta.lastTouchedVersion` (>= 2026.6.1 or an existing `mcp.servers`
  block → nested schema; older → legacy camelCase), migrates our stale
  `mcpServers.lean-ctx` entry away (dropping the key when empty, foreign
  entries preserved), and is strictly idempotent — watchdog re-runs leave
  the file byte-identical (verified via mtime). `init --agent openclaw`,
  `setup --auto`, `lean-ctx doctor` (flags stale legacy blocks) and both
  uninstall paths (editor-registry + textual `lean-ctx uninstall`, which
  now also strips an emptied `mcpServers {}` leftover) share the same
  schema logic. Invalid JSON is never text-injected for openclaw.json —
  a malformed write would take the gateway down.
- **Shell parser: `>|` noclobber redirect treated as a pipe** (GitHub #387):
  `date --fsdfs >| out 2>&1` split at the `|`, so the redirect target
  (`out`) was checked against the shell allowlist as a command and
  blocked. The segment splitter now recognises `>|` as a redirect
  operator; file-write targets are never allowlist-checked.
- **`gain --deep` crash on multibyte paths/agent ids** (GitHub #386):
  every display truncation helper (`ctx_gain::truncate_str` /
  `shorten_path`, `stats::format::truncate_cmd`, `ctx_architecture`
  hotspot paths) sliced at byte offsets and panicked mid-codepoint for
  umlauts/CJK/emoji; one helper could also underflow for tiny widths.
  All cuts are now char-boundary-safe (swept 0..=len+2 in tests).
- **`report-issue` now embeds the crash log** (GitHub #386 follow-up):
  the last 3 entries of `<data_dir>/logs/crash.log` (location, payload,
  truncated backtrace) ship with every report, so panic reports are
  actionable instead of arriving empty.
- **SIGABRT coredumps from the panic hook itself** (GitHub #378): the
  process-wide panic hook used `eprintln!`, which panics on I/O errors —
  when a background worker's stderr was gone (terminal closed → EPIPE),
  any ordinary panic became a double panic and the runtime aborted the
  whole process (38 coredumps reported). The hook now writes its message
  best-effort (`write_all`, errors ignored) and wraps the crash-log write
  in `catch_unwind`; a panic can never escalate to SIGABRT through the
  hook anymore.
- **MCP token footprint: installers no longer force the full toolset**
  (GitHub #385): every generated MCP config carried
  `LEAN_CTX_FULL_TOOLS=1`, advertising 69+ tool schemas (~15k tokens)
  to the client on every turn — lean-ctx showed up as one of the biggest
  token consumers in users' own usage breakdowns. New installs/refreshes
  now use the core toolset (13 tools + `ctx_call`/`ctx_expand` for
  on-demand access); opt back in via `tool_profile = "power"` in
  config.toml or `LEAN_CTX_FULL_TOOLS=1` in the server env.
- **Pi: stale `~/.pi/agent/mcp.json` entry defeated the embedded bridge**
  (GitHub #361, found by the tokbench independent benchmark): Pi has no
  native MCP adapter, but `init --agent pi` wrote a `lean-ctx` mcp.json
  entry that older pi-lean-ctx versions read as "adapter configured" and
  disabled their embedded MCP bridge — the session cache silently never
  engaged. The installer no longer writes that entry anywhere
  (hooks path + editor-registry target + setup target all removed) and
  `init --agent pi` migrates existing configs by deleting the stale
  entry (file removed entirely when lean-ctx was its only content).
- **Uninstall: perfect-clean guarantee** (GL #558, Discord report):
  `lean-ctx uninstall` now leaves zero artifacts behind. Backup sweep
  covers installer subdirectories (`hooks/`, `rules/`, `skills/`,
  `steering/`, VS Code `User/`, `.gemini/antigravity-cli`) and
  project-local CWD config dirs; lean-ctx-owned script backups and
  orphaned config backups are removed; `{"hooks": {}, "version": 1}`
  boilerplate shells are deleted instead of kept; now-empty installer
  directories are swept as the final filesystem step (non-empty dirs
  survive untouched); platform data dirs (`~/Library/Application
  Support/lean-ctx`, `%LOCALAPPDATA%\lean-ctx`, `~/.local/share/lean-ctx`)
  are removed. Verified end-to-end: 8-agent install + proxy enable →
  uninstall → 0 lean-ctx references, 0 `.bak` files, 0 leftover dirs.
- **Claude rules file regression** (GL #555 follow-up, GL #558):
  `rules_inject` still wrote the always-loaded
  `~/.claude/rules/lean-ctx.md` on `init --agent claude`, undoing the
  token-footprint fix. Claude Code no longer gets a rules target — the
  CLAUDE.md block + on-demand skill carry the guidance.
- **Setup `.bak` churn** (GL #558): re-running setup/init no longer
  rewrites identical hook scripts, so no backup files pile up for
  unchanged content.
- **Audit chain forked under concurrent processes** (found via GL #425
  E2E): `prev_hash` came from a per-process cache, so two processes
  appending simultaneously both chained onto the same parent (and could
  interleave half-written lines). `record()` now takes an exclusive
  advisory file lock and reads the chain tail from the file itself;
  regression test runs 4 concurrent writers and demands one valid
  100-entry chain. The evidence generator additionally splits historic
  glued lines losslessly and refuses unparseable data inside an attested
  period.
- **Claude Code: instruction footprint cut from ~12k to <500 tokens**
  (GL #555): the `~/.claude/CLAUDE.md` block imported the full ruleset via
  `@rules/lean-ctx.md` and the project `AGENTS.md` block via `@LEAN-CTX.md`.
  Claude Code expands `@`-imports inline at launch and loads every rules
  file without `paths:` frontmatter unconditionally — stacking the same
  ruleset up to three times per session (field reports: 12.3k tokens of
  memory files before the first message). The CLAUDE.md block is now
  self-contained (v3, no imports), the AGENTS.md block carries a 3-line
  inline mapping with a plain-text pointer, and the always-loaded
  lean-ctx-owned rules files (`~/.claude/rules/lean-ctx.md`, project
  `.claude/rules/lean-ctx.md`) are removed on update (marker-checked) —
  deep documentation lives in the on-demand lean-ctx skill.
- **Claude Code: compactions now actually reset the re-read cache**
  (GL #555): every Claude hook payload carries `session_id`, so the generic
  session catch-all matched before the compaction check —
  `hook_event_name: "PreCompact"` was never recorded and
  `sync_if_compacted()` never reset `full_content_delivered` flags. After a
  host compaction, `ctx_read` kept answering `[unchanged]` stubs that
  pointed at evicted context, and agents recovered by switching to native
  `Read` for the rest of the session. PreCompact is now detected ahead of
  the catch-all (regression-tested with the real payload shape), so the
  first re-read after compaction delivers full content again.
- **Tool schemas hardened for strict validators** (GL #545): 20 tool
  schemas (incl. `ctx_expand`) declared `type: object` + `properties`
  without an explicit `required` array — valid JSON Schema, but strict
  Pydantic-based backends (OpenAI, Azure, SGLang) reject it and OpenCode
  surfaces `Invalid schema for function 'lean-ctx_ctx_expand': None is not
  of type 'array'`. Every advertised schema (built-ins and plugin
  manifests) now passes `normalize_for_strict_validators()`: recursive
  explicit `required: []` on object schemas and `items` on array schemas,
  at every nesting level. Regression gate:
  `rust/tests/tool_schema_strictness.rs` walks the whole registry.
- **Windows: proxy/daemon survive AI-client MCP recycling** (GL #545):
  the auto-started proxy and daemon were spawned as plain child processes.
  On Windows they inherit the parent's console and Job object; AI clients
  (OpenCode, Codex, Claude Code) run MCP servers inside kill-on-close Jobs,
  so recycling the MCP process silently killed the proxy mid-flight —
  observed as `Cannot connect to API: The socket connection was closed
  unexpectedly`, cold-start latency and agents falling back to native
  tools. Background spawns now use `ipc::process::spawn_detached()`
  (`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP |
  CREATE_BREAKAWAY_FROM_JOB`, graceful fallback when the Job denies
  breakaway). No behaviour change on macOS/Linux.
- **Proxy history pruning defeated provider prompt caching** (GL #534): the
  Anthropic/OpenAI proxy handlers summarized everything older than the last 6
  messages on *every* request. That rolling boundary rewrote a
  previously-stable message each turn, so the provider's prefix-matching
  prompt cache (Anthropic `cache_control`, OpenAI automatic caching) missed
  from that point on — users saw uncached input jump from ~2–10k to 80–100k+
  tokens per turn (cache *writes* at 1.25× instead of reads at 0.1×). History
  is now pruned at a **frozen, cache-aware compaction boundary** that only
  advances in deterministic 16-message strides (≥8 recent messages always
  intact): between jumps the request prefix stays byte-identical and the
  prompt cache keeps hitting; a jump costs one re-write, then caching resumes
  on the smaller history. Pruning is content-deterministic and preserves
  `cache_control` breakpoints; tool-result compression is prefix-stable and
  unchanged. New `[proxy].history_mode` config key /
  `LEAN_CTX_PROXY_HISTORY_MODE` env: `cache-aware` (default), `rolling`
  (legacy max-savings), `off`. Invariant locked by a byte-stability test
  simulating 80 growing turns.
- **`ctx_edit` evidence diff corrupted by terse post-processing** (GH #382):
  the `evidence (diff)` block embeds verbatim source lines, but the generic
  terse stage still ran over `ctx_edit` output — dictionary abbreviation
  (`return 0` → `ret 0`), blank-line stripping and line-score filtering
  silently dropped/mangled diff lines, making agents conclude a correct edit
  went wrong (the file on disk was always right). Two-layer fix: `ctx_edit`
  joins the read family in the terse exemption, and the terse pipeline itself
  is now fence-aware — content inside ``` / ~~~ fences passes through
  byte-exact while surrounding prose still compresses, protecting every
  current and future tool that embeds code blocks.
- **CI green again across all three OS runners**: the billing-catalog golden
  fixture now normalizes CRLF before comparing (Windows autocrlf checkouts),
  the `path_resolve` CWD-independence test canonicalizes both sides before
  comparing (macOS `/var` symlink, Windows 8.3 short names), the
  `team_billing` module doc no longer intra-doc-links a private const
  (rustdoc `-D warnings`), and the six new org/cloud contract docs are
  classified (Experimental) in `contract_docs()`. The frozen
  `team-server-contract-v1.md` is restored byte-exact; its additive
  `storageQuotaBytes`/`roiWebhookUrl` keys moved to a new
  `team-server-contract-v2.md` (Stable), per the contract-file rule.
  Second wave: the CLI fidelity/pipe-guard integration tests pin
  `LEAN_CTX_ALLOWLIST_WARN_ONLY=1` (they assert compression behavior, not
  enforcement — on CI stderr is no TTY, so the new agent-mode allowlist
  blocked their `for`/`while` test scripts with exit 126), the
  `ISSUER_CACHE`/`ATTEMPTS` statics are documented in LOCK_ORDERING.md
  (L45/L46), and `docs/reference/generated/mcp-tools.md` is regenerated
  for the `ctx_agent` brief/return actions and `ctx_knowledge as_of`.
- **Cockpit backlog triple** (GL #454, #455, #456): the Routes view now
  understands axum — `.route("/path", get(handler))` incl. chained methods
  (`get(a).post(b)`), qualified forms (`axum::routing::post`) and module-path
  handlers — plus hand-rolled `"/api/…" =>` match routers, taking this
  codebase from 0 to 136 detected routes. The Call Graph starts framed: an
  initial zoom-to-fit runs once the force layout settles (manual pan/zoom is
  never overridden) and link opacity now fades with edge density, so 150-node
  graphs stop rendering as an over-zoomed hairball. And when token auth is on
  but the browser has none, the first 401 swaps the page for a single
  centered token prompt (validates against `/api/health`, stores in
  sessionStorage, reloads) instead of two dozen raw `unauthorized` cards.
- **Dashboard polish from the function audit** (GL #478): the Explorer tree is
  now a real WAI-ARIA tree — `role=tree/treeitem/group`, `aria-expanded`,
  roving tabindex and full keyboard support (arrows expand/collapse/navigate,
  Enter/Space toggle, Home/End jump) with a visible focus ring. Search results
  stopped pretending: clicking a hit opens an inline file preview (±12 lines
  around the match, hit line highlighted) served by the existing
  `compression-demo` endpoint, with full keyboard access. Procedures now
  auto-learn: every recorded episode re-runs workflow detection
  (`procedural_memory::auto_detect_from_episodes`), so recurring tool
  sequences appear on the Memory page without anyone calling `detect` by
  hand. The status-bar daemon indicator finally explains itself — the tooltip
  describes what green/red means and how to recover (`lean-ctx serve -d`).
- **Data truthfulness** (GL #479): the dashboard now tells the whole story
  behind its savings numbers. The verified ledger covers measured shell and
  search compression (`cli_shell`, `ctx_shell`, `ctx_search` events with raw,
  unmultiplied baselines) instead of only `ctx_read` — closing the unexplained
  24x gap between Home and the ROI view. The 2.5x native-grep counterfactual
  used by the *estimated* stats is now a documented, named constant
  (`NATIVE_SEARCH_BASELINE_FACTOR`), surfaced in the Home tooltips and in a
  new "Methodology: verified vs. estimated" card on the ROI view. Inferred
  agent activity no longer shows negative ages on UTC+N machines (event
  timestamps are local wall-clock and are now interpreted as such).
- **No more WARN noise when scanning project subdirectories** (P1, GL #438):
  `graph_index` now walks *ancestors* for project markers, so `repo/rust/src`
  inside `~/Documents` is a legitimate scan root (the `.git` lives two levels
  up). Marker-less trees under blocked home dirs stay refused.
- **Windows symlink parity at every security boundary** (P1, GL #442):
  `pathjail`, `ctx_edit`, `config_io` and `read_file_nofollow` now reject NTFS
  junctions and all other reparse points (not just symlinks) via the shared
  `pathutil::is_symlink_or_reparse` check; non-Unix `read_file_nofollow`
  previously followed links without any check.
- **Stale cache stubs can no longer mislead the agent** (P0-7, GL #419):
  staleness now treats *any* mtime change as stale (backward mtimes from
  `git checkout` previously read as fresh) and verifies the content hash before
  serving an `[unchanged]` stub when the mtime claims no change (same-second
  writes, restored timestamps). Opt out: `LEAN_CTX_CACHE_VERIFY=0`.
- **Panics are now diagnosable after the fact** (P0-8, GL #420, upstream #378):
  every panic appends thread, location, payload and backtrace to
  `~/.lean-ctx/logs/crash.log` (0o600, size-rotated) — stderr-only reporting was
  lost for daemon/LaunchAgent/MCP-child processes.
- **Copilot CLI hooks work on Windows** (#381): the generated hook entries
  carried only a `bash` command — but Copilot CLI runs the `powershell` field on
  Windows, so the hooks had no runnable command there, errored, and made the CLI
  reject every tool call. Entries now carry **both** fields, each with a quoted
  binary path (`bash` gets the MSYS-style conversion; `powershell` uses the call
  operator — Windows install paths routinely contain spaces). Also, global hooks
  were written to `~/.github/hooks/hooks.json`, a location Copilot never reads:
  they now go to the documented user-level `~/.copilot/hooks/hooks.json`
  (honoring `COPILOT_HOME`), existing pre-#381 configs are upgraded in place
  (missing-`powershell` detection), and lean-ctx entries are migrated out of the
  stale legacy file (deleted when it was ours alone, foreign hooks preserved).
- **Dashboard "ROI & Plan" view is live, not a frozen snapshot** (user-reported):
  the view fetched `/api/roi` exactly once per navigation — the cockpit's 10 s
  poll only refreshed the status footer, and the `lctx:refresh` event was only
  dispatched by the manual ↻ button. Sitting next to the live-updating footer,
  the static ROI numbers looked broken. The ROI view now re-fetches on the same
  10 s cadence while it is the active view, **flicker-free** (the "Loading…"
  placeholder renders only before the first payload; background refreshes swap
  content in place, guarded against overlapping fetches) and shows a muted
  "Updated HH:MM:SS · auto-refreshes every 10 s" line so liveness is visible.
  Drive-by: the Commander view's `lctx:refresh` listener was the only one
  without an active-view guard (and was never removed on disconnect) — it now
  follows the standard guarded pattern.
- **`proxy enable` no longer breaks Claude Pro/Max subscriptions** (community-reported): the proxy *forwards* the caller's credential upstream but never *injects* one, so it can only compress Claude traffic in API-key (pay-as-you-go) mode. A Claude Pro/Max subscription authenticates via OAuth directly against `api.anthropic.com`, and that token is rejected by any custom `ANTHROPIC_BASE_URL` — so unconditionally pointing `~/.claude/settings.json` (and the shell `ANTHROPIC_BASE_URL` export) at the local proxy produced a login loop / 401 the moment Claude Code started, while OpenAI-compatible backends (Ollama, Codex) kept working. `proxy enable` now detects whether an Anthropic API key is available (`ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN` in the environment, or an `apiKeyHelper`/key in `~/.claude/settings.json`) via `anthropic_api_key_available()` and, when none is found, **skips the Claude redirect** (leaving Claude Code on Anthropic directly), **omits the `ANTHROPIC_BASE_URL` shell export** (OpenAI/Gemini exports are unaffected), and **repairs any pre-existing stale local redirect**. It prints a clear explanation and points subscription users to the `ctx_*` MCP tools for savings; `--force` overrides for keys stored where we can't probe (e.g. a keychain). `lean-ctx doctor` gained a check that flags an enabled proxy still routing Claude through the proxy without an API key, with the exact fix (`proxy disable`, or export a key + re-enable). Documented in `docs/reference/05-advanced.md`.
- **Shell-output redirected to a file is always byte-faithful — compression never corrupts `cmd > out`**: when compression was forced (the agent shell hook runs `lean-ctx -c`, and the hook deliberately bypasses its own `[ ! -t 1 ]` pipe guard for agents), the *compressed digest* was written into a real file on a redirect — so `git status --short > files.txt`, `git diff > patch.txt`, `cmd >> log`, etc. landed an abbreviated/deduplicated summary instead of the exact bytes, producing contradictory diffs and silently dropped lines for any downstream tool that re-read the file. `exec()` now detects when stdout is a **regular file** (`fstat`/handle metadata via `std`, no new deps) and passes the output through verbatim even under `LEAN_CTX_COMPRESS`/`-c`. This is enforced at the single exec choke point, so it holds for every caller (shell hook, direct CLI, Pi/MCP bridges) and every redirect form; **pipes** (an agent's captured stdout) and **TTYs** are unaffected and keep compressing. Regression-tested both ways: a redirect-to-file is byte-identical to the raw command while the same command + env stays compressed when piped.
- **Agent hooks always use an absolute binary path** (#367): generated hook commands (Codex, Cursor, Claude, Gemini, Antigravity, …) emitted a bare `lean-ctx`, which fails with exit 127 when the host runs the hook under a non-login shell whose `PATH` lacks the install dir. `resolve_binary_path()` now always resolves to the absolute path (matching MCP setup / `doctor`); stale bare-command configs are rewritten on the next `init` / `doctor`.
- **Proxy forwards the `OpenAI-Project` header** (#366): project-scoped OpenAI keys carry their scope via `OpenAI-Project` (sent by OpenCode and the OpenAI SDK on the Responses API). The proxy's request-header whitelist dropped it, so the upstream rejected the call with `Missing scopes: api.responses.write`. `openai-project` (and `openai-organization`) are now forwarded verbatim.
- **`gemini` setup installs the Antigravity CLI plugin hooks** (#284): `lean-ctx init --agent gemini` configured the Antigravity CLI **MCP** target but never wrote its **plugin** hooks, so hooks landed only in the legacy `~/.gemini/settings.json` that `agy` ignores. The gemini path now also installs the `agy` plugin (`~/.gemini/config/plugins/lean-ctx`); auto-detect already covers the standalone `antigravity-cli` target.
- **The Antigravity CLI plugin is a self-contained, spec-"compliant" bundle** (#284): the `agy` plugin lean-ctx writes (`~/.gemini/config/plugins/lean-ctx/`) now ships its **own `mcp_config.json`** next to `plugin.json` + `hooks/hooks.json`, so the `ctx_*` tools travel with the plugin and it validates clean under `agy plugin validate` (`✔ mcpServers`, `✔ hooks`). This was verified against the real `agy` binary, which stages plugins to *exactly* this path and shape via `agy plugin install` — i.e. the reporter's documented `~/.gemini/antigravity-cli/plugins/<name>/` + root `hooks.json` layout is what the docs *say*, but `agy` v1.0.x actually reads `~/.gemini/config/plugins/<name>/` with `hooks/hooks.json` (the doc's own "global plugins" section agrees). The profile copy (`~/.gemini/antigravity-cli/mcp_config.json`) is kept for back-compat; `agy` keys MCP servers by name, so the dual definition is harmless. **Root-cause note for the "hooks still not firing" reports:** hook *execution* in `agy` is gated by its **own server-side feature flag** `enable_json_hooks` (a proto field applied via `applyFeatureProviderJSONHooksConfig`; experiment `json-hooks-enabled`) and cannot be forced from a local `~/.gemini/config/config.json` (verified). lean-ctx therefore installs the hooks in the precise location/format `agy` expects and they light up automatically once that flag reaches the account — note `agy -p` print mode bypasses the hook subsystem entirely (hooks run in interactive sessions only). `lean-ctx doctor integrations` now verifies the full bundle (`plugin.json` + `hooks/hooks.json` + plugin-local `mcp_config.json`) so install and doctor stay in lockstep and `doctor --fix` repairs any drift.
- **CEP meter counts cache hits and sessions for long-lived servers** (#361): `cep.sessions` and `total_cache_hits` could stay `0` even with confirmed cache activity — the meter only recorded on an `auto_checkpoint` that a short workload may never reach, and repeated snapshots within one process dropped the cumulative cache-hit/read delta (only the first snapshot's value was kept). CEP is now recorded on the live-stats cadence (so even brief sessions register) and accumulates per-snapshot deltas, so `lean-ctx gain` reflects real cache savings.
- **Pi: no envelope overhead on tiny reads** (#361): a `ctx_read` of a very small file appended a "Compressed N → N tokens (0%)" footer even when nothing was saved, making the payload larger than the source. The footer is now suppressed when there is no actual saving (compression stats are still recorded for telemetry); cached re-reads and genuinely compressed reads keep their footer.
- **`ctx_smells` dead-code no longer flags instantiated classes** (#365): added an end-to-end regression test (build graph → scan) confirming imported-and-instantiated Python classes are not reported as dead code while a never-referenced class still is — locking in the symbol-level call/import edges the graph builder creates.
- **`ctx_read` is byte-faithful — the terse layer no longer mangles file reads** (reported via a community A/B code-review evaluation): the server's post-dispatch terse stage (prose dictionary `return`→`ret`, `string`→`str`, … plus line-score filtering) was skipped for reads *only when the read had already saved tokens*. A verbatim `mode="full"` (or `lines:`) read saves 0 tokens, so it was silently routed through the prose compressor — abbreviating keywords and dropping repeated lines. This violated the `full` contract ("guaranteed complete content"), corrupted source the agent edits against, and could drop the exact cross-file lines needed for data-flow review. `skip_terse` now skips the whole read family (`ctx_read`, `ctx_multi_read`, `ctx_smart_read`, `ctx_compress`, `ctx_overview`) unconditionally; reads keep only their own mode-aware, structure-preserving compression (`map`/`signatures`/`aggressive`).
- **An explicit read always returns content, never a stored-reference stub** (same report): the ephemeral context firewall already exempts file reads, but the opt-in `reference_results` path did not — enabling it turned a large `ctx_read` into an `[Reference: …] Output stored …` preview the agent could not edit against. A single `firewall::is_protected_read` predicate is now the source of truth for "an explicit read returns content," honoured by both the firewall and the reference-results path, so `ctx_read`/`ctx_multi_read`/`ctx_smart_read` are never stubbed regardless of config.
- **Generated artifacts always reference the running build — autostart / MCP / hooks can't diverge** (#2444): `resolve_portable_binary()` (which backs the daemon + proxy autostart plists, the daemon spawn, the MCP server command, agent + shell hooks, and the update scheduler) resolved `which lean-ctx` *first*, so the baked path depended on ambient `PATH` ordering at generation time. On a machine with both a Homebrew and a `~/.local/bin` install this was non-deterministic — the daemon LaunchAgent captured the stale Homebrew copy while the proxy/MCP config captured `~/.local/bin`, silently running two different builds at once. The decision is now a pure, unit-tested `choose_binary_path()` that prefers the currently-running executable (`current_exe()`), falling back to `PATH` only when the running binary lives in a transient Cargo build dir (`cargo run -- setup`, where the installed copy is the intended target). Keeps generated hook commands absolute (#367).
- **MCP server can no longer go dark — every tool handler runs under a watchdog** (#271): the recurring `TypeError: Cannot read properties of undefined (reading 'invoke')` was the client losing its tool handles after the server stopped replying. Root cause: handlers were dispatched via `tokio::task::block_in_place`, which pins one of the few core async workers and — being synchronous — cannot be interrupted by a `tokio::time::timeout` on the same task, so a handler that blocked (e.g. the nested `block_in_place` inside `ctx_multi_read` exhausting the blocking pool under concurrent reads) silently swallowed the JSON-RPC response. Every handler now runs on the dedicated blocking pool via `spawn_blocking`, awaited under a watchdog deadline (`LEAN_CTX_TOOL_TIMEOUT_SECS`, default 120s; `ctx_shell`/`ctx_execute` exempt): core workers stay free for the stdio loop and on timeout/panic the server returns a clean error instead of dropping the reply. The specific nested `block_in_place` in `ctx_multi_read` is also removed at the source (now `bounded_lock` + panic guard). Covered by a 16-way concurrency stress test through the full dispatch path plus timeout/panic unit tests.
- **SIGABRT crash in the background indexer — deep ASTs no longer overflow the stack** (#378): graph indexing aborted the whole daemon on files with deeply nested syntax (machine-generated source, deep C/C++ headers, long call chains). The release profile is `panic = "unwind"`, so a worker panic can't `SIGABRT` — the crash was a **stack overflow**, whose handler calls `abort()` and which `catch_unwind` cannot intercept. Every tree-sitter AST walk recursed once per node depth on a ~2 MiB worker stack. New `core::ast_walk` provides iterative, heap-stack pre-order traversal (`for_each_descendant`, `for_each_descendant_pruned`, `find_descendant_by_kind`) — depth is now bounded by the heap, not the call stack, with identical pre-order semantics; every recursive walk on the indexing path (`deep_queries`, `cyclomatic`, swift signature params) was converted. Defense-in-depth: the indexer runs on a named `leanctx-index` thread with a 16 MiB stack + graceful spawn-failure handling, and `ModeGuard::drop` is now panic-free (`try_borrow_mut`) to remove a latent double-panic → abort path. Guarded by 20k-deep and 12k-deep overflow regression tests.
- **`ctx_read` no longer panics on UTF-8 files with multibyte characters** (#379): the structural-hint and shell-result extractors in `core::auto_findings` truncated labels with raw **byte** slices (`&s[..s.len().min(N)]`), so a cut that landed inside a multibyte codepoint (e.g. a Cyrillic `#`/`///` comment near byte 70) panicked with "byte index N is not a char boundary" — surfacing to the MCP client as a `-32603` error and an empty read. All nine truncation sites now use `str::floor_char_boundary`, which snaps the cut down to a valid boundary while preserving the byte budget. Guarded by multibyte regression tests across every layer (content hint, failed-command/test-result shell paths, and the dedup key).

### Security
- **Dashboard: attribute-safe HTML escaping everywhere** (CodeQL #61–#65):
  the central `LctxFmt.esc` used a `textContent`/`innerHTML` round-trip that
  escapes `&<>` but not quotes, and `cexpEsc` in the explorer did the same —
  a `"` in a file path, symbol name or knowledge value could break out of
  `title="…"` / `aria-label="…"` attributes (DOM XSS). All escape helpers
  (central + every per-component fallback, 35 sites across 15 files) now
  escape `& < > " '` via numeric entities; the dangerous identity fallbacks
  (`F.esc || String`) are gone. Verified by a functional breakout test.
- **CLI shell allowlist is now enforced for agents** (P0-1, GL #413):
  `lean-ctx -c` blocks allowlist violations (exit 126) whenever the caller is
  non-interactive (stderr is not a TTY) or in hook-child mode — the CLI path is
  no longer weaker than the MCP path. Humans at a terminal keep the warn-only
  behavior; `LEAN_CTX_ALLOWLIST_WARN_ONLY=1` is the explicit opt-out. The block
  message explains the one-line fix (`lean-ctx allow <cmd>`).
- **Cloud credentials are written 0o600, atomically** (P0-2, GL #414):
  `~/.lean-ctx/cloud/credentials.json` is created owner-only (dir 0o700) via
  tmp+rename; pre-existing world-readable files are tightened on load.
- **Deterministic path resolution** (P0-3, GL #415): relative tool paths are
  never resolved against the process CWD anymore (daemon CWD ≠ project);
  resolution is strictly project_root → shell_cwd → jail_root.
- **Proxy can no longer start unauthenticated** (P0-4, GL #416):
  `start_proxy_with_token(None)` now auto-resolves the session token instead of
  disabling auth. Provider routes still accept provider API keys, so IDE
  clients need no setup.
- **Postgres provider validates schema identifiers** (P0-5, GL #417): the
  agent-controlled `schema` param is restricted to `[A-Za-z_][A-Za-z0-9_$]*`
  (max 63 chars) before SQL interpolation — closes an injection vector.
- **ctx_edit rejects symlinks** (P0-6, GL #418): reads open with `O_NOFOLLOW`
  (plus an lstat pre-check on all platforms) and writes refuse symlink
  destinations — closes a TOCTOU window where a link planted inside the jail
  could read or overwrite files outside it.
- **Cloud/infra CLIs removed from the default shell allowlist** (P0-9, GL #421):
  terraform, ansible, kubectl, helm, az, aws, gcloud, firebase, heroku, vercel,
  netlify, fly, wrangler, pulumi now require explicit opt-in
  (`lean-ctx allow <cmd>`) — they mutate remote infrastructure with ambient
  credentials. Dev-essential tools (git, cargo, rm, psql, …) are unchanged.
- **Home-level IDE config dirs are jail-opt-in** (P0-10, GL #422): `~/.cursor`,
  `~/.claude` & co. are no longer automatically reachable through the PathJail
  (they expose foreign projects' sessions, MCP configs and tokens). Opt in via
  `allow_ide_config_dirs = true` or `LEAN_CTX_ALLOW_IDE_DIRS=1`; `~/.lean-ctx`
  stays allowed.

## [3.7.5] — 2026-06-06

> **The Web & Research release.** lean-ctx reaches beyond the codebase: the new
> `ctx_url_read` tool pulls web pages, PDFs and YouTube videos into context as
> compressed, citation-backed text — research, docs and transcripts without
> leaving the agent loop. Alongside it ship three field-reported fixes: background
> scans never hydrate cloud placeholders (#363), the proxy stops 401-ing
> OpenAI-compatible provider keys (#362), and the Pi extension's session cache
> finally engages (#361).

### Added
- **`ctx_url_read` — the web & research layer** (the web counterpart of `ctx_read`): fetch a public web page, PDF, or YouTube video and get back compressed, citation-backed context. HTML pages and PDFs are parsed to clean Markdown/text; a YouTube URL is resolved to its transcript and flattened into compact, quotable text. Seven distillation modes (`auto` | `markdown` | `text` | `links` | `facts` | `quotes` | `transcript`): the `facts` and `quotes` modes return discrete claims, each carrying a **confidence score** and the **source URL** it came from, so web research is auditable. Extractive, relevance-ranked research-compression distils a whole page down to a token budget (`max_tokens`, default 6000; `max_items` caps `facts`/`quotes`, default 12), and an optional `query` focuses extraction on what you actually need. Fetching is **SSRF-guarded** — only `http`/`https`, with private, loopback and link-local addresses blocked and revalidated after every redirect. Ships with the binary and is exposed automatically wherever lean-ctx runs as an MCP server (granular tool surface → 69).

### Changed
- **Pi: the embedded MCP bridge is on by default, and every read is cached through it** (#361): the bridge that holds the persistent session cache was opt-in, and even when connected only a plain `ctx_read` was routed through it — line-range reads (`offset`/`limit`) and the grep/ls/find tools always spawned a fresh one-shot CLI, so the ~13-token cached re-read essentially never happened on Pi (an independent benchmark measured `cep.sessions: 0` even with the bridge connected). The bridge now starts by default (opt out with `LEAN_CTX_PI_ENABLE_MCP=0` / `"enableMcp": false`), and **all** `ctx_read` variants — including `lines:N-M` ranges — route through it with a CLI fallback, so unchanged re-reads are cheap and register as real CEP sessions. The #168 steering ("Prefer over native …") is now also carried by the Pi extension's own tool descriptions, and `PI_AGENTS.md` plus the setup output steer agents to the `ctx_*` tools instead of the un-compressed native `read`/`bash`/`grep` (which are not routed through lean-ctx in additive mode).

### Fixed
- **Background scans never hydrate cloud placeholders (OneDrive / iCloud)** (#363): starting an agent in — or above — a cloud-synced folder made lean-ctx's directory walks read every file to index it, forcing OneDrive "Files On-Demand" (and iCloud "dataless" files) to download. That is slow, burns quota, and pops OneDrive sync warnings. A new metadata-only `core::cloud_files` check (Windows `FILE_ATTRIBUTE_OFFLINE` / `RECALL_ON_OPEN` / `RECALL_ON_DATA_ACCESS`, macOS `SF_DATALESS`) is now a `filter_entry` predicate on every walker (resident search index, `ctx_search`, graph, BM25, `ctx_tree`), so a placeholder file *or folder* is pruned before it is ever opened — detection reads attributes only and never triggers a download. The resident search index also gained the `is_safe_scan_root` guard the graph/BM25 builders already had (so it never auto-indexes `$HOME`), and the common cloud roots (`OneDrive`, `Dropbox`, `Google Drive`) are blocked as scan roots.
- **Proxy stops 401-ing OpenCode's OpenAI-compatible provider keys** (#362): the proxy's loopback auth gate only accepted `Authorization: Bearer sk-…` / `gsk_…` as a provider credential, so OpenCode (`@ai-sdk/openai`) pointed at an OpenAI-*compatible* upstream — Azure, OpenRouter, Groq, a local vLLM/Ollama gateway, or a project/service key — was rejected with `401 Unauthorized — lean-ctx proxy requires authentication`, even though #353 had already fixed the bare-`/responses` routing. On a provider route the gate now accepts any non-empty credential (the proxy binds to loopback only and forwards the header verbatim, so the real upstream still validates the key); a missing, empty, or bare-scheme `Authorization` is still rejected.

## [3.7.4] — 2026-06-05

> **The Superintelligence Context release.** All six cross-disciplinary North-Star bets are now
> wired into live code: active-context prefetch that learns which providers actually help,
> task-conditioned compression (an Information-Bottleneck proxy), self-managing memory that
> consolidates itself in the background, a context immune system (signed audit + prompt-injection
> detection), stigmergic swarm credit (per-agent heatmap + Shapley attribution), and a
> physically-grounded energy **and carbon** ledger. Alongside the science: a heavy performance
> pass — int8-quantized embeddings (turbovec), SIMD dense search, a shared file-content cache that
> kills the search double-read, lazy demand-driven startup, and lossless JSON/JSONL compaction —
> plus IDE permission inheritance, opt-out instruction-file injection, three new `--json` CLI
> commands, and a batch of proxy/runtime/dashboard fixes. Everything new is free OSS; nothing is
> feature-gated.

### Added
- **Active-context prefetch that learns — persistent provider bandit** (North-Star bet 01): `ctx_preload` used to instantiate its `ProviderBandit` fresh on every call, so it never learned which data sources were actually useful for a given kind of task. The bandit (Thompson sampling over a Beta posterior) is now **persisted per project** (`provider_bandit.json`) and closes the Active-Inference loop: task-type → prediction → execution → observation → bandit update → better future predictions. A preload that returns useful chunks is a positive signal; an empty/failed one is negative. Over time lean-ctx prefetches the providers that have repeatedly paid off for *this* project and stops wasting calls on the ones that don't.
- **Task-conditioned compression — an Information-Bottleneck proxy in `entropy` mode** (North-Star bet 02): the `entropy` read-mode compressed purely by Shannon self-information, so a rare-but-irrelevant line was kept while a common-but-task-critical line could be dropped. When an active session intent exists, `entropy_compress_task_conditioned` now **rescues low-entropy lines that mention task keywords** — keeping what is either *surprising* (high H) **or** *task-relevant* (mentions the goal's concepts), and compressing away only what is both uninformative and off-task. Falls back to pure adaptive entropy when no intent is active, so non-task reads are byte-identical.
- **Context immune system — signed audit trail + prompt-injection detection** (North-Star bet 04): two provenance/safety steps. (1) Audit entries are now **Ed25519-signed** (a `signature` over the chained `entry_hash`, keyed by the local lean-ctx identity), so a record carries cryptographic proof of which installation produced it — not just a hash chain a local writer could rebuild. (2) A conservative `detect_injection` heuristic scans tool output for known prompt-injection patterns (role-override like "ignore all previous instructions", role-hijack, ChatML/`[INST]` token smuggling, role-boundary markers). On a hit it logs a warning and emits a `SecurityViolation` audit event. It targets high-specificity phrases that almost never appear in legitimate source/docs, so false positives are rare (verified against real code and comments in tests).
- **Stigmergic swarm substrate — per-agent heatmap traces + Shapley context credit** (North-Star bet 05): the access heatmap was agent-agnostic — every read pooled anonymously. `HeatEntry` now carries a per-agent access map (a stigmergic pheromone field), populated in the **live** read path via a canonical `current_agent_id()` resolver (`LEAN_CTX_AGENT_ID` / `LCTX_AGENT_ID` / `local`, shared with the savings ledger). A new `context_credit()` computes Shapley-inspired attribution: when several agents touch the same file, each contributor earns credit proportional to how many *other* agents also benefited — the raw signal for routing one agent toward what another already found useful, and for crediting the context that actually helped the swarm.
- **`rules_injection` config — opt out of touching shared instruction files** (#343): a new top-level option (`shared` default | `dedicated`, env `LEAN_CTX_RULES_INJECTION`) controls *how* lean-ctx delivers its tool-mapping rules to the shared-instruction-file agents (Claude Code, Codex, OpenCode, Gemini CLI). The default `shared` keeps today's behavior — a marker-delimited block written into `CLAUDE.md` / `AGENTS.md` / `GEMINI.md` for zero-config discoverability. The new `dedicated` mode **never edits those user-authored files**; instead it uses each agent's own config-driven, fully-removable auto-load path and a lean-ctx-owned rules file:
  - **Claude Code & Codex** — the rules summary is injected at session start via the existing `SessionStart` hook's `additionalContext` (model-visible, nothing persisted to `CLAUDE.md`/`AGENTS.md`; any prior lean-ctx block is stripped on switch).
  - **OpenCode** — the dedicated `~/.config/opencode/rules/lean-ctx.md` is registered (by absolute path, idempotently) in `opencode.json` `instructions[]`, and the old `AGENTS.md` block is removed.
  - **Gemini CLI** — the dedicated `~/.gemini/LEANCTX.md` is registered in `settings.json` `context.fileName` (seeding the default `GEMINI.md` so the user's own context file keeps loading), and the old `GEMINI.md` block is removed.
  Switching back to `shared`, and `lean-ctx uninstall`, cleanly reverse every registration (`instructions[]` / `context.fileName` collapse back to their pristine default) and delete the dedicated files — no orphaned entries. Toggling is driven entirely by the flag: the same `rules sync` writes a block in `shared` mode and an untouched user file + separate rules file in `dedicated` mode.
- **`permission_inheritance` config — lean-ctx tools honor your IDE's permission rules** (community request): when lean-ctx is mounted as an MCP server its tools (notably `ctx_shell`) execute inside the lean-ctx process, *bypassing* the host IDE's own permission engine — so an OpenCode user who set `bash`/`rm *` to `ask`/`deny` would have that guard silently skipped whenever the agent reached for `ctx_shell` instead of the native tool. A new top-level option (`off` default | `on`, env `LEAN_CTX_PERMISSION_INHERITANCE`) makes lean-ctx *mirror* the active IDE's permission config onto its own tools. When `on`, before dispatch lean-ctx reads the IDE permission rules (v1: **OpenCode** `opencode.json` / `opencode.jsonc`, global + project merged) and applies the equivalent decision to the matching tool: `ctx_shell`/`ctx_execute` ← `bash` (incl. granular `git *` / `rm *`, and top-level command patterns), `ctx_read`/`ctx_multi_read`/`ctx_smart_read` ← `read`, `ctx_edit` ← `edit`, `ctx_search` ← `grep`. `deny` blocks the call, `ask` holds it back with an actionable message (MCP can't show an interactive prompt for these tools), and `allow` (or no matching rule) proceeds. The most specific rule wins (longest pattern; named tool beats global `*`), ties broken toward the more restrictive action. lean-ctx **never writes** the IDE's `permission` block — inheritance is read-only and runtime-only; the policy is cached briefly and the default (`off`) adds zero hot-path cost. `lean-ctx doctor` reports the status and, when on, how many OpenCode rules are being mirrored.

- **Self-managing memory — the cognition loop now actually runs, and feedback steers retention** (North-Star bet 03): the eight-step background cognition loop (seed-promote → structural repair → lateral synthesis → contradiction resolution → hebbian strengthen → decay → compact) existed and was enabled by default (`autonomy.cognition_loop_enabled`, `cognition_loop_interval_secs = 3600`) but **nothing ever triggered it** outside an explicit `ctx_knowledge action=cognition_loop` call — so knowledge never self-organized on its own. A new `core::cognition_scheduler` fires it opportunistically from the MCP dispatch path: time-gated to the configured interval, single-flight (an in-flight loop is never double-spawned), panic-safe (RAII guard frees the slot), and cheap on the hot path (one config read + two atomic loads when not due). Because the server is request-driven this beats a wall-clock thread — maintenance happens exactly when there is activity to consolidate and never when the agent is idle. Additionally, the confidence-decay schedule now closes the reward loop: a fact's explicit thumbs-up/down (`feedback_up`/`feedback_down`) scales its decay — net-positive feedback keeps it longer, net-negative forgets it faster (logarithmic, capped, and floored so a single downvote never collapses a healthy fact and nothing is ever hard-deleted).
- **Thermodynamic accounting — energy *and* carbon avoided, surfaced in `ctx_gain`** (North-Star bet 06): lean-ctx already estimated grid energy avoided (`0.4 J/saved-token`, reconciled with the website `/metrics` methodology) but only for display strings. The footprint is now a first-class, physically-grounded figure: `core::energy` adds a transparent carbon model (`G_CO2_PER_KWH = 475` g/kWh — the global-average grid intensity, override-able per machine via `LEAN_CTX_GRID_CO2_G_PER_KWH` so cleaner grids report honestly), and `GainSummary` carries `energy_wh` + `co2_grams` derived from `tokens_saved`. `ctx_gain` now shows an `Impact:` line (`… grid energy avoided | … CO₂e`) and emits both fields in its JSON, so the savings ledger's environmental dividend is auditable, not just cosmetic. All figures are surfaced as estimates; nothing is persisted into the hash-chained ledger (energy is a pure function of the already-recorded saved tokens, so the tamper-evident chain is untouched).
- **Three new `--json` CLI commands for editor/programmatic use**: `lean-ctx semantic-search` (fixes the editor search path), `lean-ctx repomap`, and `lean-ctx knowledge recall` all gain structured `--json` output so editor integrations and scripts consume results without scraping human-formatted text.
- **`gain` auto-publishes public metrics in the background**: when `gain.auto_publish` is enabled, the MCP server now performs a throttled background publish of the (opt-in) public savings metrics on startup, so the leaderboard/hero stats stay current without a manual `lean-ctx gain --publish`. Throttled so it never publishes more than once per interval and never blocks startup.
- **`dashboard --base-path` for reverse-proxy subpath mounting** (#355): the web dashboard can be served under a subpath (e.g. `https://host/leanctx/`) behind a reverse proxy; all asset and API URLs are rewritten to honor the base path.

### Performance
- **Shared file-content cache removes the search double-read** (#148): building the trigram search index and then answering a `ctx_search` query used to read the entire candidate corpus from disk **twice** — once to index, once to scan — and the BM25 index read it yet again. A new resident, bounded `core::content_cache` (LRU, invalidated by `(mtime, size)`) now lets the index build, `ctx_search`, and BM25 share a single in-memory copy per file: read once, reuse many times. Entries self-invalidate the instant a file changes on disk, the cache refuses inserts under memory pressure, and it is dropped first by the eviction orchestrator (`UnloadIndices` / `EmergencyDrop`) so it never competes with the heavier indices for headroom.
- **Lazy, demand-driven index warming on server startup** (#152): the MCP server no longer kicks off a full repo graph + BM25 scan (and extra-root scans) eagerly in `initialize`. A session that only ever calls `ctx_read` / `ctx_shell` / `ctx_tree` now pays **zero** startup indexing cost. Each tool is classified by what it actually needs (`None` / `Search` / `Heavy`); the first call to a search-backed or graph-backed tool triggers a one-shot, once-per-root background warm (extra roots warmed on that same first heavy pre-warm), so the prebuilt index is ready exactly when — and only if — something uses it.
- **int8-quantized embeddings + SIMD-friendly scoring (turbovec-inspired)**: dense embedding vectors are stored int8-quantized, cutting the resident index memory roughly 4× and making similarity scoring SIMD-friendly. Recall is preserved within tolerance; the smaller footprint also reduces eviction pressure on the shared caches.
- **SIMD cosine + threshold-gated HNSW cache for dense search**: dense/semantic search uses a SIMD cosine kernel and only builds/keeps the HNSW graph when the corpus is large enough to pay for it (threshold-gated), so small projects stay lightweight while large ones get sublinear search.
- **Read-mostly session cache + off-hot-path telemetry** (#147, #149): the per-request session state is served from a read-mostly cache and telemetry/event work is moved off the hot path, removing redundant locking and disk churn from the common `ctx_read` flow.
- **Lossless JSON/JSONL compaction**: large JSON/JSONL tool output is compacted losslessly (insignificant whitespace removed, structure preserved) before counting, so structured payloads cost fewer tokens without changing a single value.
- **Bounded cold BM25 build in the `ctx_semantic_search` MCP handler** (#150): a first semantic search on a cold index now builds the BM25 index under a bounded budget instead of an unbounded scan, so the initial query returns promptly on large repos.
- **Proxy parses each request body once**: the compressing proxy parses the request body a single time and reuses the parsed form across compression + introspection, and additionally protects multi-file read tool results from lossy command-output compression.

### Changed
- **`server::call_tool_guarded` post-processing split into composable stages** (#144): the ~1000-line guarded dispatch path is now a thin orchestrator. The self-contained, synchronous pipeline stages (budget exhaustion/warning gates, Context-IR source-kind mapping, terse-compression gating, final token-count + savings correction) live in a unit-tested `server::post_process` module, and the large `&self`-coupled side-effect blocks (tool-receipt + intent + session-save + cost attribution; shared Context-OS persist + bus events) move into named `server::post_dispatch` methods. Behaviour, ordering, and await points are identical — purely a maintainability/readability change with new direct unit tests for the extracted stages.
- **Tool registry is the single schema source** (#141): the granular per-tool schema definitions are generated from one registry instead of being maintained in parallel, retiring a recurring source of drift between the advertised tool surface and the actual handlers (guarded by an up-to-date regression test).
- **Unified path resolution across the core** (#145): project/path resolution is consolidated into one code path with a project-marker test, removing subtle inconsistencies between callers that resolved roots differently.
- **Tool descriptions steer agents to the `ctx_*` tools** (#168): MCP tool descriptions now nudge agents toward the lean-ctx tools over native equivalents, with a regression test that fails the build if the steering language regresses.

### Fixed
- **MCP advertises the full profile surface to dynamic-tools clients** (#358): clients that consume the dynamic tool categories now see the complete profile-authoritative `tools/list`, and the always-on `ctx_call` gateway is exposed so no tool is unreachable for those clients (#204).
- **Proxy accepts bare provider endpoints for the OpenCode Responses API** (#353): a provider base URL without the full path suffix is normalized correctly, so OpenCode's Responses-API requests are routed and compressed instead of failing.
- **macOS install/update no longer touches `~/Documents`** (#356): installation and update paths stop writing into `~/Documents`, avoiding spurious permission prompts and stray files on macOS.
- **Dashboard info-tip tooltips never clip** (#357): info-tip tooltips in the web dashboard are portaled to `<body>`, so they render above surrounding cards instead of being clipped by overflow.
- **Runtime robustness: bounded WAL, dead-owner lock reclaim, fact eviction & doctor thresholds** (#357-adjacent runtime hardening): the write-ahead log is now bounded, locks held by dead owners are reclaimed instead of stalling, knowledge-fact eviction is corrected, and `lean-ctx doctor` thresholds are tuned so its health checks reflect real conditions.
- **Pi: explicit `LEAN_CTX_PI_ENABLE_MCP=1` now always starts the embedded MCP bridge** (#361): a `lean-ctx` entry in `~/.pi/agent/mcp.json` (written by `lean-ctx init --agent pi`) no longer silently disables the embedded bridge. Pi has no native MCP support, so that entry alone never served the tools — meaning an explicit opt-in could leave both the bridge *and* the adapter inactive, and the session cache (with its ~13-token re-reads) never engaged. The explicit flag now wins; `/lean-ctx` only notes the possible-duplicate case when pi-mcp-adapter is genuinely also running.
- **Deterministic HNSW index construction**: the approximate-nearest-neighbor index now seeds each node's level from its insertion index (splitmix64) instead of OS entropy, so the same corpus always builds the same graph and returns the same results. This removes run-to-run recall variance (and the flaky recall test it caused) and makes semantic-search results reproducible.
- **Dashboard graph/code-map shows a clear language message instead of an endless loading/“run index build” state** (#360): for projects built from languages the code-map does not index (e.g. Lua/Luau), the Dependencies, Symbols, and Roads views now explain that the graph only supports specific languages and that BM25 search/compression still work — instead of suggesting an index rebuild that can never populate the graph. `/api/graph` reports the graph-supported languages plus any unsupported source languages detected in the project.

## [3.7.3] — 2026-06-04

> **Compression where the agent actually is — and fidelity where it matters.** A
> `shell` MCP tool so the Codex Desktop/Cloud app compresses even without
> lifecycle hooks, plus a self-diagnosing, additive shell allowlist so permitting
> one command no longer means wiping out the defaults. Navigation output (`map`/
> `signatures`) now carries line ranges so agents jump straight to a symbol, and
> already-compact formats (TOON) pass through untouched instead of being
> recompressed away. The proxy also speaks OpenAI's new Responses API.
>
> _Supersedes 3.7.2: an automated release misfire published an incomplete 3.7.2 to
> crates.io / npm before this work had landed, and those registries permanently
> reject re-publishing a version — so 3.7.3 is the first clean release of this work
> across every channel (crates.io, npm, GitHub, Homebrew)._

### Added
- **OpenAI Responses API support in the proxy** (#346, thanks [@Lctrs](https://github.com/Lctrs)): clients that moved to OpenAI's new Responses API (`POST /v1/responses`) — opencode, the OpenAI Agents SDK — were forwarded untouched because the proxy only understood Chat Completions (`messages`). The proxy now compresses the Responses-API shape too: each `function_call_output.output` (the Responses analogue of a role:`"tool"` message — a string, or an `input_text` content array) is run through the same pattern pipeline as every other tool result. The conversation `input` array is intentionally left structurally intact (no history pruning) so a `function_call` can never be separated from its matching `function_call_output` and trigger a 400. Retrieve/cancel/delete sub-paths (`/v1/responses/{id}/…`) pass through cleanly, and `/status` introspection now reports an accurate token breakdown for Responses requests (`instructions` → system prompt; `input` items → user/assistant/tool buckets; `input_image` counted). Chat Completions remains fully supported.
- **`shell` MCP tool** (#337): the instruction-only fix in 3.7.1 wasn't enough — the Codex Desktop / Cloud app loads the MCP server but its agent ignores `ctx_shell` and reaches for a native `shell`/`Bash` tool, so nothing compressed. lean-ctx now exposes a `shell` tool (familiar name + model-optimized description) that transparently delegates to the same 95+-pattern compression pipeline as `ctx_shell`, giving the Desktop/Cloud app the compression the CLI gets via hooks. Registered for all MCP clients.
- **`lean-ctx allow <cmd>`** (#341): permit a binary on the shell allowlist *additively* through the new `shell_allowlist_extra` config field — so allowing e.g. `acli` keeps `git`/`cargo`/`npm`/… intact instead of replacing the whole built-in list. `lean-ctx allow --list` prints the effective allowlist plus the exact config path in use; `--remove` reverts. Picked up on the next command — no MCP/daemon restart needed.
- **Line ranges in `map` / `signatures` output** (#340, thanks [@iohansson](https://github.com/iohansson)): every entity in the navigation-focused `map` and `signatures` views now carries a compact `@Lstart[-end]` suffix (e.g. `fn ⊛ build() → Config @L42-58`), so an agent can jump straight to a symbol instead of issuing a follow-up search. Spans are populated consistently across the tree-sitter extractors (all languages, not just Kotlin) and the regex fallbacks (TS/JS, Rust, Python, Go, generic), with Vue/Svelte SFC spans shifted back to file-absolute lines. **Mode-aware by design:** the suffix is emitted *only* in `map`/`signatures` (MCP + CLI) where locating code is the point — compression-first paths (`aggressive`, `entropy`, full-body reads, and `ctx_compress`/`ctx_outline`/`ctx_fill`/`ctx_analyze`/repo-graph) stay byte-identical and pay zero extra tokens. The `map`/`signatures` compression caches are version-bumped so stale range-less entries are never served.
- **Format-aware passthrough for already-compact output** (#342, thanks [@pomazanbohdan](https://github.com/pomazanbohdan)): `ctx_shell` / `lean-ctx -c` no longer recompress output that is *already* in a compact, token-oriented format. lean-ctx detects TOON (Token-Oriented Object Notation) by its structural markers — the tabular `key[N]{f1,f2}:` header and the length-prefixed `key[N]:` array header — and returns it verbatim, because a second pass saves little while rewriting the exact line/field shape an agent needs to validate a CLI output contract. The decision is **output-shape based, not command based**, so *any* tool emitting TOON is covered without enumerating it in `excluded_commands`. Controlled by the new `preserve_compact_formats` config (default `["toon"]`; set to `[]` to disable) and surfaced as a "Compact-format passthrough" line in `lean-ctx doctor`.
- **Clearer path to the public leaderboard** (community feedback): the `lean-ctx gain` recap now always shows a one-line, state-aware hint — how to publish to `leanctx.com/metrics` with `--leaderboard`, how to claim a display name (`--name="…"`) instead of showing up as "anonymous", and how to `--unpublish`. The `gain --publish` output likewise points a private-only publisher to the public board and nudges nameless leaderboard entries toward a handle, so getting on (and managing) the leaderboard is never a guess.
- **VS Code / Cursor extension, now publishable** (community feedback): the editor extension is consolidated into a single, marketplace-ready package (`vscode-extension`) and shipped to the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=LeanCTX.lean-ctx) and [Open VSX](https://open-vsx.org/extension/LeanCTX/lean-ctx) (Cursor, VSCodium, Windsurf) via a dedicated, tag-triggered CI workflow (`publish-vscode.yml`). It gains binary auto-detection (PATH / `~/.cargo/bin` / Homebrew, for GUI editors with a stripped PATH), `setup` / `doctor` / `gain` / `heatmap` / web-dashboard commands, one-click workspace MCP wiring, plus an Apache-2.0 license and PNG icon. The duplicate scaffold (`packages/vscode-lean-ctx`) was removed.

### Fixed
- **MCP stdio stays protocol-clean** (#348): confirmed and regression-guarded that lean-ctx routes all `tracing` diagnostics to **stderr** — never the stdout JSON-RPC transport — so a log line can never be interleaved into an MCP client's message stream and break parsing. This has held since ≤3.7.1 (the transport writes only framed JSON-RPC, the auto-started proxy runs as a subprocess with stdout redirected to `null`, and tool handlers return strings rather than printing); a source-level guard now fails the build if the logging writer is ever switched to stdout.
- **`shell_allowlist` edits silently ignored in MCP/editor mode** (#341): allowlist changes looked like no-ops while `lean-ctx -c` (CLI, warn-only) still ran the command, due to three compounding traps. (1) A malformed `config.toml` fell back to the defaults with the warning printed only to **stderr** — invisible over an MCP/stdio transport; the parse error is now surfaced in the block message and in `lean-ctx doctor`. (2) Setting `shell_allowlist` directly replaced the entire default list — the new additive `shell_allowlist_extra` (written by `lean-ctx allow`) avoids that footgun. (3) The "not in allowlist" message now names the **exact config path the runtime reads** plus the precise additive fix, so a config-path/HOME mismatch between the editor's MCP process and your shell is immediately visible. `lean-ctx doctor` gains a "Shell allowlist" check (effective command count + parse status).
- **Codex instructions no longer claim Desktop "can't" run hooks** (#350, thanks [@iohansson](https://github.com/iohansson)): the block lean-ctx injected into `~/.codex/AGENTS.md` (and `LEAN-CTX.md`) asserted as fact that "lifecycle hooks do not run" in the Codex Desktop/Cloud app — false (hooks *do* run there, trust-gated via `/hooks` since Codex 0.129.0) and traceable to a misread of `openai/codex#13019`, which is about completion notifications, not lifecycle hooks. A false absolute like that is exactly the kind of thing that confuses the agent. The instructions now make no surface-specific "hooks don't run" claim; they frame the lean-ctx MCP/CLI tools (`ctx_shell` / `ctx_read` / `ctx_search`, or `lean-ctx -c`) as the path that compresses reliably on **every** surface regardless of hook status, and `lean-ctx doctor`'s Codex note is corrected to match. A regression test fails the build if the docs ever re-introduce a "hooks do not run" / "no automatic compression" absolute.
- **Proxy no longer mangles file/source reads** (#351, external testing feedback): the request-compressing proxy treated *every* tool result as shell output, so a `Read` of a large source file was run through command-output truncation (head/tail + "safety" lines) on the very next turn — gutting the file the model was mid-refactor on and forcing an uncounted re-read. The proxy now resolves each tool result's originating tool name (`tool_use`/`tool_calls`/`function_call`/Gemini `functionResponse.name`) and **never lossy-compresses a file read or content that heuristically looks like source code**, across all four providers (Anthropic, OpenAI Chat, OpenAI Responses, Gemini). Shell/search/command output still compresses as before. History pruning is likewise code-aware: an older file read is replaced with an honest, actionable "re-read the file if you need it again" stub instead of a misleading 3-line excerpt.
- **Proxy stopped failing large-refactor and long-generation calls** (#351, external testing feedback): the request-body ceiling was 10 MiB, so a big-codebase refactor with several files in context hit a hard `400` mid-task — now `64 MiB` and configurable via `LEAN_CTX_PROXY_MAX_BODY_MB`. A single 2-minute total request timeout also aborted long streaming generations (e.g. Opus doing a large refactor) mid-stream; it is replaced by a connect timeout plus a read (idle) timeout (`LEAN_CTX_PROXY_CONNECT_TIMEOUT_SECS`, `LEAN_CTX_PROXY_READ_TIMEOUT_SECS`, defaults 15s / 300s), so a slow-but-alive stream is never cut while a genuinely dead upstream still fails.

### Changed
- **Identifier α-substitution (`§MAP`) is now opt-in** (#351, external testing feedback): `aggressive` reads on large projects used to replace long identifiers with short α-codes plus a `§MAP:` decode table (`symbol_map_auto`, previously auto-on above 50 source files). A tester found the abbreviated form obscured package/symbol names exactly when editing. It is now **off by default** — set `symbol_map_auto = true` (or `LEAN_CTX_SYMBOL_MAP=1`) to opt back in for maximum exploration savings.
- **Editing intents always read the full file** (#351, external testing feedback): when the active task classifies as `refactor`, `fix-bug`, or `generate`, `auto`-mode reads now resolve to `full` regardless of model tier — you cannot safely edit a file you can only partially see, and an abbreviated/`signatures` view just forced a follow-up read. Exploration/review intents still compress as before.
- **Per-model cost breakdown in the proxy** (#351, external testing feedback): `/status` now reports a `per_model` array (requests, estimated tokens saved, and USD saved priced from the shared model table) instead of a single flat number, and discloses that savings are request-side and do not subtract agent re-reads. Token figures remain explicitly labelled estimates.

## [3.7.1] — 2026-06-03

> **Wrapped Viral-Loop.** The honest Wrapped recap is now shareable end-to-end: a
> first-run "aha", one-click sharing, an opt-in hosted permalink, and an opt-in
> public leaderboard — privacy-safe and anonymous-first.

### Added
- **First-run "aha"** (`lean-ctx discover`): the first run surfaces a concrete, projected token saving for the current project (one-time marker in `~/.lean-ctx`), with `discover --card` exporting a shareable "Ghost Tokens" SVG. Non-UTF-8 shell histories (zsh metafied format) are now read lossily so the projection never silently sees empty history.
- **One-click share** (`gain --copy` / `--open`): copy a ready-to-post share line to the clipboard or open the generated SVG/HTML card in the browser — cross-platform (`pbcopy`/`clip`/`wl-copy`/`xclip`/`xsel`, `open`/`start`/`xdg-open`).
- **Hosted Wrapped permalink** (`gain --publish` / `--unpublish`): anonymously publish a whitelisted, privacy-safe slice of the recap and get a shareable `leanctx.com/w/<id>` URL (copied to clipboard). Whitelist-only (`deny_unknown_fields`), one-time `edit_token` stored locally for later removal, optional account claim. Server-rendered page carries per-card Open Graph / Twitter meta; `og:image` is a `resvg`-rasterized 1200×630 PNG. Contract: `docs/contracts/wrapped-permalink-v1.md`.
- **Opt-in public leaderboard** (`gain --publish --leaderboard`): off by default; when set, the card is listed on `leanctx.com/leaderboard` (server-rendered, top 50 by realized tokens saved). Only the user-chosen display name is person-facing; everything else is an aggregate. JSON at `/api/leaderboard`.
- **Per-day version in `lean-ctx gain`** (#307): each row in "Recent Days" and the `gain --daily` table now shows the lean-ctx version active that day, so a compression change can be attributed to a release. Days recorded before this field stay blank (`—`). The version is stamped on each day's stats and carried through the cross-process stats merge.

### Fixed
- **`2>&1` (and `>&`, `&>`, `N>&M`) misread as a command** (#334): the shell-allowlist parser split a single `&` as a background separator even inside a redirect, so `pnpm run compile 2>&1` was parsed as `pnpm run compile 2>` **and** a bogus command `1`, which was then blocked. A `&` adjacent to `>` is now correctly treated as part of the redirect, not a separator; genuine background `&` still splits. Fixes false `'1' is not in the shell allowlist` blocks in MCP mode (Cursor/opencode/etc.).
- **Auto-update ignored `config.toml`** (#335): a scheduler installed earlier kept running `lean-ctx update` even after the user set `updates.auto_update = false`, because the `update` command never re-checked config. Scheduled runs (`--quiet`/`--scheduled`) now obey config: `auto_update = false` skips the update **and removes the orphaned scheduler** (self-heal), and `notify_only = true` downgrades to a check (never installs). Manual `lean-ctx update` is an explicit action and always proceeds.
- **macOS bash login shells missed the hook and PATH** (user report): bash login shells (Terminal.app, IDE terminals, `bash -l`) read `~/.bash_profile`/`~/.profile`, never `~/.bashrc` — yet the hook (and the installer's `~/.local/bin` PATH export) land in `~/.bashrc`. `lean-ctx setup` now ensures the login profile sources `~/.bashrc` (idempotent, Debian/Ubuntu-style), so the hook and PATH take effect in login shells. `install.sh` prints the matching one-liner; uninstall removes the snippet. zsh is unaffected (it always reads `~/.zshrc`).
- **Event feed flooded with false "denied" policy violations**: auto-preload candidates from the project graph are repo-relative (e.g. `rust/src/core/foo.rs`); the path jail resolved them against the daemon's CWD (not the project root), so every candidate failed with "no existing ancestor" and was logged as a policy violation. Relative candidates now resolve against the jail root, and a genuinely missing file is no longer mislabeled as a security denial. As defense-in-depth, `ctx_preload` now resolves its jail root from the dispatch-provided project root when no explicit `path` and no session root are available, so it never silently jails against the daemon CWD in any IDE.
- **`ctx_search` and the background index build could hang on special files (FIFOs, sockets, devices)** (#336): a regular-file guard now skips non-regular paths before any blocking read — `read_to_string` on a named pipe blocks forever waiting for a writer, which surfaced as random, unlogged hangs. `ctx_search` additionally enforces a wall-clock deadline (`LEAN_CTX_SEARCH_DEADLINE_MS`, default 10s) and returns partial results with a note instead of hanging. Reproduced with a real FIFO and covered by regression tests (`search_skips_named_pipe_without_hanging`, `build_skips_named_pipe_without_hanging`).
- **No compression in the Codex Desktop / Cloud app** (#337): lean-ctx's transparent shell/file compression for Codex is hook-driven (the `codex-pretooluse` hook reroutes commands through `lean-ctx -c`), but the Codex Desktop and Cloud app run in app-server mode where lifecycle hooks **do not fire** (OpenAI codex#13019) — so identical commands compressed in the Codex CLI but not in the app. The Codex instructions (`~/.codex/AGENTS.md` + `LEAN-CTX.md`) now state this explicitly and direct the agent to proactively route work through the MCP tools (`ctx_shell`/`ctx_read`/`ctx_search`) or `lean-ctx -c` in the Desktop/Cloud app, which is the channel that *is* available there. `lean-ctx doctor` adds a Codex note so a healthy config no longer looks like a silent failure. (Hooks remain the automatic path in the Codex CLI once trusted via `/hooks`.)

## [3.7.0] — 2026-06-01

> **Shadow Mode + Meaningful Instructions.** Rules injected into agents are now
> actionable (concrete tool names, examples, workflow), and a new `shadow_mode`
> transparently intercepts native Read/Grep/Shell calls for users who want full
> automatic routing.

### Added
- **Shadow Mode** (`lean-ctx config set shadow_mode true`): transparently intercepts native Read/Grep/Shell via hooks, strengthens MCP instructions to MUST-level, activates immediate bypass hints on first native tool use, logs all intercepts to `~/.lean-ctx/shadow.log` for audit transparency. Visible in `lean-ctx doctor` and `lean-ctx status`.
- **6-step workflow in all injected rules**: Orient → Locate → Read → Edit → Verify → Record — agents can follow blindly without memorizing tool names.
- **Tool Mapping table in rules**: every injected rule file now includes a MANDATORY table with exact tool names, parameters, and runnable examples (`ctx_read("src/main.rs", "full")`).
- **Proactive section in RULES_DEDICATED**: `ctx_overview` at session start, `ctx_compress` at phase boundaries, `ctx_knowledge(action="wakeup")` for prior findings.
- **Compression Bypass ladder**: `lines:N-M` → `full` → `raw=true` — documented escape hatch when compression hides detail.
- **Risk Gate guidance**: before editing exported symbols, auth, DB schemas, or 3+ files — run `ctx_impact` + `ctx_callgraph`.
- **Registry-driven hook refresh + doctor staleness check**: `lean-ctx doctor` detects stale hooks, IDE path misconfiguration, and auto-refreshes outdated rules on first tool call.
- **Reference appendices generated from code**: `docs-gen` renders MCP tool reference, CLI reference, and journey golden outputs directly from source — with CI drift-gate to catch divergence.
- **Complete user-journey reference** (14 journeys): install-to-first-save through performance tuning, with IDE quickstarts and golden output examples.
- **Semantic-index observability** (#249): `lean-ctx index status` and `lean-ctx doctor` surface BM25 state (idle/building/ready/failed), build duration, persisted size, and failure notes.
- **Verified savings ledger** (`lean-ctx savings [summary|verify|export]`): an auditable, append-only per-event record (`~/.lean-ctx/savings/ledger.jsonl`) behind the aggregate `gain` numbers. Each value-producing read logs the counterfactual (baseline vs actual tokens), resolved pricing model, the **tokenizer that produced the counts** (`o200k_base`, recorded separately from the model so the proxy gap is disclosed), a privacy-preserving repo hash, and a tamper-evident SHA-256 hash chain. Cross-process safe (advisory file lock). Local-only, on by default; opt out with `LEAN_CTX_SAVINGS_LEDGER=off`.
- **Bounce-netting (honest savings)**: a compressed read later invalidated by a full re-read now records a negative "bounce" event, so `lean-ctx savings` and `gain --wrapped` report the **realized** saving (gross → bounce → net) instead of a gross upper bound. Bounce persists across processes via the ledger.
- **Wrapped share card + page** (`lean-ctx gain --svg` / `--share`): export the Wrapped recap as a dependency-free 1200×630 SVG (social/OG image) or a self-contained, self-hostable HTML page (opt-in permalink, SVG embedded, zero telemetry). `--share --base-url=…` emits Open Graph / Twitter meta for rich link unfurling. Every surface labels the pricing model, marks fallback prices `(est.)`, and states USD as an upper bound.

### Changed
- **Proper uninstall** (user request): `lean-ctx uninstall` is now complete and self-contained. It (1) **stops every process first** — daemon, proxy, and stray `lean-ctx` PIDs (current process + IDE-owned MCP servers excluded) so nothing respawns or holds the files being removed, and (2) **removes the binary itself** — the managed copy/symlink in `~/.local/bin` (or `$LEAN_CTX_INSTALL_DIR`), `/usr/local/bin`, and the running executable (unlinked safely on Unix). Package-manager installs defer with the right `cargo`/`brew` command; an in-repo `target/release` build is never touched. New `--keep-binary` flag; `install.sh --uninstall` (and `curl … | sh -s -- --uninstall`) run the same teardown for users without the binary on PATH. Previously the binary was left behind with only a printed `rm` hint.
- **Edit-failure recovery context** (#331): when `ctx_edit` can't apply an edit, the error now carries the information the agent would otherwise re-read the file to find. Identical `old_string`/`new_string` are rejected outright; an already-applied edit (`new_string` present, `old_string` gone) is named explicitly; a mismatch surfaces the closest matching line plus a whitespace/indent hint and re-reads the file in full. If the target file **doesn't exist**, lean-ctx searches the enclosing repo and suggests same-named files (moved vs. truly missing); if `old_string` lives in a **different file**, it points there — so the agent re-targets instead of assuming it picked the right file. All searches respect `.gitignore` and are bounded (depth/file/hit caps) so they stay cheap on the error path.
- **Rules version v10 → v11**: all templates (`RULES_SHARED`, `RULES_DEDICATED`, `lean-ctx.mdc`, `lean-ctx-hybrid.mdc`) rewritten with actionable structure. Existing installations auto-upgrade on next `lean-ctx setup` or `lean-ctx update`.
- **MCP instructions include workflow hint**: "Orient(ctx_overview) → Locate(ctx_search) → Read(ctx_read) → Edit → Verify → Record".
- **`bypass_hint.rs` respects shadow_mode**: when active, hints trigger on first native use (not after 5 calls) with stronger "intercepted" wording.
- **Hook redirect messaging**: in shadow_mode, redirected Read/Grep outputs include a header explaining the interception and suggesting direct `ctx_*` usage.

### Fixed
- **Config.toml overwritten on update** (#330): all config writes now use `toml_edit`-based format-preserving merge with atomic backup. User comments, formatting, and unknown keys survive any write. Minimal-diff mode: only non-default values are written (no config bloat).
- **WSL cache hit rate near 0%** (#329): `mtime=None` on DrvFS no longer causes spurious invalidation; path normalization uses `canonicalize` (with verbatim-prefix stripping) for consistent cache keys; `lean-ctx cache stats` now shows both CLI and MCP session cache metrics.
- **Semantic index stuck "warming up" forever** (#249): on a repo whose BM25 index exceeded the disk cap, the index rebuilt from scratch every call. Three fixes: (1) disk persist ceiling decoupled from RAM profile (default 512 MB); (2) `save` reports typed `SaveOutcome` with actionable notes; (3) `ctx_compose` deferred message is state-aware and honest.
- **Test-runner output compressed/truncated, losing pass/fail summaries**: test-runner commands across all ecosystems are now kept verbatim; test-outcome markers survive truncation on every code path.
- **Knowledge store split on Windows** (#325): forward-slash/casing-normalized project hash converges CLI and MCP on a single store. Pre-fix backslash-keyed stores auto-migrate.
- **Parallel `remember` calls clobbered each other** (#326): read-modify-write serialized with in-process + cross-process file locks; atomic temp-file-then-rename saves prevent JSON corruption.
- **Windows `\\?\` prefix from canonicalize**: `normalize_tool_path` now uses `safe_canonicalize` (strips extended-length prefix) and skips root-only paths (`/`, `C:/`).
- **IDE hook integrations check**: doctor now correctly parses hook binary path from minified JSON.
- **Docs-drift gate line-ending agnostic**: Windows CI no longer fails due to CRLF vs LF in generated docs.
- **Benchmark system info detection on Windows**: RAM + CPU detection now works on all platforms.

### Security
- **Shell-command injection in the Node SDK** (CodeQL `js/shell-command-constructed-from-input`): switched to `execFileSync` — no shell interpretation.
- **XSS in VS Code sidebar webview** (CodeQL `js/xss`, 3× high): all dynamic values escaped.
- **Missing origin check on webview message handler** (CodeQL `js/missing-origin-check`): rejects untrusted origins.

## [3.6.26] — 2026-05-30

> **EPIC 6 — Perfect-First (Track A).** A focused correctness + hygiene pass so the
> session/knowledge layer behaves perfectly across projects, the disk footprint stays
> bounded, and cold-start UX is useful immediately.

### Fixed
- **Windows file paths corrupted in tool output** (#324): absolute Windows paths in `ctx_search`/`ctx_compose` output (and every tool using `protocol::shorten_path*`) were rendered with separators stripped (`C:\Users\…\win-build-log.txt` → `CUserszir…win-build-log.txt`) because client render layers (JSON/markdown/terminal) treated backslashes as escape sequences. All displayed paths are now normalized to forward slashes, which are valid on Windows and never escape-interpreted. `shorten_path_relative` also relativizes on slash-normalized strings (component-boundary checked) so it works regardless of the client's separator style.
- **Project root never resolves to HOME / `/` / agent sandbox dirs** (#2361): `best_root_from_uris`, `root_from_env`, `resolve_roots_once`, and the `initialize` handler now reject broad/unsafe directories as a project root via `pathutil::is_broad_or_unsafe_root`, even when a client reports one. This was the root cause of cross-project context bleed (the "HOME mega-session").
- **Cross-project session leakage** (#2362): `SessionState::load_latest()` no longer falls back to the global `latest.json` pointer — it is strictly project-scoped and returns `None` for an unsafe cwd. A new `load_global_latest_pointer()` covers the explicit "show my last session anywhere" UX, and `consolidate_latest()` loads the session for its explicit project root instead of the process cwd.
- **Noise auto-findings suppressed** (#2363): findings whose files live in VCS/dependency/build/cache dirs, virtualenvs, vendored code, home dotfiles (`~/.ssh/config` …), or binary/log artifacts are dropped, and `ctx_search` no longer emits `Found `?` in N files` when no meaningful pattern could be identified. Knowledge recall now boosts exact key/category matches above incidental lexical hits.
- **Cold-start `ctx_overview` returns a useful partial view** (#2365): instead of only "INDEXING IN PROGRESS, try again", it returns detected project markers, a depth-2 gitignore-aware tree, and persistent knowledge while the graph builds in the background.

### Added
- **`lean-ctx sessions doctor [--apply]`** (#2362): detects sessions rooted at a broad/unsafe path and non-destructively quarantines them to `sessions/quarantine/`.
- **Archive FTS disk cap enforcement** (#2364): the archive index (`archives/index.db`) now enforces an on-disk size cap (default 500 MB, override via `LEAN_CTX_ARCHIVE_DB_MAX_MB`) by pruning the oldest entries + VACUUM. A new daemon-safe `storage_maintenance` pass also prunes accumulated quarantined BM25 indexes on startup, and `lean-ctx doctor` gains an **Archive FTS** footprint check.

### Changed
- **Self-healing rules refresh** (#2365): when an outdated rules file is detected on the first tool call of a session, lean-ctx auto-refreshes the rules on disk (off the async runtime) instead of only nudging the user to run `lean-ctx setup`.

## [3.6.24] — 2026-05-30

### Added
- **Knowledge Intelligence — Revision Tracking**: `KnowledgeFact` gains a `revision_count` field. Confirmations increment it, supersedes carry it forward. Output distinguishes "Remembered (revision 1)" vs "Confirmed (revision N, confirmed Nx)" vs "Updated → revision N (previous archived)". Recall shows `rev N` for multi-revision facts. Backward-compatible via `#[serde(default)]`.
- **Knowledge Intelligence — Cross-Key Conflict-Surfacing**: `find_cross_key_similar()` detects semantically similar facts across different keys using Jaccard similarity (threshold > 0.35). When `remember` stores a fact, similar facts from other keys are surfaced in a `SIMILAR FACTS` section with similarity percentages. New `judge` action lets agents resolve pairs as `supersedes`/`compatible`/`unrelated`. `JudgedPair` storage suppresses future noise for already-judged pairs. Recall output annotates facts with `↳ supersedes`/`↳ compatible` relationship arrows.
- **Knowledge Intelligence — Activity-weighted Documentation Nudges**: Replaces the fixed 30-call counter with weighted activity scoring. Edits +4, shell test/build +3, shell +2, new file read +1, cache-hit +0, knowledge/session calls reset to 0. Triggers only when `weighted_score >= 20` AND `significant_tools >= 5` AND no documentation in 8 minutes. Contextual nudge text based on dominant tool type (shell-heavy, edit-heavy, or generic). Fallback 30-call counter preserved as safety net.
- **`bunx` in default shell allowlist** (#310).

### Fixed
- **RAM Guardian measures daemon RSS instead of CLI process** (#317): `lean-ctx doctor` was showing the CLI's ~14 MB instead of the daemon's actual memory. Added `get_rss_bytes_for_pid(pid)` for Linux (`/proc/{pid}/status`) and macOS (`ps -o rss= -p {pid}`). Doctor now reads the daemon PID and reports its real RSS with `(daemon)` label.
- **Orphan MCP processes no longer accumulate RAM** (#317): Added parent-process watchdog (checks every 5s if parent PID changed, exits cleanly when IDE closes) and startup orphan cleanup (kills `lean-ctx` processes reparented to PID 1). Prevents MCP server processes from surviving after IDE restarts.
- **`lean-ctx restart` no longer kills active MCP servers** (#317): `find_killable_pids()` excludes MCP server processes from force-kill during restart, preventing a kill loop where the IDE immediately respawns them.
- **Jira Cloud 410 Gone error** (#315): Migrated from deprecated `GET /rest/api/3/search` to `POST /rest/api/3/search/jql` with `nextPageToken` pagination. Server/Data Center deployments (detected via `JIRA_DEPLOYMENT=server`) continue using `GET /rest/api/2/search`.
- **Provider discovery ignores project root** (#316): `handle_discover()` and `handle_mcp_resources()` now pass `project_root` to `init_with_project_root()` so project-local provider configs are found.
- **Cross-source hints path normalization** (#316): `hints_for_file()` now accepts `project_root` for consistent `graph_relative_key` normalization.
- **JSONC parser tolerates trailing commas** (#311, #312): Prevents parse failures in MCP config files with trailing commas. Also detects duplicate MCP scope registration (workspace + user) and warns.
- **CI structural test relaxations**: Three tests (`scenario_shell_compression_with_saved_tokens_skips_terse`, `raw_shell_skips_all_postprocessing`, `ctx_handoff_create_show_list_pull_clear`) relaxed to check for component presence instead of exact multiline matches, preventing false failures from unrelated code changes.

### Changed
- **Reverted thinking-mode guard** (#313): The `is_thinking_mode_active()` defensive check in PreToolUse hooks was removed — the original Claude Code bug it worked around has been fixed upstream, and the guard could reduce token savings.

### Hardening
- **Graceful error handling**: Replaced potential panics with proper error returns and added logging for silent save failures across knowledge, session, and stats persistence.

### Refactoring
- **CLI dispatch split**: Extracted `dispatch.rs` (1800+ lines) into `analytics.rs`, `network.rs`, and other submodules.
- **Doctor module split**: Decomposed `doctor/mod.rs` (2321 lines) into `common.rs` + `checks.rs`.
- **Editor registry split**: Split `writers.rs` (2580 lines) into a proper module with subfiles.
- **Server dedup**: Consolidated duplicated `has_project_marker` / `PROJECT_MARKERS` logic.

## [3.6.25] — 2026-05-30

### Added
- **Jira Cloud OAuth 2.0 (3LO)** (#318): authenticate built-in and custom Jira data sources via the standard 3-legged OAuth flow instead of Basic auth + API token. New `lean-ctx provider auth jira` runs the interactive flow (loopback redirect, browser consent, accessible-resource/`cloudId` discovery), persists tokens to `~/.lean-ctx/credentials/jira-oauth.json` (`0600`), and auto-refreshes on expiry with refresh-token rotation. `lean-ctx provider list` / `provider logout` round out the surface. The CLI is secret-free: users register their own Atlassian OAuth app and supply the client id/secret via env. Basic auth continues to work unchanged; OAuth is selected automatically when a credential exists or `JIRA_AUTH=oauth` is set.
- **Context-pressure triage in the Context Cockpit** (#249): the Context Manager moves from observation to triage. The *Files in Context* table gains sortable **Used** (re-read count), **Last** (recency), and **Evict** columns — the Evict score combines high token cost + long idle + rarely re-read so the best eviction candidate is one click away. A triage banner maps the live pressure band to a concrete next action (Healthy / Elevated → prefer `map`+`signatures` / High → compress or evict / Critical → evict or handoff pack). The ledger now tracks per-item `access_count` (backward-compatible via `#[serde(default)]`).
- **Offline-first Context Cockpit**: Chart.js, D3 and the UI fonts are now self-hosted (no external CDN), so the dashboard renders identically offline and with large sessions; libs degrade gracefully with an inline notice if one fails to load. Added a dashboard-wide **⌘K / Ctrl+K command palette** with fuzzy search across every view, quick actions (refresh, theme toggle) and full keyboard navigation, plus an embedded favicon and clearer route labels.
- **Friendly first run (UX P0.3)**: running bare `lean-ctx` in an interactive terminal now prints a short quickstart (one obvious next step: `lean-ctx setup`) instead of silently starting the stdio MCP server and appearing to hang. MCP clients (which pipe stdin, not a TTY) and explicit `lean-ctx mcp` are unaffected — they still get the server.
- **`--help` leads with the essentials (UX P1)**: a `GETTING STARTED` block (`setup` / `doctor` / `gain`) now sits at the top of the help, above the full reference — newcomers see the 3 commands they need first instead of scanning 150 lines.
- **Efficiency Epic — resident line-search index**: `ctx_search` now narrows candidate files in memory via a RAM-resident trigram index (`core/search_index.rs`) before reading them, eliminating the per-call directory walk + full-corpus read. Benchmarked **17×–1000× faster** (p50, warm) on a 2000-file corpus with byte-identical recall. Falls back to the walk path when the index is absent/building; opt-out via `LEAN_CTX_DISABLE_SEARCH_INDEX=1`.
- **`ctx_compose` task composer**: one call returns extracted keywords, semantically ranked files, exact match locations, and the most relevant symbol's body inline — replacing the typical search→read→outline→read chain.
- **Benchmark harness** (`rust/benches/efficiency.rs` + `benchmarks/efficiency/`): reproducible latency (p50/p95/p99) + token report comparing the walk and resident-index paths, with a recall-parity assertion.
- **Submodular context packing** (`core/context_packing.rs`): generic greedy max-coverage selector with a provable `1 − 1/e` approximation guarantee (Nemhauser–Wolsey–Fisher). `ctx_compose` now uses it to inline the *non-redundant set* of symbol bodies with maximal keyword coverage under a token budget, instead of just the first match. Budget via `LEAN_CTX_COMPOSE_SYMBOL_TOKENS` (default 600).
- **Search index Bloom tier** (`core/search_index.rs`): monorepos whose trigram postings would exceed the memory budget now build compact per-file Bloom filters (~3× smaller, ~12 bits/trigram) instead of falling back to a full directory walk. Bloom filters have **zero false negatives** (a superset of true matches that `ctx_search` regex-verifies), so recall is identical to the exact tier. The `MAX_FILES` ceiling rose 20k→200k. Proven by a parity fuzz test (Bloom ⊇ postings for every query) + end-to-end recall test.
- **Hebbian co-access graph** (`core/cooccurrence.rs`): a persistent, decaying "files that fire together, wire together" association graph. Files surfaced for the same task strengthen their mutual link (LTP); every update decays all weights (the forgetting curve) and prunes below threshold. Bounded by neighbour/file caps. Becomes an associative retrieval signal over time.
- **Spreading-activation retrieval** (`core/spreading_activation.rs`): ACT-R-style associative ranker. Activation seeds at the files a task names and spreads over the project graph (fan-out-normalised, decaying → provably convergent even on cycles), surfacing structurally-close files lexical search misses. `ctx_compose` runs it over the **union of the static import/call graph and the learned co-access graph** as a budgeted, additive `## Related (associative…)` section (`LEAN_CTX_COMPOSE_GRAPH_BUDGET_MS`, default 1500).
- **Retrieval eval harness** (`tests/retrieval_eval.rs`): a labelled benchmark (queries + relevance judgments) measuring recall@k, MRR and R-precision. Gates the associative ranker as **regression-free** (recall ≥ lexical for every query) with a measured gain (mean recall@3 1.00 vs 0.00 lexical, R-precision 1.00 — it recovers in-cluster files without flooding unrelated ones).

### Hardening
- **`ctx_compose` semantic ranking is wall-time budgeted (H1)**: the only `O(corpus)` stage (a cold BM25 build) runs in a cache-sharing worker thread bounded by `LEAN_CTX_COMPOSE_BUDGET_MS` (default 2500). On overrun the call returns immediately with exact-match + symbol sections and a "warming" note, while the worker finishes warming the resident cache for the next call — the agent loop can no longer stall on a cold index.
- **`ctx_compose` full-path test coverage (H2)**: new `tests/ctx_compose_scenarios.rs` exercises the semantic + exact-match + symbol pipeline on a real mini-corpus and asserts the tight-budget degradation path never stalls.
- **Instruction token cap is priority-aware (H3)**: the compression/output-style guidance suffix is now protected from truncation; only the variable session/knowledge/gotcha blocks are shed when the 1200-token cap is exceeded. Previously a large on-disk session could silently drop the agent's output-style contract.

### Changed
- **`lean-ctx config` points to the simpler surface (UX P2)**: the full config dump now ends with a tip toward `config show` (the 5 high-level knobs) and `config set <key> <value>`, so the 100+ keys no longer feel like the only entry point. The simplified config template (`config init`) now defaults `compression_level = "lite"`, matching the new friendly default.
- **Friendly-by-default output style (UX P0)**: the default `compression_level` is now `lite` (plain-English "concise" guidance — bullets, no filler) instead of `standard` (the symbolic dense style). New users, and anyone opening their generated rules files or inspecting the MCP instructions, now see readable directives rather than the `→ ∵ ∴` vocabulary or `CRP MODE`. The denser symbolic "power modes" stay one line away (`compression_level = "standard" | "max"`, or `LEAN_CTX_COMPRESSION`). This only shapes the model's *prose*; tool-output compression is governed separately and is unchanged — engine efficiency is unaffected.
- **`ctx_read` auto-mode delivers task-relevant bodies**: in `map`/`signatures` mode with an active task, the body of the best-matching symbol is inlined, avoiding a follow-up full read. The `map` heuristic threshold was raised 3000→6000 tokens, and the redundant double disk read in auto-mode selection was removed (cached token counts are reused).
- **Alpha/§MAP symbol substitution is now off by default** for agent-facing output (it traded per-call bytes for agent decode work). CLI/batch pipelines can opt back in with `LEAN_CTX_SYMBOL_MAP=1`.
- **Resident graph-index cache** (`core/graph_cache.rs`): `try_load_graph_index` reuses a deserialized `ProjectIndex` from RAM, instead of re-reading + decompressing + parsing on every graph query.
- **BM25 + graph caches use a `(mtime, size)` content fingerprint** instead of mtime alone: coarse (1–2 s) filesystem mtime could miss a same-second background rebuild; pairing it with the file size catches those rewrites without the cost of hashing a multi-MB index on every per-query freshness check. A rebuild is still picked up immediately within the TTL window.

### Fixed
- **CLI `--help` banner tool count no longer drifts (UX P0)**: the `N MCP tools` figure in the banner is now derived from the live registry (`server::registry::tool_count()`) instead of a hardcoded literal — it read `61` while the README and feature catalog already said `63`. A unit test pins the banner to the registry count so the three figures can never diverge again.
- **Instruction token-cap truncation was O(lines) tokenizations** — `truncate_to_token_cap` re-counted tokens once per line while walking back from the end. On large session/knowledge blocks this is wasteful, and it timed out the coverage job's ptrace-instrumented run. Replaced with a binary search over line boundaries (O(log lines) tokenizations, identical output).
- **CI: `dropin_install_tests` failed on shell-less runners** (regression from #309): the new "is the shell installed?" guard skips writing zsh hooks when no `zsh` binary is present, but the drop-in install tests assert the hooks are written — so they failed on the zsh-less `ubuntu-latest` runner. Added `LEAN_CTX_SHELL_HOOK_FORCE` (`1`/`true`/`all` or a comma list like `zsh,bash`) to force hook installation regardless of detection — useful in minimal containers / custom images, and the seam the tests use to stay host-independent.
- **`ctx_edit` concurrent-edit timeout under multi-agent load** (#320): the global cache write-lock was held across the entire disk I/O of an edit, so a second agent editing a *different* file could time out waiting on the first. Edits now serialize per file via a shared `core::path_locks` registry, perform disk I/O with no global lock, and take the global cache lock only briefly to apply the resulting cache effect. Concurrent edits to different files now run in parallel; edits to the same file remain correctly serialized.
- **Eval harness reported zero recall on Windows**: `recall_at_k`/`mean_reciprocal_rank` compared retrieved paths (OS separator, `\` on Windows) against expected fixtures (`/`) with `ends_with`, so every comparison missed and recall/MRR collapsed to 0 on Windows. Both sides are now normalized to `/` before comparison.
- **Flaky CI on Windows**: made the `ctx_tree` token-savings test deterministic via a synthetic fixture (instead of walking the live repo, whose size + path tokenization varied by platform), and de-flaked `spawned_background_task_doesnt_block_caller` by polling for completion with a generous deadline instead of a fixed sleep.

### Hardening
- **Per-file advisory lock registry** (`core/path_locks.rs`): a process-wide `per_file_lock(path)` shared by `ctx_read` and `ctx_edit` serializes access to the *same* file without contending on a global lock, with bounded GC of unused entries. Lock-ordering documentation (`LOCK_ORDERING.md`) updated accordingly.

### Refactoring
- **`config/mod.rs` split**: extracted the enum surface (`TeeMode`, `TerseAgent`, `OutputDensity`, `ResponseVerbosity`, `CompressionLevel`, `RulesScope`) into `config/enums.rs`, trimming ~250 lines from the module.
- **Premium `lean-ctx wrapped` artifact**: the shareable text summary is now TTY-aware with ANSI colouring, box drawing and a savings sparkline (plain text when piped / `NO_COLOR`).

## [3.6.23] — 2026-05-28

### Fixed
- **`lean-ctx update` creates `.zshenv` on systems without zsh** (#309): `install_all_with_style()` unconditionally wrote shell hooks for both zsh and bash regardless of whether the shell was installed. Now checks for shell binary existence (`/bin/zsh`, `/usr/bin/zsh`, etc.) before installing hooks. Systems with only bash no longer get a spurious `.zshenv`.
- **`lean-ctx config set` rejects valid config keys** (#308): The `config set` command only supported ~12 hardcoded keys while the config schema defines 80+. Implemented a generic schema-based setter (`config/setter.rs`) that validates any key against the ConfigSchema, parses values by type (bool, integer, float, string, enum, string[]), and performs a TOML round-trip with full serde validation. Keys like `proxy_enabled`, `profile`, `compression_level`, `memory_profile` now work as expected.

### Added
- **`lean-ctx gain`: 30-day USD savings** (#307): The dashboard now shows a "past 30 days" line with the estimated dollar savings for the last 30 days, in addition to the all-time total.
- **`lean-ctx gain`: version in Recent Days header** (#307): The "Recent Days" section now displays the current lean-ctx version (e.g. `v3.6.23`) for easier troubleshooting in screenshots.
- **Generic `config set` with enum validation**: Setting enum keys (e.g. `compression_level`) now shows allowed values on invalid input instead of a generic error.

## [3.6.22] — 2026-05-28

### Security
- **Security Hardening V2 (8 phases)**: Comprehensive security audit and hardening across the entire codebase:
  - **Phase 1**: Shell substitution blocking — `eval`, `exec`, `source`, backtick-at-command-position detection
  - **Phase 2**: Role system hardening — parameterized `roles_dir_project_from()`, stricter role validation
  - **Phase 3**: Shell file access controls — lock-timeout secret redaction
  - **Phase 4**: PathJail bypass removal — eliminated `#[cfg(feature = "no-jail")]` escape hatches in tests
  - **Phase 5**: Secret detection unification — consolidated redaction pipeline
  - **Phase 6**: Dangerous flag detection — `--checkpoint-action`, `GIT_SSH=`, `PATH=` override warnings
  - **Phase 7**: HTTP + audit hardening — request validation, audit trail improvements
  - **Phase 8**: Unicode normalization (U+2028/U+2029 → newline), CLI warn-first validation, empty-allowlist gap fix

### Fixed
- **Critical: preToolUse hook DENY loop** (#306): Cursor and other AI agents entered infinite retry loops when lean-ctx hooks returned DENY responses. Eliminated all DENY paths — hooks now always return valid ALLOW JSON, even for disabled mode, invalid payloads, or non-shell tools. Removed `build_dual_deny_output()` entirely.
- **Graph index disappears after upgrade** (user report): CLI `index build-full` and Dashboard used different project root hashes (CLI used raw cwd, Dashboard promoted to git root). Unified `detect_project_root()` to always promote to git root, matching Dashboard behavior. Users in subdirectories now see the same index.
- **`index build-full` incomplete rebuild**: Previously only cleared JSON graph index + BM25. Now also clears `call_graph.json.zst`, `graph.db`, and `graph.meta.json`, then rebuilds the SQLite property graph. Timeout increased from 2min to 5min.
- **Knowledge overflow from `finding-auto` duplicates**: Auto-consolidated findings without a file reference all received the key `finding-auto`, creating hundreds of duplicate facts. The cognition loop's contradiction resolver couldn't keep up, causing `contradict` event spam in the dashboard. Keys are now generated from the finding summary (unique per finding).
- **`cargo build --release` truncated by lean-ctx**: Heavy build commands hit the 8MB/120s output limit. Added adaptive exec limits: build tools (`cargo build`, `npm install`, `docker build`, etc.) now get 32MB/10min instead of 8MB/2min.
- **Disabled hook test expected empty output** (#306 follow-up): Updated `hook_rewrite_disabled_produces_no_output` test to expect ALLOW JSON output instead of empty stdout.

### Added
- **`ctx_tree` / `lean-ctx ls` gitignore toggle**: New `respect_gitignore` parameter (MCP) / `--no-gitignore` flag (CLI) to show files regardless of `.gitignore` rules. Default: gitignore respected (backward compatible). Fixes user report where all-gitignored folders appeared empty.
- **`LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE` env var**: Completely replaces the config-based allowlist for deterministic testing. Unlike `LEAN_CTX_SHELL_ALLOWLIST` (which merges), this overrides everything.
- **37 heavy-command prefixes for adaptive exec limits**: `cargo build/test/clippy`, `npm install/ci`, `docker build`, `go build/test`, `mvn`, `gradle`, `dotnet`, `swift`, `flutter`, `pip install`, `bundle install`, `mix compile`, and more.

## [3.6.21] — 2026-05-27

### Fixed
- **RAM Guardian now performs real cache eviction under memory pressure** (#300): Previously, the `memory_guard` eviction callback only called `jemalloc_purge()`, which returns already-freed pages to the OS but never evicts actual data (SessionCache, BM25 index, etc.). Now a new `EvictionOrchestrator` bridges the RSS-based memory guardian to the `HomeostasisController`, enabling 5-stage graduated eviction: trim compressed outputs → evict probationary entries → unload BM25 index → evict protected entries → emergency full cache clear.
- **`jemalloc_purge()` error handling**: Previously swallowed errors with `let _ =`. Now logs failures via `tracing::debug` for diagnosability.
- **`is_under_pressure()` no longer expensive in hot loops**: Was calling `MemorySnapshot::capture()` (which does `Config::load()` + syscalls) on every invocation in BM25/graph index builders. Now reads a cached `AtomicU8` flag set by the guardian thread — O(1) with zero allocations.

### Added
- **`EvictionOrchestrator`** (`core/eviction_orchestrator.rs`): New module connecting `memory_guard` (RSS monitoring) to `HomeostasisController` (graduated eviction). Holds `Arc` references to `SessionCache` and `SharedBm25Cache`, executes eviction actions with non-blocking `try_read`/`try_write` to avoid stalling the guardian thread.
- **SessionCache eviction methods**: `trim_compressed_outputs()`, `evict_probationary()`, `evict_to_budget()`, `approximate_bytes()`, `trim_shared_blocks()` — enable fine-grained memory reclamation under pressure.
- **BM25 cache management**: `bm25_cache::unload()` drops the cached index (rebuilt on next search), `bm25_cache::memory_usage()` reports current heap usage.
- **Doctor pressure hints**: RAM Guardian check now shows the active pressure level and recommends `memory_profile = "low"` or increasing `max_ram_percent` when under pressure.

## [3.6.20] — 2026-05-27

### Fixed
- **Critical: OnceLock reentrancy deadlock on Linux** (#301): All shell hook commands (`ls`, `cat`, etc.) and `lean-ctx update` hung after upgrading to v3.6.19. Caused by `active_profile_name()` calling `Config::load()`, which re-entered `find_project_root()`'s `OnceLock` via `SessionState::load_latest()` → `normalize_loaded_session()` → `active_profile()`. Fixed by reading the `profile` config key directly from disk (bypassing the full `Config::load()` pipeline) and removing the `active_profile()` call from session normalization.

## [3.6.19] — 2026-05-26

### Added
- **Built-in `passthrough` profile**: No output modification — always full content, zero compression. Use via `LEAN_CTX_PROFILE=passthrough` or `lean-ctx config set profile passthrough`. Includes `default_mode=full`, `crp_mode=off`, `degradation.enforce=false`, `pipeline: all false`, `max_tokens_per_file=10M`, `max_context_tokens=1M`.
- **Persistent profile selection via config.toml**: New `profile` field in config.toml provides a fallback when `LEAN_CTX_PROFILE` env var is not set. Resolution order: env var → config.toml → "coder" default. Set via `lean-ctx config set profile <name>`.
- **Profile config schema entry**: `lean-ctx config show` now displays the `profile` key.

### Fixed
- **`LEAN_CTX_FULL_TOOLS=0` incorrectly treated as ON**: `is_ok()` only checked existence, not value. Now `=0` and `=false` are correctly treated as disabled.
- **`mode=full` returning stubs/deltas in passthrough mode**: `handle_full_with_auto_delta` ignored `no_degrade` and passthrough profiles. Cache stubs and auto-deltas are now skipped when `no_degrade=true` or when the active profile has `default_mode=full` + `crp_mode=off`.
- **MCP schema claimed default mode was `full`**: The `ctx_read` tool description said "default: full" but the actual default was `auto` (resolved by AutoModeResolver). Agents that omitted the `mode` argument got compressed output instead of full content. Schema now correctly states "default: auto".
- **Silent fallback to `coder` profile**: When `LEAN_CTX_PROFILE` pointed to a non-existent profile name, lean-ctx silently fell back to `coder` without any warning. Now logs a `tracing::warn` with the missing profile name and creation instructions.

## [3.6.18] — 2026-05-26

### Added

- **Structured read modes for non-code files** — `ctx_read` mode `map` now produces token-efficient semantic summaries for Markdown (heading outline with nesting), JSON (key structure with types and counts), YAML (key hierarchy), TOML (section headers + top-level keys), and lock files (workspace crate dependency summaries for Cargo.lock, package counts for package-lock.json/yarn.lock/go.sum). Up to 95% token savings vs. full reads on large config and documentation files (#299)
- **Unified AutoModeResolver** — New centralized module (`auto_mode_resolver.rs`) consolidates all auto-mode selection logic that was previously scattered across `mode_predictor.rs`, `context_gate.rs`, and `intent_router.rs`. Single entry point `resolve()` produces a deterministic mode decision with full trace logging. Config/data files like `Cargo.toml`, `package.json` correctly get `full` mode while structured formats (JSON, YAML, TOML, lock files) are routed to `map` mode (#297)
- **GraphProvider unified facade** — `GraphProvider` enum now wraps both `PropertyGraph` (SQLite, symbol-level) and `ProjectIndex` (JSON, file-level) behind a single API. New methods: `file_catalog()`, `file_info()`, `files_in_dir()`, `index_dir()`. All 12 consumer modules (`ctx_overview`, `ctx_graph`, `ctx_impact`, `ctx_symbol`, `ctx_prefetch`, `ctx_preload`, `heatmap`, `task_relevance`, `graph_export`, `dashboard`) migrated from direct `ProjectIndex` usage to `GraphProvider` (#298)
- **Template instructions SSoT** — New `rules_canonical.rs` module provides `canonical_hybrid_instructions()` as the single source of truth for all template instruction generation. `CLAUDE.md`, `lean-ctx.mdc`, and daemon LITM injection all derive from the same canonical table, eliminating instruction drift (#296)
- **CLI graph query commands** — Five new CLI subcommands for querying the code graph without the daemon: `lean-ctx graph related <file>`, `lean-ctx graph impact <file>`, `lean-ctx graph symbol <name>`, `lean-ctx graph context <file>`, `lean-ctx graph status`
- **UTF-8 locale enforcement** — `apply_utf8_locale()` sets `LC_CTYPE=C.UTF-8` as fallback when no UTF-8 locale is inherited from the parent process. Applied to all 5 shell spawn paths (MCP `execute_command_with_env`, CLI `exec_direct`/`exec_inherit`/`exec_buffered`, CLI `passthrough`). Fixes Cyrillic/CJK/emoji M-notation mangling on Linux where Cursor spawns without user shell profile

### Fixed

- **`mode=full` silently downgraded** (#295) — Explicit `mode=full` requests were being overridden by the pressure degradation system and context gate heuristics. `full` mode is now treated as an explicit user intent that bypasses all degradation, bounce tracking, and overlay-based downgrades
- **Shell allowlist blocking legitimate commands** (#294) — Expanded allowlist for Cursor workflows: `$()` command substitution relaxed to only block dangerous patterns (not all subshells), argument-position backticks allowed, `gh` data commands (`pr list`, `issue list`, `api`, `run list`) now compressible instead of passthrough. Prevents agent retry loops on blocked commands
- **Bypass hint false positives** (#292) — Reduced false "you should use lean-ctx tools" warnings when agents legitimately use native Read/Grep for specific use cases. Doctor warnings for config downstream improved
- **`ctx_prefetch` crash without graph** — `ctx_prefetch` now gracefully falls back to direct prefetching of `changed_files` when no graph is available, instead of returning "no graph available" error. Fixes failures in fresh/temporary project directories
- **PropertyGraph race condition on Windows** — Background graph build populates symbol nodes and edges before `file_catalog` entries, causing `ctx_overview` to report "0 files". `open_best_effort` now requires `file_catalog_count > 0` on both the early-return and fallback paths before considering a PropertyGraph as populated
- **UTF-8 locale for shell commands** — MCP server and CLI now set `LC_CTYPE=C.UTF-8` fallback for child processes, fixing Cyrillic and CJK output mangling on Linux

### Changed

- **Token efficiency optimizations** — Comprehensive audit-driven improvements across the engine:
  - BM25 index cache uses `Arc<BM25Index>` instead of `clone()` — eliminates full index copies on every access
  - Stats now adjusted after post-processing (terse, hints) to reflect actual tokens sent to models
  - Cache hit token benchmark uses dynamic `count_tokens()` measurement instead of hardcoded constant
  - Compression floor lowered from 50 to 30 tokens, enabling pattern compression for small outputs
  - `INSTRUCTION_CAP` switched from byte-based (4096) to token-based (1200 tokens) for accurate truncation
  - Graph index scan shares content cache with edge builder, eliminating redundant file I/O
  - Deduplicated `extract_content_hint` into single shared function
  - SessionCache eviction upgraded from segmented LRU to RRF (Reciprocal Rank Fusion) scoring combining recency, frequency, and size signals
- **Dead code removal** — Removed unused `migrate_index_to_property_graph` and `remove_file_catalog` functions after graph consolidation

## [3.6.17] — 2026-05-25

### Added

- **Antigravity CLI 2.0 as separate init target** — `lean-ctx init --agent antigravity-cli` writes MCP config to `~/.gemini/antigravity-cli/mcp_config.json`, distinct from the IDE target (`antigravity`). `lean-ctx init --agent gemini` now auto-configures both Antigravity IDE and CLI paths (#284)
- **Doctor: daemon diagnostics** — `lean-ctx doctor` shows `systemctl --user is-active` state on Linux, warns when `loginctl enable-linger` is not set (required for boot-time start without login), and displays crash-loop log restart count with file path (#288, #289)
- **Crash-loop log path API** — New `crash_loop_log_path()` public function for programmatic access to the MCP server restart history

### Fixed

- **Uninstall completeness (#274)** — `.bak` files containing lean-ctx content, `.lean-ctx.invalid.*.bak` temporaries, `~/.config/lean-ctx` XDG data directory, and project-local `.lean-ctx/` + `.lean-ctx-id` files are now cleaned up. Claude CLI MCP registry entries removed via `claude mcp remove`. `--keep-config` flag preserves MCP configs and rules for reinstall
- **Linux daemon autostart (#288, #289)** — `systemctl --user enable` failures now print actionable error messages with manual fix commands. `is_installed()` checks `systemctl is-enabled` in addition to service file existence. Linger hint displayed when linger is not active
- **Windows paths with spaces** — Shell hook rewrites use `shell_tokenize()` (respects single/double quotes, backslash escapes) instead of `split_whitespace()`. `shell_quote()` properly quotes arguments containing special characters
- **Windows drive-letter grep parsing** — `parse_grep_line()` and `extract_file_from_match()` correctly skip `C:` drive prefix, preventing misinterpretation as file path separator
- **Panic loop-undo (#277, #271)** — `catch_unwind` handler in `call_tool` now calls `record_error_outcome()` on the loop detector, so panicking tools are correctly counted as failures and subject to throttling instead of infinite retry
- **PowerShell detection DRY (#286)** — Replaced inline `shell.to_lowercase().contains("powershell")` check in `shell/exec.rs` with `platform::is_powershell()`, single source of truth
- **Windows CI (#286)** — Fixed `unused variable: quiet` in `daemon_autostart.rs` on non-Unix platforms. Shell wrapping tests now use platform-aware assertions (`expect_wrapped()` helper) that work on both Unix (single-quotes) and Windows (double-quotes with escaping)
- **Index scoping** — Project index scans restricted to project root via `is_safe_scan_root()` guard. `index status` CLI output shows real values instead of nulls
- **Workflow singleton** — Workflow state is now agent-scoped (`workflow-{agent_id}.json`) instead of global `active.json`. Stale workflows auto-cleaned after TTL expiry
- **JSONC UTF-8 safety** — `strip_json_comments` uses `floor_char_boundary`/`ceil_char_boundary` for all string slicing, preventing panics on multi-byte characters in comments
- **`ls -lah` size passthrough** — Human-readable sizes (e.g. `4.0K`, `1.2M`) from `ls -lh`/`ls -lah` are preserved instead of being converted to `0B`
- **MCP server crash hardening** — `Mutex::lock().unwrap()` in hot paths replaced with graceful fallbacks. `memory_guard` uses eviction loop instead of `process::exit`. CSPRNG fallback for dashboard nonce generation
- **Proxy: accept provider API keys on loopback** — Provider routes now accept API keys from local clients (#276)

### Changed

- **Antigravity IDE renamed** — The existing Antigravity target is now labeled "Antigravity IDE" in display names and doctor output, distinguishing it from the new "Antigravity CLI" target

## [3.6.16] — 2026-05-22

### Added

- **First-class OpenClaw agent support** — `lean-ctx init --agent openclaw` writes the MCP server entry to `~/.openclaw/openclaw.json` under `mcp.servers.lean-ctx` (nested JSON structure), installs global rules to `~/.openclaw/rules/lean-ctx.md`, and copies the LeanCTX SKILL.md to `~/.openclaw/skills/lean-ctx/`. `lean-ctx doctor` detects OpenClaw installations. `lean-ctx setup` auto-configures when `~/.openclaw/` exists
- **Context package graph-native architecture (`.ctxpkg` v2)** — New `ContextGraph` data model (`ContextNode`, `ContextEdge`) with activation weights and temporal metadata. Graph-merge composition with conflict detection and contradiction resolution. Ed25519 package signing with hex-encoded key verification. Manifest schema version 2 with scoped package names (`@scope/name`) and conformance levels (Basic, Graph, Cognitive). New `docs/specs/` with JSON schema
- **LeanCTX Custom GPT documentation** — Knowledge base and system prompt prepared for creating a ChatGPT Custom GPT to answer lean-ctx documentation questions (files in `docs/gpt/`, gitignored)

### Fixed

- **`ctx_session` finding panic on em-dash (#272)** — `parse_finding_value` crashed on multi-byte separators like `" — "` (space + U+2014 EM DASH + space = 5 bytes) because the code assumed a 3-byte ASCII separator. Now dynamically determines separator length using `str::len()`. Added 6 regression tests including exact repro with Cyrillic text from the issue report
- **Panic handler returns `isError: false`** — The `catch_unwind` block in the MCP server returned panics as successful tool results (`isError: false`), hiding crashes from AI agents. Now returns `CallToolResult::error` so `isError: true` is set correctly

## [3.6.15] — 2026-05-22

### Fixed

- **MCP crash: "Cannot read properties of undefined (reading 'invoke')"** — Identified and fixed 4 distinct crash vectors that caused intermittent MCP server death on v3.6.14 (#271):
  - 5 `Mutex::lock().unwrap()` calls in the MCP request hot path (`list_tools`, `active_tool_defs`, `ctx_load_tools`) replaced with graceful fallbacks that degrade instead of crashing
  - `memory_guard` hard `process::exit(137)` replaced with 3-attempt eviction loop — server now aggressively reclaims memory but never hard-exits
  - Nested `block_in_place` in `bounded_lock` eliminated to prevent Tokio blocking-pool exhaustion under concurrent tool calls
  - CSPRNG `expect()` in dashboard nonce/token generation replaced with time-based fallback
- **`parse().unwrap()` for SocketAddr** in 2 dashboard routes replaced with direct `SocketAddr::new()` construction
- **`tempfile().expect()` in `ctx_execute`** replaced with graceful error return

### Changed

- **Dashboard: modular route architecture** — Monolithic `context.rs` (617 lines) and `graph.rs` (364 lines) split into focused sub-modules (`context/{core,overlay,diagnostics,aggregated}.rs`, `graph/{deps,callgraph,analysis}.rs`)
- **Dashboard: API consolidation** — 3 new aggregated endpoints (`/api/context-summary`, `/api/context-capabilities`, `/api/context-history`) reduce parallel fetches from 18 to 11 in the Context Manager view
- **Dashboard: shared frontend utilities** — Extracted common rendering logic (gauges, formatters, path shortening) into `lib/shared.js`; TTL-cached API layer in `lib/api.js` with event-based data broadcasting
- **Dashboard: removed dead code** — Deleted legacy `dashboard.html` (3057 lines) and `CockpitContextLayer` component

### Added

- **Context Commander** — New action-oriented dashboard component with context pressure visualization, budget bands, and risk analysis
- **Configurable proxy timeout** — `LEAN_CTX_PROXY_TIMEOUT_MS` env var / `proxy_timeout_ms` in config.toml (default: 200ms) (#270)
- **Dynamic tool categories** — `LCTX_DEFAULT_CATEGORIES` env var / `default_tool_categories` in config.toml to control which tool categories are active by default
- **Global degradation disable** — `LCTX_NO_DEGRADE=1` env var / `no_degrade = true` in config.toml to globally disable all read mode degradation

## [3.6.14] — 2026-05-22

### Added

- **First-class Augment AI agent support** — `lean-ctx init --agent augment` wires up both Augment configuration surfaces: Auggie CLI (`~/.augment/settings.json`) and VS Code extension (`globalStorage/augment.vscode-augment/.../mcpServers.json`, JSON array with stable UUID-keyed upserts). Rules injected at `~/.augment/rules/lean-ctx.md`. `lean-ctx doctor` reports per-surface MCP drift including `"disabled": true` detection. Full cross-platform support (Linux, macOS, Windows). Contributed by @parker-brown-family (#264, #267)
- **Context package system renamed to `.ctxpkg`** — Package format, CLI commands, transport envelopes, and documentation all use `.ctxpkg` extension. Legacy `.lctxpkg` files remain importable for backward compatibility
- **`ctx_multi_read` server-side output cap** — Output capped at 512KB by default (configurable via `LCTX_MAX_MULTI_READ_BYTES`) to prevent MCP client-side truncation. When exceeded, remaining files are skipped with a clear warning (#263)
- **Degradation policy warning** — `auto_degrade_read_mode()` now emits an explicit `⚠ Context pressure` warning when `mode=full` is downgraded to `mode=map` or `mode=signatures`, including the verdict and bypass hint (`start_line=1` or `ctx_compress`) (#262)
- **28 new regression tests** — 14 UTF-8 boundary tests (Cyrillic, CJK, emoji, exact user scenario), 10 degradation verdict tests, 4 `ctx_multi_read` cap tests

### Fixed

- **UTF-8 character boundary panics** — 13 string truncation sites across the codebase now use `str::floor_char_boundary()` / `str::ceil_char_boundary()` instead of raw byte slicing, preventing panics on multi-byte characters like Cyrillic, CJK, or emoji. Affected: `hash_fast` (4096 byte prefix/suffix), curl/cargo/test/just pattern compression, codebook display, gotcha tracker, mcp_compress, ctx_edit preview, ctx_preload hints, dashboard context, tool_defs, stats format, dashboard token masking, cloud email masking. Report and initial PR by @cburgess (#265, #266)
- **Context package system hardening** — Fixed critical `receive --apply` bug, Graph edge import (uses `get_node_by_symbol` instead of `get_node_by_path`), Session/Patterns/Insights import, auto-load caching (prevents re-application), registry validation, HMAC signing (signs all fields including metadata), CLI flag parsing (`--flag value` and `--flag=value`), memory leaks (`.leak()` removed), HTTP response status checking for `send`
- **`lean-ctx update` proxy race condition** — `post_update_rewire()` now restarts the proxy and waits for health before writing `ANTHROPIC_BASE_URL` to Claude Code settings, preventing a connectivity gap (#234)

### Changed

- **Removed `PackageLayer::Artifacts`** — Dead enum variant removed; builder derives layers from actual content
- **Manifest validation expanded** — Checks hex format of hashes, `byte_size > 0`, duplicate layers
- **Import hardened** — File extension check (accepts `.ctxpkg` and `.lctxpkg`), size limit (`MAX_PACKAGE_FILE_BYTES`)

## [3.6.13] — 2026-05-21

### Added

- **Plan mode support for VS Code, Claude Code, and Windsurf** — New `plan_mode.rs` module detects IDE plan/read-only contexts and exposes a curated subset of 12 read-only tools (`ctx_read`, `ctx_search`, `ctx_tree`, `ctx_overview`, `ctx_plan`, `ctx_metrics`, `ctx_compress`, `ctx_session`, `ctx_knowledge`, `ctx_graph`, `ctx_retrieve`, `ctx_provider`). `lean-ctx setup` auto-configures VS Code `planAgent.additionalTools` and Claude Code `permissions.allow` entries. Includes `lean-ctx doctor` plan mode status check
- **MCP `readOnlyHint` tool annotations** — All read-only MCP tools now declare `readOnlyHint: true` in their tool definitions, enabling IDE plan agents to use them without explicit user approval. Write tools (`ctx_edit`, `ctx_fill`, `ctx_delta`, `ctx_handoff`, `ctx_ledger`, `ctx_multi_read`) correctly declare `readOnlyHint: false`
- **Dynamic tool filtering** — New `server/dynamic_tools.rs` module filters exposed tools based on client capabilities. Plan-mode clients only see read-only tools; full-mode clients see all 62 tools
- **GitLab provider** — Built-in GitLab data source provider (issues, merge requests, pipelines) activates automatically when `GITLAB_TOKEN` is set. Joins GitHub, Jira, and PostgreSQL as built-in providers
- **Provider consolidation pipeline (production-wired)** — `apply_artifacts_to_stores()` now runs in a background thread from both `ctx_provider` and `ctx_preload`, indexing provider data into BM25, Graph, Knowledge, and Session Cache. Previously, provider data was only cached — now it's fully searchable, generates cross-source hints in `ctx_read`, and contributes knowledge facts
- **MCP Bridge stdio transport support** — `[providers.mcp_bridges.<name>]` now accepts `command` + `args` for stdio-based MCP servers in addition to HTTP `url`. Bridges register with unique IDs (`mcp:<name>`) and support `resources`, `read_resource`, and `tools` actions
- **Cross-source hints in `ctx_read`** — When reading a file, `ctx_read` now shows related issues, PRs, and external data linked via the graph index (e.g., "Related: [Issue] github://issues/42 — Auth bug")
- **`ctx_semantic_search` external result attribution** — Search results from external providers now show clear type labels: `[Issue]`, `[PR]`, `[Ticket]`, `[Schema]`, `[Wiki]` with full provider URIs
- **`lean-ctx doctor` MCP bridge diagnostics** — New diagnostic section validates configured MCP bridges (URL reachability, config completeness, `auto_index` status warning)
- **`lean-ctx doctor` plan mode check** — Reports whether VS Code and Claude Code are configured for plan mode tool access
- **13 wiring-proof integration tests** — New `provider_wiring_proof.rs` test suite proves every connection in the provider pipeline is functional (consolidation → BM25/Graph/Knowledge/Cache → search/hints/recall). Catches "functional silos" where code exists but isn't connected to runtime
- **10 E2E provider pipeline scenarios** — New `provider_pipeline_e2e.rs` covers full pipeline, cross-source edges, knowledge extraction, MCP bridge registration, multi-source consolidation
- **Plan mode scenario tests** — New `plan_mode_scenarios.rs` with 11 tests covering VS Code settings injection, Claude Code permissions, idempotency, merge behavior, and status detection
- **Power user worksession test suite** — New `power_user_worksession.rs` with 12 end-to-end scenarios simulating a full coding session: initial read → edit → diff → search → knowledge → cache → overview → multi-read → compress → graph → context
- **Lock contention hardening tests** — New `lock_contention_hardening.rs` with 14 scenarios testing bounded lock timeouts, concurrent access, I/O health escalation, and WSL2/NFS environment detection
- **`LEAN_CTX_CLIENT_HINT` env override** — Client capability detection can now be overridden for testing and edge-case environments
- **`lean-ctx doctor` provider status** — Shows active providers and their auth status
- **`lean-ctx doctor` Copilot CLI MCP check** — Separate diagnostic for Copilot CLI MCP configuration (distinct from VS Code MCP)
- **VS Code Extension `.vscode/mcp.json` support** — New standard path with `type: "stdio"` transport
- **`ctx_ledger reset` clears cache delivery flags** — Prevents stale "already delivered" states
- **Knowledge.json size warning** — Warns when knowledge file exceeds 1 MB during load
- **CLI smoke tests** — New integration tests for `gain --json`, `grep`, `ls`, `doctor` commands

### Fixed

- **PowerShell `@args` splatting fails on single commands** — `_lc` function now resolves the native command via `Get-Command -CommandType Application` before invocation, preventing "not recognized" errors when `@args` is used with compound argument strings
- **Fish shell `lean-ctx-off` leaks env var** — `set -e LEAN_CTX_ENABLED` (which removes the var) changed to `set -gx LEAN_CTX_ENABLED 0` (which sets it to 0), matching Bash/Zsh behavior and preventing child shells from re-activating
- **Bash/Zsh `lean-ctx-off` leaks env var** — `unset LEAN_CTX_ENABLED` changed to `export LEAN_CTX_ENABLED=0` for consistent disable semantics across shells
- **Provider init ignores project root** — `ctx_provider` and `ctx_preload` now call `init_with_project_root(Some(root))` instead of `init_builtin_providers()`, enabling config-based provider discovery scoped to the actual project directory
- **Windows CI failure: dead `is_running_in_powershell()`** — Removed unused `#[cfg(windows)]` function that triggered `-Dwarnings` failure on `windows-latest` CI
- **Lock contention in 12 MCP tools** — `ctx_read`, `ctx_edit`, `ctx_delta`, `ctx_fill`, `ctx_handoff`, `ctx_knowledge`, `ctx_multi_read`, `ctx_smart_read`, `ctx_prefetch`, `ctx_ledger`, `ctx_preload`, `ctx_provider` now use bounded lock acquisition with adaptive timeouts instead of indefinite waits
- **Adaptive timeout death spiral** — SlowFs/Degraded environments now get *longer* timeouts (1.5×/2×), not shorter, preventing cascading failures
- **UTF-8 safe truncation** — No more panics on multi-byte character boundaries in hook handlers, `ctx_read`, `ctx_overview`, and server dispatch
- **Cache staleness for missing files** — A missing file is now correctly treated as stale (previously wasn't)
- **`compound_lexer` Unicode** — Switched from byte-based to char-based parsing; fixed `$(…)` subshell detection
- **Windows shell output decoding** — Tries UTF-8 first, then Active Code Page (ACP) as fallback
- **`ctx_read` lock contention** — Returns actionable error message instead of hanging silently
- **`ctx_read` not-found** — Provides actionable hint after retry failure
- **BM25 zstd decompression bomb** — Bounded decode prevents memory exhaustion from malformed compressed index
- **Copilot hooks merge** — No longer overwrites existing hooks during setup
- **`ctx_knowledge` rehydrate time budget** — Capped at 10 seconds to prevent blocking
- **`ctx_execute` respects `GIT_PAGER`/`PAGER`** — Only sets pager env vars when not already set by user

### Changed

- **`providers.auto_index` default is now `true`** — New installations automatically index provider data into BM25/Graph/Knowledge stores. Previously defaulted to `false` (cache-only)
- **MCP tool count** — 61 → 62 (added `ctx_provider`)
- **Tool descriptions** — Updated `pkgdesc` in AUR packages and `description` in Cargo.toml to reflect 62 tools
- **`ctx_read` post-dispatch** — Enrichment bounded to 3s; ledger/eviction/elicitation run async (no longer inline in output)
- **VS Code/Copilot client detection** — Now also recognizes "Visual Studio Code" and "vscode" client identifiers
- **Knowledge rehydrate limit** — Maximum archives reduced from 12 to 4 for faster startup
- **Shell pattern pipeline** — ANSI-stripped output flows through all compressor stages

### Removed

- **Dead code cleanup** — Removed `Config::providers_mcp_bridges()` (unused after `init.rs` refactoring), `hints_from_index()` (unused wrapper), `is_running_in_powershell()` (Windows-only, never called), unused `ProjectIndex` import
- **Inline eviction/elicitation hints in `ctx_read` response** — Now only debug-logged, no longer appended to tool output

## [3.6.12] — 2026-05-21

### Added

- **Context Engine architecture** — Cross-source intelligence engine that unifies file reads, shell output, and external data sources into a single context graph. Includes `ContentChunk` abstraction, `ProviderRegistry`, cross-source edge hints, provider bandit (Thompson sampling), and active inference prefetching
- **Config-based data source providers** — Connect any REST API to lean-ctx without code. Drop a TOML/JSON file into `~/.config/lean-ctx/providers/` and lean-ctx auto-discovers it. Supports 6 auth methods (bearer, API key, basic, header, query param, none), dot-notation response extraction, and project-local providers
- **Built-in providers** — GitHub (issues, PRs, actions), Jira (issues, sprints, projects), PostgreSQL (tables, schema, queries) activate automatically when their env vars are set
- **`ctx_provider` tool** — MCP tool to query any registered data source: `ctx_provider(provider="github", resource="issues", params={...})`
- **MCP Bridge integration** — Connect external MCP servers as data sources via `[providers.mcp_bridges.<name>]` config. Supports HTTP (`url`) and stdio (`command`+`args`) transports. Each bridge gets a unique ID (`mcp:<name>`), supports `resources`, `read_resource`, and `tools` actions. New `mcp_resources` convenience action on `ctx_provider` lists all resources from configured bridges
- **Full provider consolidation pipeline** — All provider data (GitHub, GitLab, Jira, Postgres, MCP bridges, custom REST) now flows through the complete consolidation pipeline into BM25 index, Graph index, Knowledge facts, AND session cache. Background thread applies artifacts to all stores without blocking tool responses
- **`lean-ctx doctor` MCP bridge check** — New diagnostic section validates configured MCP bridges (URL reachability, config completeness, `auto_index` status)
- **`core/io_health` module** — Environment detection (WSL2, NFS, FUSE, sshfs), freeze counter with 60s decay window, adaptive timeout calculation (Fast/SlowFs/Degraded escalation levels)
- **`server/bounded_lock` module** — Self-healing lock acquisition helpers for all MCP tools; returns `None` on timeout allowing graceful degradation instead of indefinite hangs
- **`core/output_sanitizer` module** — Last-pass output filter that detects and removes degenerate CJK runs, symbol floods, and garbled artifacts before output reaches the client
- **`lean-ctx proxy cleanup` command** — Removes stale `ANTHROPIC_BASE_URL` entries from Claude Code/Codex settings when the proxy is disabled
- **`lean-ctx doctor` stale proxy check** — New diagnostic that detects `ANTHROPIC_BASE_URL` pointing to local proxy when proxy is not enabled, with actionable fix instructions
- **Website docs** — New pages: Context Control & Overlays (`/docs/context-control`), Budgets & SLOs (`/docs/budgets-and-slos`), Observatory (`/docs/observatory`)

### Fixed

- **Garbled Chinese characters in Cursor Thought panel** (#257, moshuying report) — Unicode-heavy compression symbols (`→`, `✓`, `✗`, `⚠`, `∴`) confused Cursor's lightweight Thought summarizer model, causing degenerate completion. Three-layer fix: (1) output sanitizer removes CJK artifact lines, (2) Cursor-aware ASCII-safe symbol substitution in compression prompts, (3) TDD shortcuts use ASCII-only replacements (`->`, `ok`, `FAIL`, `WARN`)
- **Stale ANTHROPIC_BASE_URL after proxy disable** (#256) — Users who disabled the proxy were left with `ANTHROPIC_BASE_URL` pointing to `127.0.0.1:4444` in Claude Code settings, causing 401 errors. `doctor --fix` and `proxy cleanup` now auto-detect and remove stale URLs. Proxy 401 responses include actionable JSON error messages
- **Random freezes on WSL2/NFS/FUSE** — Self-healing I/O protection layer: `safe_canonicalize_bounded()` now applies timeout on ALL platforms (was Windows-only); 12 registered tools use `bounded_lock` helpers with adaptive timeouts. System auto-detects slow environments and adapts: 3+ freezes in 60s → degraded mode (ReDev1L report)
- **Proxy auto-starts without explicit enable** — `spawn_proxy_if_needed()` now checks `proxy_enabled == Some(true)` before spawning (webut report)
- **Multi-user port conflict** — Proxy port is now deterministic per-user via UID-based assignment (`4444 + (uid - 1000) % 1000`). Supports three override levels: env var → config key → UID-based auto-port (webut report)
- **Hardcoded port 4444 fallbacks** — All proxy subcommands now use `default_port()` instead of hardcoded 4444
- **BM25 stale-index noise** — Downgraded "stale index detected" log from WARN to DEBUG
- **Windows test failure** — `canonicalize_bounded` test now uses `std::env::temp_dir()` instead of hardcoded `/tmp`
- **Shell allowlist test flake** — Empty allowlist test explicitly sets env var instead of removing it
- **CI documentation check** — Updated MCP tool count 61→62 across all docs to match registry
- **Bare URL rustdoc warnings** — Wrapped bare URLs in doc comments with angle brackets

### Changed

- **`providers.auto_index` default is now `true`** — New installations automatically index provider data into BM25/Graph/Knowledge. Previously defaulted to `false` (cache-only)
- **`ctx_semantic_search` external result formatting** — Provider-sourced results now show clear attribution: `[Issue] github://issues/42 — Auth bug` instead of raw URIs
- **MCP Bridge unique IDs** — Each configured MCP bridge registers with `mcp:<name>` instead of shared `mcp_bridge`, allowing multiple bridges to coexist
- **MCP tool count** — 61 → 62 (added `ctx_provider`)
- **Compression symbols** — TDD shortcuts now use ASCII-safe symbols (`->` instead of `→`, `ok` instead of `✓`) for better downstream model compatibility
- **Rules injection** — Cursor config files (`.cursorrules`, `.cursor/rules/`) now receive ASCII-safe compression prompts; other editors get full Unicode prompts

## [3.6.11] — 2026-05-20

### Fixed

- **Linux proxy restart loop (11258+ restarts)** — When the lean-ctx binary is replaced during runtime (e.g. upgrade), Linux marks `/proc/self/exe` with `(deleted)` suffix. `find_binary()` in the systemd unit generator would write this corrupted path into `ExecStart`, causing systemd to pass `(deleted)` as a CLI argument on every restart. Now uses `resolve_portable_binary()` which strips the suffix. Additionally, the CLI dispatch defensively removes `(deleted)` from args if already present in existing units (webut report)
- **Windows ctx_read hangs** — Session lock acquire and path canonicalization now have bounded timeouts (5s for RwLock, 2s for `canonicalize()`) preventing indefinite hangs on Windows reparse points and network paths (Butetengoy report)
- **Manifest generator uses stale tool_defs** — `gen_mcp_manifest` now reads from `ToolRegistry` (61 tools) instead of static `granular_tool_defs()` (56 tools), ensuring the website manifest always reflects the actual registered tool count

### Changed

- **Context budget auto-escalation** — `pressure_downgrade()` now applies more aggressive mode downgrades based on `ContextPressure`: SuggestCompression downgrades `auto`→`map`, ForceCompression downgrades `full`→`map` and `auto|map`→`signatures`
- **Cache-stable LITM output** — Dynamic session statistics (`ACTIVE SESSION v…`) moved from output prefix to suffix, preserving a stable prefix for LLM prefix-caching compatibility
- **ToolRegistry as SSOT for list_tools** — `list_tools` handler now reads tool definitions from the registry instead of static `tool_defs/`, eliminating schema drift between exposed schemas and handler implementations
- **OnceLock for project root** — `find_project_root()` result cached via `std::sync::OnceLock`, eliminating repeated `git rev-parse` subprocess calls
- **Compaction sync tail-seek** — `find_latest_compaction()` reads only the last 4KB of `context_radar.jsonl` instead of the entire file, bounding I/O for large radar logs

### Removed

- Dead code cleanup: removed unused functions, `#[allow(dead_code)]` attributes replaced with `_` prefixes or deleted across 8 files

## [3.6.10] — 2026-05-20

### Fixed

- **Knowledge recall blocks all agents for 58s** — Embedding engine loading (ONNX model ~25MB) no longer blocks recall. New `try_shared_engine()` returns instantly if model isn't loaded yet; auto/hybrid mode uses non-blocking path. Only explicit `mode=semantic` may trigger model load. Retrieval signal persistence moved to background thread (`save_knowledge_deferred`) so 436KB+ JSON writes don't stall the MCP thread (#ReDev1L report)
- **`start_line=1` forces unnecessary disk re-reads** (#253) — Clients like opencode that always send `start_line=1` no longer trigger mode override to `lines:1-999999` + `fresh=true`. `start_line=1` is now correctly treated as a no-op since line 1 is the default. Only `start_line > 1` activates the lines-mode override
- **Git write-commands incorrectly compressed** — `git commit`, `git push`, `git pull`, `git merge`, `git rebase`, `git cherry-pick`, `git tag`, `git reset` are now classified as verbatim (zero compression). Prevents terse engine from abbreviating subcommands in output that AI agents may re-use (daviddatu\_ report)
- **PowerShell command wrapping** — Single full-command strings (e.g. `git commit -m "..."`) are no longer incorrectly wrapped in `& '...'` quotes on PowerShell, which caused "executable not found" errors
- **Terse dictionary safety** — Removed git subcommand abbreviations (`commit→cmt`, `branch→br`, `checkout→co`, `merge→mrg`, `rebase→rb`, `stash→st`) from the GIT dictionary to prevent output corruption

## [3.6.9] — 2026-05-19

### Added

- **Context IR hot-path lineage** — Every tool call now records source kind, tokens, duration, and content excerpt into the Context Intermediate Representation for full lineage tracking
- **Plugin-ready traits** — Extracted `CompressionPattern` trait (patterns/) and `ContextProvider` trait (providers/) for future plugin extensibility
- **Pytest verbose compression** — Dedicated pattern for `pytest -v` output: consolidates per-test lines, strips fixtures/collection/metadata, preserves tracebacks and test identifiers (#251, contributed by @sisyphusse1-ops)
- **Active Context Gate** — Pressure-based auto-downgrade: when context utilization exceeds 75%, reads are automatically downgraded (full→map, map→signatures). Φ scores now computed with real task context from SessionState

### Fixed

- **Workflow persistence blocking reads after crash** — Workflows inactive >30 minutes are now auto-expired on load and at runtime. Read-only tools (`ctx_read`, `ctx_multi_read`, `ctx_smart_read`, `ctx_search`, `ctx_tree`, `ctx_session`) always pass through the workflow gate regardless of state
- **Misleading cache-hit message** — Changed "Already in your context window" to neutral `[unchanged, use cached context]` with hint about `fresh=true` for forced re-read. Prevents confusion when server-scoped cache returns hits for files not seen by the current agent
- **Unable to clear context pressure (#244)** — `ctx_ledger(action=reset)` now correctly clears all ledger state
- **Windows CI CRLF assertion** — Normalized line endings in `include_str!` test assertions
- **Flaky CI tests** — Serialized environment-variable tests (`serial_test`), fixed anomaly persistence debounce race, relaxed attention stress threshold for shared runners

### Changed

- **ARCHITECTURE.md** — Fixed documentation drift: updated tool counts, Context IR description, dispatch flow diagram, removed references to non-existent files
- **CONTRACTS.md** — Restructured as "LeanCTX Protocol Family" with Extension Contracts section for future plugin interfaces
- **README.md** — Conversion-optimized structure with better hero section, install commands, and social proof

### Tests

- 18 new scenario tests for workflow staleness + cache message fixes (`bazsi_reported_scenarios.rs`)
- 4 new workflow staleness/passthrough tests (`workflow_done_scenarios.rs`)
- Context IR hot-path recording tests, trait implementation tests, doc integrity tests (`hardening_ir_traits.rs`)
- Adversarial safety tests for pytest xfail/xpass and test name preservation

## [3.6.8] — 2026-05-18

### Added

- **Post-RRF Reranking Pipeline** — New `core/search_reranking.rs` module with 5 scientifically-grounded signals applied after Reciprocal Rank Fusion:
  - **Query-Type Classifier** (SACL, EMNLP 2025) — Auto-detects Symbol / Natural Language / Architecture queries and adjusts BM25:Dense weight ratio (1.4:0.6 / 1.0:1.0 / 0.6:1.4)
  - **Definition Boost** (CoRNStack, ICLR 2025) — Symbol queries boost defining chunks (struct/function/class) by 3x via ChunkKind + AST keyword matching
  - **File Coherence Boost** (SweRank, 2025) — Files with multiple relevant chunks get a normalized 20% score boost
  - **Noise Penalties** (CoRNStack) — Test files (0.3x), legacy/compat (0.3x), examples (0.3x), barrel/index (0.5x), type stubs (0.7x) are automatically down-ranked
  - **MMR Diversity** (Carbonell & Goldstein, SIGIR 1998) — File-saturation decay prevents single-file dominance in top-k results via greedy reselection
- **BM25 Path-Enrichment** (SACL, +7–12.8% recall) — File stem and parent directory are doubled into BM25 document content, enabling path-aware queries like "auth handler"
- **`find_related` action** in `ctx_semantic_search` — Chunk-based similarity search: given a file path + line, finds semantically related code chunks across the project

### Fixed

- **Workflow "done" state blocks all tools permanently** — `handle_complete` now clears the workflow file (terminal state) instead of persisting it. Added safety nets: gate auto-clears stale "done" workflows, `list_tools` no longer restricts visibility in terminal state, and `ctx_handoff` pull/import refuses to restore "done" workflows
- **`ctx_read` lines:N-M mode hangs on large files** — Line-range reads no longer trigger expensive `build_graph_related_hint` and `find_similar_and_update_semantic_index` computations (fast path bypasses all hint generation)

### Tests

- 15 new reranking scenario tests covering symbol boost, NL queries, test penalization, diversity, coherence, legacy/compat, type stubs, architecture classification, barrel files, qualified symbols, and multi-signal interaction
- 10 new workflow scenario tests validating stop/clear/complete/handoff behavior with "done" state

## [3.6.7] — 2026-05-18

### Added

- **3-Layer Model Registry** (#242) — Replaced hardcoded substring matching for model context windows with a data-driven registry system:
  - **Bundled registry** (`data/model_registry.json`) — compiled into binary, covers 40+ models
  - **Local registry** (`~/.config/lean-ctx/model_registry.json`) — auto-updated via `lean-ctx update`
  - **User overrides** (`[model_context_windows]` in config.toml) — highest priority
  - Supports exact match, prefix match (e.g. `gpt-5.5-0513` matches `gpt-5.5`), and family fallback
  - GPT-5.5: 1,048,576 | GPT-4.1: 1,047,576 | Gemini: 1,048,576 | Claude: 200,000

- **ctx_shell `env` parameter** (#241) — New optional `env` object in tool schema lets LLMs explicitly pass environment variables to child processes. Useful for agent runtime vars (e.g. `CODEX_THREAD_ID`).

- **Agent env auto-forwarding** (#241) — `CODEX_*`, `CLAUDE_*`, `OPENCODE_*`, `HERMES_*` prefixed environment variables from the parent MCP server process are automatically forwarded to child commands. Solves the problem of agent hosts starting MCP servers with a stripped environment.

- **PathJail container bypass** (#240) — PathJail automatically disables in Docker/Podman containers via `is_container()` detection. Manual opt-out via `path_jail = false` in config.toml or `LEAN_CTX_NO_JAIL=1` env var.

- **Copilot CLI support** (#243) — Dedicated `CopilotCli` config type that writes to `~/.copilot/mcp-config.json` with the correct format (`mcpServers` key, `"type": "local"`, `"tools": ["*"]`). Copilot CLI is now a separate target from VS Code.

### Fixed

- **Benchmark honesty** — Structural compression modes (`map`, `signatures`) are now excluded from "best mode" ranking for non-code file types (Markdown, JSON, CSS, HTML, YAML, XML). These modes extract code structures (functions, classes) and are not applicable to data/markup files. Previous reports showed misleading 100% savings for JSON and 99.9% for Markdown; corrected to 0.5% and 5.6% respectively.

- **Copilot CLI MCP config** (#243) — `lean-ctx init --agent copilot` now writes to `~/.copilot/mcp-config.json` (not VS Code's Application Support path). Uses `"mcpServers"` container key, `"type": "local"`, and includes required `"tools": ["*"]` field per [GitHub docs](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers).

- **PathJail CWD fallback** (#240) — Project root derivation now includes a guarded CWD fallback with `is_broad_or_unsafe_root()` protection. Differentiated error messages explain why a path was rejected and how to fix it.

- **Invalid JSON config handling** — All IDE config writers now use text-based injection for invalid JSON files instead of destructive overwrites. Original files are preserved; users get clear instructions on how to fix syntax errors.

### Changed

- **VS Code / Copilot split** — The combined "VS Code / Copilot" target is now two separate targets: "VS Code" (`agent_key: vscode`) and "Copilot CLI" (`agent_key: copilot`). Existing VS Code configurations are not affected.

## [3.6.6] — 2026-05-17

### Added

- **ABC-Inspired Agent Hardening** — 5-phase enforcement inspired by the Agentic Brownfield Coding project:
  - **Bypass Hints** — Detects when agents use native Read/Grep instead of lean-ctx tools and emits a single-line reminder with cooldown logic. Configurable via `bypass_hints` config key or `LEAN_CTX_BYPASS_HINTS` env var (modes: `gentle`, `firm`, `off`).
  - **Tool Description Enhancement** — All core tool descriptions now explicitly state "replaces native X" to guide AI agents directly from the MCP schema.
  - **Rules Deduplication** — Removed redundant tool mapping tables from injected rules. Tool descriptions now carry the mapping, rules focus on mode selection, anti-patterns, and editing workflow.
  - **`lean-ctx harden` CLI** — Activates strict enforcement mode (`LEAN_CTX_HARDEN=1` in MCP configs). Optionally denies Bash in Claude Code's `permissions.deny`.
  - **`lean-ctx export-rules` CLI** — Exports high-confidence knowledge facts as editor-native rules (MDC for Cursor, `AGENTS.md`, `CLAUDE.md`).

### Fixed

- **`git status --porcelain` truncation** — Shell compression no longer truncates `git status` output when it doesn't match specific section parsing (e.g. `--porcelain`, `--short` flags). Developers now always see full status information.
- **`init --agent` rules injection** — Global rules and skill file are now correctly injected. Fixed data dir split causing empty `gain` field in responses. (#238, #239)
- **Integration test alignment** — `rules_consistency` and `rules_inject` tests updated to match new deduplicated rule content.

## [3.6.5] — 2026-05-17

### Fixed

- **CLAUDE_CONFIG_DIR support** — MCP instructions and rules file paths now respect `CLAUDE_CONFIG_DIR` env var instead of hardcoding `~/.claude`. Absolute paths under `$HOME` are collapsed to tilde form for display. Includes integration tests. (#235, contributed by @cburgess)
- **OpenCode rules location** — Rules are now written to `~/.config/opencode/AGENTS.md` (SharedMarkdown fenced section) instead of `~/.config/opencode/rules/lean-ctx.md` which OpenCode never loads. Doctor check and uninstall updated accordingly. (#237)
- **Linux CI warnings** — Fixed `unreachable_pub` in Landlock module, `borrow_as_ptr` in syscall wrappers, `unnecessary_wraps` on `remove_linux_scheduler`, and `unused_variables`/`dead_code` for platform-gated items.
- **MCP Resource Notifications** — `notifications/resources/updated` sent to subscribed clients after significant ledger changes (new entries, pressure threshold crossings). Enables proactive context refresh in supporting IDEs.
- **`ctx_load_tools`** — New tool for explicit category management (load/unload/list). After each change, `notifications/tools/list_changed` is sent to subscribed clients so they re-fetch the tool list.
- **`notifications/tools/list_changed`** — Outbound notification sent after dynamic tool category load/unload via `ctx_load_tools`. Clients automatically re-fetch the tool list.
- **MCP Peer Storage** — Server stores the rmcp `Peer<RoleServer>` from `initialize()` for bidirectional notification delivery.

## [3.6.4] — 2026-05-17

### Added

- **Cognition Loop** — Hebbian-inspired 8-step background knowledge reorganization: seed promote, structural repair, fidelity check, lateral synthesis, contradiction resolution, co-retrieval strengthening, decay, and compaction. Trigger manually via `ctx_knowledge action=cognition_loop` or configure automatic runs with `autonomy.cognition_loop_interval_secs`. (#cognition-loop)
- **Knowledge Archetypes** — Typed knowledge nodes with 10 archetypes (Architecture, Decision, Gotcha, Convention, Dependency, Pattern, Workflow, Preference, Observation, Fact). Archetypes influence salience-based ranking and are auto-inferred from category names. Fully backward-compatible via `#[serde(default)]`.
- **Fidelity Scoring** — Two-tier quality metric (structural + semantic) for knowledge facts. Structural fidelity is computed deterministically from source presence, confirmation count, confidence, freshness, and feedback. Fidelity scores influence recall ranking.
- **Hebbian Edge Strengthening** — Knowledge relation edges now carry `strength` (0.0–1.0) and `decay_rate` fields. Co-retrieved facts strengthen their edges via a saturating Hebbian formula. Exponential time-based decay and threshold-based pruning keep the graph lean.
- **Cross-Agent Knowledge Bridge** — Controlled sharing of high-confidence facts between agents. Only publishable archetypes (Architecture, Convention, Decision, Dependency, Gotcha) with confidence ≥ 0.8 can be shared. Imported facts carry provenance tracking and a 10% trust penalty. New actions: `bridge_publish`, `bridge_pull`, `bridge_status`.
- **Auto-Update Scheduler** — Native `lean-ctx update --schedule` with OS-specific schedulers (macOS LaunchAgent, Linux systemd/cron, Windows Task Scheduler). Subcommands: `--schedule off`, `--schedule status`, `--schedule notify`, `--schedule 12h`. Default is OFF — requires explicit opt-in.
- **Setup Auto-Update Opt-In** — Interactive `lean-ctx setup` now asks whether to enable automatic updates (Step 9/11). Respects user freedom: default is N, non-interactive mode never enables, and the setting is always changeable via CLI or config.
- **`--quiet` flag for updater** — `lean-ctx update --quiet` suppresses output when already current. Used by the auto-update scheduler to avoid noisy cron/LaunchAgent logs.
- **Session Update Notification** — One-shot per-session update hint via `session_update_hint()`. Returns a single notification when a newer version is available, then stays silent for the rest of the session.
- **`[updates]` config section** — New config block with `auto_update` (default false), `check_interval_hours` (default 6), and `notify_only` (default false). Overridable via `LEAN_CTX_AUTO_UPDATE`, `LEAN_CTX_UPDATE_INTERVAL_HOURS`, `LEAN_CTX_UPDATE_NOTIFY_ONLY` env vars.

### Security

- **Constant-time token comparison** — Proxy bearer token validation uses `subtle::ConstantTimeEq` to prevent timing side-channels.
- **Header forwarding allowlist** — Proxy no longer blindly forwards all headers; only an explicit `FORWARDED_HEADERS` allowlist is passed through.
- **Secret detection** — Regex-based scanning for API keys, tokens, and credentials in file reads and tool output. Integrated into `io_boundary` as a pre-read filter.
- **Shell allowlist** — Configurable command allowlist for sandboxed shell execution with `extract_base_command` validation.
- **Audit trail** — SHA-256 chained audit log for security-relevant events (tool denials, cross-project reads, capability checks). CLI: `lean-ctx audit`.
- **Capability-based access control** — `Capability` enum with per-tool requirements and per-role grants. Tools are denied if the agent's role lacks the required capabilities.
- **macOS Seatbelt sandboxing** — `sandbox-exec` based process isolation for shell commands on macOS.
- **Linux Landlock sandboxing** — Landlock LSM-based filesystem restrictions for shell commands on Linux.
- **OWASP Agentic Top 10 alignment** — Module mapping lean-ctx security features to the OWASP Top 10 for Agentic Applications.
- **Signed handoff bundles** — Ed25519 signatures on agent handoff bundles for provenance verification.
- **PathJail expanded** — 16 path-like parameter keys now validated (including `destination`, `old_path`, `new_path`, `config_path`, `output`).
- **Reference store** — Large tool outputs (>4000 chars) stored server-side with opaque IDs to prevent context bloat.
- **Proxy metrics** — Atomic counters for request totals, tokens saved, and bytes compressed.

## [3.6.3] — 2026-05-17

### Fixed

- **Windows PowerShell `lean-ctx -c` quoting bug** — Dynamic aliases (npm, pnpm, etc.) failed on PowerShell 5 with `ObjectNotFound` error because `@args` inside double-quoted strings was splatted instead of treated literally. Fixed by extracting the script block body into a variable with backtick-escaped `@args`.
- **`commit`→`cmt` string mangling** — The terse compression dictionary replaced "commit" inside compound words (`pre-commit`), quoted strings, and colon-delimited contexts. Fixed `replace_whole_word` to use a proper word-boundary function that treats hyphens, underscores, and quotes as word characters.
- **Dashboard Live Observatory "0 tokens" display** — Non-file tools (e.g. `ctx_search`, `ctx_shell`) showed "Original · 0 tokens" when clicking "Compare". Now shows a token savings summary bar for non-file operations and reserves the full before/after text comparison for file reads (`ctx_read`, `ctx_multi_read`).

## [3.6.2] — 2026-05-16

### Fixed

- **Token Buddy broken ASCII art** — Buddy sprite displayed as comma-separated single line instead of multi-line ASCII art. Root cause: `ascii_art` (a JSON array) was passed directly to the HTML escaper without joining with newlines. Fixed in `cockpit-overview.js`.
- **Context Ledger not recording MCP reads** — Files read via the MCP server path were not appearing in the "Files in Context" dashboard section. Root cause: the dispatch layer was checking the wrong data directory (`~/.lean-ctx` vs `~/.config/lean-ctx` set via `LEAN_CTX_DATA_DIR`). Ledger recording now correctly happens in `dispatch/mod.rs` after tool execution.
- **Config schema validation rejecting `ide_paths` and `lsp` sections** — Users configuring per-IDE allowed paths or LSP binary overrides received "Unknown key" warnings. Added `ide_paths` (dynamic keys), `lsp` (with language-specific entries), and top-level `project_root` to the schema.

### Changed

- **Dashboard navigation icons** — Replaced ASCII-art navigation indicators (`[~]`, `[##]`, `[<>]`, etc.) with clean SVG outline icons (Feather-style). Each view now has a distinct, professional icon.
- **"Index required" guidance** — Dependencies, Call Graph, and Symbols pages now show a clear empty state with instructions to run `lean-ctx index build` when no index data is available, instead of generic "loading" or error messages.

## [3.6.1] — 2026-05-16

### Added

- **`lean-ctx config apply`** — New command to validate config, restart daemon/proxy, and run safety checks (RAM limits, session count). Alias: `config reload`. (#231)
- **`ctx_multi_read fresh` parameter** — New `fresh: bool` argument to bypass cache and force full re-read for all paths. Essential for subagents that don't share the parent's cache. (#230)
- **Per-IDE allowed paths** — Configure project-specific file access restrictions per IDE integration. (#221)
- **Response verbosity control** — Configurable verbosity levels for tool responses. (#222)
- **LSP graceful degradation** — LSP server now degrades gracefully when tree-sitter parsing fails, with `doctor` health check and `config.toml` configuration support.
- **FTS5 archive search** — Full-text search over archived context entries using SQLite FTS5 for fast historical queries.
- **Project root configuration** — Explicit `project_root` config option for multi-project workspaces.
- **`lean-ctx restart` command** — Restart all lean-ctx processes cleanly without manual kill.
- **Zed `ctx_edit` guard** — Prevents accidental edits in Zed when file is not in project scope.
- **`LEAN_CTX_SAVINGS_FOOTER` env var** — Shows compression savings in shell output when enabled.
- **`enable_wakeup_ctx` config option** — Control whether background context wakeup is active.

### Fixed

- **pi-lean-ctx disabling built-in tools** (#232) — Pi extension now runs in "additive" mode by default, preserving Pi's native tools (`read`, `bash`, `ls`, `find`, `grep`). Set `LEAN_CTX_PI_MODE=replace` for the old behavior that disables overlapping builtins.
- **`ctx_multi_read` stale cache** (#230) — Subagents that inherit the parent's process but not its cache state can now use `fresh: true` to bypass stale entries.
- **`ctx_read` deadlock with concurrent subagents** (#226, #229) — Reduced lock contention by minimizing `blocking_write()` scope and adding a timeout guard. Prevents async runtime contention when multiple agents read the same file simultaneously.
- **Zombie process management** — Complete overhaul: `lean-ctx stop` now unloads macOS LaunchAgent/Linux systemd service before sending SIGTERM, distinguishes MCP server/hook child processes (which are not killed, as IDEs respawn them), and cleans up reliably without requiring a reboot.
- **XSS in cockpit-live.js** — Sanitized user-controlled strings in dashboard output to prevent script injection.
- **MCP config not updated after `lean-ctx update`** (#224) — `settings.json` / MCP config now auto-refreshes after binary update so IDEs pick up new tool versions immediately.
- **`ctx_shell` missing compression info** (#225) — `renderCall`/`renderResult` properly delegated to `baseBashTool`; compression savings now visible in Pi agent output.
- **Windsurf hooks installation** — `hooks.json` is now installed regardless of the `--global` flag, fixing cases where Windsurf-specific hooks were silently skipped.
- **Windows LSP URI handling** — Correct `file:///C:/` URI format on Windows; prevents "file not found" errors in LSP diagnostics.
- **Opencode backup integration** — Fixed configuration backup path resolution for opencode IDE.
- **Dashboard "Context Handles" empty** — Frontend correctly maps API fields (`ref_label`, `source_path`, `pinned` as string→boolean).
- **Chat messages/logs ordering** — Newest entries displayed first across all dashboard sections.
- **CI stability** — Test timeout increased to 90 min for Windows cold-cache; `--lib` flag for macOS tests prevents daemon hangs; `msys2/setup-msys2` action pinned to prevent supply-chain attacks; background index build skipped when `LEAN_CTX_DISABLED` is set.

### Changed

- **Dashboard redesigned** — Three separate tabs (Live Context, Items, System) consolidated into a single vertically-scrolling page. Eliminates duplicate information, provides a unified view with stat grid (IDE, Context %, Files, Saved tokens, Tool Calls), estimated context window, context handles, chat history, and recent activity — all on one page.
- **Proxy status simplified** — Removed confusing standalone "Proxy" cell. Status now integrated into the "IDE" cell showing hook tier (e.g., "Full (9/9)" for Cursor Tier 1). Cursor users no longer see misleading "Proxy: Idle" since Cursor does not route through external proxies.
- **Model detection improved** — Background models (flash, mini, haiku, nano, small) are now ignored when persisting detected model, ensuring only the primary user-facing model is stored. Model detection staleness window extended from 1h to 24h.
- **`model_context_window` consolidated** — Redundant branches merged: Claude/O-series → 200k, GPT/Codex/DeepSeek → 128k, Gemini → 1M, Mistral/Codestral → 256k.
- **Pi extension dependencies** — Deprecated `@mariozechner` libraries replaced with `@earendil-works` packages. (#220)
- **Clippy clean** — All warnings resolved across the entire codebase (`needless_pass_by_value`, `if_same_then_else`, `uninlined_format_args`, `redundant_closure`, `map_unwrap_or`, `collapsible_if`).
- **Documentation** — Tool counts harmonized to 56+ across all docs; LSP and FTS5 features documented.
- **Codebase streamlining** — UX hardening pass: clearer error messages, reduced log noise, faster startup.

## [3.6.0] — 2026-05-14

### Added

- **Context Radar** — Full budget breakdown showing system prompt (rules), user messages, agent responses, lean-ctx tools, other MCP tools, native reads, and shell output as percentage of context window. Compaction-aware: distinguishes current-window metrics from cumulative session totals. Exposed via `ctx_session budget`, dashboard API, and `ctx_radar` tool.
- **Unified Context Intelligence** — IDE hooks across Cursor (10 observe events including afterMCPExecution, postToolUse, afterShellExecution, beforeReadFile, afterAgentResponse, afterAgentThought, beforeSubmitPrompt, preCompact, sessionStart, sessionEnd), Claude Code (PostToolUse, UserPromptSubmit, Stop, PreCompact, SessionStart/End), Windsurf (post_mcp_tool_use, post_run_command, post_cascade_response, pre_user_prompt), and Codex/Gemini. Captures ~90% of context traffic automatically — no user configuration needed.
- **LLM Proxy Introspection** — Request analyzer (`introspect.rs`) for Anthropic, OpenAI, and Gemini APIs with `RequestBreakdown` struct providing exact system prompt tokens, message tokens, tool definition tokens, and image counts. Ground-truth token counts when proxy is active.
- **Rules Scanner** — Scans `.cursorrules`, `.cursor/rules/*.mdc`, `AGENTS.md`, and global rules at MCP server start. Counts tokens per file and provides `RulesTokens` estimate for system prompt budget.
- **Windows Named Pipe IPC** — Reliable daemon IPC using `WaitNamedPipeW` for proper pipe existence checks (replaces broken `fs::metadata`), retry loop with 50ms backoff on `ERROR_PIPE_BUSY` and `NotFound`, stderr fallback to `inherit()` instead of `null()` for visible errors. 5 new Windows-specific unit tests. (PR #219)
- **Dashboard Context Cockpit** — Complete redesign with tab-based UI: Overview (KPIs, pressure gauge), Budget Radar (stacked bar chart with legends), Context Items (active files with compression stats), Runtime (control plane, dynamic tools, bounce detection), and Timeline (recent events). Each section includes user-friendly explanations.
- **Bounce Detection** — New `bounce_tracker` module detects when compressed reads are immediately followed by full re-reads ("bounces"), tracks wasted tokens per file extension, and adjusts savings metrics to report honest numbers.
- **Context Gate** — New `context_gate` module provides pre-dispatch mode override (bounce-prevention, intent-target, graph-proximity, knowledge-relevance) and post-dispatch recording with eviction/elicitation hints for every read operation.
- **MCP Resources** — 5 subscribe-capable resources (`lean-ctx://context/summary`, `/pressure`, `/plan`, `/pinned`, `/bounce`) expose context state to supporting IDEs without tool-call overhead.
- **MCP Prompts** — 5 slash commands (`/context-focus`, `/context-review`, `/context-reset`, `/context-pin`, `/context-budget`) for IDE-native context management.
- **Elicitation** — Rate-limited context decision suggestions (max 1x per 20 tool calls) for pressure, large files, and budget exhaustion with graceful fallback hints.
- **Dynamic Tools** — 6 tool categories (core, arch, debug, memory, metrics, session) with on-demand loading via `tools/list_changed` for clients that support it; Windsurf 100-tool limit handled automatically.
- **Client Capability Detection** — Runtime detection of 9 IDE clients with Tier 1–4 classification; dynamically gates MCP resources, prompts, elicitation, and dynamic tools based on client support.
- **Dashboard Control Plane** — 4 new API endpoints (`/api/context-bounce`, `/api/context-client`, `/api/context-pressure`, `/api/context-dynamic-tools`) with Runtime Control Plane panel showing IDE indicator, pressure gauge, bounce stats, and dynamic tool status.
- **Hybrid Enforcement** — Automatic rewrite of `rg`, `ls`, and `find` commands to lean-ctx equivalents via shell hooks, ensuring all reads go through the cached MCP path.
- **Silent-by-default** — All meta output (budget warnings, session hints, compression stats) gated behind `protocol::meta_visible()`, keeping tool results clean for programmatic consumers.
- **Pi Extension improvements** — Builtin tool replacement: ctx_ versions automatically disable overlapping Pi builtins. MCP bridge cleanup removes redundant CLI tool prefix filter. (PR #216)

### Fixed

- **Budget not resetting on `/new`** — `BudgetTracker` and `context_radar.jsonl` now reset on MCP `initialize` (the real session boundary when IDE starts a new connection), not on task change. SharedSession mode correctly skips reset to avoid killing counters for other clients in daemon setups.
- **Tool preference lost after compaction** — LITM `end_block` now includes tool-preference reinforcement line (`ctx_read>Read ctx_shell>Shell ...`) for sessions with 3+ tool calls, surviving IDE compaction.
- **`ctx_read` hang in subagents** (#215) — Removed redundant `tokio::task::block_in_place` call and minimized `cache_lock.blocking_write()` scope to prevent async runtime contention.
- **`ctx_read` 57s on large files** — Introduced 32KB content limit for semantic indexing and 200-entry cap for similarity search, reducing 64KB Cyrillic markdown from 57s to 0.59s.
- **Windows `cargo-binstall` failures** (#213) — Development-only binaries (`gen_mcp_manifest`, `gen_tdd_schema`) moved from `[[bin]]` to `[[example]]` so `cargo install` and `cargo-binstall` skip them.
- **Windows `doctor` bashrc false positive** (#214) — `is_active_shell_impl` now checks `BASH_VERSION` on Windows before flagging `.bashrc` as outdated.
- **Windows `env.sh` bash validation** — Skip `bash -n` syntax check on Windows where backslash paths are invalid bash.
- **Windows named pipe `pipe_exists_true` test** — Changed `#[test]` to `#[tokio::test]` since `ServerOptions::create()` requires a Tokio runtime context.
- **macOS process hangs on update** — Atomic binary replacement prevents corruption during self-update.
- **`env.sh` for-loop syntax error** (#212) — Removed `2>/dev/null` from `for _lf in` loop that broke POSIX shell parsing.
- **JSONL audit trail lost on reset** — Session reset and new session events now rotate `context_radar.jsonl` to `.prev` instead of truncating.

### Changed

- **Logging defaults** — CLI default remains `warn` (clean output); daemon/MCP mode defaults to `info`. Early `init_logging()` in `run()` skips MCP entry paths so `init_mcp_logging()` can set its own level.
- **Radar memory cap** — `load_events()` caps at 50,000 entries (keeps last N), preventing unbounded memory growth in extremely long sessions.
- **LITM compaction threshold** — Tool-preference injection in `end_block` lowered from >10 to >3 tool calls, matching typical compaction timing in Claude Code (5–8 calls).
- **`lettre` advisory ignored** — RUSTSEC-2026-0141 (Boring TLS hostname verification) added to `deny.toml` and `audit.toml` ignore lists; lean-ctx uses rustls, not Boring TLS.

## [3.5.25] — 2026-05-13

### Added

- **Process concurrency guard** — New `process_guard` module limits concurrent `lean-ctx` processes to 4 via `flock`/`fcntl` slot locks, preventing CPU saturation when multiple agents trigger simultaneous operations.
- **Terse pipeline input cap & timeout** — `compress()` now skips inputs >64KB and enforces a 500ms deadline with per-stage budget checks, preventing runaway CPU on large outputs (#210).
- **Trigram set cap** — `scoring.rs` limits the `seen_trigrams` HashSet to 10,000 entries, preventing unbounded memory growth on large inputs.
- **Property-based compression tests** — Added `proptest` dev-dependency with invariant tests: `safeguard_ratio` never inflates, `entropy_compress` never exceeds original tokens, `compress_output` never inflates, and entropy output is a subset of input lines.
- **Canonical rules policy** — New `rules_canonical.rs` module provides a single source of truth for all rule generation (MUST USE / NEVER USE tables, MCP instructions) across Hybrid and MCP modes.
- **Contract tests for rules consistency** — 11 cross-IDE contract tests verify generated rules contain MUST/NEVER language, no contradictions between Hybrid/MCP modes, and correct tool mappings.
- **MCP JSON `instructions` field** — Editor MCP configs now include an `instructions` field (where clients support it) with the canonical lean-ctx tool policy, truncated per client constraints.

### Changed

- **Rules language strengthened** — All rule templates, `.cursorrules`, MDC files, and SKILL.md now use `CRITICAL: ALWAYS`, `MUST USE`, and `NEVER USE` instead of `PREFER` / `should`. Ensures agents treat lean-ctx tool usage as mandatory.
- **Background index throttled** — `spawn_index_build_background` now runs with `nice -n 19` and `ionice -c 3` (Linux) to prevent CPU contention during setup.
- **env.sh self-heal hardened** — Container self-heal logic now includes a 60-second cooldown and PID-lock check (max 4 concurrent), preventing heal loops in multi-shell environments.
- **Dictionary optimization** — `apply_dictionaries` performs case-insensitive `contains()` check before `replace_whole_word`, reducing unnecessary string operations.
- **Quality gate optimization** — `extract_identifiers` capped at 200 entries; identifier lookup in `check()` uses HashSet instead of linear `contains()`.
- **Entropy compression safeguard** — `entropy_compress` now falls back to the original content when compression would inflate token count.

### Fixed

- **100% CPU on `terse` with large inputs** (#210) — Combination of input cap, timeout budget, trigram cap, and process guard eliminates all known CPU hotspot scenarios.
- **Stale `include_str!` paths in integration tests** — `security_hardening.rs` and `security_resolve_path_guard.rs` updated to reference modularized file locations (`session/state.rs`, `tools/server_paths.rs`, registry-only dispatch).
- **Clippy warnings** — Fixed `map().flatten()` → `and_then()`, needless borrows, trailing commas, raw string hashes, and `let...else` patterns across multiple files.

## [3.5.24] — 2026-05-13

### Changed

- **Eliminate `CliRedirect` hook mode** — Removed the `HookMode::CliRedirect` variant entirely. All agents now use either `Hybrid` (MCP for reads/search + shell hooks for command compression) or `Mcp` (MCP only). Cursor and Gemini CLI, previously CliRedirect, are now Hybrid with full MCP support. This ensures reads and searches always go through the cached MCP path while shell commands are compressed via hooks — the best of both worlds.
- **Cursor: automatic MCP installation** — `lean-ctx init --agent cursor` and `lean-ctx setup` now automatically install the lean-ctx MCP server config in `~/.cursor/mcp.json` with all 50+ tools auto-approved. Previously, CliRedirect mode actively prevented MCP installation, causing Cursor to miss cached reads and search compression.
- **Gemini CLI: Hybrid mode with MCP** — Gemini CLI now gets MCP server config alongside its shell hooks, enabling cached reads via `ctx_read` while preserving shell compression via hooks.
- **All agents default to Hybrid** — `recommend_hook_mode()` now returns `Hybrid` for all agents with shell access (Cursor, Gemini, Codex, Claude Code, OpenCode, Crush, Hermes, Pi, Qoder, Windsurf, Amp, Cline, Roo, Copilot, Kiro, Qwen, Trae, Antigravity, Amazon Q, Verdent). Only unknown agents without shell access fall back to `Mcp`.
- **Hybrid rules template v2** — Updated `.cursor/rules/lean-ctx.mdc` template to clearly instruct agents to use `ctx_read` and `ctx_search` (MCP) for reads/search, and `lean-ctx -c` (CLI) for shell commands.
- **SKILL.md updated** — Removed `--mode cli-redirect` examples, updated to show Hybrid as the default mode for all agents.

### Added

- **`LEAN_CTX_QUIET=1` production mode** — New environment variable that suppresses all informational output for production use: savings footers (`[lean-ctx: X→Y tok, -Z%]`), session-start hook messages, tee-log hints, and verbose reroute messages. Shell compression still runs — only the human-visible annotations are hidden. Codex users can set this in `~/.codex/config.toml` under `[mcp_servers.lean-ctx.env]` to match default Codex output verbosity.
- **Redirect subprocess timeout increased** — `handle_redirect` timeout increased from 3s to 10s for more reliable operation on slow filesystems.

### Removed

- **`HookMode::CliRedirect`** — Enum variant, all match arms, `CLI_REDIRECT_RULES` constant, `build_cli_redirect_instructions()` function, and the `lean-ctx-cli-redirect.mdc` template file have been removed.
- **`DedicatedCliRedirect` / `CursorMdcCliRedirect`** — Rules injection variants removed from `rules_inject.rs`.
- **`disable_agent_mcp()` call path** — The `init_cmd.rs` code path that called `disable_agent_mcp()` for CliRedirect agents has been removed. All agents now call `configure_agent_mcp()`.

### Fixed

- **Cursor reads/search not using MCP** — Root cause: CliRedirect mode prevented MCP installation, and `.cursorrules` / rule files instructed CLI-first usage. Now all rule files consistently instruct Hybrid mode (MCP reads + CLI shell).
- **Inconsistent rule files** — `.cursorrules`, `AGENTS.md`, project-level and global `.cursor/rules/lean-ctx.mdc` now all consistently instruct Hybrid mode instead of conflicting CLI-first vs MCP-first directives.

## [3.5.23] — 2026-05-13

### Added

- **RAM Guardian — adaptive memory management** — New `memory_guard` module with RSS-based memory monitoring, peak tracking, and adaptive tiered eviction. Background guard task monitors memory pressure and triggers cache eviction at configurable thresholds (`max_ram_percent`, default 5%). Uses `jemalloc` as global allocator on Unix for aggressive memory return (`dirty_decay_ms:1000`, `muzzy_decay_ms:1000`). New `jemalloc_purge()` and `force_purge()` for explicit arena cleanup. Platform-specific RSS reading via `task_info()` (macOS) and `/proc/self/status` (Linux). New dependencies: `tikv-jemallocator`, `tikv-jemalloc-ctl`, `zstd`, `memmap2`.
- **zstd-compressed session cache** — `CacheEntry` now stores content as zstd-compressed `Vec<u8>` instead of raw `String`, reducing in-memory cache footprint by ~60–80%. New `CacheEntry::new()`, `content()`, `set_content()` API. `SessionCache::store()` signature changed from `content: String` to `content: &str`.
- **Memory estimation and unload for indexes** — `BM25Index::memory_usage_bytes()` / `unload()` and `EmbeddingIndex::memory_usage_bytes()` / `unload()` enable the RAM Guardian to reclaim index memory under pressure.
- **Dashboard memory API** — New `/api/memory` endpoint exposing RSS, peak RSS, system RAM, pressure level, allocator type, and max sessions.
- **`lean-ctx doctor` RAM Guardian diagnostics** — Doctor output now shows current RSS, system RAM, percentage, limit, and allocator type.
- **Configurable savings footer suppression** — New `savings_footer` config option (`auto` | `always` | `never`) and `LEAN_CTX_SAVINGS_FOOTER` env var. In `auto` mode (default), token savings footers like `[42 tok saved (30%)]` are shown in CLI but suppressed in MCP/agent context to prevent context pollution. Addresses user feedback about footers being added to agent context.
- **Explicit server shutdown** — `LeanCtxServer::shutdown()` clears cache, saves session, and triggers `force_purge()` on MCP client disconnect.
- **Config schema: `max_ram_percent`, `savings_footer`** — Both new configuration keys exposed via `lean-ctx config schema` with types, defaults, descriptions, and env var overrides.

### Fixed

- **CLI savings footer bypass** — `cli/common.rs::print_savings()` was formatting footers independently of `protocol::format_savings()`, ignoring the `savings_footer` configuration. Now delegates to the central formatting function.
- **Daemon-delegated output footer leakage** — When CLI commands (read, grep, ls) delegate to the daemon, the daemon's response could contain savings footers even when the CLI client has `LEAN_CTX_SAVINGS_FOOTER=never`. New `filter_daemon_output()` function strips footer lines client-side based on the client's own footer configuration.
- **Shared session store cap** — Reduced `MAX_CACHED_SESSIONS` from 64 to 8 to prevent unbounded memory growth in multi-IDE setups.

### Changed

- **`CacheEntry` API** — Direct field access (`entry.content`) replaced with method call (`entry.content()`). All tools (`ctx_compress`, `ctx_delta`, `ctx_share`, `ctx_dedup`, `ctx_read`, `ctx_preload`) and tests updated.

## [3.5.22] — 2026-05-13

### Fixed

- **read: overlay/FUSE stat() race** — `read_file_lossy` now opens the file first and uses `fstat()` on
  the file descriptor instead of a separate `stat()` syscall. Fixes sporadic "No such file or directory"
  errors in Docker overlay/FUSE filesystems (e.g. Codex sandboxes) where `stat()` can return ENOENT
  for files that exist. Adds a single retry with 50 ms backoff on NotFound before giving up.

### Added

- **Native Windows daemon support — IPC abstraction layer** — New `ipc/` module (`mod.rs`, `process.rs`, `unix.rs`, `windows.rs`) provides a platform-independent daemon transport layer. Unix uses UDS (unchanged behavior), Windows uses Named Pipes (`\\.\pipe\lean-ctx-{hash}`). All OS-specific code (`libc::kill`, `PermissionsExt`, `UnixStream`) is now isolated in `ipc/unix.rs` and `ipc/windows.rs` — no other module needs `#[cfg(unix)]` for daemon logic. `windows-sys` 0.59 added as target dependency. Implements [#209](https://github.com/yvgude/lean-ctx/issues/209).
- **HTTP-based daemon shutdown** — New `POST /v1/shutdown` endpoint enables cross-platform graceful daemon shutdown. `stop_daemon()` now tries HTTP shutdown first, then `SIGTERM`/`TerminateProcess` as fallback, then force kill as last resort. No more direct `libc::kill(SIGTERM)` in `daemon.rs`.
- **`build_app_router()` extraction** — Shared Axum router construction extracted from `serve()` and `serve_uds()`, eliminating ~70 lines of code duplication. Both TCP (`serve()`) and IPC (`serve_ipc()`) use the same router builder.
- **Parallel call graph build with progress tracking** — `CallGraph::build_parallel()` uses rayon for concurrent file analysis. New `get_or_start_build()` returns cached results immediately or starts a background build with live progress (`BuildProgress` struct with `files_total`, `files_done`, `edges_found`). Dashboard polls via `/api/call-graph/status`.
- **Dashboard: call graph progress bar** — `cockpit-graph.js` shows a live progress bar during call graph builds instead of a blank loading state. Auto-polls every 2s and renders the completed graph once ready.
- **Dashboard: project file browser in Compression Lab** — `cockpit-compression.js` now has two tabs: "Recent" (context ledger/events) and "Project" (all indexed files from `/api/graph-files`). Project tab includes search, file count, and token count per file. New `/api/graph-files` API endpoint returns indexed files sorted by token count.
- **Dashboard: improved compression lab layout** — Sidebar/main grid layout with responsive breakpoint at 900px. File list shows token counts, mode auto-switches when selecting recently read files, search input for project files.

### Fixed

- **100% CPU after `lean-ctx setup` on Ubuntu** — Two root causes fixed: (1) `env.sh` self-heal script could recursively spawn `lean-ctx init` via `BASH_ENV` outside containers. Now guarded with container detection (`/.dockerenv`), recursion guard (`_LEAN_CTX_HEAL`), and `LEAN_CTX_ACTIVE` propagation. (2) Graph index scanning could scan entire `$HOME` when `setup` was run outside a project. Now guarded with `is_safe_scan_root()` check, cross-process lock (`startup_guard`), 50k entry limit, and 2-minute timeout. `LEAN_CTX_NO_INDEX` env var skips indexing entirely. Fixes [#210](https://github.com/yvgude/lean-ctx/issues/210).
- **`daemon.rs`/`daemon_client.rs` now platform-independent** — Removed all `#[cfg(unix)]` gates from `lib.rs`, `cli/dispatch.rs`, and `setup.rs` for daemon modules. `daemon_client.rs` auto-start works on all platforms (previously returned `None` on non-Unix).
- **Dashboard call graph timeout** — Increased from 15s/30s to 60s to accommodate larger projects during initial build.

### Changed

- **`serve_uds()` replaced by `serve_ipc()`** — Takes a `DaemonAddr` enum instead of a `PathBuf`. Callers use `daemon::daemon_addr()` instead of `daemon::daemon_socket_path()`.
- **`daemon_socket_path()` removed** — Replaced by `daemon::daemon_addr()` which returns a `DaemonAddr` enum. All call sites updated (`setup.rs`, `dispatch.rs`).
- **Security hardening test updated** — `uds_socket_sets_permissions` now checks `ipc/unix.rs` instead of `http_server/mod.rs` (chmod 600 logic moved during IPC extraction).

## [3.5.21] — 2026-05-12

### Fixed

- **graph.db and graph.meta.json now honor LEAN_CTX_DATA_DIR** — Property graph files are stored in `$DATA_DIR/graphs/<project_hash>/` (consistent with the JSON graph index). Transparent migration moves existing files from `<project>/.lean-ctx/` on first access. `CodeGraph::open()` signature changed from `&Path` to `&str`. All 12+ call sites updated. Hardcoded `.lean-ctx/graph.db` strings in `ctx_impact` and `ctx_architecture` replaced with actual resolved paths. Fixes [#205](https://github.com/yvgude/lean-ctx/issues/205).
- **Graph index UX: correct labels and configurable cap** — `lean-ctx gain` now shows "files" instead of misleading "nodes" when using the JSON graph index fallback. A "(capped)" suffix appears when the file scan limit is reached. New config key `graph_index_max_files` (default: 5000, up from hardcoded 2000). Warning emitted when cap is hit. Fixes [#206](https://github.com/yvgude/lean-ctx/issues/206).
- **Config documentation accuracy** — Removed phantom `[compaction]` section and non-existent `[archive]` fields from website docs. Corrected wrong defaults (`compression_level`: "off" not "standard", `buddy_enabled`: true not false, `custom_aliases` fields: `command`/`alias` not `name`/`command`, `loop_detection.blocked_threshold`: 0 not 6, `autonomy.consolidate_cooldown_secs`: 120 not 300). Added missing sections (`[cloud]`, `[proxy]`, `[memory.*]`, etc.). Fixes [#208](https://github.com/yvgude/lean-ctx/issues/208).

### Added

- **Dashboard expandable event details** — Event cards in the Live Observatory are now clickable with an accordion pattern. Expanded panels show all available metrics: token savings bar, compression strategy, before/after lines, mode, path, duration. New `/api/events/:id` endpoint for lazy-loading full event details. Implements [#207](https://github.com/yvgude/lean-ctx/issues/207).
- **`lean-ctx config schema`** — New CLI command that outputs a complete JSON schema of all configuration keys, types, defaults, descriptions, and env var overrides. Single source of truth for config documentation.
- **`lean-ctx config validate`** — New CLI command that validates `config.toml` against the schema. Warns about unknown keys with Levenshtein-distance "did you mean?" suggestions. Exit code 1 on errors (CI-friendly).
- **Graph property graph tests** — 6 new tests covering `graph_dir()` with/without `LEAN_CTX_DATA_DIR`, transparent migration (move and skip-when-exists), `meta_path()` integration, and `CodeGraph::open()` with custom data directory.

## [3.5.20] — 2026-05-12

### Fixed

- **Codex installer respects `CODEX_HOME`** — `lean-ctx init --agent codex` now reads the `CODEX_HOME` environment variable to determine the Codex config directory. Previously, all Codex files (`config.toml`, `hooks.json`, `AGENTS.md`, `LEAN-CTX.md`) were always written to `~/.codex`, even when `CODEX_HOME` pointed elsewhere. All 11 call sites updated to use `resolve_codex_dir()`. Fixes [#202](https://github.com/yvgude/lean-ctx/issues/202).
- **Codex feature flag migrated from `codex_hooks` to `hooks`** — The installer now writes `hooks = true` (the current Codex feature flag) instead of the deprecated `codex_hooks = true`. Existing `codex_hooks = true` entries are automatically migrated to `hooks = true` during install. The uninstall parser also handles both variants. Fixes [#203](https://github.com/yvgude/lean-ctx/issues/203).
- **`lean-ctx ls` rejects unsupported flags** — Flags like `-la`, `-l`, `-R` are now rejected with a clear error message and usage hint instead of being silently treated as path arguments. Supported flags: `--all`/`-a`, `--depth N`. The shell hook continues to pass `ls` flags transparently to the system `ls`. Fixes [#201](https://github.com/yvgude/lean-ctx/issues/201).
- **Windows path format for inline rewrites** — `handle_rewrite_inline()` (used by the OpenCode plugin) now returns native OS paths instead of unconditionally converting to Unix/MSYS format (`/c/Users/...`). On Windows, `sanitize_exe_path()` normalizes MSYS paths via `normalize_tool_path()`. Bash shell hooks still use `to_bash_compatible_path()` as before. New `from_bash_to_native_path()` function provides the inverse conversion. Fixes [#204](https://github.com/yvgude/lean-ctx/issues/204).

### Added

- **Path normalization tests** — 11 new `normalize_tool_path()` tests covering MSYS drives, backslashes, double slashes, trailing slashes, and verbatim prefixes. 6 new `from_bash_to_native_path()` tests including Windows/Unix roundtrips. Platform-specific `sanitize_exe_path()` tests for Windows MSYS normalization.

## [3.5.19] — 2026-05-12

### Added

- **Shell hook drop-in install** — Users with `.d/`-style dotfiles (chezmoi, yadm, stow, oh-my-zsh `custom/`) now get hook fragments installed as numbered drop-in files (e.g. `~/.zshenv.d/00-lean-ctx.zsh`) instead of inline fenced blocks. Detection is automatic (`Style::Auto`); override with `--style=inline` or `--style=dropin`. Transparent migration between styles preserves hand-edits via timestamped backups (`.lean-ctx-<UTC>.bak`). (#196)
- **Output policy classification** — New `OutputPolicy` enum (`Passthrough`, `Verbatim`, `Compressible`) provides centralized command classification for the compression pipeline. Commands like `gh api`, `az login`, `docker ps`, `kubectl get pods` are now correctly classified and never compressed.

### Fixed

- **Dashboard: 7 frontend data mismatch bugs** — Complete attribute-by-attribute audit of all 17 dashboard pages revealed field name mismatches between frontend components and backend API responses:
  - `cockpit-overview.js` — SLO compliance now calculated from `slo.snapshot.slos` array; Verification card uses `verif.total`/`verif.pass`; `streak_days === 0` no longer hidden by falsy check
  - `cockpit-health.js` — SLOs render from `.slos` (not `.results`); Anomalies handle direct array response; Verification uses correct `total`/`pass`/`warn_runs` fields; Bug Memory (Gotchas) uses `trigger`/`resolution`/`occurrences`/`first_seen` and handles enum `severity`/`category`
  - `cockpit-agents.js` — Swimlanes use actual API fields (`id`, `role`, `status`, `status_message`, `last_active_minutes_ago`, `pid`) instead of expected-but-absent `name`/`model`/`tool_calls`
  - `cockpit-memory.js` — Episodes use `actions.length` for tool count, `tokens_used` for token display, and parse tagged `Outcome` enum correctly
  - `cockpit-live.js` — `tokens_saved === 0` no longer hidden by falsy check in `buildToolDetail`
  - `cockpit-compression.js` — Removed unsupported `diff` mode from UI
  - `cockpit-graph.js` — Tooltip dynamically shows "B", "tok", or "lines" based on available size metric
- **Token Pressure accuracy** — Context field `temperature` now uses `pressure.utilization` (weighted decay) instead of raw `total_tokens_sent / window_size`, and `budget_remaining` uses `pressure.remaining_tokens` for consistency with the Token Pressure card
- **Truncation bug causing increased token usage** — Removed aggressive 8000-byte fallback truncation in `patterns::compress_output` that produced `[… N lines omitted …]` markers, causing AI models to retry commands. Large outputs now flow through the safety-aware `compress_if_beneficial` pipeline instead. Fixes [#199](https://github.com/yvgude/lean-ctx/issues/199).
- **Dashboard format utilities** — `pc()` NaN guard for percentage formatting; `fu()` type guard for unit formatting; `fmtNum` normalized to consistent 'K' suffix
- **Dashboard route visibility** — All dashboard route handlers narrowed from `pub fn` to `pub(super) fn`
- **Clippy `duration_suboptimal_units`** — `Duration::from_millis(30_000)` → `Duration::from_secs(30)` in 4 locations
- **Shell hook: `ls` and `find` missing from alias list** — Both commands are now included as `Category::DirList` in the generated shell hook, so `ls` and `find` output is tracked/compressed in hooked shells. Fixes [#200](https://github.com/yvgude/lean-ctx/issues/200).
- **Shell hook: non-interactive agent commands not tracked** — The TTY guard (`[ ! -t 1 ]`) now has an agent-aware bypass: when `LEAN_CTX_AGENT`, `CODEX_CLI_SESSION`, `CLAUDECODE`, or `GEMINI_SESSION` env vars are present, commands are tracked even in non-interactive shells (Docker, Codex `bash -c`). Fixes [#200](https://github.com/yvgude/lean-ctx/issues/200).
- **Flaky SSE replay test** — Rewrote `events_endpoint_replays_tool_call_event` to append directly to the event bus instead of depending on a fire-and-forget `spawn_blocking` task, eliminating CI timing failures on contended runners.

## [3.5.18] — 2026-05-12

### Fixed

- **`gh api` output no longer compressed** — Commands like `gh api repos/.../actions/jobs/.../logs` are now passthrough (no compression, no truncation). Previously, large API responses were silently truncated by the generic 8000-byte fallback, making CI log debugging impossible. Also applies to `gh run view --log` and `--log-failed` flags.

## [3.5.17] — 2026-05-12

### Security

- **[Critical] LLM Proxy bearer token auth** — The proxy server now supports optional bearer token authentication via `LEAN_CTX_PROXY_TOKEN` environment variable, preventing unauthorized access from other local processes.
- **[Critical] Symlink hijack protection on all write paths** — `write_atomic()` and context package `atomic_write()` now reject writes through symlinks, preventing an attacker from redirecting config writes to arbitrary files.
- **[Critical] Shell command validation — documented accepted risk** — Explicitly documented in SECURITY.md that shell command validation is delegated to the AI agent's permission model by design, with CWD jail and output capping as compensating controls.
- **[High] Claude binary path validation** — `claude mcp add-json` now validates that the resolved `claude` binary comes from a trusted directory (`.claude/`, `/usr/local/bin/`, `/opt/homebrew/`, etc.), preventing PATH hijack attacks. Override with `LEAN_CTX_TRUST_CLAUDE_PATH=1`.
- **[High] TOCTOU mitigation for config writes** — New `write_atomic_with_backup_checked()` validates file mtime between read and write, detecting concurrent external modifications.
- **[High] Auto-approve transparency** — `lean-ctx setup` now displays a banner listing all auto-approved MCP tools with count. New `--no-auto-approve` flag disables auto-approve in editor configurations.
- **[High] Full integrity verification for context packages** — `verify_integrity()` now validates `content_hash`, `sha256` (composite hash of name:version:content_hash), and `byte_size` — previously only `content_hash` was checked.
- **[High] PathJail TOCTOU — documented accepted risk** — Documented in SECURITY.md that the race condition between `jail_path` check and file operation requires `openat`/`O_NOFOLLOW` at syscall level for complete mitigation.
- **[High] Database TLS — documented accepted risk** — Cloud server DB connection is localhost-only by default. Production deployments should use `?sslmode=require` in `DATABASE_URL`.
- **[Medium] Timestamped config backups** — Backup files now include Unix epoch timestamps (e.g., `.lean-ctx.1715464800.bak`) instead of overwriting a single `.lean-ctx.bak` file.
- **[Medium] Email enumeration timing fix** — Login endpoint now performs a dummy Argon2id verification when the user doesn't exist, equalizing response time to prevent email existence oracle attacks.
- **[Medium] Verification token TTL reduced** — Email verification tokens reduced from 24h to 2h. Old pending tokens are now invalidated before issuing new ones.
- **[Medium] Knowledge fact provenance tracking** — `KnowledgeFact` struct now includes `imported_from: Option<String>` field, set to `name@version` when facts are imported from context packages.

### Fixed

- **Dependabot: mermaid security update** — Updated mermaid from 10.9.5 to 10.9.6 in cookbook examples (CSS injection fix).

## [3.5.16] — 2026-05-11

### Security

- **[Critical] Path traversal in `tee show`** — The `lean-ctx tee show <filename>` CLI command accepted path separators and `..` in the filename argument, allowing reads of arbitrary files outside the tee log directory. Now enforces strict basename-only validation.
- **[Critical] Python/Shell injection via `intent` parameter** — The `ctx_execute` tool interpolated the `intent` parameter raw into generated Python and shell scripts, allowing code injection through crafted intent strings. Now sanitized to alphanumeric characters only (max 200 chars).
- **[Critical] CSPRNG failure silently ignored** — Two `getrandom::fill` calls (token generation + CSP nonce) silently discarded errors, which could result in predictable all-zero tokens/nonces. Now panics on CSPRNG failure to guarantee cryptographic safety.
- **[Critical] Dashboard path traversal bypass** — The `/api/compression-demo` endpoint allowed absolute paths to bypass `pathjail` filesystem jail. All paths now go through `jail_path` unconditionally.
- **[Critical] MCP stdio integer overflow** — Malicious `Content-Length` headers could cause integer overflow in frame length calculation, leading to unbounded memory allocation. Now uses `checked_add` with strict size cap.
- **[High] Token exposure on loopback** — Anonymous loopback GET requests to the dashboard received the auth token injected into HTML, allowing any local process to steal it. Now requires explicit `?token=` query parameter.
- **[High] Nonce-based CSP replaces `unsafe-inline`** — Dashboard Content-Security-Policy upgraded from `script-src 'unsafe-inline'` to per-response cryptographic nonce, eliminating XSS via inline script injection.
- **[High] Panic payloads leaked to MCP clients** — Tool panics returned full panic messages (potentially containing secrets/paths) to clients. Now returns generic error; details logged server-side only.
- **[High] `ctx_execute` output not redacted** — Output from `ctx_execute` bypassed the redaction engine, potentially leaking secrets. Now applies `redact_text_if_enabled` like `ctx_shell`.
- **[High] Cross-project data access via `ctx_share`** — Shared agent data was stored in a flat directory, allowing agents from different projects to read each other's data. Now scoped under `project_hash` subdirectory.
- **[High] PowerShell command interpolation** — On Windows, PowerShell commands were interpolated into script strings. Now writes to temp file and executes via `-File`.
- **[High] Cloud server error information leak** — `internal_error` helper returned raw database/OS error strings to HTTP clients. Now returns generic `{"error":"internal_error"}`.
- **[High] SSE subscriber cap enforced** — The 64-subscriber-per-channel cap previously only logged a warning but still allowed new subscriptions. Now returns `None` and falls back to dead channel, preventing resource exhaustion.
- **[High] Rust sandbox inherited full environment** — The `execute_rust` function (rustc + compiled binary) did not strip inherited environment variables, exposing secrets and enabling `LD_PRELOAD`-style attacks. Now applies the same `env_clear()` + allowlist as other sandbox runtimes.
- **[Medium] Argon2id password hashing** — Cloud server passwords migrated from salted SHA-256 to Argon2id with legacy fallback for existing hashes.
- **[Medium] SQLite busy_timeout** — Added 5-second busy_timeout to all SQLite connections to prevent `SQLITE_BUSY` errors under contention.
- **[Medium] ReDoS mitigation for filter rules** — Both runtime and validation paths for user-authored filter TOML patterns now use `RegexBuilder` with 1 MiB DFA size limit.
- **[Medium] Context summary redaction** — `/v1/context/summary` endpoint now redacts events at `Summary` level before aggregation, preventing leakage of sensitive knowledge keys/categories.
- **[Medium] A2A handoff error sanitization** — Parse and write errors no longer include OS-level details or filesystem paths in HTTP responses.
- **[Medium] `ctx_search` and `ctx_tree` parameter clamping** — `max_results` capped at 500, `depth` capped at 10 to prevent resource exhaustion.
- **[Medium] `ctx_shell` cwd fail-closed** — Invalid working directory now returns error instead of silently falling back to process cwd.
- **[Medium] Community detection graceful degradation** — All SQLite `unwrap()` calls in `community.rs` replaced with proper error handling returning empty graphs instead of panicking.
- **[Medium] Defense-in-depth path canonicalization** — `read_file_lossy` now verifies canonical paths stay within project root (warning-only layer behind primary `jail_path` enforcement).
- **[Medium] Sandbox environment isolation** — `ctx_execute` subprocesses now start with `env_clear()` + explicit allowlist (PATH, HOME, LANG, TERM, TMPDIR) instead of inheriting all parent environment variables.
- **[Medium] Hook temp file hardening** — Temp directory for hook redirects now has `chmod 700` (Unix), and filenames include PID scoping to prevent symlink races.
- **[Medium] PowerShell temp file cleanup** — `.ps1` temp files are now deleted on all exit paths (success, spawn error, wait error).
- **[Medium] `ctx_execute` temp file lifecycle** — `.dat` temp files are now cleaned up by Rust after sandbox execution (regardless of script success), with file size validation before processing.
- **[Medium] `/health` rate limiting** — Health endpoint no longer bypasses rate limiter and concurrency semaphore, preventing use as amplification oracle.
- **[Low] `validate_filter_file` regex bounds** — Validation path now uses bounded `RegexBuilder` matching runtime behavior.
- **[Low] Corrected `check_secret_path_for_tool` tool name** — Changed hardcoded `"ctx_read"` to `"resolve_path"` for accurate policy logging.

### Fixed

- **Structural output protection** — `git diff`, `git show`, `git blame`, `git log -p`, `git stash show`, `diff`, `colordiff`, `icdiff`, and `delta` output was being mangled by up to three compression layers (pattern compression + terse pipeline + generic compressors like log_dedup/truncation). These commands now get a dedicated fast path: only the specific pattern compressor runs (light cleanup: strip `index` headers, limit context lines), all other compression stages are bypassed. Every `+`/`-` line, hunk header, and blame annotation is preserved verbatim. Also protected in the MCP server path (`ctx_shell`).
- **zsh shell hook breaks command completion** — After sourcing the lean-ctx shell hook, tab completion for aliased commands (`git`, `cargo`, `docker`, etc.) stopped working. Added a zsh completion wrapper (`_lean_ctx_comp`) that delegates to the original command's completion function via `_normal`. Fixes [#193](https://github.com/yvgude/lean-ctx/issues/193).

### Added

- **Roadmap: Context Runtime research modules** — 13 new core modules implementing research from information theory, graph theory, and cognitive science:
  - `adaptive_chunking` — Content-defined chunking with Rabin-Karp fingerprinting and entropy-aware split points
  - `attention_placement` — Attention allocation scoring based on recency, frequency, and structural importance
  - `cognitive_load` — Cognitive load estimation using Halstead metrics and cyclomatic complexity
  - `cyclomatic` — Cyclomatic complexity analysis via control-flow graph extraction
  - `gamma_cover` — Gamma cover set selection for minimal representative context subsets
  - `graph_features` — Property graph feature extraction (betweenness, clustering coefficient, community bridge detection)
  - `information_bottleneck` — Information bottleneck compression with iterative Blahut-Arimoto
  - `mdl_selector` — Minimum Description Length model selection for compression strategy
  - `memory_consolidation` — Memory consolidation with exponential decay and importance-weighted retention
  - `progressive_compression` — Multi-level progressive compression with quality gates
  - `splade_retrieval` — Sparse Lexical and Expansion retrieval for context-aware search
  - `structural_diff` — AST-level structural diff for semantic change detection
  - `structural_tokenizer` — Language-aware tokenization using tree-sitter AST for 18 languages
- **Louvain community detection O(m)** — Rewrote `community.rs` from O(n²) adjacency scan to edge-list-based Louvain with modularity optimization, supporting weighted edges and hierarchical communities.
- **Enhanced PageRank** — Damped PageRank with configurable alpha, convergence detection, and seed biasing for context-aware node ranking.
- **SPLADE-enhanced BM25** — BM25 index now supports sparse expansion terms for improved recall on semantically related queries.
- **Config module restructured** — Split monolithic `config.rs` into `config/mod.rs`, `config/memory.rs`, `config/proxy.rs`, `config/serde_defaults.rs` for maintainability.
- **`shell_activation` config option** — New `shell_activation` setting in `config.toml` with three modes: `always` (default, backward-compatible), `agents-only` (auto-activates only in AI agent sessions like Claude Code, Cursor, Windsurf), and `off` (fully manual). Controlled via config file or `LEAN_CTX_SHELL_ACTIVATION` environment variable. Addresses feedback that lean-ctx shell hooks were too invasive for users who only need it in specific AI agent contexts.
- **`.lean-ctx-id` project identity file** — Projects can now declare a unique identity via a `.lean-ctx-id` file in the project root. This takes highest priority in composite project hashing, solving Docker environments where multiple projects share the same mount path (e.g. `/workspace`). Simply create a file with a unique name (e.g. `echo "my-project-alpha" > .lean-ctx-id`).
- **Identity-aware storage for all caches** — `graph_index`, `semantic_cache`, `bandit`, and `embedding_index` now use composite project hashes (path + identity markers) instead of path-only hashes. Includes automatic migration from legacy storage locations. Fixes cross-project context bleed in Docker environments.
- **Security hardening test strengthened** — Dashboard token embedding no longer falls back to loopback bypass; tests now verify the stricter `valid_query`-only gate.

## [3.5.15] — 2026-05-11

### Fixed

- **Dashboard "unauthorized" on localhost** — Users accessing the dashboard on `localhost` after v3.5.14 saw `/api/stats: unauthorized` because the browser didn't have the auth token. The server now auto-injects the token into HTML for loopback connections (`127.0.0.1`, `::1`) so the JS fetch interceptor can authenticate API calls automatically. API auth remains fully active — no bypass, no CSRF risk. Fixes webut's report.
- **Dashboard probe sends Bearer** — The `dashboard_responding` health probe now sends the saved Bearer token, so the "already running" detection works correctly with auth-enabled dashboards.
- **Large file crash / MCP hang** — Reading very large files (multi-GB) via `ctx_read` or `ctx_smart_read` caused the MCP server to allocate unbounded RAM and crash. Now enforced at 4 layers: binary file detection rejects before any I/O, `metadata().len()` checks reject before allocation, `read_file_lossy` refuses unbounded reads on `stat()` failure, and MCP dispatch returns `Err(ErrorData)` instead of `Ok("ERROR:...")` to prevent client retries. Fixes sb's report.

### Added

- **Binary file detection** (`core::binary_detect`) — Detects 100+ binary file extensions (Parquet, SQLite, ONNX, ZIP, images, ML models, bytecode, archives, fonts, disk images) plus magic-byte NULL check on the first 8 KB. Returns human-readable file type labels (e.g. "columnar data file", "ML model file"). Used across `ctx_read`, `ctx_smart_read`, `ctx_multi_read`, and `ctx_prefetch`.
- **Live Observatory event explanations** — Every event in the dashboard's Live Observatory now has a `?` help icon. Click to expand an inline explanation of what the event means and whether user action is needed. SLO violations ("violated · CompressionRatio") and compression events ("entropy_adaptive · 293 → 264 lines") are now clearly documented. Event type legend added to "How it works" section.
- **3 new security hardening tests** — `dashboard_api_auth_never_bypassed_for_loopback`, `dashboard_probe_sends_bearer_token`, loopback injection signature validation.
- **`memory_cleanup` setting** — New config/env option (`LEAN_CTX_MEMORY_CLEANUP`) with two modes: `aggressive` (default, 5 min idle TTL — best for single-IDE use) and `shared` (30 min TTL — best when multiple IDEs or models share lean-ctx context). Visible in `lean-ctx doctor` and `lean-ctx config`. Suggested by sb.

### Improved

- **Graceful error messages for binary/oversize files** — Instead of crashing or returning generic errors, binary files get a helpful message like "Binary file detected (.parquet, columnar data file). Use a specialized tool for this file type." Oversize files suggest `mode="lines:1-100"` for partial reads.
- **MCP error semantics** — Binary/oversize file errors now return `Err(ErrorData::invalid_params(...))` at the MCP dispatch level, signaling to clients that retrying won't help. Previously returned `Ok("ERROR: ...")` which caused some clients to retry indefinitely.

## [3.5.14] — 2026-05-10

### Performance

- **BLAKE3 hashing** — Replaced all MD5 (`md5_hex`, `md5_hex_bytes`) with BLAKE3 via centralized `core::hasher` module. 12 duplicate hash functions eliminated across the codebase. BLAKE3 is ~3x faster than MD5 for large inputs with better collision resistance.
- **Tree-sitter Query Cache** — Compiled tree-sitter `Query` objects are now cached in `OnceLock<HashMap>` statics in `chunks_ts`, `signatures_ts`, and `deep_queries`. Eliminates re-compilation of query patterns on every file parse. Parser instances reuse via `thread_local!`.
- **Token cache upgrade** — Token cache enlarged from 256→2048 entries with BLAKE3-based keys and LRU-like eviction (half-evict instead of full clear). Reduces redundant BPE tokenization across sessions.
- **SQLite Property Graph optimized** — Added `PRAGMA cache_size = -8000`, `mmap_size = 256MB`, `temp_store = MEMORY`. 5 new composite indices on `nodes(kind)`, `nodes(kind, file_path)`, `edges(kind)`, `edges(source_id, kind)`, `edges(target_id, kind)`. `busy_timeout(5000ms)` for WAL contention.
- **Parallel indexing** — `rayon::par_iter` for CPU-bound deep-query parsing in `ctx_impact build` (embeddings feature path).
- **ModePredictor Arc** — `ModePredictor` stored as `Arc<ModePredictor>` to avoid deep cloning on every `ctx_read` call.
- **Compact JSON serialization** — `ProjectIndex::save()` uses `serde_json::to_string` (compact) instead of `to_string_pretty`, reducing index file size and serialization time.
- **Server dispatch deduplicated** — `count_tokens` called once per request instead of redundantly after terse pass when content unchanged.

### Improved

- **Rules: Mode Selection Decision Tree** — Adopted community-contributed improvement (credit: Zeel Connor). Rules now include a numbered decision tree for `ctx_read` mode selection and an anti-pattern warning against using `full` for context-only files. Applied across all rule formats (shared, dedicated, Cursor MDC, CLI-redirect).
- **Flaky test fixes** — BM25 tests (`save_writes_project_root_marker`, `max_bm25_cache_bytes_reads_env`) now acquire `test_env_lock()` to prevent `env::set_var` race conditions. ContextBus tests use isolated temp SQLite databases via `test_bus()` instead of shared global DB.

### Added

- **`core::hasher` module** — Centralized BLAKE3 hashing: `hash_hex(bytes)`, `hash_str(s)`, `hash_short(s)`. Single source of truth for all non-cryptographic hashing.
- **`core::community` module** — Louvain-based community detection on the Property Graph (file clustering by dependency).
- **`core::pagerank` module** — PageRank computation on the Property Graph for file importance scoring.
- **`core::smells` module** — Code smell detection (long functions, deep nesting, high complexity).
- **`ctx_smells` tool** — MCP + CLI tool for code smell analysis with graph-enriched scoring.
- **58 MCP tools** — Up from 57 in previous release (added `ctx_smells`).

## [3.5.13] — 2026-05-10

### Fixed

- **Instruction files no longer compressed** — SKILL.md, AGENTS.md, RULES.md, .cursorrules, and files in `/skills/`, `/.cursor/rules/`, `/.claude/rules/` are now **always delivered in full mode**, bypassing all heuristic/bandit/adaptive mode selection. This was the root cause of agents losing instructions after v3.4.7 when the Intent Router was introduced. Guards added in 5 code paths: `resolve_auto_mode`, `predict_from_defaults`, `select_mode_with_task`, `auto_degrade_read_mode`, and CLI `read_cmd`. Fixes #159 regression, resolves GlemSom's report.
- **Markdown files exempt from aggressive compression** — `.md`, `.mdx`, `.txt`, `.rst` files no longer fall into the `aggressive` default bucket in `predict_from_defaults`. These file types return `None` (= full mode) to prevent stripping prose/instruction content.
- **Windows Claude Code PowerShell compatibility** — Claude Code hook matchers now include `PowerShell|powershell` on Windows, so PreToolUse hooks fire regardless of whether Claude uses Bash or PowerShell. Rewrite script also accepts PowerShell tool names. Fixes #192.

### Added

- **`is_instruction_file()` public API** — Reusable guard function detecting instruction/skill/rule files by filename and path patterns. Used across MCP, CLI, and server dispatch paths.
- **Lean4 formal proofs** — Theorems 12-13 in `ReadModes.lean`: instruction files always resolve to full mode, content is always preserved.
- **7 new regression tests** — `instruction_file_detection`, `resolve_auto_mode_returns_full_for_instruction_files`, `defaults_never_compress_markdown`, and PowerShell hook matcher tests.

## [3.5.12] — 2026-05-09

### Improved

- **RAM optimization: eliminate double tokenization** — `extract_chunks` in `bm25_index.rs`, `artifact_index.rs`, and `chunks_ts.rs` no longer allocates a `tokens: Vec<String>` per chunk. Token count is computed inline; the vector is set to `Vec::new()`. `add_chunk` tokenizes from `content` once for the inverted index and overwrites `token_count` from the fresh result. This eliminates one redundant allocation + tokenization pass per chunk during index build.
- **MemoryProfile fully wired** — The `MemoryProfile` enum (`low` / `balanced` / `performance`) now actively controls runtime behavior:
  - `max_bm25_cache_bytes()` respects profile limits (64 / 128 / 512 MB), with explicit user config taking precedence.
  - Semantic cache (`SemanticCacheIndex`) is skipped entirely when `memory_profile = low`.
  - Embedding engine loading is skipped in `ctx_semantic_search` and `ctx_knowledge` when `memory_profile = low`.
- **Doctor shows active memory profile** — `lean-ctx doctor` now displays the effective memory profile (low / balanced / performance), its source (env / config / default), and what it controls (cache limits, embedding status). Helps users understand and debug RAM behavior.
- **MCP manifest regenerated** — Updated `mcp-tools.json` to reflect current tool count (57 granular tools).

## [3.5.11] — 2026-05-09

### Fixed

- **Cache-loop elimination for hybrid-mode agents** — When an agent reads a file with `mode=auto` (compressed) and then re-reads with `mode=full`, the full content is now delivered immediately instead of returning a 2-line "already in context" stub. Previously, agents (especially smaller/local models) needed 3 calls to get full content: auto → full (stub) → fresh. A new `full_content_delivered` flag on cache entries tracks whether uncompressed content was already sent for the current hash.
- **Cache stub text no longer provokes unnecessary calls** — The "file already in context" message no longer suggests `fresh=true`, which misled weaker models into making a redundant third call. New text: "File content unchanged since last read (same hash). Already in your context window."
- **AGENTS.md Pi-header replaced on non-Pi agents** — When a project had `AGENTS.md` from a prior `lean-ctx init --agent pi` but was later initialized for OpenCode or another agent, the Pi-specific header ("CLI-first Token Optimization for Pi") persisted. The generic lean-ctx block now replaces it automatically.
- **Doctor check count mismatch (16/15)** — The daemon health check incremented `passed` but was not counted in `effective_total`, causing the summary to show e.g. "16/15 checks passed". Fixed by including the daemon check in the total (`+5` instead of `+4`).
- **"INDEXING IN PROGRESS" no longer blocks read output** — When the graph index is still building, the autonomy pre-hook returned the indexing notice as auto-context, which was prepended to the actual tool output. This is now suppressed — the file content is returned immediately while indexing continues in the background.

### Improved

- **RAM usage reduced during compaction/checkpoint** — Four targeted optimizations to prevent memory spikes reported during OpenCode session compaction:
  - **Codebook uses borrows instead of clones** — `build_from_files` now accepts `&[(&str, &str)]` instead of `Vec<(String, String)>`, eliminating a full duplication of all cached file contents (~2MB saved at 500k tokens).
  - **Auto-checkpoint skips signature extraction** — Periodic checkpoints now use `include_signatures: false`, avoiding expensive tree-sitter parsing. Explicit `ctx_compress` calls still extract signatures.
  - **Compressed output variants capped at 3 per cache entry** — Prevents unbounded growth of the `compressed_outputs` HashMap.
  - **Codebook early-exit at >50,000 lines** — Skips the codebook deduplication phase entirely for very large caches, preventing HashMap/HashSet memory explosions.

## [3.5.10] — 2026-05-09

### Added

- **4-layer terse compression engine** — Scientifically grounded compression pipeline replacing the legacy `output_density` / `terse_agent` settings with a unified `CompressionLevel` system (`off` / `lite` / `standard` / `max`):
  - **Layer 1 — Deterministic Output Terse** (`engine.rs`): Surprisal scoring, content/function-word filtering, filler-line removal, and a quality gate that preserves all paths and identifiers.
  - **Layer 2 — Pattern-Aware Residual** (`residual.rs`): Runs after pattern compression, applies terse on the remaining output with attribution split.
  - **Layer 3 — Agent Output Shaping** (`agent_prompts.rs`): Scale-aware brevity prompts injected into LLM instructions — telegraph-English-inspired format for `max`, dense atomic facts for `standard`, concise bullets for `lite`.
  - **Layer 4 — MCP Description Terse** (`mcp_compress.rs`): Compresses tool descriptions and lazy-load stubs for reduced schema overhead.
- **Unified `CompressionLevel` configuration** — Single `compression_level` setting in `config.toml` replaces the legacy `output_density` and `terse_agent` options. Resolution order: `LEAN_CTX_COMPRESSION` env var → `compression_level` config → legacy fallback. CLI: `lean-ctx compression <off|lite|standard|max>` (alias: `lean-ctx terse`).
- **Quality gate for terse compression** (`quality.rs`) — Ensures all file paths and code identifiers survive compression. If `max` level fails the quality check, automatically falls back to `standard`. Inputs shorter than 5 lines skip compression entirely.
- **Agent prompt injection across all IDEs** (`rules_inject.rs`) — Compression prompts are automatically injected into 7 agent rules files (Cursor `.cursorrules`, `~/.cursor/rules/lean-ctx.mdc`, Claude `.claude/rules/lean-ctx.md`, AGENTS.md, CRUSH, Qoder, Kiro). Injection runs from `lean-ctx compression`, `lean-ctx setup`, and on MCP server startup — ensuring retroactive consistency when users change settings.
- **Context Proof V2** (`context_proof_v2.rs`) — Proof-carrying context with claim extraction, quality levels Q0–Q4, and structured verification output.
- **Claim extractor** (`claim_extractor.rs`) — Decomposes session context into atomic verifiable claims for the proof system.
- **29 new Lean4 formal proofs** — Two new proof modules bringing the total to **82 machine-checked theorems** with zero `sorry`:
  - `TerseQuality.lean` (12 theorems): Quality gate correctness, conjunction semantics, idempotence, empty-set triviality.
  - `TerseEngine.lean` (17 theorems): Compression level ordering, Max-to-Standard fallback correctness, structural marker preservation, filter-subset invariant, high-score line protection.
- **Terse evaluation harness** (`terse_eval.rs`) — Integration test covering git diff, JSON API, Docker build, Cargo build, and Rust error outputs across all compression levels.
- **Domain-aware dictionaries** (`dictionaries.rs`) — Whole-word replacement dictionaries for general programming terms, Git operations, and domain-specific abbreviations. Applied after quality gate to prevent identifier corruption.
- **Surprisal-based line scoring** (`scoring.rs`) — Information-theoretic scoring using bigram surprisal to identify high-information-density lines for preservation.

### Improved

- **Dashboard: shared utilities refactored** — New `shared.js` library with common dashboard utilities, reducing code duplication across cockpit components.
- **Dashboard: cockpit components polished** — Updated Context Explorer, Agent Sessions, Graph Visualizer, Knowledge Base, Memory Inspector, Compression Stats, and Overview with improved layouts, consistent styling, and better data presentation.
- **Setup flow consolidated** — Premium feature configuration (compression, TDD) unified into a single interactive prompt flow. Shell alias refresh integrated.
- **Test suite robustness** — `terse_agent_tests.rs` rewritten to explicitly control both `LEAN_CTX_COMPRESSION` and `LEAN_CTX_TERSE_AGENT` env vars, eliminating dependency on local config state. Mutex poison recovery added. 5 new tests for the `CompressionLevel` system alongside 6 fixed legacy backward-compat tests.
- **Intensive benchmarks updated** — `intensive_benchmarks.rs` now benchmarks the new 4-layer terse pipeline instead of the removed `protocol::compress_output`.

### Fixed

- **Token counter overflow** (`counter.rs`) — `savings_pct` no longer panics when dictionary replacements expand text beyond the original token count.
- **Short input over-compression** — Inputs shorter than 5 lines are now passed through unchanged, preventing the terse engine from removing single-line outputs like file reads.
- **Legacy pipeline cleanup** — Removed deprecated `compress_output`, `OutputDensity` functions from `protocol.rs`. All compression now routes through the unified terse pipeline.

## [3.5.9] — 2026-05-09

### Fixed

- **Codex config corruption with tool approval entries (GitHub #191)** — When Codex auto-adds per-tool approval entries (`[mcp_servers.lean-ctx.tools.ctx_read]`, etc.) to `config.toml`, the parent `[mcp_servers.lean-ctx]` section could be missing (e.g. after a v3.5.6 upgrade removed it). `upsert_codex_toml` now detects orphaned `[mcp_servers.lean-ctx.*]` sub-tables and inserts the parent section **before** them instead of appending at the end, which Codex's TOML parser rejected with "invalid transport".
- **AGENTS.md reference uses absolute path** — The lean-ctx block in `~/.codex/AGENTS.md` now references `` `~/.codex/LEAN-CTX.md` `` instead of `LEAN-CTX.md (same directory)`, preventing AI agents from misinterpreting the relative reference as the project working directory.

### Security

- **fast-uri 3.1.0 → 3.1.2 (VSCode extension)** — Fixes GHSA-v39h-62p7-jpjc (malformed fragment decoding) and GHSA-q3j6-qgpj-74h6 (URI parsing vulnerability).

### Improved

- **Dashboard cockpit polish** — Refined Context Explorer with improved layout, resizable panels, and better file tree navigation. Updated styling across all cockpit components for consistency. Improved graph visualization layout and memory inspector presentation.

## [3.5.8] — 2026-05-08

### Security

- **CodeQL #40 (High): XSS in dashboard search** — `cockpit-search.js` fallback `esc()` function was `function(s) { return String(s); }` — no HTML escaping. Replaced with safe `textContent`→`innerHTML` implementation matching `format.js`.
- **CodeQL #38/#39 (Medium): Unpinned GitHub Actions** — `codecov/codecov-action@v4` and `EmbarkStudios/cargo-deny-action@v2` are now pinned to commit SHAs (`b9fd7d16…`, `5bb39ff5…`) in `ci.yml`.

### Fixed

- **Codex config corruption on mode change (GitHub #189)** — When `lean-ctx setup` or `lean-ctx update` ran with v3.5.6 (where Codex was CLI-Redirect mode), `remove_codex_toml_section` removed the `[mcp_servers.lean-ctx]` parent section but left orphaned sub-tables like `[mcp_servers.lean-ctx.env]`, causing Codex to fail with "invalid transport in mcp_servers.lean-ctx".
  - `remove_codex_toml_section` now removes **all** TOML sub-tables via prefix matching when removing a parent section.
  - `ensure_codex_mcp_server` now detects orphaned sub-tables and inserts the parent section **before** them instead of appending at the end.
  - `ensure_codex_mcp_server` now uses `toml_quote_value` for Windows backslash-safe TOML quoting (was using raw `format!` with double quotes).

## [3.5.7] — 2026-05-08

### Security

- **BM25 index memory balloon fix (GitHub #188)** — Oversized BM25 cache files (observed up to 50 GB in monorepos with vendor/generated code) could cause the daemon to allocate unbounded memory on startup, leading to system-wide swapping and OOM conditions. This release implements an 8-layer defense:
  1. **Load-time size guard** — `BM25Index::load()` now checks file metadata before reading. Indexes exceeding the configurable limit (default 512 MB) are quarantined by renaming to `.quarantined` and skipped.
  2. **Save-time size guard** — `BM25Index::save()` refuses to persist serialized data exceeding the limit, preventing bloated indexes from being written in the first place.
  3. **Chunk count warning** — Indexes with >50,000 chunks trigger a `tracing::warn` suggesting `extra_ignore_patterns` in `config.toml`.
  4. **Default vendor/build ignores** — 14 glob patterns (`vendor/**`, `dist/**`, `build/**`, `.next/**`, `__pycache__/**`, `*.min.js`, `*.bundle.js`, etc.) are now excluded from BM25 indexing by default.
  5. **File count cap** — `list_code_files()` stops collecting after 5,000 files per project, preventing runaway indexing in massive repos.
  6. **Configurable limit** — New `bm25_max_cache_mb` setting in `config.toml` (default: 512). Override per-project or via `LEAN_CTX_BM25_MAX_CACHE_MB` env var.
  7. **Project root marker** — `save()` writes a `project_root.txt` file alongside each index, enabling orphan detection when the original project directory is deleted.
  8. **`lean-ctx doctor` BM25 health check** — Doctor now scans all vector directories, warns about large indexes (>100 MB), and fails for oversized indexes. `lean-ctx doctor --fix` automatically prunes quarantined, oversized, and orphaned caches.

### Fixed

- **Codex integration mode changed from CLI-Redirect to Hybrid** — Codex exists in three variants (CLI, Desktop App, Cloud Agent) that share `~/.codex/config.toml`. Only the CLI variant has reliable shell hooks; Desktop and Cloud require MCP. lean-ctx now treats Codex as **Hybrid** (MCP + CLI hooks where available) instead of CLI-Redirect, ensuring all three variants work correctly.
- **Codex hook installer now writes MCP server entry** — `lean-ctx init --agent codex` now ensures `[mcp_servers.lean-ctx]` exists in `~/.codex/config.toml`. Previously, only CLI hooks and `codex_hooks = true` were written, leaving Desktop/Cloud variants without MCP access.
- **Codex LEAN-CTX.md upgrade detection** — `install_codex_instruction_docs()` now compares file content instead of just checking for the string "lean-ctx". This ensures the instruction file is updated when the template changes (e.g., CLI-only → Hybrid mode), instead of being silently skipped on every subsequent install.
- **Dashboard HTTP parser handles large POST bodies** — The dashboard TCP handler now reads complete HTTP messages using `Content-Length` header parsing instead of assuming the entire request fits in the first read. POST requests to API endpoints (e.g., knowledge CRUD, memory management) no longer fail silently when the body exceeds 8 KB. Maximum message size enforced at 2 MB.

### Added

- **Cockpit dashboard (complete rewrite)** — The localhost dashboard has been rebuilt from scratch as a modular single-page application:
  - **12 Web Components**: Overview, Live Activity, Context Explorer, Knowledge Base, Graph Visualizer, Agent Sessions, Memory Inspector, Compression Stats, Health Monitor, Search, Remaining Token Budget, Navigation.
  - **Modular Rust backend**: Monolithic route handler (~1,200 lines) replaced with 10 focused route modules (`routes/agents.rs`, `context.rs`, `graph.rs`, `knowledge.rs`, `memory.rs`, `stats.rs`, `system.rs`, `tools.rs`, `helpers.rs`, `mod.rs`).
  - **Shared JS libraries**: `api.js` (fetch wrapper with token auth), `charts.js` (SVG charting), `format.js` (number/byte/duration formatting), `router.js` (hash-based SPA routing), `shared.js` (common utilities).
  - **Full CSS redesign**: 800+ lines of modern CSS with dark theme, responsive layout, data tables, card grids, and chart containers.
  - Legacy dashboard preserved at `/legacy` route for backwards compatibility.
- **`lean-ctx cache prune` command** — New CLI command to scan `~/.lean-ctx/vectors/`, remove quarantined (`.quarantined`) files, oversized indexes, and orphaned directories (project root no longer exists). Reports count and freed space.
- **`lean-ctx doctor` BM25 cache health check** — Proactive diagnostics for BM25 index health, integrated into the standard doctor report. `--fix` auto-prunes.

### Improved

- **Codex instruction docs now document Hybrid mode** — `~/.codex/LEAN-CTX.md` now includes both MCP tool table (ctx_read, ctx_shell, ctx_search, ctx_tree) and CLI fallback instructions, with guidance on when to use which path depending on the Codex variant.
- **Website: Codex moved to Hybrid in Context OS table** — All 11 locale files and the ContextOsPage agent table updated. Codex now correctly appears under Hybrid mode instead of CLI-Redirect.
- **Website: Codex editor guide updated** — DocsGuideEditorsPage now describes Codex as running in Hybrid mode across CLI, Desktop, and Cloud variants.

## [3.5.6] — 2026-05-08

### Fixed

- **Daemon auto-restart on setup and update** — `lean-ctx setup` and `lean-ctx update` now automatically stop and restart the daemon with the current binary. Previously, a running daemon would be left untouched, causing stale-binary mismatches after updates. Both interactive and non-interactive (`--yes`) flows are covered.
- **Proactive stale daemon cleanup** — `is_daemon_running()` now removes orphaned PID and socket files when the referenced process is dead. This prevents connection attempts to stale Unix Domain Sockets after crashes or reboots.
- **UDS connection timeouts** — All daemon socket connections now have a 3-second connect timeout and 10-second I/O timeout. Previously, connections to a stale or unresponsive socket could block indefinitely, cascading into system-wide hangs.
- **Daemon readiness wait reduced** — The CLI auto-start readiness loop was reduced from 12 seconds to 3 seconds, keeping CLI commands responsive even when the daemon is slow to start.

### Improved

- **Website navigation completeness** — Added `/docs/concepts/multi-agent` to the Docs mega dropdown. Mobile navigation now includes all Context OS pages (Integrations, Shared Sessions, Context Bus, SDK) that were previously desktop-only.
- **Daemon documentation updated** — Integrations pillar and Context OS overview pages now document auto-restart on update, stale-file cleanup, and connection timeouts across all 11 languages.

## [3.5.5] — 2026-05-08

### Fixed

- **Search command compression blocked by auth-flow false positive** — `rg`, `grep`, `find`, `fd`, `ag`, and `ack` outputs were silently skipped by the compression pipeline whenever the search results contained OAuth-related strings (`device_code`, `user_code`, `verification_uri`, etc.) anywhere in the matched source code. This caused 0% savings for any `rg` search over a codebase that implements or references OAuth device-code flows — even though the output was search results, not an actual auth prompt. The fix skips the `contains_auth_flow` guard for search commands in both the CLI (`shell/compress.rs`) and MCP (`ctx_shell`) paths. Real auth flows (e.g. `az login`, `gh auth login`) are still preserved verbatim for non-search commands. Reported by aguarella (Discord).
- **Central `shorter_only` guard for all shell patterns** — Added a centralized length check in `patterns/mod.rs` that wraps every compressor (`FilterEngine`, `try_specific_pattern`, `json_schema`, `log_dedup`, `test`). No pattern can return `Some(result)` unless `result` is strictly shorter than the original output. Eliminates a class of bugs where patterns claimed compression without actually reducing size.
- **`grep` compressor removes verbatim threshold** — Removed the `<= 100 lines` early return that passed small `rg`/`grep` outputs through uncompressed. All search outputs are now grouped by file with per-file match limits, regardless of size. Combined with the `shorter_only` guard, small outputs that can't be meaningfully compressed correctly return `None` instead of faking 0% savings.
- **`gh` CLI verbatim returns replaced with `None`** — `gh pr diff`, `gh api`, `gh search`, `gh workflow`, and unknown `gh` subcommands no longer return `Some(output.to_string())` (which falsely claimed compression). They now return `None`, allowing fallback compressors or the caller to handle the output appropriately.
- **`safeguard_ratio` aligned with CLI behavior** — The MCP compression guard now uses a 5% floor only for small outputs (<2,000 tokens) and allows aggressive compression for large outputs, matching the CLI pipeline behavior.
- **`ctx_shell` search command inflation guard** — For search commands (`rg`, `grep`, etc.), the MCP handler now explicitly checks `c.len() <= output.len()` before using the compressed result, preventing any inflation from reaching the agent.
- **Codex `AGENTS.md` overwrite** — `install_codex_instruction_docs` now uses marked-block insertion (`<!-- lean-ctx -->...<!-- /lean-ctx -->`) instead of overwriting `~/.codex/AGENTS.md`, preserving user instructions. Reported by Vitu (Discord).

### Added

- **Knowledge CLI: export/import/remove** — Full CLI parity with MCP `ctx_knowledge`:
  - `lean-ctx knowledge export [--format json|jsonl|simple] [--output <path>]`
  - `lean-ctx knowledge import <path> [--merge replace|append|skip-existing] [--dry-run]`
  - `lean-ctx knowledge remove --category <cat> --key <key>`
  - Core: `import_facts()` with merge strategies, `export_simple()` for interop, `parse_import_data()` with auto-format detection.
  - Context OS: knowledge `import` events tracked via `KnowledgeRemembered` bus event.
- **Context OS optimizations** — Connection pooling for Context Bus R/W, broadcast channels replacing mutex-guarded Vec, inverted token index for BM25 search, LRU session eviction, metrics consolidation cleanup.

### Fixed (cont.)

- **Dashboard scroll after fullscreen** — `switchView()` now closes any active fullscreen before tab transitions, restoring scroll in all views. (GitHub #186)

## [3.5.4] — 2026-05-07

### Fixed

- **`gh` CLI compression safety** — Unknown `gh` subcommands (`gh pr diff`, `gh api`, `gh search`, `gh workflow`, `gh auth`, `gh secret`, etc.) now pass through verbatim instead of being truncated to 10 lines. Previously, fallback compressors (JSON, log-dedup) could also strip content from `gh api` and `gh search` output. The fix returns `Some(output)` for unmatched commands (blocking fallback compression), matching the safe behavior already used by `git` and `glab` patterns.
- **Uninstall proxy cleanup** — `lean-ctx uninstall` now cleans up Claude Code (`ANTHROPIC_BASE_URL` in `settings.json`) and Codex CLI (`OPENAI_BASE_URL` in `config.toml`) proxy settings. Previously only shell exports (Gemini) were removed, leaving Claude/Codex pointing at the dead local proxy after uninstall. If a saved upstream exists, Claude Code settings are restored to the original URL.
- **CLI `ls`/`grep` daemon path resolution** — `lean-ctx ls .` and `lean-ctx grep <pattern> .` now resolve relative paths to absolute before sending to the daemon, fixing incorrect directory listings when the daemon's CWD differs from the CLI's CWD.

### Added

- **Context Bus v2: Multi-Agent Coordination** — Major upgrade to the event bus with versioned events, causal lineage, consistency levels, and multi-agent conflict detection.
  - **Event versioning**: Every event now carries a monotonic `version` per (workspace, channel) and an optional `parentId` for causal chains.
  - **Consistency levels**: Events classified as `local` (informational), `eventual` (shared, async), or `strong` (requires sync) — enables agents to prioritize reactions.
  - **K-bounded staleness guard**: When a shared-mode agent falls behind by >10 events, tool responses include a `[CONTEXT STALE]` warning.
  - **Knowledge conflict detection**: Concurrent writes to the same knowledge key by different agents inject `[CONFLICT]` warnings before proceeding.
  - **Enriched payloads**: Event payloads now include `path`, `category`, `key`, and `reasoning` (from active session task) for richer observability.
  - **SSE backfill on lag**: When a broadcast subscriber falls behind, missed events are automatically backfilled from SQLite instead of dropped.
  - **New REST endpoints**: `GET /v1/context/summary` (materialized workspace view), `GET /v1/events/search` (FTS5 full-text search), `GET /v1/events/lineage` (causal chain traversal).
  - **Team Server scopes expanded**: `ctx_session`, `ctx_knowledge`, `ctx_artifacts`, `ctx_proof`, `ctx_verify` mapped to `sessionMutations`, `knowledge`, `artifacts`, `search` scopes.
  - **Session race fix**: `SharedSessionStore::get_or_load` uses atomic `entry` API to prevent TOCTOU races under concurrent agent loads.
- **Configurable proxy upstreams** — Teams routing through custom API gateways can now set `proxy.anthropic_upstream`, `proxy.openai_upstream`, and `proxy.gemini_upstream` via `lean-ctx config set` or environment variables. Upstreams are resolved once at proxy startup (env > config > default).
- **Proxy upstream diagnostics** — `lean-ctx doctor` validates proxy upstream URLs (self-referential loop detection, URL format) and reports which upstreams are active.
- **6 new adversarial compression tests** — `gh pr diff`, `gh api`, `gh search`, `gh workflow` verbatim passthrough, plus shell-hook-level diff preservation test.

### Changed

- **Dry-run uninstall** — `lean-ctx uninstall --dry-run` now previews Claude Code and Codex proxy cleanup actions.

## [3.5.3] — 2026-05-07

### Fixed

- **Dashboard command counter** — Shell commands in track-only mode (e.g. `git status`, `docker ps`) that use `exec_inherit` are now counted via `exec_inherit_tracked()`, and `record_shell_command` no longer skips zero-token commands. Previously many commands went unrecorded in the dashboard.
- **SLO false positives** — `CompressionRatio` SLO now requires a minimum of 5,000 original tokens before evaluating, and the threshold was raised from 0.75 to 0.90. Eliminates constant "violated CompressionRatio" warnings caused by `full` mode reads.
- **X11 clipboard in vim** — Removed explicit stripping of `DISPLAY`, `XAUTHORITY`, and `WAYLAND_DISPLAY` environment variables from `exec_buffered`, restoring X11 clipboard sync after exiting vim/vi in Claude Code.
- **pack_cmd unwrap** — `LocalRegistry::open()` now returns a graceful error instead of panicking on IO failures.
- **cursor.rs JSON type safety** — `merge_cursor_hooks` now validates JSON types before unwrapping, preventing panics when `hooks.json` contains unexpected structures.

### Added

- **Rules-staleness detection** — On the first MCP tool call of a session, lean-ctx checks whether the agent's rules file contains the current version marker. If outdated, a `[RULES OUTDATED]` warning is injected into the tool response, prompting the agent to re-read rules or run `lean-ctx setup`.

### Changed

- **Codebase maintainability** — Split `doctor.rs` (2,348 lines) into `doctor/{mod,integrations,fix}.rs` and `uninstall.rs` (1,859 lines) into `uninstall/{mod,agents,parsers}.rs` for better modularity.
- **Cloud-server cleanup** — Removed unused `jwt_secret` field from cloud-server config and auth state.

## [3.5.2] — 2026-05-07

### Fixed

- **Agent zombie cleanup** — `cleanup_stale()` now marks dead processes as `Finished` immediately regardless of age, fixing the "phantom agents" bug where terminated MCP sessions (e.g. from Claude Code subagents, `/superpowers`, `/gsd` plugins) stayed listed as "Active" in the Agent World dashboard indefinitely. Previously, agents were only cleaned up after 24 hours. Fixes the issue reported by daviddatu_.
- **Dashboard live-filter** — `build_agents_json()` now calls `cleanup_stale()` on every API request and additionally filters by `is_process_alive()` as a safety net, ensuring the Agent World dashboard never shows zombie entries.
- **CLI/MCP feature parity** — new `core::tool_lifecycle` module ensures CLI commands (`lean-ctx read`, `lean-ctx grep`, `lean-ctx ls`, `lean-ctx -c`) trigger the same side effects as MCP tools: session tracking, Context Ledger updates, heatmap recording, intent detection, and knowledge consolidation. Previously CLI-only users lost ~60% of Context OS features.
- **Daemon double-recording bug** — CLI reads routed through the daemon no longer record a second `(sent, sent)` stats entry with 0% savings, which was diluting the overall savings rate on the dashboard.
- **Search savings accuracy** — `ctx_search` now estimates native grep baseline cost at 2.5× raw match tokens (accounting for context lines, separators, and full paths), up from 1× which showed misleadingly low savings.
- **Track-mode dilution** — Shell commands in track-only mode (no compression) no longer record `(0, 0)` token entries that inflated command counts without contributing savings, improving the dashboard savings rate from ~30% to 86%+.
- **Crash-loop backoff guard** — MCP server startup now detects rapid restart loops (>5 starts in 30s) and applies exponential backoff (up to 60s), preventing system hangs during binary updates.
- **Stats flush for short-lived CLI** — explicit `stats::flush()` calls after CLI `read`, `grep`, `ls`, `diff`, `deps` commands ensure token savings from hook subprocesses are persisted to disk immediately.

### Changed

- **Agent HookMode reclassification** — CRUSH, Hermes, OpenCode, Pi, and Qoder moved from `CliRedirect` to `Hybrid` mode because their hook mechanisms cannot guarantee full interception of all tool types. Only Cursor, Codex CLI, and Gemini CLI remain in pure CLI-redirect mode.
- **Claude Code Hybrid mode** — Claude Code now uses Hybrid mode (MCP + hooks) instead of CLI-redirect. `lean-ctx init --agent claude` installs the MCP server entry in `~/.claude.json` and configures PreToolUse hooks for Bash compression. This ensures full functionality even in headless (`-p`) mode where PreToolUse hooks don't fire.
- **Antigravity dedicated hook** — `lean-ctx init --agent antigravity` now has its own installation function (no longer shares with Gemini CLI), correctly configuring MCP at `~/.gemini/antigravity/mcp_config.json` and hook matchers for Antigravity's native tools (`run_command`, `view_file`, `grep_search`).

## [3.5.1] — 2026-05-06

### Fixed

- **Tool Registry not initialized** — `ctx_tree`, `ctx_discover_tools`, and 23 other trait-based tools returned "Unknown tool" because the registry was never wired up at server startup. All 56 advertised tools are now dispatchable. Fixes #184.
- **Copilot CLI MCP path** — `lean-ctx init --agent copilot` now creates `.github/mcp.json` with the correct `"mcpServers"` key (per GitHub Copilot CLI spec), in addition to `.vscode/mcp.json` with the VS Code `"servers"` key. Previously wrote to the wrong path (`.github/copilot/mcp.json`) with the wrong key format.
- **Agent-scoped project rules** — `lean-ctx init --agent copilot` no longer creates `.cursorrules` or `.claude/rules/` files. Project rules are now scoped to the requested agent(s).
- **SKILL.md for Copilot/VS Code** — `lean-ctx setup` now installs SKILL.md for GitHub Copilot / VS Code users, and `lean-ctx doctor` checks the correct path (`~/.vscode/skills/lean-ctx/SKILL.md`).

## [3.5.0] — 2026-05-06

### Added

- **Context OS Runtime** — full integration of shared sessions, event bus, and SSE endpoints for real-time multi-agent collaboration. Agents can subscribe to context changes, broadcast events, and share session state across workspaces.
- **Daemon Mode** — persistent background daemon with CLI-first dispatch. `lean-ctx daemon start/stop/status` manages the process. All CLI commands route through the daemon for sub-millisecond response times and shared state.
- **Context Package System** — versioned, shareable context bundles with `lean-ctx pack create/list/info/export/import/install/remove/auto-load`. Package layers (knowledge, gotchas, config, graph) enable portable project intelligence.
- **Context Field Theory (CFT)** — unified model for context management with Context Potential Function, Rich Context Ledger, Context Overlay System, Context Handles, and Context Compiler.
- **Provider Framework** — pluggable provider system with GitLab integration and caching layer for external context sources.
- **Autonomy Drivers** — configurable agent autonomy levels with intent routing and degradation policies.
- **Context IR** — intermediate representation for context compilation, enabling cross-provider optimization.
- **Instruction Compiler** — `lean-ctx instructions` command compiles project-specific rules into optimized agent instructions.
- **Context Proof System** — `lean-ctx proof` generates verifiable context provenance chains for audit trails.
- **Team Server: Context OS scopes** — `SessionMutations`, `Knowledge`, and `Audit` scopes for fine-grained team permissions via `lean-ctx team token create`.
- **Qoder & QoderWork support** — new editor integration for Qoder IDE. PR #180 by @zsefvlol.
- **56 MCP tools** — exposed all registered tools for installed agents, including new `ctx_verify`, `ctx_proof`, `ctx_provider`, `ctx_artifacts`, `ctx_index` tools. Fixes #176.
- **38 Context OS integration tests** — comprehensive test suite covering multi-client concurrency, event bus, shared sessions, and SSE endpoints.
- **Windows OpenCode guide** — step-by-step manual for OpenCode on Windows 10. PR #181 by @HamedEmine.

### Changed

- **CLI-First Architecture** — all new modules (daemon, providers, instruction compiler, proof, overview, knowledge, compress, verify) are accessible as CLI subcommands, reducing MCP schema overhead.
- **Server Refactor** — modular tool registry with `ToolTrait`, pipeline stages, and per-tool dispatch for cleaner extensibility.
- **A2A alignment** — `ScratchpadEntry` now aligns with `A2AMessage` types for cross-agent interoperability.
- **HTTP-MCP contract** — extended with full Context OS API surface documentation.
- **Shell pattern library** — expanded to 95+ output compression patterns including clang, fd, glab, just, ninja.
- **Property Graph** — enhanced with metadata layer and reproducibility contract.

### Fixed

- **CLI relative path resolution** — paths are now resolved to absolute before sending to the daemon, preventing "file not found" errors when working directory differs.
- **`install.sh` POSIX compliance** — rewritten as pure POSIX sh so `curl | sh` works on dash (Ubuntu/Debian default). PR #175 by @narthanaj.
- **Qoder MCP config** — added `LEAN_CTX_FULL_TOOLS` to Qoder configuration for complete tool exposure. Includes clippy fixes.
- **Team SSE endpoint** — removed dead code and properly wired `audit_event` into the SSE stream.

## [3.4.7] — 2026-05-01

### Added

- **`ctx_call` meta-tool** — compatibility tool for MCP clients with static tool registries (e.g. Pi Coding Agent). Invoke any `ctx_*` tool by name via a stable schema without requiring dynamic `tools/list` refresh. Fixes #174.
- **Interactive Graph Explorer** — `ctx_graph action=export-html` generates a self-contained, interactive HTML visualization with pan/zoom, node selection, transitive highlighting, and PNG export.
- **Self-Hosted Team Server** — `lean-ctx team serve` enables shared context across workspaces with token-based auth, scoped permissions, rate limiting, and audit logging.

### Changed

- **Dual-format hook output** — `lean-ctx hook rewrite/redirect` now emits a combined JSON response compatible with both Cursor (`permission`/`updated_input`) and Claude Code (`hookSpecificOutput`). All IDEs that support PreToolUse hooks now work with the same command.
- **JetBrains config format** — `~/.jb-mcp.json` now uses the official `mcpServers` snippet format matching JetBrains AI Assistant documentation (was: nonstandard `servers` array).
- **Shell hook block markers** — `lean-ctx init --global` now writes stable `# lean-ctx shell hook — begin/end` markers, making updates idempotent and safe across reinstalls.

### Fixed

- **Claude Code hooks not intercepting subagent calls** — `extract_json_field` in hook handlers was too rigid for pretty-printed or spaced JSON from Claude Code. Now robustly handles all formatting styles. Fixes Discord report.
- **Claude Code hooks overwriting other plugins** — `install_claude_hook_config` now *merges* PreToolUse hooks instead of replacing the entire matcher group, preserving hooks from other plugins (e.g. obra/superpowers).
- **`lean-ctx doctor` false positive "pipe guard missing"** — on Windows Git Bash with XDG config paths, doctor now correctly detects shell hooks in both `~/.lean-ctx/` and `~/.config/lean-ctx/` directories, with both forward and backslash path separators. Fixes Discord report.
- **Pi Coding Agent array parameters** — `get_str_array` now accepts JSON-encoded strings (e.g. `"[\"a\",\"b\"]"`) in addition to native JSON arrays, fixing `ctx_multi_read` for the Pi MCP bridge. Fixes #173.
- **Windows CI test failure** — `workspace_config` tests now use `serde_json::json!` for path serialization, preventing invalid JSON escapes on Windows.

## [3.4.6] — 2026-04-30

### Added

- **Unified call graph tool** — new `ctx_callgraph` supports `direction=callers|callees` behind one stable entry point.
- **Graph diagram in unified graph API** — `ctx_graph` now supports `action=diagram` (with `kind=deps|calls` and optional `depth`).
- **Release-gate hardening tests** — added golden/edge coverage for `tokens.rs`, `preservation.rs`, `handoff_ledger.rs`, and workflow store roundtrips.
- **README entry paths** — new 3-tier onboarding/runtime paths (`Quick`, `Power`, `Enterprise`) with concrete commands and expected outcomes.
- **Knowledge graph auto-bootstrap** — when the dashboard's knowledge graph is empty, lean-ctx now automatically generates initial facts (project root, languages, index stats) so users see data immediately.
- **Startup guard (cross-process lock)** — new `core::startup_guard` module provides file-based locking with stale eviction, used to serialize concurrent startup and background maintenance.
- **Cookbook TypeScript SDK** — real integration examples with typed SDK.

### Changed

- **Deprecation aliases (no breaking change)**:
  - `ctx_callers`/`ctx_callees` now route to `ctx_callgraph` with deprecation hints.
  - `ctx_graph_diagram` now routes to `ctx_graph action=diagram` with deprecation hint.
  - `ctx_wrapped` now routes to `ctx_gain action=wrapped` with deprecation hint.
- **Tool metadata alignment** — descriptors, editor auto-approve lists, and docs updated for the unified entry points and 49-tool manifest.
- **Documentation/version hygiene** — README and VISION now consistently reference 49 MCP tools and current runtime state.
- **Legacy cleanup** — removed unlinked `core/watcher.rs` orphan module (no runtime references).
- **Cloud: OAuth2 client credentials** — cloud sync now supports OAuth2 token-based authentication.
- **Memory: configurable policies + knowledge relations** — knowledge facts support temporal relations and configurable retention policies.

### Fixed

- **SIGABRT under concurrent MCP startup** — multiple agent sessions starting simultaneously could crash the process. Fixed with `catch_unwind` at the process entry point, a cross-process startup lock, and capped Tokio worker/blocking threads. Fixes #171.
- **Dashboard stale index auto-rebuild** — `graph_index` and `vector_index` now detect when indexed files are missing and automatically rebuild, preventing empty Knowledge Graph and broken Compression Lab views.
- **Dashboard Compression Lab path healing** — when a file path from the index no longer exists (e.g. after refactoring), the API now tries suffix/filename matching against indexed files and returns actionable candidates. The UI shows clickable suggestions instead of a bare error.
- **Background maintenance stampede** — rules injection, hook refresh, and version checks are now guarded by a cross-process lock, preventing multiple instances from running expensive maintenance simultaneously during agent session initialization.
- **Panic hardening in verification/stats paths** — replaced remaining production `unwrap()` usage in critical library paths:
  - `core/output_verification.rs` fallback regex paths
  - `core/stats/mod.rs` optional buffer extraction
- **CLI guidance consistency** — `lean-ctx wrapped` now clearly points users to the canonical `lean-ctx gain --wrapped` path.
- **Cookbook npm audit vulnerabilities** — resolved all reported npm audit issues in the cookbook package.

## [3.4.5] — 2026-04-28

### Added

- **Agent Harness: Roles & Permissions** — 5 built-in roles (`coder`, `reviewer`, `debugger`, `ops`, `admin`) with configurable tool policies and shell access. Custom roles via `.lean-ctx/roles/*.toml` with inheritance. Server-side middleware blocks unauthorized tools with clear feedback. `ctx_session action=role` to list/switch roles at runtime.
- **Agent Harness: Budget Tracking** — per-session budget enforcement against role limits (context tokens, shell invocations, cost USD). Warning at 80%, blocking at 100%. `ctx_session action=budget` to check status. Budgets reset on role switch or session reset.
- **Agent Harness: Events** — new `EventKind` variants: `RoleChanged`, `PolicyViolation`, `BudgetWarning`, `BudgetExhausted`. All rendered in TUI Observatory with appropriate icons and colors.
- **Agent Harness: Cost Attribution** — real-time per-tool-call cost estimation using `ModelPricing`, recorded into the budget tracker for accurate USD tracking.
- **Agent Harness documentation** — new docs page with full i18n (53 keys × 11 languages), accessible at `/docs/agent-harness`.
- **`LEAN_CTX_DATA_DIR` for cloud config** — cloud client now respects the `LEAN_CTX_DATA_DIR` environment variable for its config directory. PR #168 by @glemsom.

### Fixed

- **MCP server crash recovery** — tool handler panics no longer kill the server (`panic = "unwind"` + `catch_unwind`). Server returns error message and stays alive for the next call. PR #167 by @DustinReynoldsPE.
- **`lean-ctx setup` ignoring config changes** — running setup a second time no longer silently ignores the user's new choices for `terse_agent` and `output_density`. Values are now upserted instead of skipped when keys already exist in `config.toml`.
- **Dashboard cost mismatch with `lean-ctx gain`** — dashboard computed cost savings with hardcoded pricing ($2.50/M input) while `gain` used dynamic model-specific rates. Dashboard now syncs pricing from the gain API for consistent numbers.
- **`ctx_session` tool description missing actions** — `role` and `budget` actions were implemented but not listed in the MCP tool descriptor, so LLMs couldn't discover them. Now documented in granular tool defs and templates.

### Credits

- @DustinReynoldsPE — MCP panic recovery (PR #167)
- @glemsom — `LEAN_CTX_DATA_DIR` cloud support (PR #168)

## [3.4.4] — 2026-04-28

### Fixed

- **Observatory File Heatmap blank** — the File Heatmap panel in `lean-ctx watch` stayed empty because historical per-file access data was never loaded on TUI startup. Now pre-populates from the persistent `heatmap.json` so file activity is visible immediately. Also fixed `EventTail` offset tracking to prevent event loss during concurrent writes. Fixes #166.
- **Windows agent hook installs** — `dirs::home_dir()` does not respect `HOME`/`USERPROFILE` overrides on Windows, causing hooks to install into incorrect directories during CI and in some user setups. Introduced a centralized `core::home::resolve_home_dir()` that checks `HOME`, `USERPROFILE`, and `HOMEDRIVE+HOMEPATH` before falling back to `dirs::home_dir()`. All 13 agent installers and the hook manager now use this resolver.
- **Windows `claude mcp add-json` invocation** — `.cmd` shims cannot be executed directly via `CreateProcess`; now routes through `cmd /C` for reliable invocation.
- **Clippy 1.95 compliance** — resolved all new lints introduced by Rust 1.95: `needless_raw_string_hashes`, `map_unwrap_or`, `unnecessary_trailing_comma`, `duration_suboptimal_units`, `while_let_loop` across 30+ source files.
- **`cargo-deny` 0.19 migration** — updated `deny.toml` to new schema, removed deprecated advisory fields, added missing dependency licenses (`0BSD`, `CDLA-Permissive-2.0`).
- **Windows benchmark stability** — `bench_rrf_eviction_vs_legacy` no longer panics from `Instant` underflow on short-lived processes.
- **Coverage timeout** — `benchmark_task_conditioned_compression` now skipped under tarpaulin instrumentation and uses smaller input to prevent CI timeouts.
- **Uninstall dry-run** — `lean-ctx uninstall --dry-run` no longer accidentally removes components.

### Changed

- **License updated to Apache-2.0** — all references across the repository and website (11 languages) updated from MIT to Apache-2.0.
- **Clippy pedantic across entire codebase** — comprehensive refactoring to satisfy `clippy::pedantic` with zero warnings: `Copy` derives, `map_or`/`is_ok_and` patterns, `Duration::from_hours/from_mins`, `while let` loops, and raw string simplification.
- **`cfg(tarpaulin)` declared in Cargo.toml** — prevents `unexpected_cfgs` lint failures when coverage attributes are used.

## [3.4.3] — 2026-04-27

### Fixed

- **Pi Agent compression loop** — agents using `pi-lean-ctx` could get stuck in a compression loop where `bash` output was too aggressively compressed, preventing the agent from extracting needed information. The `bash` tool now supports a `raw=true` parameter that bypasses compression entirely when exact output is critical. Fixes #159.
- **Hook handlers ignore `LEAN_CTX_DISABLED`** — `handle_rewrite`, `handle_codex_pretooluse`, `handle_copilot`, and `handle_rewrite_inline` now check `LEAN_CTX_DISABLED` env var and exit immediately when set. This prevents Claude Code subagents and rewind operations from being blocked by hooks. Fixes #162.
- **Telemetry claims in README/SECURITY.md** — replaced inaccurate "Zero telemetry / Zero network requests" claims with honest documentation of what network activity exists (daily version check, opt-in anonymous stats). Fixes #160.

### Added

- **Version check opt-out** — new `update_check_disabled = true` config option and `LEAN_CTX_NO_UPDATE_CHECK=1` env var to completely disable the daily version check against `leanctx.com/version.txt`.
- **Pi Agent `raw` parameter** — `bash` tool in `pi-lean-ctx` now accepts `raw=true` to skip compression, matching `ctx_shell raw=true` behavior in the MCP server.
- **`is_disabled()` guard** — centralized helper in `hook_handlers.rs` for consistent `LEAN_CTX_DISABLED` checks across all hook entry points.
- **New integration tests** — `hook_rewrite_disabled_produces_no_output` and `codex_pretooluse_disabled_exits_cleanly` verify the disabled guard behavior. `run_hook_test` helper explicitly removes inherited env vars to prevent test pollution.

### Changed

- **Data sharing default flipped** — `lean-ctx setup` now asks `[y/N]` (opt-in) instead of `[Y/n]` (opt-out). Users must explicitly choose to enable anonymous stats sharing.
- **Pi Agent tool prompts overhauled** — `description` fields for all 5 Pi tools (`bash`, `read`, `ls`, `find`, `grep`) rewritten to provide clear guidance on which tool to use for which task, aligning with Pi Agent's architecture where `description` is the primary LLM guidance field. Redundant `promptGuidelines` removed from `ls`/`find`/`grep`.
- **Pi Agent explicit entry point** — `pi-lean-ctx` now uses `./extensions/index.ts` as explicit entry point instead of relying on default resolution. PR #158 by @riicodespretty.

### Credits

- @glemsom — Pi Agent prompt improvements (PR #157) and architectural insights on `promptGuidelines` behavior (PR #161)
- @johnwhoyou — `LEAN_CTX_DISABLED` hook handler fix (PR #163)
- @riicodespretty — explicit extension entry point (PR #158)
- @pavelxdd — telemetry transparency request (Issue #160)

## [3.4.2] — 2026-04-26

### Fixed

- **Unicode SIGABRT in `ctx_overview`** — directory path truncation used byte-index slicing (`&dir[len-47..]`) which panicked on multi-byte UTF-8 characters (Chinese, Japanese, Korean, emoji paths). Replaced with `truncate_start_char_boundary()` that respects char boundaries. Fixes #154.
- **Windows shell detection in Git Bash / MSYS2** — `find_real_shell()` now checks `MSYSTEM`/`MINGW_PREFIX` env vars before `PSModulePath`, preventing incorrect PowerShell detection when running inside Git Bash. Fixes #156.

### Added

- **Shell hint in MCP instructions (Windows)** — on Windows, instructions now include detected shell type with explicit guidance (e.g. "SHELL: bash (POSIX). Use POSIX commands, not PowerShell cmdlets"), helping LLMs generate correct commands for the active shell environment.
- **Shell mismatch hint in `ctx_shell` responses (Windows)** — when a command fails and contains PowerShell cmdlets while the detected shell is POSIX, a correction hint is appended to the response.
- **`shell_name()` public API** — returns the short shell basename (e.g. "bash", "powershell", "cmd") for use in instructions and hints.

## [3.4.1] — 2026-04-25

Performance and token optimization release. Reduces per-session overhead by up to 64%.

### Added

- **`LEAN_CTX_NO_CHECKPOINT` env var** — disable auto-checkpoint injection independently from `minimal_overhead`
- **`PreparedSave` pattern** — `Session.save()` split into `prepare_save()` (CPU-only serialization under lock) + `write_to_disk()` (background I/O via `tokio::task::spawn_blocking`), removing disk I/O from the tool response hot path
- **`md5_hex_fast`** — 8x faster fingerprinting for outputs >16 KB by hashing prefix + suffix + length instead of full content
- **Benchmark tests** — 8 new tests covering token overhead budgets, cache effectiveness, compression density, session save latency, and MD5 performance

### Changed

- `count_tokens` called once per tool response (was up to 4x) — cached result reused for hints, cost attribution, and logging
- `CostStore` writes deferred to background thread via `spawn_blocking`
- `mcp-live.json` writes debounced to every 5th tool call (80% fewer disk writes)
- `compress_output` skipped entirely for `Normal` density (no string copy)
- Auto-checkpoint, meta-strings (savings/stale notes, shell hints, archive hints), and session blocks now all suppressed under `minimal_overhead`

### Fixed

- Integer overflow crash in `shell_efficiency_hint` when output tokens exceeded input tokens — now uses `saturating_sub`
- Synchronous `save()` restores retry counter on disk write failure, preserving auto-save semantics

## [3.4.0] — 2026-04-25

Addresses GitHub issues #150, #151, #152, #153.

### Changed (BREAKING)

- **Lazy tools now the default** — Only 9 core tools are exposed by default instead of 46. This reduces per-turn input token overhead by ~80%. Use `LEAN_CTX_FULL_TOOLS=1` to opt back in to all tools. The `ctx_discover_tools` tool lets agents discover and load additional tools on demand. (#153)

### Added

- **JSONC comment support** — `lean-ctx setup` and all editor config writers now parse JSON with `//` and `/* */` comments using a built-in JSONC stripper. Config files with comments (e.g. `opencode.json`) are no longer treated as invalid and overwritten. (#151)
- **XDG Base Directory compliance** — New installs use `$XDG_CONFIG_HOME/lean-ctx` (default `~/.config/lean-ctx/`) instead of `~/.lean-ctx`. Existing `~/.lean-ctx` directories are detected and used automatically — no migration required. (#152)
- **`minimal_overhead` config option** — Set `minimal_overhead = true` in config or `LEAN_CTX_MINIMAL=1` env var to skip session/knowledge/gotcha blocks in MCP instructions, minimizing token overhead for cost-sensitive workflows. (#153)
- **Shell hook disable** — New `--no-shell-hook` flag for `lean-ctx init`, `shell_hook_disabled = true` config option, and `LEAN_CTX_NO_HOOK=1` env var to disable the `_lc()` shell wrapper across all shells (bash, zsh, fish, PowerShell). MCP tools remain fully active. (#150)

### Fixed

- Shell hook source lines now use the resolved data directory path instead of hardcoded `~/.lean-ctx`, matching XDG-compliant installations.
- `upsert_source_line` detection works for both legacy and XDG hook paths (including Windows backslash paths).

## [3.3.9] — 2026-04-24

### Security & Safety Hardening (GitHub Issue #149)

Comprehensive response to the [TheDecipherist adversarial security review](https://github.com/TheDecipherist/rtk-test/blob/main/docs/rtk-findings.md) comparing lean-ctx vs RTK across 16 safety-critical scenarios. The review was conducted against v3.2.5 — many findings were already fixed in 3.3.x, and v3.3.9 addresses the remaining gaps.

#### Already Fixed (confirmed with adversarial tests since v3.3.x)
- **`git diff` code content**: `compress_diff_keep_hunks()` preserves all `+`/`-` changed lines, only trims context to max 3 lines per hunk
- **`df` root filesystem**: Verbatim passthrough — no compression applied to `df` output
- **`pytest` xfail/xpass**: Summary explicitly includes `xfailed`, `xpassed`, `skipped`, and `warnings` counters
- **`git status` DETACHED HEAD**: Passes through verbatim including "HEAD detached at" warning
- **`ls` shows `.env`**: No file filtering — all files including `.env` are shown
- **`pip list` all packages**: Full package list preserved — no truncation
- **`git stash` verbatim**: Passes git stash output through unchanged
- **`ruff` file:line:col**: Preserves all location references in linter output
- **`find` full paths**: Preserves complete absolute paths
- **`wc` via pipe**: Correctly reads stdin (piped input)
- **Log `CRITICAL`/`FATAL` severity**: `log_dedup` and `safety_needles` explicitly recognize and preserve CRITICAL, FATAL, ALERT, EMERGENCY severity levels

#### Fixed in v3.3.9
- **`git show` diff content** (CRITICAL): `compress_show()` now preserves full diff content using `compress_diff_keep_hunks()` instead of reducing to `hash message +N/-M`. Code review via `git show` is now safe.
- **`docker ps` health status** (CRITICAL): Added fallback detection for `(unhealthy)`, `(healthy)`, `(health: starting)`, and `Exited(N)` annotations that survive even when column-based parsing misaligns.
- **`git log` default cap** (HIGH): Increased from 50 to 100 entries (was ~20 in v3.2.5). With explicit `-n`/`--max-count`, no limit is applied. Truncation message clearly indicates omitted count.

#### New Adversarial Tests
- `adversarial_git_show_preserves_diff_content` — verifies code changes survive `git show`
- `adversarial_git_show_preserves_security_change` — verifies security-relevant removals (e.g. CSRF) are visible
- `adversarial_docker_ps_unhealthy_narrow_columns` — verifies health status survives tight column layouts
- `adversarial_docker_ps_exited_containers` — verifies crashed containers are shown
- `adversarial_git_log_100_plus_commits` — verifies 100-entry cap and truncation message
- `adversarial_git_log_explicit_limit_unlimited` — verifies `-n` bypasses default cap
- `adversarial_safeguard_ratio_prevents_over_compression` — verifies safety net prevents >85% compression
- `adversarial_shell_hook_preserves_errors_in_truncation` — verifies CRITICAL/ERROR lines survive shell hook truncation

### Dependency Security
- **rustls-webpki**: Confirmed already on patched version 0.103.13 (GHSA-82j2-j2ch-gfr8, DoS via panic on malformed CRL BIT STRING)

## [3.3.8] — 2026-04-24

### Bug Fixes
- **Windows TOML path quoting** (GitHub Issue #147): `lean-ctx update` and `lean-ctx setup` now write Windows paths in Codex `config.toml` using TOML single-quoted literal strings (`'C:\...'`) instead of double-quoted strings. Double-quoted TOML strings treat backslashes as escape sequences, causing Codex to fail with "too few unicode value digits". Affects all Windows users with backslash paths in Codex MCP config.

### Improvements
- **Leaner `ls` output** (PR #148 by @glemsom): `lean-ctx ls` now runs plain `ls` instead of `ls -la` by default, reducing token overhead. The agent can add `-la` flags when needed.

## [3.3.7] — 2026-04-23

### New Features
- **`lean-ctx ghost` CLI**: New command that reveals hidden token waste — shows unoptimized shell commands, redundant reads, and oversized contexts with monthly USD savings estimate. Supports `--json` for CI integration.
- **`ctx_review` MCP tool**: Automated code review combining impact analysis (`ctx_impact`), caller tracking (`ctx_callers`), and test file discovery. Three actions: `review` (full analysis), `diff-review` (review changed files from git diff), `checklist` (structured review questions).
- **Content-Defined Chunking** (Rabin-Karp): Opt-in rolling-hash chunking for `ctx_read` that creates stable chunk boundaries, improving LLM prompt cache hit rates across edits. Enable via `content_defined_chunking = true` in `config.toml`.
- **Claude Code Plugin Manifest**: `.claude-plugin/manifest.json` added for future Claude Code plugin marketplace integration.

### Improvements
- **Cache-Safety Doctor Check**: `lean-ctx doctor` now verifies that `cache_alignment` and `provider_cache` modules are operational (12 checks total).
- **`provider_cache` module activated**: Previously dormant cache provider module is now wired into the diagnostic pipeline.

## [3.3.6] — 2026-04-23

### Security Hardening
- **GitHub Actions pinned to SHA**: All 10 Actions across CI, Release, and CodeQL workflows are now pinned to immutable commit SHAs instead of mutable version tags, preventing supply-chain attacks. (CodeQL #24-#36)
- **File system race condition fixed**: TOCTOU vulnerability in VS Code extension's MCP config writer eliminated. (CodeQL #37)
- **CodeQL Python false positive resolved**: Stale `language:python` scan configuration removed; explicit CodeQL workflow now covers only Rust, JavaScript/TypeScript, and Actions.
- **Email masking in CLI**: `lean-ctx login/register/forgot-password` now mask email addresses in console output. (CodeQL #21-#23)

### Bug Fixes
- **TypeScript `.js` import resolution** (GitHub Issue #146): The graph builder now correctly resolves relative `.js` specifiers to `.ts` source files per the TypeScript module resolution spec. Covers `.js→.ts/.tsx`, `.jsx→.tsx/.ts`, `.mjs→.mts`, `.cjs→.cts`.
- **Graceful client disconnect**: When an IDE cancels the MCP connection before initialization completes, lean-ctx now exits silently instead of printing a confusing `expect initialized request` error.
- **Session ID uniqueness**: Session IDs now include an atomic counter suffix, preventing collisions when two sessions are created within the same millisecond.

### Improvements
- **Environment variable forwarding** (PR #144 by @glemsom): `pi-lean-ctx` now forwards the parent process environment to the lean-ctx subprocess, so config env vars (`LEAN_CTX_TERSE_AGENT`, `LEAN_CTX_ALLOW_PATH`, etc.) work correctly.

## [3.3.5] — 2026-04-23

### Multi-Project Workspace Support (GitHub Issue #141)
- **`allow_paths` in config.toml**: New config field to explicitly allow additional paths in PathJail. Useful for mono-repos and multi-project workspaces where projects live outside the detected root.
- **Auto-detect multi-root workspaces**: When the CWD has no project markers but contains 2+ child directories with markers (`.git`, `Cargo.toml`, `package.json`, etc.), lean-ctx auto-detects this as a workspace and allows all child projects via PathJail.
- **Improved error messages**: PathJail errors now include a hint suggesting `LEAN_CTX_ALLOW_PATH` or `allow_paths` in `config.toml`.

### Windows PowerShell Fixes (GitHub Issue #142)
- **Pipe-guard in profile snippet**: The `[Console]::IsOutputRedirected` check is now embedded directly in the PowerShell profile source line, preventing errors when IDEs redirect stdout.
- **Binary path resolution**: `resolve_portable_binary()` now takes only the first line of `where` output on Windows, and prefers `.cmd`/`.exe` variants to avoid corrupted path detection.

### CLI Improvements
- **`excluded_commands` via CLI** (PR #143 by @glemsom): `lean-ctx config set excluded_commands "make,go build"` now works.

### CI Stability
- **Fixed flaky test**: `startup_prefers_workspace_scoped_session` race condition resolved with timestamp separation.
- **Windows CI**: Python-dependent sandbox tests now skip gracefully when Python is unavailable on the runner.

## [3.3.4] — 2026-04-23

### Heredoc Support (GitHub Issue #140)
- **Smart heredoc detection in `ctx_shell`**: Heredocs are no longer blanket-rejected. Only heredoc + file redirect combinations (`cat <<EOF > file.txt`) are blocked. Legitimate uses like `psql <<EOF`, `git commit -m "$(cat <<'EOF'...)"`, and input piping are now allowed through.
- **Hook passthrough for heredoc commands**: The PreToolUse hook (Claude Code, Codex, Copilot) no longer wraps heredoc-containing commands in `lean-ctx -c '...'`. Heredocs cannot survive the quoting round-trip (newlines get escaped to `\\n`), so they are passed through to the shell directly.

### Headless MCP Mode
- **New `LEAN_CTX_HEADLESS=1` environment variable**: When set, the MCP server skips all auto-setup during `initialize()` — no rules injection, no hook updates, no version check, no agent registry writes. Session management and all MCP tools remain fully functional. Designed for users who manage their own configuration (e.g. custom launchers with `--append-system-prompt`).

### Cloud Auth Hardening
- **Login and Register are now separate commands**: `lean-ctx login` only calls `/api/auth/login`. `lean-ctx register` only calls `/api/auth/register`. The previous behavior auto-fell back to registration on any non-specific login error (network, 500, DNS), which caused users to unknowingly create duplicate accounts.
- **Clear error messages**: Specific guidance for wrong password, unverified email, non-existent account, and server errors.

### Interactive Setup with Premium Features
- **Setup wizard extended to 7 steps**: New "Premium Features" step offers configuration of Terse Agent Mode (off/lite/full/ultra), Tool Result Archive (on/off), and Output Density (normal/terse/ultra) during `lean-ctx setup`.

### Dependency Updates
- **Dependabot #12 resolved**: `rand 0.8.5` phantom dependency removed via `cargo update` (GHSA-cq8v-f236-94qc).
- Updated: `tokio` 1.52.1, `rustls` 0.23.39, `rmcp` 1.5.0, `uuid` 1.23.1, and 20+ other transitive dependencies.

### Premium Features — Tool Result Archive, Terse Agent Mode, Compaction Survival

#### Tool Result Archive + ctx_expand (Zero-Loss Compression)
- **Archive-on-disk**: Large tool outputs (>4096 chars) are automatically archived to `~/.lean-ctx/archives/` before density compression. The compressed response includes an `[Archived: ... Retrieve: ctx_expand(id="...")]` hint so the agent can retrieve the full original output at any time.
- **New MCP tool `ctx_expand`**: Retrieve archived tool output by ID. Supports full retrieval, line-range retrieval (`start_line`/`end_line`), pattern search (`search`), and listing all archives (`action="list"`).
- **Session-scoped archives**: Each archive entry is tagged with the session ID, enabling per-session listing and cleanup.
- **TTL-based cleanup**: Archives older than `max_age_hours` (default 48h) are automatically cleaned up. Configurable via `archive.max_age_hours` in `config.toml` or `LEAN_CTX_ARCHIVE_TTL` env var.
- **Idempotent storage**: Content-hash-based IDs ensure the same output is never stored twice.
- **Config**: `archive.enabled`, `archive.threshold_chars`, `archive.max_age_hours`, `archive.max_disk_mb` in `config.toml`. Env overrides: `LEAN_CTX_ARCHIVE`, `LEAN_CTX_ARCHIVE_THRESHOLD`, `LEAN_CTX_ARCHIVE_TTL`.

#### Bidirectional Token Optimization (Terse Agent Mode)
- **New `terse_agent` config**: Controls agent output verbosity via instructions injection. Levels: `off` (default), `lite` (concise, bullet points), `full` (max density, diff-only), `ultra` (expert pair-programmer, minimal narration).
- **Smart CRP interaction**: Terse `lite`/`full` are skipped when CRP mode is `tdd` (already maximally dense). `ultra` always applies as an additional layer.
- **CLI toggle**: `lean-ctx terse <off|lite|full|ultra>` for instant switching.
- **Per-project override**: `terse_agent = "full"` in `.lean-ctx.toml`.
- **Env override**: `LEAN_CTX_TERSE_AGENT=full`.

#### Compaction Survival (Session-Resilience)
- **`build_resume_block()`**: Generates a compact (~500 token) session resume containing task, decisions, modified files, next steps, archive references, and stats.
- **Automatic injection**: The resume block is injected into MCP instructions whenever an active session with tool calls exists, ensuring context survives agent compaction events.
- **New `ctx_session(action="resume")` action**: Explicit retrieval of the resume block for agents that need on-demand session state.

### Bug Fixes

#### `ctx_expand` not registered in MCP tool listing
- **Fixed**: `ctx_expand` was implemented (dispatch handler, archive storage, tool definition in `list_all_tool_defs()`) but was missing from `granular_tool_defs()` — the function that the MCP server actually uses to build the `tools/list` response. Agents could never discover or call `ctx_expand` despite the feature being fully coded. Now registered as tool #47.

#### `TerseAgent::effective()` ignores environment variable
- **Fixed**: `TerseAgent::effective()` was supposed to let `LEAN_CTX_TERSE_AGENT` override the config.toml value, but fell through to the config value when the env var was set to `"off"`. Rewritten to explicitly check the env var first, then fall back to config.

#### CLI dispatch sync — `terse`, `register`, `forgot-password` not wired in `main.rs`
- **Fixed**: `lean-ctx terse`, `lean-ctx register`, and `lean-ctx forgot-password` were implemented in `cli/dispatch.rs` but the primary dispatch in `main.rs` was missing the match arms. All three commands now work from the CLI.
- **New**: `lean-ctx forgot-password <email>` — sends a password reset email via the LeanCTX Cloud API. Previously referenced in help text but not implemented.
- **Help text**: Updated in both `main.rs` and `cli/dispatch.rs` to consistently list `terse`, `register`, and `forgot-password`.

#### `lean-ctx doctor` ignores `LEAN_CTX_DATA_DIR` (Discord: GlemSom)
- **Fixed**: `doctor` now uses `lean_ctx_data_dir()` instead of hardcoded `~/.lean-ctx` at all 4 locations: shell-hook checks, Docker env.sh path, data directory check, and `compact_score()`. Users with custom `LEAN_CTX_DATA_DIR` will now see correct paths in doctor output.

#### Windows "path escapes project root" (GitHub Issue #139)
- **Fixed**: `pathjail.rs` now uses `safe_canonicalize_or_self()` (which strips the `\\?\` verbatim prefix) instead of raw `std::fs::canonicalize()`. This resolves the mismatch where Windows canonicalized paths (`\\?\C:\Users\...`) didn't match normal paths (`C:/Users/...`), causing false "path escapes project root" errors on Windows with Codex.
- **Windows path normalization hardened**: `is_under_prefix_windows` now strips `\\?\` prefix before comparison, and `allow_paths_from_env` uses the safe canonicalization consistently.

### Shell Quoting Hardening

#### Bug fixes — Argument preservation for complex shell commands
- **Direct argv execution in `-t` mode**: Shell aliases (`_lc gh`, `_lc find`, etc.) now bypass the argv-to-string-to-argv round-trip entirely when multiple arguments are present. `exec_argv()` calls `Command::new().args()` directly, preserving em-dashes (`—`), `#` signs, nested quotes, and all other special characters exactly as the user's shell parsed them. Single-string commands still use `sh -c` for backward compatibility.
- **Single-quote wrapping for hook rewrites**: `wrap_single_command` in hook handlers now uses POSIX single-quote escaping (`'...'` with `'\''` for embedded single quotes) instead of double-quote escaping. This protects `$`, backticks, `!`, and `"` from unintended expansion when commands are passed through Claude Code, Codex, or Copilot hooks.
- **`gh` added to full passthrough**: All `gh` CLI commands (not just `gh auth`) are now excluded from compression and tracking. The GitHub CLI's output is typically short, and its complex argument patterns (multi-word `--comment` values, issue references with `#`) are prone to quoting issues.

#### Code quality
- 20+ new unit tests covering: `exec_direct` / `exec_argv` direct execution, `quote_posix` edge cases (em-dash, `$`, backtick, nested quotes), `wrap_single_command` special characters (`$HOME`, backticks, `find` with long exclude lists, `!`), and `gh` full passthrough verification.
- All integration tests updated for new single-quote format.

## [3.3.3] — 2026-04-28

### Session Stability + Dashboard Clarity

#### Bug fixes — Session root handling (PR #138)
- **Stale session root across checkouts**: Fixed issue where switching between project directories could load a session from a different workspace. New `load_latest_for_project_root()` scans all session files and returns the most recent session matching the target project root, using canonicalized path comparison.
- **Session normalization extracted**: `normalize_loaded_session()` now handles empty-string cleanup and stale project root healing in a single place, called from both `load_by_id()` and `load_latest_for_project_root()`.
- **Startup context detection**: New `detect_startup_context()` derives the correct project root and shell working directory at MCP server startup, even when the IDE provides only a subdirectory path (e.g. `repo/src`).
- **Trusted re-rooting**: `resolve_path()` now checks `startup_project_root` before allowing session re-rooting from absolute paths. Only paths matching the trusted startup root can trigger a re-root, preventing accidental session takeover by untrusted paths.
- **Helper functions**: Added `session_matches_project_root()`, `has_project_marker()`, and `is_agent_or_temp_dir()` to `session.rs` for robust session matching and stale-root detection.

#### Improvements — Dashboard and metrics clarity
- **0%-savings tools hidden from `lean-ctx gain`**: Write-only tools like `ctx_edit` that don't compress output are no longer shown in the "Top Commands" section, preventing confusing "0% savings" entries.
- **0%-savings tools hidden from `ctx_metrics`**: The MCP `ctx_metrics` tool now filters out tools with zero token activity from the "By Tool" breakdown.

#### Code quality
- Fixed all clippy warnings: resolved `MutexGuard` held across await points in tests, `vec!` macro used where array literal suffices, and `Default::default()` struct update with all fields specified.
- All 1295 tests pass with zero warnings, zero clippy errors, full parallel execution.

#### Closed issues
- **#137** (stale session root across checkouts): Fixed by PR #138.

## [3.3.2] — 2026-04-22

### Codex Hook Fix + Docker Knowledge Collision Prevention

#### Bug fixes — Codex CLI integration (PR #136)
- **Codex PreToolUse hook**: Added dedicated `handle_codex_pretooluse()` handler that uses block-and-reroute pattern (exit code 2) instead of the incompatible `updatedInput` field. Commands matched by lean-ctx compression rules are blocked with an actionable re-run suggestion.
- **Codex SessionStart hook**: New `handle_codex_session_start()` injects a short instruction telling Codex to prefer `lean-ctx -c "<command>"` for shell commands.
- **Refactored rewrite logic**: Extracted `rewrite_candidate()` from `handle_rewrite()` to share rewrite detection across Claude Code, Codex, Copilot, and inline-rewrite handlers. Eliminates duplicated skip/wrap/compound logic.
- **New `hooks/support.rs` module**: Shared helpers for hook installation — `install_named_json_server`, `upsert_lean_ctx_codex_hook_entries`, `ensure_codex_hooks_enabled`. Reduces code duplication across agent integrations.
- **Hook dispatch updated**: `lean-ctx hook codex-pretooluse` and `lean-ctx hook codex-session-start` subcommands added to both `main.rs` and `dispatch.rs`.
- **Doctor integration**: `doctor --fix` now sets `LEAN_CTX_QUIET=1` when running in JSON mode to suppress noisy setup output.

#### Bug fixes — Knowledge hash collisions in Docker environments
- **New `project_hash.rs` module**: Composite project hashing that combines the project root path with a detected project identity marker. Prevents knowledge collisions when different projects share the same Docker mount path (e.g. `/workspace`).
- **8 identity detection sources** (checked in priority order):
  1. `.git/config` → remote "origin" URL (normalized: lowercase, stripped `.git` suffix, SSH→path conversion)
  2. `Cargo.toml` → `[package] name`
  3. `package.json` → `"name"` field
  4. `pyproject.toml` → `[project] name` or `[tool.poetry] name`
  5. `go.mod` → `module` path
  6. `composer.json` → `"name"` field
  7. `settings.gradle` / `settings.gradle.kts` → `rootProject.name`
  8. `*.sln` → solution filename
- **Backward compatible**: When no identity marker is found, hash falls back to path-only (identical to pre-3.3.2 behavior). Existing projects without git/manifest files see zero change.
- **Auto-migration**: On `load()`, if the new composite hash directory doesn't exist but the old path-only hash does, knowledge files are automatically copied to the new location. Ownership verification prevents one project from claiming another's data.
- **Consolidated hashing**: Removed duplicate `hash_project()` from `gotcha_tracker.rs` — now uses shared `project_hash::hash_project_root()`.
- **20 new tests**: Collision avoidance, identity detection for all 8 ecosystems, git URL normalization, migration file copying, ownership verification (accept/reject), backward compatibility, empty directory handling.

#### Closed issues
- **#125** (feat: more cmdline compression): Closed — all requested patterns (bun, deno, vite) already implemented in v3.3.0+ and expanded further in v3.3.1.
- **#135** (bug: Codex PreToolUse hook uses unsupported updatedInput): Fixed by PR #136.

## [3.3.1] — 2026-04-18

### Shell Hook Hardening: Complete Developer Environment Coverage

Addresses user-reported issues where `npm run dev` hangs and shell compression is too aggressive for human-readable output. Massively expands passthrough command coverage across all developer ecosystems.

#### Bug fixes
- **`npm run dev` no longer hangs**: Script runner commands (`npm run dev`, `yarn start`, `pnpm serve`, `bun run watch`, etc.) are now recognized as long-running processes and bypass compression entirely. Previously, `exec_buffered` would wait forever for the dev server to exit.
- **`npm run` compression less aggressive**: `compress_run` now shows up to 15 lines verbatim (was 5) and keeps the last 10 lines of longer output (was 3).
- **Case-sensitive passthrough patterns fixed**: Patterns like `bootRun`, `-S`, `-A`, `-B` now correctly match after case normalization in `is_excluded_command`.

#### Shell passthrough expansion (~85 new entries)
- **Package manager script runners**: `npm run dev/start/serve/watch/preview/storybook`, `npm start`, `npx`, `pnpm run dev/start/serve/watch`, `pnpm dev/start/preview`, `yarn dev/start/serve/watch/preview/storybook`, `bun run dev/start/serve/watch/preview`, `bun start`, `deno task dev/start/serve`, `deno run --watch`
- **Python**: `flask run`, `uvicorn`, `gunicorn`, `hypercorn`, `daphne`, `django-admin runserver`, `manage.py runserver`, `python -m http.server`, `streamlit run`, `gradio`, `celery worker/beat`, `dramatiq`, `rq worker`, `ptw`, `pytest-watch`
- **Ruby/Rails**: `rails server/s`, `puma`, `unicorn`, `thin start`, `foreman start`, `overmind start`, `guard`, `sidekiq`, `resque`
- **PHP/Laravel**: `php artisan serve/queue:work/queue:listen/horizon/tinker`, `php -S`, `sail up`
- **Java/JVM**: `gradlew bootRun/run`, `gradle bootRun`, `mvn spring-boot:run`, `mvn quarkus:dev`, `sbt run/~compile`, `lein run/repl`
- **Go**: `go run`, `air`, `gin`, `realize start`, `reflex`, `gowatch`
- **.NET**: `dotnet run`, `dotnet watch`, `dotnet ef`
- **Elixir**: `mix phx.server`, `iex -S mix`
- **Swift**: `swift run`, `swift package`, `vapor serve`
- **Zig**: `zig build run`
- **Rust**: `cargo run`, `cargo leptos watch`, `bacon`
- **Task runners**: `make dev/serve/watch/run/start`, `just dev/serve/watch/start/run`, `task dev/serve/watch`, `nix develop`, `devenv up`
- **CI/CD**: `docker compose watch`, `skaffold dev`, `tilt up`, `garden dev`, `telepresence`, `act`
- **Networking/monitoring**: `mtr`, `nmap`, `iperf/iperf3`, `ss -l`, `netstat -l`, `lsof -i`, `socat`
- **Load testing**: `ab`, `wrk`, `hey`, `vegeta`, `k6 run`, `artillery run`

#### Smart script-runner detection
- New heuristic: any `npm run`/`pnpm run`/`yarn`/`bun run`/`deno task` command where the script name contains `dev`, `start`, `serve`, `watch`, `preview`, `storybook`, `hot`, `live`, or `hmr` is automatically treated as passthrough. Catches variants like `npm run dev:ssr`, `yarn start:production`, `pnpm run serve:local`, `bun run watch:css`.

#### New adversarial tests (12 tests)
- `npm install` package name/count preservation
- `npm install` explicit package names (`express`, `lodash`, `axios`)
- `cargo build` error codes (E0308, E0599) with file:line
- `eslint` rule IDs and error counts
- `go build` file:line error locations
- `docker build` step failure errors
- `tsc` type error codes (TS2304, TS2339) with file references
- `dotnet build` CS0246 errors and build result
- `composer install` package counts
- `cargo test` failure counts
- `kubectl get pods` CrashLoopBackOff/Error status
- `terraform plan` destructive action preservation

#### New passthrough tests (15 test functions)
Organized by ecosystem: npm, pnpm, yarn, bun/deno, Python, Ruby, PHP, Java, Go, .NET, Elixir, Swift/Zig, Rust, task runners, CI/CD, networking, load testing, smart detection, false-positive guard.

#### Website
- Fixed i18n validation: removed duplicate `docsGettingStarted.evalInit*` keys from 10 locale files that caused GitLab CI pipeline failure.

---

## [3.3.0] — 2026-04-21

### Adversarial Safety Hardening

This release addresses all 7 confirmed DANGEROUS compression findings from the [TheDecipherist/rtk-test](https://github.com/TheDecipherist/rtk-test) adversarial test suite (April 2026). LeanCTX now passes **16/16** comparative safety tests (up from 9/16 in v3.2.5).

#### CRITICAL fixes
- **`git diff` code content preserved**: Compression no longer reduces diffs to `file +N/-M`. All `+`/`-` lines (actual code changes) are preserved. Only `index` headers and excess context lines (>3 per hunk) are trimmed. Large diffs (>500 lines) show first 200 + last 50 lines per file. Security-relevant changes (CSRF bypasses, credential removals) are always visible.
- **`docker ps` health status preserved**: Refactored to header-based column parsing. `(unhealthy)`, `Exited (1)`, and multi-word statuses are always preserved verbatim. Container names and images included in output.
- **`df` verbatim passthrough**: Disk usage output is no longer compressed at all. Root filesystem info (`/dev/sda1 ... /`) can never be hidden by "last N lines" heuristics. Output is typically small (<30 lines), making compression unnecessary.
- **`npm audit` CVE IDs preserved**: Vulnerability details including CVE IDs, severity levels, package names, and fix recommendations are retained (up to 30 detail lines) alongside the summary counts.

#### HIGH fixes
- **`git log` truncation increased to 50**: Default truncation raised from 20 to 50 entries. User-specified `--max-count` / `-n` arguments are now respected (no truncation applied). Truncation message updated to suggest `--max-count=N`.
- **`pytest` xfail/xpass/warnings**: Summary now includes `xfailed`, `xpassed`, and `warnings` counters. Example: `pytest: 15 passed, 1 failed, 2 xfailed, 1 xpassed, 2 warnings (3.5s)`.
- **`grep`/`rg` verbatim up to 100 lines**: Outputs with ≤100 lines pass through unchanged. File grouping and context stripping only applies to larger outputs.
- **`pip uninstall` package names listed**: Shows all successfully uninstalled package names (up to 30) instead of just a count.
- **`docker logs` safety-needle scan**: Middle-section truncation now scans for critical keywords (FATAL, ERROR, CRITICAL, panic, OOMKilled, etc.) and preserves up to 20 safety-relevant lines.

#### Additional hardening
- **`git blame` verbatim up to 100 lines**: Small blame outputs pass through unchanged. Larger outputs summarize by author with line ranges.
- **`curl` JSON sensitive key redaction**: Keys matching `token`, `password`, `secret`, `auth`, `credential`, `api_key`, etc. have their values replaced with `REDACTED` in schema output.
- **`ruff check` file:line:col preserved**: Outputs with ≤30 issues pass through verbatim, preserving all `file:line:col` references. Larger outputs show first 20 references plus rule summary.
- **`log_dedup` regex fix**: Fixed a greedy regex (`[^\]]*` → `[^\]\s]*`) in timestamp stripping that consumed entire log messages, preventing proper deduplication. Added `CRITICAL` to severity detection.
- **`lightweight_cleanup` brace collapse**: Now only activates for outputs >200 lines with runs of >5 consecutive brace-only lines. Inserts `[N brace-only lines collapsed]` marker.
- **Safeguard ratio**: If pattern compression removes >95% of content (on outputs >100 tokens), the original output is returned with a warning to prevent over-compression.

### New: Safety Needles Module

New `safety_needles.rs` module provides centralized safety-critical keyword detection used across all compression paths. Keywords include: `CRITICAL`, `FATAL`, `panic`, `FAILED`, `unhealthy`, `Exited`, `OOMKilled`, `CVE-`, `denied`, `unauthorized`, `error`, `WARNING`, `segfault`, `SIGSEGV`, `SIGKILL`, `out of memory`, `stack overflow`, `permission denied`, `certificate`, `expired`, `corrupt`.

The `truncate_with_safety_scan` function in `shell.rs` ensures these keywords are preserved even during generic middle-section truncation (up to 20 safety-relevant lines kept).

### New: `lean-ctx safety-levels`

New command that displays a transparency table showing exactly how each command type is compressed:

- **VERBATIM** (7 commands): `df`, `git status`, `git stash`, `ls`, `find`, `wc`, `env` — zero compression
- **MINIMAL** (11 commands): `git diff`, `git log`, `docker ps`, `grep`, `ruff`, `npm audit`, `pytest`, etc. — light formatting, all safety-critical data preserved
- **STANDARD** (8 commands): `cargo build`, `npm install`, `eslint`, `tsc`, etc. — structured compression
- **AGGRESSIVE** (4 commands): `kubectl describe`, `aws`, `terraform`, `docker images` — heavy compression for verbose output

Also lists global safety features (needle scan, safeguard ratio, auth detection, min token threshold).

### New: `lean-ctx bypass "command"`

Runs any command with **zero compression** — guaranteed raw passthrough. Sets `LEAN_CTX_RAW=1` internally. Use when you need absolute certainty that output is unmodified:

```bash
lean-ctx bypass "git diff HEAD~1"   # guaranteed unmodified
lean-ctx -c "git diff HEAD~1"      # compressed (hunk-preserving)
```

### New: `lean-ctx init <shell>` (eval pattern)

Shell hook initialization now supports the industry-standard `eval` pattern used by starship, zoxide, atuin, fnm, and fzf. The shell code is always generated by the currently-installed binary, ensuring it's never stale after upgrades:

```bash
# bash: add to ~/.bashrc
eval "$(lean-ctx init bash)"

# zsh: add to ~/.zshrc
eval "$(lean-ctx init zsh)"

# fish: add to ~/.config/fish/config.fish
lean-ctx init fish | source

# powershell: add to $PROFILE
lean-ctx init powershell | Invoke-Expression
```

The existing file-based method (`lean-ctx init --global`) continues to work unchanged.

### New: Adversarial Test Suite in CI

21 dedicated adversarial + regression tests now run on every push/PR via a new `adversarial` job in GitHub Actions CI. Tests cover all 16 comparative scenarios from the external audit plus additional safety regression checks. This ensures compression safety is continuously verified.

### Changed
- `compression_safety.rs`: New module with structured `CommandSafety` table and `SafetyLevel` enum
- `shell_init.rs`: Refactored hook generation into `generate_hook_posix()`, `generate_hook_fish()`, `generate_hook_powershell()` for reuse by both file-based and eval-based init
- `ci.yml`: New `adversarial` job running `cargo test --test adversarial_compression`

## [3.2.9] — 2026-04-20

### Fixed
- **UTF-8 text corrupted on Windows PowerShell** (#131): `lean-ctx -c` with non-ASCII output (Russian, Japanese, Chinese, Arabic, etc.) produced mojibake because `String::from_utf8_lossy` misinterpreted Windows system codepage bytes as UTF-8. Introduced `decode_output()` that tries UTF-8 first, then falls back to Win32 `MultiByteToWideChar` for proper codepage-to-Unicode conversion. On PowerShell, additionally injects `[Console]::OutputEncoding = UTF8` and sets `SetConsoleOutputCP(65001)`. Fixed across shell hook, MCP server execute, and sandbox runners.
- **MCP `ctx_shell` commands hang on stdin** (#132, credit: @xsploit): Child processes spawned by the MCP server inherited the JSON-RPC stdin pipe, causing commands like `git` to block instead of receiving EOF. Fixed by setting `stdin(Stdio::null())` on all MCP child processes. Added `GIT_TERMINAL_PROMPT=0` and `GIT_PAGER=cat` to prevent interactive prompts.

### Added
- **MCP command timeout**: Shell commands executed via `ctx_shell` now have a configurable timeout (default 120s). Override with `LEAN_CTX_SHELL_TIMEOUT_MS` env var. Timed-out commands return exit code 124 with a clear error message.
- **Regression tests**: Added `execute_command_closes_stdin` and `git_version_returns_when_git_is_available` tests to prevent future stdin inheritance regressions.

## [3.2.8] — 2026-04-20

### Fixed
- **Codex `config.toml` parse error** (empty `[]` section header): Uninstall left orphaned `[mcp_servers.lean-ctx.tools.*]` sub-sections when removing the main `[mcp_servers.lean-ctx]` section, producing an invalid empty `[]` header on re-setup. Uninstall now removes all `mcp_servers.lean-ctx.*` sub-sections, and the writer defensively skips `[]` lines.
- **Gemini CLI MCP server not loading** (wrong config path): Setup wrote to `~/.gemini/settings/mcp.json` but Gemini CLI reads MCP servers from `~/.gemini/settings.json` under the `mcpServers` key. The MCP config was never loaded by Gemini CLI. Fixed with a new `GeminiSettings` writer that merges `mcpServers` into the existing `settings.json` without overwriting other keys (e.g. `hooks`).
- **Gemini CLI `autoApprove` not recognized**: Gemini CLI uses `"trust": true` for auto-approval, not `autoApprove`. Fixed to use the correct field.
- **Codex `codex_hooks=false` after reinstall**: Uninstall set `codex_hooks = false` but setup didn't reset it to `true`, leaving hooks disabled.

### Added
- **Autonomous intent inference**: `ctx_read` automatically infers a `StructuredIntent` from file access patterns (after 2+ files touched) without requiring explicit agent calls. `ctx_preload` auto-sets intent from task description when none is active or confidence is low.
- **Auto agent registration**: MCP `initialize` handler automatically registers the connecting agent in the `AgentRegistry` based on client name (Cursor/Claude → coder, Antigravity/Gemini → explorer, etc.). Override via `LEAN_CTX_AGENT_ROLE` env var.
- **Context Layer dashboard tab**: New "Context Layer" tab in the localhost dashboard with Pipeline Stats, Context Window pressure, Mode Distribution, and Context Ledger table. Backed by new API endpoints `/api/pipeline-stats`, `/api/context-ledger`, `/api/intent`.
- **Pipeline & Ledger persistence**: `PipelineStats` and `ContextLedger` now persist to disk (`pipeline_stats.json`, `context_ledger.json`) so dashboard data survives server restarts.
- **Codex/Cursor hooks in setup**: `lean-ctx setup` now explicitly installs Codex hook scripts and Cursor hooks as a dedicated step, ensuring hooks are present even on first setup.

### Changed
- **IDE config audit**: All 13 supported IDE configurations verified against official vendor documentation (Cursor, Claude Code, Codex, Windsurf, VS Code/Copilot, Gemini CLI, Antigravity, Amazon Q, Hermes, Cline, Roo Code, Amp, Kiro).

## [3.2.6] — 2026-04-19

### Fixed
- **Project root stuck at agent sandbox path** (#124): The MCP session could retain a stale project root from a temporary directory (e.g. `~/.claude`, `/tmp/`). Fixed with multi-layer healing: `initialize` now validates roots against project markers, `session::load_by_id` detects and corrects agent/temp roots, and `resolve_path` can auto-update a suspicious root when given an absolute project path. Agents like Codex that start in sandbox directories now correctly resolve the actual project.
- **`lean-ctx gain` showing 0% for Shell Hooks** (#126): Small savings percentages were rounded to 0% in the "Savings by Source" and "Live Observatory" sections. Introduced `format_pct_1dp` for one-decimal-place display, `<0.1%` for very small values, and `n/a` when no input data exists.
- **`install.sh` fails on WSL2/Ubuntu** (`set: Illegal option -o pipefail`): `curl -fsSL leanctx.com/install.sh | sh` failed because `install.sh` used Bashisms but was executed by POSIX `sh` (dash). Added a POSIX-compliant preamble that auto-detects and re-executes under `bash`, with a clear error message if `bash` is unavailable. Both `| sh` and `| bash` now work.
- **Dashboard "Live Observatory" showing 0 tokens saved**: The Live Observatory pulled data exclusively from the active MCP session, ignoring shell hook savings. Now falls back to today's aggregate daily stats when no MCP session is active.

### Added
- **`rules_scope` configuration**: Control where agent rule files are installed — `"global"` (home directory only), `"project"` (repo-local only), or `"both"` (default). Avoids duplicate rule files that waste context tokens. Configurable via `config.toml`, `LEAN_CTX_RULES_SCOPE` env var, `lean-ctx config set rules_scope`, or per-project `.lean-ctx.toml` override.
- **Codex/Claude path jail auto-allowlist**: When running inside Codex CLI (`CODEX_CLI_SESSION` set), `~/.codex` is automatically added to allowed paths. Similarly, `~/.claude` is auto-allowed for Claude Code sessions. No manual `LCTX_ALLOW_PATH` needed.
- **`bunx` and `vp`/`vite-plus` CLI compression** (#125): Shell hook now routes `bunx` commands through the bun compressor and `vp`/`vite-plus` through the Next.js build compressor.
- **`lean-ctx update` auto-refreshes setup**: Running `lean-ctx update` now automatically re-runs the full setup (shell hooks, MCP configs, rules) after updating, even when already on the latest version. Ensures all wiring stays current.
- **Website docs**: `rules_scope` documented on configuration page in all 11 languages.

## [3.2.5] — 2026-04-18

### Fixed
- **Critical: shell hook recursion causing 100% CPU/memory** — The `.zshenv` / `.bashenv` shell hooks introduced in v3.2.4 were missing the `LEAN_CTX_ACTIVE` recursion guard. When an AI agent (Claude Code, Codex, etc.) ran a command, `lean-ctx -c` spawned a new shell that re-triggered the hook infinitely, causing a fork bomb. Fixed by checking `LEAN_CTX_ACTIVE` before intercepting and adding a double-guard in `exec()`. Users must run `lean-ctx setup` after updating to refresh the hooks.

## [3.2.4] — 2026-04-18

### Fixed
- **Git stash compression too aggressive** (#114): `git stash list` with ≤5 entries is now preserved verbatim. `git stash show -p` correctly routes to the diff compressor instead of the stash compressor. Added `safeguard_ratio` to `ctx_shell` to prevent over-compression (minimum 15% of original output preserved).
- **Windows Bash hook path stripping** (#113): On Windows with Git Bash / MSYS2, the lean-ctx binary path had slashes stripped (`E:packageslean-ctx.exe` instead of `/e/packages/lean-ctx.exe`). `resolve_binary()` now applies `to_bash_compatible_path` on all platforms.
- **Windows UNC path breakage** (`\\?\` prefix): `std::fs::canonicalize()` on Windows adds extended-length path prefixes that break tools and string comparisons. New `core::pathutil` module provides `safe_canonicalize()` and `strip_verbatim()` used consistently across graph indexing, session state, path jailing, architecture tool, and hook handlers.
- **Dashboard showing empty graphs**: `detect_project_root_for_dashboard()` was using the MCP session's temp sandbox directory instead of the actual project. Now validates project roots against `.git` and project markers before using them; falls through to `shell_cwd` when project_root is invalid. Added `--project=` CLI flag and `LEAN_CTX_DASHBOARD_PROJECT` env var for explicit override.
- **Dashboard Call Graph/Route Map empty states**: Enriched `/api/call-graph` and `/api/routes` responses with metadata (indexed file count, symbol count, route candidates) so the UI shows actionable guidance instead of generic "nothing found" messages.
- **Codex uninstall incomplete** (#116): `lean-ctx uninstall` now correctly removes the `[mcp_servers.lean-ctx]` section from Codex's TOML config, removes `~/.codex/hooks.json`, and resets the `codex_hooks` feature flag.
- **Repo-local config missing fields** (#98): `merge_local()` now supports `auto_consolidate`, `dedup_threshold`, `consolidate_every_calls`, `consolidate_cooldown_secs`, and bidirectional `silent_preload` override from `.lean-ctx.toml`.

### Added
- **Hermes Agent support** (#112): Full integration for Hermes Agent (Nous Research). `lean-ctx init --agent hermes --global` configures MCP via YAML (`~/.hermes/config.yaml`), creates `HERMES.md` rules. Setup auto-detects `~/.hermes/`, doctor checks Hermes config, uninstall cleans up YAML + rules.
- **Kotlin graph analysis** (#96): `ctx_graph`, `ctx_callers`, and `ctx_callees` now produce meaningful results for Kotlin projects. Tree-sitter-backed import extraction, call-site analysis, type-definition extraction, and Java interop with stdlib filtering.
- **Repo-local configuration** (#98): `.lean-ctx.toml` in project root for per-project overrides. Supports `extra_ignore_patterns` (graph/overview exclusions), autonomy settings, and all config fields. `lean-ctx cache reset --project` clears only current project's cache.
- **Post-update MCP refresh**: `lean-ctx update` now verifies and refreshes MCP configurations for all detected editors after binary replacement.
- **Dashboard "Savings by Source"**: Live Observatory and `lean-ctx gain` now show a breakdown of MCP Tools vs. Shell Hooks with individual compression rates and proportional bars.
- **Pi MCP bridge resilience**: Host-cancelled tool calls are handled cleanly with abort signal forwarding and error normalization. Hung MCP calls timeout after 120s with automatic reconnect and retry for read-safe tools. Bridge status includes diagnostics (last error, hung tool, retry state).

### Community
- Merged PR #111 — fix Windows graph path compatibility (@Chokitus)
- Merged PR #115 — handle host-cancelled MCP tool calls in Pi bridge (@frpboy)
- Merged PR #118 — improve dashboard empty-state UX for Route Map and Call Graph (@frpboy)
- Merged PR #122 — timeout and retry hung MCP tool calls in Pi bridge (@frpboy)

## [3.2.3] — 2026-04-17

### Fixed
- **Claude Code project rules missing** (cowwoc): `lean-ctx init --agent claude-code` now creates `.claude/rules/lean-ctx.md` in the project root (project-local rules), in addition to the existing global `~/.claude/rules/lean-ctx.md`. Claude Code reads both locations.
- **`--help` missing commands** (#109): `watch` (live TUI dashboard) and `cache` (file cache management) were implemented but not listed in `lean-ctx --help`.
- **install.sh fails without Rust** (#108): `curl -fsSL https://leanctx.com/install.sh | sh` now auto-detects missing `cargo` and downloads a pre-built binary instead of failing. Users with Rust still get a source build by default.

## [3.2.2] — 2026-04-17

### Added
- **Smart Shell Mode**: New `-t` / `--track` subcommand for human shell usage — full output preserved, only stats recorded. Shell aliases (`_lc`) now default to track mode instead of compress mode, eliminating unwanted output compression for interactive users.
- **`lean-ctx-mode` shell function**: Switch between `track` (default), `compress`, and `off` modes without editing config files. Available in both POSIX (bash/zsh) and Fish shells.
- **`_lc_compress` shell function**: Explicit compression wrapper for power users who want compressed output in their terminal.
- **Unified Rewrite Registry** (`rewrite_registry.rs`): Single source of truth for all 24+ rewritable commands, used consistently across shell aliases, hook rewrite, and compound command lexer.
- **Compound Command Lexer** (`compound_lexer.rs`): Intelligent splitting of `&&`, `;`, `||` compound commands for selective rewriting — only rewritable segments get wrapped with `-c`.
- **Extended hook support**: Copilot hooks now recognize `runInTerminal`, `run_in_terminal`, `shell`, and `terminal` tool names in addition to `Bash`/`bash`.
- **Dashboard API routes**: New `/api/symbols`, `/api/call-graph`, `/api/routes`, `/api/search` endpoints for the web dashboard.
- **22 IDE/agent targets**: Rules injection now supports Crush, Verdent, Pi Coding Agent, AWS Kiro, Antigravity, Qwen Code, Trae, Amazon Q Developer, and JetBrains IDEs (22 total).

### Fixed
- **Shell commands compressed for humans** (#101): `ls`, `git status`, and other aliased commands were always compressed because `_lc` used `-c`. Now defaults to `-t` (track) which preserves full output.
- **"Authorization required" on Ubuntu** (#101): `exec_buffered` pipe redirection triggered X11/Wayland auth errors on headless Linux. Track mode uses `exec_inherit_tracked` (direct stdio), avoiding this entirely.
- **Token counting accuracy**: `stats::record` now uses `count_tokens()` (tiktoken) instead of byte length for output measurement.
- **Dashboard Windows path normalization**: Compression Lab demo paths now correctly handle Windows absolute paths (merged PR #102).
- **Dashboard "d streak" label**: Fixed to display "days streak" (merged PR #106).

### Community
- Merged PR #102 — fix compression lab path resolution (@frpboy)
- Merged PR #103 — add symbols API route (@frpboy)
- Merged PR #104 — add call graph API route (@frpboy)
- Merged PR #106 — fix dashboard streak label (@frpboy)

## [3.2.1] — 2026-04-17

### Fixed
- **crates.io publish**: Claude Agent Skill assets (`SKILL.md`, `install.sh`) are now packaged inside the Rust crate so `cargo publish` verification succeeds.
- **Release CI**: Build `aarch64-unknown-linux-musl` via `cargo-zigbuild` for reliable ARM64 musl cross-compilation (fixes glibc symbol leaks from `gcc-aarch64-linux-gnu`).

## [3.2.0] — 2026-04-17

### Breaking
- **License changed from MIT to Apache-2.0**. All code from this release onwards is Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

### Added
- **Context Engine + HTTP server mode**: `lean-ctx serve` exposes all 48 MCP tools via REST endpoints with rate limiting, timeouts, and graceful shutdown — enables embedding lean-ctx as a library.
- **Memory Runtime (autopilot)**: Adaptive forgetting, salience tagging, consolidation engine, prospective memory triggers, and dual-process retrieval router — all token-budgeted and zero-config.
- **Reciprocal Rank Fusion (RRF) cache eviction**: Replaces the Boltzmann-weighted eviction scoring. RRF handles signal incomparability (recency vs frequency vs size) without tuned weights (K=60).
- **Claude Code 2048-char truncation fix**: Auto-detects Claude Code and delivers ultra-compact instructions (<2048 chars). Full instructions installed as `~/.claude/rules/lean-ctx.md`.
- **Claude Agent Skills auto-install**: `lean-ctx init --agent claude` installs `SKILL.md` + `scripts/install.sh` under `~/.claude/skills/lean-ctx/`.
- **ARM64 Linux support**: `aarch64-unknown-linux-musl` binary in release pipeline. Docker instructions updated for Graviton/ARM64.
- **IDE extensions**: JetBrains (Kotlin/Gradle), Neovim (Lua), Sublime Text (Python), Emacs (Elisp) — all thin-client architecture.
- **Security layer**: PathJail (FD-based, single choke point for 42 tools), bounded shell capture, size caps, TOCTOU prevention in `ctx_edit`, symlink leak fix in `ctx_search`, prompt-injection fencing.
- **Unified Gain Engine**: `GainScore` (0–100), `ModelPricing` (embedded cost table), `TaskClassifier` (13 categories), `ctx_gain` MCP tool, TUI/Dashboard/CLI integration.
- **Docker/Claude Code MCP self-healing**: `env.sh` re-injects MCP config when Claude overwrites `~/.claude.json`. Doctor detects and hints fix.
- **Compression deep optimization**: Thompson Sampling bandits for adaptive thresholds, Tree-sitter AST pruning, IDF-weighted deduplication, Information-Bottleneck task filtering, Verbatim Compaction.
- **`lean-ctx -c` now compresses on TTY** (fixes #100): Previously skipped compression when stdout was a terminal, showing 0% savings.
- **Quality column in `ctx_benchmark`**: Shows per-strategy preservation score (AST + identifier + line preservation).

### Fixed
- **CLI `-c` TTY bypass** (#100): `lean-ctx -c 'git status'` now compresses even in terminal (sets `LEAN_CTX_COMPRESS=1`).
- **Windows `Instant` overflow**: RRF eviction test used `now - Duration` which underflows on Windows. Fixed with `sleep`-based offsets + `checked_duration_since`.
- **rustls-webpki CVE**: Updated from 0.103.11 to 0.103.12 (wildcard/URI certificate name constraint fix).
- **MCP server hangs on large projects**: Parallelized tool calls prevent blocking.
- **Dashboard ERR_EMPTY_RESPONSE in Docker**: Bind host + panic recovery → HTTP 500 JSON instead of empty response.
- **Kotlin graph analysis**: AST-span-based symbol ranges for accurate call-graph edges.

### Refactored
- **Dead code elimination**: Removed 598 lines (unused `eval.rs`, CEP benchmark, dead CLI helpers). Reduced `#[allow(dead_code)]` from 32 to 5.
- **Cache store zero-copy**: Replaced `CacheEntry` clone with lightweight `StoreResult` struct (no content duplication).
- **Entropy dedup**: Precomputed n-gram sets with size-ratio filter (exact Jaccard, no allocation storms).
- **Clippy clean**: 0 warnings with `-D warnings` across all targets (1029 tests passing).

### Community
- Merged PR #94 (responsive dashboard — @frpboy)
- Merged PR #95 (MCP performance — @frpboy)
- Merged PR #97 (Kotlin graph support — @Chokitus)

## [3.1.5] — 2026-04-15

### Fixed
- **`claude_config_json_path()` simplified**: Removed over-complex `parent()` fallback logic that guessed at `.claude.json` locations. Now directly uses `$CLAUDE_CONFIG_DIR/.claude.json` as documented by Claude Code.
- **`lean-ctx init --agent claude` now prints config path**: Previously gave zero feedback about where MCP config was written. Now shows `✓ Claude Code: MCP config created at /path/to/.claude.json` — immediately reveals path mismatches (e.g. Docker USER mismatch writing to `/root/.claude.json` instead of `/home/node/.claude.json`).
- **`refresh_installed_hooks()` hardcoded `~/.claude/`**: Hook detection in `hooks.rs` ignored `$CLAUDE_CONFIG_DIR`, always checking `~/.claude/hooks/` and `~/.claude/settings.json`. Now uses `claude_config_dir()`.
- **Rules injection hardcoded `~/.claude/CLAUDE.md`**: `rules_inject.rs` always wrote to `~/.claude/CLAUDE.md` regardless of `$CLAUDE_CONFIG_DIR`. Now uses `claude_config_dir()`.
- **Uninstall hardcoded `~/.claude/`**: `remove_rules_files()` and `remove_hook_files()` couldn't find Claude Code files when `$CLAUDE_CONFIG_DIR` was set. Now uses `claude_config_dir()`.
- **Doctor display hardcoded `~/.claude.json`**: `lean-ctx doctor` always showed `~/.claude.json` even when `$CLAUDE_CONFIG_DIR` pointed elsewhere. Now shows the actual resolved path.

## [3.1.4] — 2026-04-15

### Added
- **`CLAUDE_CONFIG_DIR` support**: `lean-ctx init --agent claude`, `lean-ctx doctor`, `lean-ctx uninstall`, hook installation, and all Claude Code detection paths now respect the `$CLAUDE_CONFIG_DIR` environment variable. Previously hardcoded to `~/.claude.json` and `~/.claude/`.
- **`CLAUDE_ENV_FILE` Docker hint**: `lean-ctx init --global` and `lean-ctx doctor` now recommend setting `ENV CLAUDE_ENV_FILE` alongside `ENV BASH_ENV` in Docker containers. Claude Code sources `CLAUDE_ENV_FILE` before every command — this is the [officially recommended](https://code.claude.com/docs/en/env-vars) shell environment mechanism.
- **Doctor check for `CLAUDE_ENV_FILE`**: In Docker environments, `lean-ctx doctor` now shows separate checks for both `BASH_ENV` and `CLAUDE_ENV_FILE`.

### Fixed
- **Claude Code `_lc` not found in Docker** (#89): Root cause was that `BASH_ENV` alone doesn't work for Claude Code — it uses `CLAUDE_ENV_FILE` to source shell hooks before each command. Recommended Dockerfile now includes `ENV CLAUDE_ENV_FILE="/root/.lean-ctx/env.sh"`.
- **`CLAUDE_CONFIG_DIR` ignored everywhere**: `setup.rs`, `rules_inject.rs`, `doctor.rs`, `hooks.rs`, `uninstall.rs`, and `report.rs` all hardcoded `~/.claude.json` / `~/.claude/`. Now all paths go through `claude_config_json_path()` / `claude_config_dir()` which check `$CLAUDE_CONFIG_DIR` first.
## [3.1.3] — 2026-04-15

### Docker & Container Support

- **Auto-detect Docker/container environments** via `/.dockerenv`, `/proc/1/cgroup`, and `/proc/self/mountinfo`
- **Write `~/.lean-ctx/env.sh`** during `lean-ctx init --global` — a standalone shell hook file without the non-interactive guard (`[ -z "$PS1" ] && return`) that most `~/.bashrc` files have
- **Docker BASH_ENV warning**: when Docker is detected and `BASH_ENV` is not set, `lean-ctx init` now prints the exact Dockerfile line needed: `ENV BASH_ENV="/root/.lean-ctx/env.sh"`
- **`lean-ctx setup` auto-fallback**: detects non-interactive terminals (no TTY on stdin) and automatically runs in `--non-interactive --yes` mode instead of hanging
- **`lean-ctx doctor` Docker check**: new diagnostic that warns when running in a container with bash but without `BASH_ENV` set

### Critical Fix

- **`BASH_ENV="/root/.bashrc"` never worked in Docker** — Ubuntu/Debian `.bashrc` has `[ -z "$PS1" ] && return` which skips the entire file in non-interactive shells. The new `env.sh` approach bypasses this completely.

## [3.1.2] — 2026-04-14

### Fix Agent Search Loops in Large Projects

#### Fixed

- **Agents looping endlessly on search in large/monorepo projects** — root cause was a triple failure: over-aggressive compression hid search results from the agent (only 5 matches/file, 80-char truncation, then generic_compress cut to 6 lines), loop detection only caught exact-duplicate calls (threshold 12 was far too high), and no cross-tool or pattern-similarity tracking existed. Agents alternating between `ctx_search`, `ctx_shell rg`, and `ctx_semantic_search` with slight query variations were never detected as looping.

#### Improved

- **Smarter loop detection** — thresholds lowered from 3/8/12 to 2/4/6 (warn/reduce/block). Added cross-tool search-group tracking: any 10+ search calls within 300s triggers block regardless of tool or arguments. Added pattern-similarity detection: searching for "compress", "compression", "compress_output" etc. now counts as the same semantic loop via alpha-root extraction.
- **Configurable loop thresholds** — new `[loop_detection]` section in `config.toml` with `normal_threshold`, `reduced_threshold`, `blocked_threshold`, `window_secs`, and `search_group_limit` fields.
- **Better search result fidelity** — grep compression now shows 10 matches per file (was 5) with 160-char line truncation (was 80), preserving full function signatures. `generic_compress` scales with output size (shows ~1/3 of lines, max 30) instead of a fixed 6-line truncation.
- **Search commands bypass generic compression** — grep, rg, find, fd, ag, and ack output is no longer crushed by `generic_compress`. Pattern-specific compression is applied when available, otherwise results are returned uncompressed.
- **Actionable loop-detected messages** — blocked messages now guide agents to use `ctx_tree` for orientation, narrow with `path` parameter, and use `ctx_read mode='map'` instead of generic "change your approach" text.
- **Monorepo scope hints** — when `ctx_search` results span more than 3 top-level directories, a hint is appended suggesting the agent use the `path` parameter to scope to a specific service.

## [3.1.1] — 2026-04-14

### Windows Shell Hook Fix + Security

#### Fixed

- **PowerShell npm/pnpm/yarn broken on Windows** — the `foreach` loop in the PowerShell hook resolved npm to its full application path (`C:\Program Files\nodejs\npm.cmd`). When this path contained spaces, POSIX-style quoting caused PowerShell to output a string literal instead of executing the command. Now uses bare command names, consistent with git/cargo/etc. (fixes [#38](https://github.com/yvgude/lean-ctx/issues/38))
- **PowerShell `_lc` off-by-one** — `$args[1..($args.Length)]` produced an extra `$null` element. Replaced with `& @args` splatting which correctly handles all argument counts.
- **Password shown in cleartext during `lean-ctx login`** — interactive password prompt now uses `rpassword` to disable terminal echo, so passwords are never visible.

#### Improved

- **Shell-aware command quoting** — `shell_join` moved from `main.rs` to `shell.rs` with runtime shell detection. Three quoting strategies: PowerShell (`& 'path'` with `''` escaping), cmd.exe (`"path"` with `\"` escaping), and POSIX (`'path'` with `'\''` escaping). Previously used compile-time `cfg!(windows)` which was untestable and broke Git Bash on Windows.
- **11 new unit tests** for `join_command_for` covering all three shell quoting strategies with paths containing spaces, special characters, and empty arguments.

#### Dependencies

- Added `rpassword 7.4.0` for secure password input.

## [3.1.0] — 2026-04-14

### LeanCTX Cloud — Web Dashboard & Full Data Sync

#### Added — Cloud Dashboard

- **Web Observatory** — full-featured cloud dashboard at `leanctx.com/dashboard` mirroring the local Observatory. Includes Overview, Daily Stats, Commands, Performance (CEP), Knowledge, Gotchas, Adaptive Models, Buddy, and Settings views.
- **Login & Registration** — email/password authentication with email verification, password reset via magic link, and dedicated login/register pages.
- **SPA Navigation** — client-side routing with `history.pushState` for each dashboard view with dedicated URLs (`/dashboard/stats`, `/dashboard/knowledge`, etc.).
- **Timeframe Filters** — 7d/30d/90d/All time filters on Overview and Stats pages with live chart updates.
- **Knowledge Table** — searchable, filterable knowledge entries with category badges, confidence stars, and proper table layout with horizontal scroll on mobile.

#### Added — Complete Data Sync

- **Buddy Sync** — full `BuddyState` (ASCII art, animation frames, RPG stats, rarity, mood, speech) synced as JSON to the cloud and rendered with live animation on the dashboard.
- **Feedback Thresholds Sync** — learned compression thresholds per language synced to the cloud via new `/api/sync/feedback` endpoint and displayed on the Performance page.
- **Gotchas Sync** — both universal and per-project gotchas (`~/.lean-ctx/knowledge/*/gotchas.json`) are merged and synced.
- **CEP Cache Metrics** — daily `cache_hits` and `cache_misses` derived from CEP session data for accurate historical stats (previously hardcoded to 0).
- **Command Stats** — per-command token savings with source type (MCP/Hook) breakdown.

#### Added — Cloud Server

- **REST API** — Axum-based API server with endpoints for stats, commands, CEP scores, knowledge, gotchas, buddy state, feedback thresholds, and adaptive models.
- **PostgreSQL Schema** — tables for users, api_keys, email_verifications, password_resets, stats_daily, knowledge_entries, command_stats, cep_scores, gotchas, buddy_state, feedback_thresholds.
- **Email Verification** — SHA256-token-based email verification flow with configurable SMTP.
- **Password Reset** — secure token-based password reset with expiry.

#### Improved

- **Cost Model alignment** — cloud dashboard now uses the same `computeCost()` formula as the local dashboard (input $2.50/M + estimated output $10/M with 450→120 tokens/call reduction), replacing the previous input-only calculation.
- **Adaptive Models explanation** — expanded Models page with "What Adaptive Models Do For You" (before/after comparison), "How Models Are Built" (4-step flow), and "Compression Modes" reference table.
- **Daily Stats accuracy** — hit rate and cache data now correctly display from CEP-enriched daily stats.
- **Dashboard icons** — all SVG icons render with correct dimensions via explicit CSS utility classes.
- **Stats bar color** — Original tokens bar changed to blue for better visibility against the green Saved bar.

#### Removed

- **Teams & Leaderboard** — removed team creation, invites, and leaderboard features in favor of utility-focused dashboard.
- **File Watcher** — removed unused `watcher.rs` module.

#### Security

- **rand crate** — updated to `>= 0.9.3` to fix unsoundness with custom loggers (GHSA low severity).

#### Fixed

- **Token count test threshold** — updated `bench_system_instructions_token_count` thresholds to accommodate cloud server feature additions.

## [3.0.3] — 2026-04-12

### Dashboard Reliability + Automatic Background Indexing

#### Added

- **Background indexing orchestrator** — automatically builds and refreshes dependency graph, BM25 index, call graph, and route map in the background once a project root is known.
- **Dashboard status endpoint** — `GET /api/status` exposes per-index build states (`idle|building|ready|failed`) for progress display and troubleshooting.
- **Routes cache** — dashboard route map results are cached per project to avoid repeated scans.

#### Improved

- **Dashboard APIs are non-blocking** — graph/search/call-graph/routes endpoints return a `building` status instead of hanging while indexes are being built.
- **Dashboard UI** — views show “Indexing…” + auto-retry with backoff instead of confusing empty states or timeouts.
- **Auto-build on real usage** — MCP server triggers background builds when the project root is detected from `ctx_read` and also from `ctx_shell` (via effective working directory), without requiring manual reindex commands.

#### CI

- **AUR release hardening** — AUR job runs only when `AUR_SSH_KEY` is present, verifies SSH access up front, and fails loudly on auth issues.
- **Homebrew verification** — formula update step asserts the expected version + SHA are written before pushing.

#### Kiro IDE Support

- **Kiro steering file** — `lean-ctx init --agent kiro` and `lean-ctx setup` now create `.kiro/steering/lean-ctx.md` alongside the MCP config, ensuring Kiro uses lean-ctx tools instead of native equivalents.
- **Project-level detection** — `install_project_rules()` automatically creates the steering file when a `.kiro/` directory exists.

#### Fixed

- **`lean-ctx doctor` showed 9/10 instead of 10/10** — session state check was displayed but never counted towards the pass total.
- **Dashboard browser error on Linux** — suppressed Chromium stderr noise (`sharing_service.cc`) when opening dashboard via `xdg-open`.

## [3.0.2] — 2026-04-12

### Symbol Intelligence + Hybrid Semantic Search

#### Added — New MCP Tools

- **Symbol & outline navigation**
  - `ctx_symbol` — read a specific symbol by name (code span only)
  - `ctx_outline` — compact file outline (symbols + signatures)
- **Call graph navigation**
  - `ctx_callers` — find callers of a symbol
  - `ctx_callees` — list callees of a symbol
- **API surface extraction**
  - `ctx_routes` — extract HTTP routes/endpoints across common frameworks
- **Visualization**
  - `ctx_graph_diagram` — Mermaid diagram for dependency graph / call graph
- **Memory hygiene**
  - `ctx_compress_memory` — compress large memory/config markdown while preserving code fences/URLs

#### Improved — `ctx_semantic_search`

- **Search modes**: `bm25`, `dense`, `hybrid` (default)
- **Filters**: `languages` + `path_glob` to scope results
- **Automation**: auto-refreshes stale BM25 indexes; incremental embedding index updates
- **Performance**: process-level embedding engine cache (no repeated model load)

#### Fixed

- **Route extraction**: Spring-style Java methods with generic return types are now detected correctly.
- **Graph diagrams**: `depth` is now respected when filtering edges for `ctx_graph_diagram`.

## [3.0.1] — 2026-04-10

### LeanCTX Observatory — Real-Time Data Visualization Dashboard

#### Added — Observatory Dashboard (`lean-ctx dashboard`)

- **Event Bus** — New `EventKind`-based event system with ring buffer (1000 events) and JSONL persistence (`~/.lean-ctx/events.jsonl`) with auto-rotation at 10,000 lines. Captures `ToolCall`, `CacheHit`, `Compression`, `AgentAction`, `KnowledgeUpdate`, and `ThresholdShift` events in real time.
- **Live Observatory** — Real-time event feed showing all tool calls, cache hits, compression operations, agent actions, and knowledge updates with token savings, mode tags, and file paths. Filter by category (Reads, Shell, Search, Cache).
- **Knowledge Graph** — Interactive D3 force-directed graph visualizing project knowledge facts. Nodes sized by confidence, colored by category (Architecture, Testing, Debugging, etc.). Click nodes for detail panel showing temporal validity, confirmation count, and source session.
- **Dependency Map** — Force-directed visualization of file dependencies extracted via tree-sitter. Nodes sized by token count, colored by language, with edges representing import relationships. Smart edge resolution for module-style imports (`api::Server` → file path).
- **Compression Lab** — Side-by-side comparison of all compression modes (`map`, `signatures`, `aggressive`, `entropy`) for any file. Shows original content, compressed output, token savings percentage, and line reduction.
- **Agent World** — Multi-agent monitoring panel showing active agents, pending messages, shared contexts, agent types, roles, and last active times.
- **Bug Memory (Gotcha Tracker)** — Visual dashboard for auto-detected error patterns with severity, category, trigger/resolution, confidence scores, occurrence counts, and prevention statistics.
- **Search Explorer** — BM25 search index visualization with language distribution chart, top chunks by token count, and symbol-level detail.
- **Learning Curves** — Adaptive compression threshold visualization showing per-language entropy/Jaccard thresholds and compression outcome scatter plots (compression ratio vs. task success).

#### Added — Terminal TUI (`lean-ctx watch`)

- **`ratatui`-based Terminal UI** — Live event feed, file heatmap, token savings, and session stats in the terminal. Reads from `events.jsonl` with tail-based polling.

#### Added — Event Instrumentation

- `ctx_read`, `ctx_shell`, `ctx_search`, `ctx_tree` and all tools now emit `ToolCall` events with token counts, mode, duration, and path.
- Cache hits emit `CacheHit` events with saved token counts.
- `entropy_compress_adaptive()` emits `Compression` events with before/after line counts and strategy.
- `AgentRegistry.register()` emits `AgentAction` events.
- `ProjectKnowledge.remember()` emits `KnowledgeUpdate` events.
- `FeedbackStore` emits `ThresholdShift` events when learned thresholds change significantly.

#### Added — New Dashboard APIs

- `GET /api/events` — Latest 200 events from JSONL file (cross-process visibility).
- `GET /api/graph` — Full project dependency index.
- `GET /api/feedback` — Compression feedback outcomes and learned thresholds.
- `GET /api/session` — Current session state.
- `GET /api/search-index` — BM25 index summary with language distribution and top chunks.
- `GET /api/compression-demo?path=<file>` — On-demand compression of any file through all modes with original content preview.

#### Fixed

- **Live Observatory** showed "unknown" for all events due to flat vs. nested `kind` object mismatch — implemented `flattenEvent()` parser supporting all 6 event types.
- **Agent World** status comparison was case-sensitive (`Active` vs `active`) — now case-insensitive.
- **Learning Curves** scatter plot showed 0 for x-axis — now computes compression ratio from `tokens_saved / tokens_original` when `compression_ratio` field is absent.
- **Compression Lab** failed to load files — added `rust/` prefix fallback for path resolution and `original` content field in API response.
- **Dependency Map** edges not connecting — added module-to-file path resolution for `api::Server`-style import targets.

---

## [3.0.0] — 2026-04-10

### Major Release: Waves 1-5 — Intelligence Engine, Knowledge Graph, A2A Protocol, Adaptive Compression

This is a **major release** bringing lean-ctx from 28 to **34 MCP tools**, adding 8 read modes (new: `task`), persistent knowledge with temporal facts, multi-agent orchestration (A2A protocol), adaptive compression with Thompson Sampling bandits, and a complete fix for the context dropout bug (#73).

---

#### Wave 1 — Neural Token Optimization & Graph-Aware Filtering

- **Neural token optimizer** — Attention-weighted compression that preserves high-information-density lines using Shannon entropy scoring with configurable thresholds.
- **Graph-aware Information Bottleneck filter** — Integrates the project knowledge graph into `task` mode filtering, preserving lines that reference known entities (functions, types, modules) from the dependency graph.
- **Task relevance scoring** — Renamed `information_bottleneck_filter` → `graph_aware_ib_filter` with KG-powered entity recognition for smarter context selection.

#### Wave 2 — Context Reordering & Entropy Engine

- **LITM-aware context reordering** — Reorders compressed output using a U-curve attention model (Lost-in-the-Middle), placing high-importance content at the start and end of context windows where LLM attention is strongest.
- **Adaptive entropy thresholds** — Per-language BPE entropy thresholds with Kolmogorov complexity adjustment that auto-tune based on file characteristics.
- **`task` read mode** — New compression mode that filters content through the Information Bottleneck principle, preserving only task-relevant lines. Achieves 65-85% savings while maintaining semantic completeness.

#### Wave 3 — Persistent Knowledge & Episodic Memory

- **`ctx_knowledge` tool** — Persistent project knowledge store with temporal validity, confidence decay, and contradiction detection. Actions: `remember`, `recall`, `timeline`, `rooms`, `search`, `wakeup`.
- **Episodic memory** — Facts have temporal validity (`valid_from`/`valid_until`) and confidence scores that decay over time for unused knowledge.
- **Procedural memory** — Cross-session knowledge that automatically surfaces relevant facts based on the current task context.
- **Contradiction detection** — When storing a new fact that contradicts an existing one in the same category, the old fact is automatically superseded.

#### Wave 4 — A2A Protocol & Multi-Agent Orchestration

- **`ctx_task` tool** — Google A2A (Agent-to-Agent) protocol implementation with full task lifecycle: `create`, `assign`, `update`, `complete`, `cancel`, `list`, `get`.
- **`ctx_cost` tool** — Cost attribution per agent with token tracking. Actions: `record`, `summary`, `by_agent`, `reset`.
- **`ctx_heatmap` tool** — File access heatmap tracking read counts, compression ratios, and access patterns. Actions: `show`, `hot`, `cold`, `reset`.
- **`ctx_impact` tool** — Measures the impact of code changes by analyzing dependency chains in the knowledge graph.
- **`ctx_architecture` tool** — Generates architectural overviews from the project's dependency graph and module structure.
- **Agent Card** — `.well-known/agent.json` endpoint for A2A agent discovery with capabilities, supported modes, and rate limits.
- **Rate limiter** — Per-agent sliding window rate limiting (configurable, default 100 req/min).

#### Wave 5 — Adaptive Compression (ACON + Bandits)

- **ACON feedback loop** — Adaptive Compression via Outcome Normalization. Tracks compression outcomes (quality signals from LLM responses) and adjusts thresholds automatically.
- **Thompson Sampling bandits** — Multi-armed bandit approach for selecting optimal compression parameters per file type and language. Uses Beta distributions with configurable priors.
- **Quality signal detection** — Automatically detects quality signals in LLM responses (re-reads, error patterns, follow-up questions) to feed the ACON loop.
- **`ctx_shell` cwd tracking** — Shell working directory is now tracked across calls. `cd` commands are parsed and persisted in the session. New `cwd` parameter for explicit directory control.

#### Fix: Context Dropout Bug (#73)

All five root causes of the "lean-ctx loses context after initial read phase" bug have been fixed:

- **Monorepo-aware `project_root`** — `detect_project_root()` now finds the outermost ancestor with a project marker (`.git`, `Cargo.toml`, `package.json`, `go.work`, `pnpm-workspace.yaml`, `nx.json`, `turbo.json`, etc.), not the nearest `.git`.
- **`ctx_shell` cwd persistence** — New `shell_cwd` field in session state. `cd` commands are parsed and the working directory persists across `ctx_shell` calls. Priority: explicit `cwd` arg → session `shell_cwd` → `project_root` → process cwd.
- **`ctx_overview`/`ctx_preload` root fallback** — Both tools now fall back to `session.project_root` when no `path` parameter is given (previously defaulted to server process cwd).
- **Relative path resolution** — All 15+ path-based tools now use `resolve_path()` which tries: original path → `project_root` + relative → `shell_cwd` + relative → fallback.
- **Windows shell chaining** — `;` in commands is automatically converted to `&&` when running under `cmd.exe`.

#### Improved — Diagnostics

- **`lean-ctx doctor`** — New session state check showing `project_root`, `shell_cwd`, and session version.

#### Stats

- **34 MCP tools** (was 28)
- **8 read modes** (was 7, new: `task`)
- **656+ unit tests** passing
- **14 integration tests** passing
- **24 supported editors/AI tools**

## [2.21.11] — 2026-04-09

### Fix: Dashboard, Doctor, and MCP Reliability (#72)

#### Fixed — Doctor gave false positives for broken MCP configs
- **MCP JSON validation** — `doctor` now validates the actual JSON structure of each MCP config file instead of just checking for the string "lean-ctx". Checks for `mcpServers` → `lean-ctx` → `command` fields, verifies the binary path exists, and reports **per-IDE** status (valid vs. broken configs).
- **Honest stats check** — A missing `stats.json` is now reported as a warning ("MCP server has not been used yet") instead of counting as a passed check.

#### Fixed — Dashboard showed empty state without guidance
- The empty state now includes an actionable **troubleshooting checklist** with IDE-specific steps (Cursor reload, Claude Code init, config validation).

#### Fixed — No session created until first tool call batch
- A session is now created immediately on MCP `initialize`, so `doctor --report` always shows session info even before any tools are used.

#### Fixed — Tool calls only logged when >100ms
- All tool calls are now logged regardless of duration. Previously, fast calls were silently dropped, making the tool call log appear empty.

#### Fixed — macOS binary hangs at `_dyld_start` after install
- On macOS, copying the binary (via `cp`, `install`, or download) could strip the ad-hoc code signature, causing the dynamic linker to hang indefinitely on startup. Both `install.sh` and the self-updater now run `xattr -cr` + `codesign --force --sign -` after placing the binary.

## [2.21.10] — 2026-04-09

### Fix: Auth/Device Code Flow Output Preserved

#### Fixed — OAuth device code output no longer compressed (#71)
- **Auth flow detection** — New `contains_auth_flow()` function detects OAuth device code flow output using a two-tier approach:
  - **Strong signals** (match alone): `devicelogin`, `deviceauth`, `device_code`, `device code`, `device-code`, `verification_uri`, `user_code`, `one-time code`
  - **Weak signals** (require URL in same output): `enter the code`, `use a web browser to open`, `verification code`, `waiting for authentication`, `authorize this device`, and 10 more patterns
- **Shell hook passthrough** — 21 auth commands added to `BUILTIN_PASSTHROUGH`: `az login`, `gh auth`, `gcloud auth`, `aws sso`, `firebase login`, `vercel login`, `heroku login`, `flyctl auth`, `vault login`, `kubelogin`, `--use-device-code`, and more. These bypass compression entirely.
- **MCP tool passthrough** — `ctx_shell::handle()` now checks output for auth flows before compression. If detected, full output is preserved with a `[lean-ctx: auth/device-code flow detected]` note.
- **Shell hook buffered path** — `compress_if_beneficial()` also checks for auth flows before any compression, covering the `exec_buffered` path when stdout is not a TTY.

#### Impact
Previously, when Codex or Claude Code ran an auth command (e.g. `az login --use-device-code`), the device code was hidden from the user because lean-ctx compressed the output. Now the full output including auth codes is preserved.

**Workaround for older versions:** Add `excluded_commands = ["az login"]` to `~/.lean-ctx/config.toml`, or prefix commands with `LEAN_CTX_DISABLED=1`.

## [2.21.9] — 2026-04-09

### First-Class MCP Support for Pi Coding Agent

#### Added — pi-lean-ctx v2.0.0 with Embedded MCP Bridge
- **Embedded MCP client** — pi-lean-ctx now spawns the lean-ctx binary as an MCP server (JSON-RPC over stdio) and registers all 20+ advanced tools (ctx_session, ctx_knowledge, ctx_semantic_search, ctx_overview, ctx_compress, ctx_metrics, ctx_agent, ctx_graph, ctx_discover, ctx_context, ctx_preload, ctx_delta, ctx_edit, ctx_dedup, ctx_fill, ctx_intent, ctx_response, ctx_wrapped, ctx_benchmark, ctx_analyze, ctx_cache, ctx_execute) as native Pi tools.
- **Automatic pi-mcp-adapter compatibility** — If lean-ctx is already configured in `~/.pi/agent/mcp.json` (via pi-mcp-adapter), the embedded bridge is skipped to avoid duplicate tool registration.
- **Dynamic tool discovery** — Tool schemas come directly from the MCP server at runtime, not hardcoded. The `disabled_tools` config is respected.
- **Auto-reconnect** — If the MCP server process crashes, the bridge reconnects automatically (3 attempts with exponential backoff). CLI-based tools (bash, read, grep, find, ls) continue working regardless.
- **`/lean-ctx` command enhanced** — Now shows binary path, MCP bridge status (embedded vs. adapter), and list of registered MCP tools.

#### Added — Pi auto-detection in `lean-ctx setup`
- **Pi Coding Agent** is now auto-detected alongside Cursor, Claude Code, VS Code, Zed, and all other supported editors. Running `lean-ctx setup` writes `~/.pi/agent/mcp.json` automatically.
- **`lean-ctx init --agent pi`** now also writes the MCP server config to `~/.pi/agent/mcp.json` with `lifecycle: lazy` and `directTools: true`.

#### Improved — Pi diagnostics
- **`lean-ctx doctor`** now shows three Pi states: "pi-lean-ctx + MCP configured", "pi-lean-ctx installed (embedded bridge active)", or "not installed".

#### Documentation
- **README** for pi-lean-ctx completely rewritten with MCP tools table, pi-mcp-adapter compatibility guide, and `disabled_tools` configuration.
- **PI_AGENTS.md** template updated with MCP tools section.

## [2.21.8] — 2026-04-09

### Self-Updater Shell Alias Refresh + Thinking Budget Tuning

#### Fixed — `lean-ctx update` now refreshes shell aliases automatically
- **Shell alias auto-refresh** — `post_update_refresh()` now detects all shell configs (`~/.zshrc`, `~/.bashrc`, `config.fish`, PowerShell profile) with lean-ctx hooks and rewrites them with the latest `_lc()` function. Previously, `lean-ctx update` only refreshed AI tool hooks (Claude, Cursor, Gemini, Codex) but left shell aliases untouched, meaning users had to manually run `lean-ctx setup` to get new hook logic like the pipe guard.
- **Multi-shell support** — If a user has hooks in both `.zshrc` and `.bashrc`, both are now updated (previously only the first match was handled).
- **Post-update message** — Now explicitly tells users to `source ~/.zshrc` or restart their terminal.

#### Changed — Thinking Budget Tuning
- `FixBug` intent: Minimal → **Medium** (bug fixes benefit from deeper reasoning)
- `Explore` intent: Medium → **Minimal** (exploration is lightweight)
- `Debug` intent: Medium → **Trace** (debugging needs full chain-of-thought)
- `Review` intent: Medium → **Trace** (code review needs thorough analysis)

#### Improved — README & Deploy Checklist
- **README** — Added "Updating lean-ctx" section with all update methods, added pipe guard troubleshooting entry.
- **Deploy checklist** — Added "Shell Hook Refresh", "README / GitHub Updates" sections, and two new common pitfalls.

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
