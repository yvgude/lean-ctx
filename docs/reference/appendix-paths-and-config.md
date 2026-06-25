# Appendix — Paths, Env Vars & Config

Where lean-ctx stores everything, every environment variable that changes its
behavior, and every config section. Source: `rust/src/core/data_dir.rs`,
`rust/src/core/config/`.

---

## 1. Directories (XDG Base Directory layout)

Since GH #408 lean-ctx separates its files into the standard XDG categories so
the **config dir can be mounted read-only**. Typed resolvers live in
`rust/src/core/paths.rs` (`config_dir()`, `data_dir()`, `state_dir()`,
`cache_dir()`, `runtime_dir()`).

| Category | Default | Override | Contents |
|----------|---------|----------|----------|
| **Config** | `$XDG_CONFIG_HOME/lean-ctx` (`~/.config/lean-ctx`) | `LEAN_CTX_CONFIG_DIR` | `config.toml`, `env.sh`, `shell-hook.*` — **RO-safe** |
| **Data** | `$XDG_DATA_HOME/lean-ctx` (`~/.local/share/lean-ctx`) | `LEAN_CTX_DATA_DIR` | `sessions/`, `vectors/`, `graphs/`, `knowledge/`, `archives/`, `memory/`, `packages/`, `agents/`, `stats.json`, `client-id.json` |
| **State** | `$XDG_STATE_HOME/lean-ctx` (`~/.local/state/lean-ctx`) | `LEAN_CTX_STATE_DIR` | `events.jsonl`, `journal.md`, `*.log`, `mcp-live.json`, `heatmap.json`, `pipeline_stats.json`, `cost_attribution.json`, `context_ledger.json`, `cooccurrence/`, `tee/`, `dashboard.token`, `agent_runtime_env.json` (`0600`) |
| **Cache** | `$XDG_CACHE_HOME/lean-ctx` (`~/.cache/lean-ctx`) | `LEAN_CTX_CACHE_DIR` | `semantic_cache/`, `models/`, `anomaly_detector.json`, `*_learned.json`, `litm_calibration.json`, `context_ir_v1.json`, `latest-version.json`, `.first_run_wow_done` |
| **Runtime** | `dirs::data_local_dir()/lean-ctx` | — | `daemon.pid`, `daemon.sock`, `daemon-*.log`, `*.lock` |

Unix dir permissions: `0700`. The **Runtime** dir is the OS data-local dir
(`~/.local/share/lean-ctx` on Linux, `~/Library/Application Support/lean-ctx` on
macOS); it holds only daemon IPC and is intentionally separate from the data dir.

### Resolution order (per category)

Each resolver applies the same three steps:

