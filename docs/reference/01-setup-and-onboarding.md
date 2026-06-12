# Journey 1 — Setup & Onboarding

> You just installed the `lean-ctx` binary. Nothing is wired up yet. This
> journey covers every command that connects lean-ctx to your tools and every
> function those commands call.

Source files referenced here:
- `rust/src/setup.rs` — the setup engine
- `rust/src/cli/dispatch/mod.rs` — command routing
- `rust/src/cli/dispatch/help.rs` — quickstart / help text
- `rust/src/doctor/mod.rs` — diagnostics
- `rust/src/status.rs` — connection status
- `rust/src/core/editor_registry/` — per-editor MCP config writers
- `rust/src/rules_inject.rs` — agent rules injection

---

## 0. What "being set up" actually means

For lean-ctx to help you, three things must be true:

1. **Your AI tool knows about lean-ctx** — its MCP config lists the `lean-ctx`
   server (so the editor launches `lean-ctx` and can call `ctx_*` tools).
2. **Your shell knows about lean-ctx** — a hook in your shell RC file lets
   `lean-ctx -c "git status"` etc. compress command output.
3. **A data directory exists** — `~/.lean-ctx/` holds stats, sessions, caches,
   and config.

Every setup command below is just a different amount of hand-holding to reach
that state.

---

## 1. `lean-ctx onboard` — the recommended first command

**What it does:** Connects every AI tool found on your machine using sensible
defaults, with zero questions, then prints one clear "you're connected" message.

```bash
lean-ctx onboard
```

**Under the hood** (`setup::run_onboard` in `rust/src/setup.rs`):

1. Calls `run_setup_with_options({ non_interactive: true, yes: true, fix: true })`
   — the same engine as `setup`, but it makes every decision for you.
2. Reads the resulting `SetupReport`, finds the `editors` step, and lists which
   tools were actually `created`/`updated`/`already` configured.
3. Prints: connected tools, the data dir path, and exactly one next step
   (reload shell → restart AI tool → ask it to read a file).

**Files changed:** MCP config for each detected editor, shell RC hook,
`~/.lean-ctx/` created. Rules/skills are **not** injected unless you previously
opted in (see §2, step 4).

**Why it exists:** the full `setup` wizard is 12 steps; most users want "just
connect it." `onboard` is that path — time-to-value in seconds.

---

## 2. `lean-ctx setup` — the guided wizard (full control)

**What it does:** An interactive, 12-step wizard. Use it when you want to decide
about the proxy, telemetry, auto-updates, compression level, and tool profile.

```bash
lean-ctx setup
```

**Routing** (`dispatch/mod.rs`): with no flags it calls `setup::run_setup()`.
With `--non-interactive`, `--yes/-y`, `--fix`, `--json`, or `--skip-rules` it
calls `run_setup_with_options(...)` instead (no prompts).

### The first-run menu (`first_run_setup_level`)

Before step 1, if you've never chosen a level, it asks:

| Choice | inject_rules | inject_skills | Meaning |
|--------|:---:|:---:|---------|
| **[1] Minimal** (default) | ✗ | ✗ | Just MCP tools, no config-file edits |
| **[2] Standard** | ✓ | ✗ | MCP tools + agent rules for optimal mode selection |
| **[3] Full** | ✓ | ✓ | Tools + rules + skills + shell hooks |

The choice is persisted to `config.toml` (`[setup] auto_inject_rules`,
`auto_inject_skills`) so it's never asked again. This is the "non-invasive by
default" behavior: lean-ctx will not touch your rules files unless you say so.

### The 12 steps (`run_setup`)

| Step | Name | What it does | Files touched |
|------|------|--------------|---------------|
| 1 | Shell Hook | `cmd_init --global` + `install_all` — installs aliases + universal hook | `~/.zshenv`, `~/.bashenv`, RC files |
| 2 | Daemon | Starts/restarts the IPC daemon for fast CLI routing | UDS socket, PID file |
| 3 | AI Tool Detection | Detects installed editors, writes each one's MCP config | per-editor MCP JSON/TOML/YAML |
| 4 | Agent Rules | Injects `lean-ctx` rules **only if opted in** (preserves your content) | `*/rules/lean-ctx.*`, `AGENTS.md` blocks |
| 5 | API Proxy (optional) | Asks y/N; if yes, installs proxy autostart + env vars | LaunchAgent/systemd, RC env exports |
| 6 | Skill Files | Installs `SKILL.md` **only if opted in** | `*/skills/lean-ctx/` |
| 7 | Environment Check | Ensures data dir, migrates split dirs, runs compact doctor | `~/.lean-ctx/` |
| 8 | Help Improve | Asks y/N for anonymous stats sharing | `config.toml [cloud]` |
| 9 | Auto-Updates | Asks y/N; installs the 6-hourly update scheduler | LaunchAgent/systemd |
| 10 | Tool Profile | Choose minimal/standard/power MCP tool set | `config.toml [tools]` |
| 11 | Advanced Tuning (optional) | Compression level + tool-result archive | `config.toml` |
| 12 | Code Intelligence | Builds the property graph in the background (if in a project) | `~/.lean-ctx/` graph caches |

It ends with an auto-approve transparency banner, a `✓ Setup complete!` summary,
and **Next steps** (reload shell, restart IDE, verify with `lean-ctx gain`).

### `run_setup_with_options` — the non-interactive engine

This is the function every other entry point funnels through (onboard, install,
bootstrap, update rewire). It performs the same wiring without prompts and
returns a structured `SetupReport` (steps, items, warnings) that can be printed
as JSON with `--json`. Key options (`SetupOptions`):

- `non_interactive` / `yes` — run without a TTY; `yes` is required to actually
  write the shell hook in non-interactive mode.
- `fix` — overwrite invalid/corrupt MCP configs (merge-based repair).
- `skip_rules` — never touch rules files (CLI flag wins over config).
- `force_inject_rules` — always inject rules (overrides config).
- `skip_proxy` / `no_auto_approve`.

The decision for rules injection is: `skip_rules` → off; else `force_inject_rules`
→ on; else respect `config.toml`'s `should_inject_rules()`.

---

## 3. `lean-ctx install` — the natural alias

**What it does:** Plain `lean-ctx install` now runs the guided `setup` (it used
to error with a usage message — fixed for UX). `install --repair` (or `--fix`)
runs the non-interactive, merge-based refresh.

```bash
lean-ctx install            # = lean-ctx setup
lean-ctx install --repair   # non-interactive repair (no deletes)
```

---

## 4. `lean-ctx bootstrap` — zero-config CI/scripts

**What it does:** Non-interactive setup + fix with sensible defaults. Identical
to `install --repair` but named for automation. `--json` emits a machine
report. Use this in Dockerfiles / CI.

```bash
lean-ctx bootstrap [--json]
```

---

## 5. `lean-ctx init` — shell aliases & single-agent config

Two distinct uses:

- **`lean-ctx init --global`** — installs only the shell aliases/hook
  (`lean-ctx-on`, `lean-ctx-off`, `lean-ctx-mode`, `lean-ctx-status`) into your
  shell RC. This is step 1 of `setup`, callable on its own.
- **`lean-ctx init --agent <name>`** — configures MCP + rules + skill + hook for
  **one** specific agent (e.g. `cursor`, `claude`, `gemini`, `pi`). Calls
  `setup::setup_single_agent`, the single source of truth shared with `setup`.
  Use this when you only use one tool, or to re-wire after an editor update.

Supported agent keys are enumerated in `agent_mcp_targets` (cursor, claude,
windsurf, codex, gemini, antigravity, copilot, crush, pi, qoder, cline, roo,
kiro, verdent, qwen, trae, amazonq, opencode, hermes, vscode, zed, aider,
continue, neovim, emacs, sublime, …). An unknown key returns
`Unknown agent '<x>'`.

> Recommendation: most users should use `onboard` (all tools) or `setup`
> (guided). `init --agent` is the targeted/expert path.

---

## 6. `lean-ctx doctor` — "is everything wired up?"

**What it does:** Runs ~27 diagnostic checks across binary, data dir, MCP
configs, shell hook, daemon, proxy, caches, memory, and capacity, then prints a
summary with an action-oriented footer.

```bash
lean-ctx doctor                 # full diagnostics
lean-ctx doctor --fix           # auto-repair what's fixable
lean-ctx doctor --json          # machine-readable
lean-ctx doctor integrations    # per-IDE wiring health (every detected agent)
```

**Footer** (`doctor/mod.rs`): shows `N/M checks passed`; if any need attention,
it prints `N check(s) need attention. Auto-repair what's fixable:
lean-ctx doctor --fix`. Otherwise `Everything looks good.`

`--fix` routes to `doctor::fix::run_fix`, which re-runs the merge-based setup and
repairs MCP/rules/hook drift.