1. **Explicit override** — `LEAN_CTX_<CATEGORY>_DIR` set & non-empty → wins.
2. **Single-dir backward-compat** — existing installs never split silently. If
   `LEAN_CTX_DATA_DIR` is set, **or** legacy `~/.lean-ctx` holds data, **or**
   mixed `$XDG_CONFIG_HOME/lean-ctx` holds data (the pre-#408 layout), then **all**
   categories collapse onto that one directory — byte-for-byte the old behavior.
3. **XDG split default** — a fresh install uses the per-category default above.

"Holds data" = contains a data marker (`stats.json`, `sessions/`, `vectors/`,
`graphs/`, `knowledge/`). `config.toml`/hooks alone do **not** count, so a
config-only dir does not pin the other categories back onto it.

> **Don't hardcode `LEAN_CTX_DATA_DIR`** in editor MCP configs — it forces the
> legacy single-dir layout (everything under one path). Leave it unset for the
> clean XDG split; existing installs are auto-detected and keep working.

### Migrate an existing install to the split (opt-in)

Legacy/mixed installs keep working in single-dir mode. To adopt the four-dir
layout on demand:

| Command | Effect |
|---------|--------|
| `lean-ctx doctor` | Reports `XDG layout: N item(s) in single dir` when a split is available |
| `lean-ctx doctor --fix` | Moves data/state/cache out of the config dir into their XDG homes |

The migration is **all-or-nothing** (partial moves would re-collapse via
back-compat), **idempotent/resumable** (existing destinations are skipped, never
clobbered) and **crash-safe** (atomic `rename`, copy+remove fallback across
filesystems; the source is removed only after a successful copy). An explicit
`LEAN_CTX_DATA_DIR` is treated as a deliberate single-dir choice and is never
auto-split; runtime files are left in place.

### Read-only config sandbox (the #408 goal)

Once split, the config dir holds only `config.toml` + hooks, so it can be mounted
read-only while the writable categories live elsewhere:

```
--ro    $XDG_CONFIG_HOME/lean-ctx     # config.toml, shell hooks
--rw    $XDG_DATA_HOME/lean-ctx       # sessions, vectors, graphs, knowledge
--rw    $XDG_STATE_HOME/lean-ctx      # events, journals, logs, ledgers
--tmpfs $XDG_CACHE_HOME/lean-ctx      # semantic cache, models (regenerable)
#       runtime (daemon.pid/sock) lives in the OS data-local dir
```

Project-local lean-ctx data (in the repo, not these dirs): `.lean-ctx.toml`
(project config override), `.lean-ctx-id`, `.lean-ctx/`.

---

## 2. Environment variables

There are ~120 env vars; the ones you'll actually touch are below. The full list
is in `rust/src/core/config/`. Most have a matching `config.toml` key — the env
var always wins.

### The ones you'll use

| Variable | Purpose | Default |
|----------|---------|---------|
| `LEAN_CTX_DISABLED=1` | Bypass ALL compression + disable shell hook | unset |
| `LEAN_CTX_RAW=1` | Uncompressed output for one command | unset |
| `LEAN_CTX_DATA_DIR` | Explicit data dir; **also forces legacy single-dir mode** (see §1) | `$XDG_DATA_HOME/lean-ctx` |
| `LEAN_CTX_CONFIG_DIR` | Explicit config dir (`config.toml`, hooks) | `$XDG_CONFIG_HOME/lean-ctx` |
| `LEAN_CTX_STATE_DIR` | Explicit state dir (events, logs, ledgers) | `$XDG_STATE_HOME/lean-ctx` |
| `LEAN_CTX_CACHE_DIR` | Explicit cache dir (semantic cache, models) | `$XDG_CACHE_HOME/lean-ctx` |
| `LEAN_CTX_PROJECT_ROOT` | Explicit project root | auto-detected |
| `LEAN_CTX_TOOL_PROFILE` | `minimal\|standard\|power` | config / power |
| `LEAN_CTX_PROFILE` | Active context profile | config / `coder` |
| `LEAN_CTX_COMPRESSION` | `off\|lite\|standard\|max` | config / `lite` |
| `LEAN_CTX_MEMORY_PROFILE` | `low\|balanced\|performance` | `performance` |
| `LEAN_CTX_PROXY_PORT` | Proxy port | `4444` |
| `LEAN_CTX_NO_UPDATE_CHECK=1` | Disable update check | unset |
| `LEAN_CTX_ALLOW_PATH` | Extra PathJail roots (path list; see §5) | unset |
| `LEAN_CTX_EXTRA_ROOTS` | Multi-root workspace roots (path list; see §5) | unset |

### Provider tokens (for `ctx_provider`)

`GITHUB_TOKEN` / `GH_TOKEN`, `GITLAB_TOKEN` / `CI_JOB_TOKEN`, `JIRA_URL` +
`JIRA_EMAIL` + `JIRA_TOKEN`, `DATABASE_URL`. Optional LLM enhance:
`OPENROUTER_API_KEY`, `ANTHROPIC_API_KEY`.

### Internal (set by lean-ctx itself — don't set these)

`LEAN_CTX_MCP_SERVER`, `LEAN_CTX_ACTIVE`, `LEAN_CTX_HOOK_CHILD`,
`LEAN_CTX_HEADLESS`, `LEAN_CTX_PLUGIN_DIR`, etc.

---

## 3. Config file (`config.toml`)

Global at `<CONFIG_DIR>/config.toml` (`$XDG_CONFIG_HOME/lean-ctx/config.toml`,
or the single dir for legacy/mixed installs — see §1); per-project override at
`<repo>/.lean-ctx.toml` (merged, project wins). Manage with `lean-ctx config`
(`set`, `schema`, `validate`, `show`).

### Sections

| Section | What it controls |
|---------|------------------|
| (root keys) | compression, cache, shell hook, profiles, memory caps, savings footer, proxy tri-state |
| `[tools]` | `profile` (minimal/standard/power), explicit `enabled` list |
| `[setup]` | `auto_inject_rules`, `auto_inject_skills`, `auto_update_mcp` |
| `[archive]` | Zero-loss tool-output archive: `enabled`, `threshold_chars` (800), `max_age_hours` (48), `max_disk_mb` (500) |
| `[search]` | BM25/dense/splade weights + candidate counts |
| `[autonomy]` | Auto preload/dedup/consolidate, cognition loop |
| `[providers]` | GitHub/GitLab/Jira/Postgres + MCP bridges |
| `[loop_detection]` | Per-tool call limits to prevent agent loops |
| `[updates]` | `auto_update`, `check_interval_hours` (6), `notify_only` |
| `[boundary_policy]` | Cross-project search/import + universal gotchas |
| `[secret_detection]` | Secret redaction in output |
| `[cloud]` | `contribute_enabled` + sync timestamps |
| `[proxy]` | Upstream URLs for Anthropic/OpenAI/Gemini |
| `[memory.*]` | Knowledge/episodic/procedural/lifecycle/gotcha/embeddings caps |
| `[llm]` | Optional local LLM enhance (Ollama) |

Key defaults worth knowing:
- `compression_level = "lite"` (root) — light compression on by default.
- `savings_footer = "always"` config default, but the **`SavingsFooter` enum
  default is `Never`** so no inline footer tokens are emitted unless enabled.
- `memory_profile = "performance"`, `memory_cleanup = "aggressive"`.
- `[memory.knowledge] max_facts = 200` — the source of doctor's "facts at
  capacity" warning.

---

## 4. Files written outside the lean-ctx dirs

| Category | Examples | Written by |
|----------|----------|-----------|
| Shell hook | `~/.zshenv`, `~/.bashenv`, fish, PowerShell profile | `setup` step 1 / `init --global` |
| Agent aliases | `~/.zshrc`, `~/.bashrc` (lean-ctx-on/off/mode/status) | `setup` / `init --global` |
| MCP configs | `~/.cursor/mcp.json`, `~/.claude.json`, ~30 editors | `setup` step 3 / `init --agent` |
| Agent rules (opt-in) | `~/.cursor/rules/lean-ctx.mdc`, `AGENTS.md` blocks | `setup` step 4 |
| Skills (opt-in) | `~/.claude/skills/lean-ctx/`, … | `setup` step 6 |
| Proxy env (opt-in) | RC exports, `~/.claude/settings.json`, Codex `config.toml` | `proxy enable` |
| Autostart | `~/Library/LaunchAgents/com.leanctx.{proxy,daemon,autoupdate}.plist`; systemd user units on Linux | setup steps 5/9 |
| Binary | `~/.local/bin/lean-ctx` | installer / `dev-install` |

Every edit to an existing file goes through `config_io::write_atomic`, which
writes a `*.lean-ctx.bak` backup first. Rules injection only rewrites content
between `<!-- lean-ctx -->` markers — your own content is preserved.
`lean-ctx uninstall` reverses all of the above.

---

## 5. Filesystem boundary — `path_jail`, `allow_paths`, `extra_roots` (GH #392)

All tool file access (`ctx_read`, `ctx_edit`, `ctx_tree`, …) is jailed under the
current `project_root` (**PathJail**). Three knobs widen or remove that boundary —
they overlap, so here is exactly what each one does:

| Knob | Effect | Use when |
|------|--------|----------|
| `allow_paths = ["…"]` (root key) | **Adds** directories to PathJail's whitelist. Tools may read/edit under them, but `ctx_tree`/`ctx_search` do **not** scan them. | One extra directory needs to be readable/editable (e.g. a shared skills folder). |
| `extra_roots = ["…"]` (root key) | Same whitelist effect as `allow_paths` **plus** multi-root scanning: `ctx_tree`, `ctx_search`, overview treat them as additional project roots. | Multi-repo workspaces. |
| `path_jail = false` (root key) | **Disables PathJail entirely** — every absolute path is allowed. | Sandboxed environments (bwrap, containers, VMs) where the OS is the boundary. |
| `allow_ide_config_dirs = true` (root key) | **Adds every supported editor's config dir** to the read whitelist — registry-derived (`~/.cursor`, VS Code, Cline/Roo, JetBrains, …). Opt-in; exposes other agents' sessions/credentials. | Letting the agent manage MCP setup across editors. |

Env equivalents (path-list syntax, `:` on Unix / `;` on Windows):
`LEAN_CTX_ALLOW_PATH` (= `allow_paths`), `LEAN_CTX_EXTRA_ROOTS` (= `extra_roots`).

Notes that save debugging time:

- **`~`, `$VAR` and `${VAR}` are expanded** in `allow_paths` / `extra_roots` /
  the env vars (since v3.8.1). On older versions `"$HOME/code"` was matched
  literally and silently never applied.
- `allow_paths = ["/"]` technically whitelists everything; prefer the explicit
  `path_jail = false` — `lean-ctx doctor` flags the `"/"` pattern.
- Config changes are picked up on the next tool call (mtime-based reload); no
  MCP server restart needed. If a change appears to do nothing, run
  `lean-ctx doctor`: it reports config parse errors (a broken `config.toml`
  silently falls back to defaults) and dead `allow_paths` entries (unset
  `$VAR`, missing directory), plus the effective jail state.
- **Compile-time off-switch:** building with the `no-jail` cargo feature
  removes the jail entirely (for trusted single-user builds).
- **Removed:** the `LEAN_CTX_NO_JAIL=1` env var (≤ 3.7.3). It was replaced by
  the `path_jail = false` config key and the `no-jail` compile feature; setting
  the old env var has no effect on current versions.
- Home-level IDE config dirs are excluded from the jail's whitelist by default.
  Opt in with `allow_ide_config_dirs = true` (or `LEAN_CTX_ALLOW_IDE_DIRS=1`):
  the allow-list is **derived from the editor registry**, so it covers every
  supported editor — including non-dotfile layouts like VS Code
  (`~/Library/Application Support/Code/User`), Cline/Roo and JetBrains — and
  never drifts as editors are added. `~/.lean-ctx` is always allowed, and a
  config file that lives directly in `$HOME` never widens the jail to the whole
  home directory. `lean-ctx setup` asks once (informed consent) and the
  relaxation is audited by `lean-ctx doctor`. These dirs expose other agents'
  sessions and credentials.