**Golden output — `doctor integrations`** checks **every detected agent**, not
just Cursor/Claude, and reports MCP config, hook freshness, and the rules file
per agent. Hooks are verified for **staleness** (a hook pointing at an old binary
path fails with `stale binary … — run lean-ctx setup --fix`), and JetBrains is
shown as an **MCP snippet** because it has no auto-wiring (you paste it once):

<details>
<summary><code>lean-ctx doctor integrations</code> — per-IDE wiring health (excerpt)</summary>

```text
  Integration health:
  ✓  Cursor
       ✓  MCP config  ok (~/.cursor/mcp.json)
       ✓  Hooks  ok (~/.cursor/hooks.json)
  ✓  Claude Code
       ✓  MCP config  ok (~/.claude.json)
       ✓  Hooks  ok (~/.claude/settings.json)
       ✓  Instructions  ~/.claude/CLAUDE.md block + skill
  ✓  Codex CLI
       ✓  Codex MCP  ok (~/.codex/config.toml)
       ✓  Codex hooks  enabled (~/.codex/config.toml)
       ✓  Codex hooks.json  ok (~/.codex/hooks.json)
  ✓  VS Code
       ✓  VS Code MCP  ok (~/Library/Application Support/Code/User/mcp.json)
  ✓  JetBrains IDEs
       ✓  MCP snippet  ready — paste into Settings → Tools → AI Assistant → MCP (~/.jb-mcp.json)
       ✓  Rules file  ~/.jb-rules/lean-ctx.md
```

</details>

A healthy run ends with no repair line; otherwise it prints
`Repair: run lean-ctx setup --fix`. Add `--json` for the same data as a
`schemaVersion`-stamped report.

---

## 7. `lean-ctx status` — the quick connection check

**What it does:** A lighter-weight "am I connected?" report (setup report + MCP
target states), JSON-capable. Use `status` for a fast yes/no; use `doctor` for
deep diagnostics.

```bash
lean-ctx status
lean-ctx status --json
```

**Golden output — a healthy `status`** is five lines: the doctor ratio, the last
setup result, and how many agents have MCP + rules wired up:

```text
lean-ctx status  v3.6.26
  doctor: 6/6
  last setup: 2026-05-30T20:06:46+00:00  success=true
  mcp: 28/28 configured (detected tools)
  rules: 17/17 up-to-date (detected tools)
  report saved: /Users/you/.lean-ctx/status/latest.json
```

`mcp: 28/28` and `rules: 17/17` count **detected** agents (rules count is lower
because MCP-only agents receive guidance via MCP instructions — see the
[installation matrix](../integrations/installation-matrix.md)).

---

## 8. What gets written where (setup recap)

| Artifact | Path (example) | Written by |
|----------|----------------|------------|
| Data dir | `~/.lean-ctx/` | setup step 7 |
| Shell hook | `~/.zshenv` / `~/.bashenv` + RC files | setup step 1 |
| MCP config | `~/.cursor/mcp.json`, `~/.claude.json`, … | setup step 3 |
| Agent rules (opt-in) | `~/.cursor/rules/lean-ctx.mdc`, `AGENTS.md` blocks | setup step 4 |
| Skill files (opt-in) | `~/.claude/skills/lean-ctx/`, … | setup step 6 |
| Proxy env (opt-in) | RC exports + LaunchAgent/systemd | setup step 5 |
| Update scheduler (opt-in) | LaunchAgent/systemd | setup step 9 |

Every modification of an existing file goes through `config_io::write_atomic`,
which writes a `.lean-ctx.bak` backup first. Rules injection only ever rewrites
the content **between** `<!-- lean-ctx -->` markers, preserving everything else.

---

## UX notes captured during this walkthrough

These are the friction points found while documenting setup; fixes already
shipped are marked ✓.

- ✓ Plain `lean-ctx install` no longer errors — it runs setup.
- ✓ `onboard` added as the zero-prompt golden path.
- ✓ Data dir path corrected across all guides (`~/.lean-ctx`, not
  `~/.local/share/lean-ctx`).
- ✓ "Premium Features" step renamed to "Advanced Tuning (optional)".
- ✓ Skill-skip message no longer points to the wrong flag.
- ◯ Open: the interactive wizard is still 12 steps — consider collapsing
  optional opt-ins (proxy, telemetry, auto-update) behind a single
  "Configure advanced options? [y/N]" gate so the common path is ~4 prompts.
