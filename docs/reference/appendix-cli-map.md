# Appendix — CLI Command Map

Every CLI command lean-ctx exposes, grouped by purpose. Source of truth:
`rust/src/cli/dispatch/mod.rs`. Aliases are shown in parentheses.

> Tip: `lean-ctx help` shows the short list of everyday commands;
> `lean-ctx help all` shows the full reference.

## Getting started / setup

| Command | Purpose |
|---------|---------|
| `onboard` | Zero-prompt golden path: connect all detected AI tools |
| `setup` | Guided wizard (full control); flags: `--non-interactive`, `--yes`, `--fix`, `--json`, `--skip-rules`, `--no-auto-approve` |
| `install` | Alias for `setup`; `install --repair` = non-interactive refresh |
| `bootstrap` | Non-interactive setup + fix (CI/scripts); `--json` |
| `init --global` | Install shell aliases/hook only |
| `init --agent <name>` | Configure MCP + rules + skill + hook for one agent |
| `doctor` | Diagnostics; `--fix`, `--json`, `integrations` |
| `status` | Quick "am I connected?"; `--json` |

## Daily use

| Command | Purpose |
|---------|---------|
| `-c` / `exec "cmd"` | Run a shell command with compressed output (`--raw` = full) |
| `-t` / `--track "cmd"` | Track a command (full output + stats, no compression) |
| `shell` | Interactive shell with compression |
| `bypass "cmd"` | Run with zero compression |
| `read <file>` | Read with compression; `-m/--mode`, `--fresh` |
| `diff <a> <b>` | Compressed file diff |
| `grep <pattern> [path]` | Search with compressed output |
| `find <pattern> [path]` | Find files (compressed) |
| `ls [path]` | Compressed directory map; `--depth`, `-a` |
| `deps [path]` | Show project dependencies |
| `gain` | Token-savings dashboard; `--live`, `--graph`, `--daily`, `--json`, `--wrapped`, `--svg`, `--share`, `--copy`, `--open`, `--publish`, `--leaderboard`, `--unpublish`, `--cost`, `--tasks`, `--agents`, `--heatmap` |
| `savings [--period day\|week\|month\|all] [--format table\|json\|markdown]` | Cost-intelligence report: token reduction, estimated versus actual cost, CPAO, and top savings sources; example: `lean-ctx savings --period month --format markdown` |
| `value-report [--format table\|markdown\|json] [--last N]` | Outcome report for recent assessed tasks, including acceptance and CPAO; example: `lean-ctx value-report --format markdown --last 20` |
| `value [--session <id>\|--all] [--json]` | What lean-ctx did in a session (tokens kept out of context, security events), recomputed from the verified savings ledger and audit trail; exits 1 when a chain is broken |
| `statusline [--wrap "<cmd>"]` | Claude Code status line (`statusLine.command`), set by `init --agent claude` when none exists; `--wrap` chains your own status line and appends lean-ctx's segment to its first line |
| `prompt-segment [--shell zsh\|bash\|fish\|plain]` | One dim shell-prompt segment for the current project, or nothing; wired by `init --prompt` (zsh/bash/fish, `off` removes it), `plain` for Starship and other prompt engines — see [value display](../guides/value-display.md) |
| `prove speed --suite <file> [--runs N] [--budget N] [--json] [--out FILE]` | Signed A/B measurement: the same live model answers each task with a raw context dump and with lean-ctx's context; `--verify [FILE]` re-checks a proof (exit 1 if tampered) — see [value display](../guides/value-display.md#speed) |
| `init --git-trailer [off]` | Opt-in `prepare-commit-msg` hook that adds a `lean-ctx:` trailer with what was saved since the last trailered commit; never touches an existing hook or fails a commit |
| `shadow [--latest\|--list\|--force]` | Inspect or generate local baseline-versus-treatment recommendations; example: `lean-ctx shadow --latest` |
| `token-report` (`report-tokens`) | Token + memory report; `--json` |
| `learning` | Adaptive-learning state: `status`, `export [file]`, `import <file\|->` — share learned thresholds + LITM calibration with your team (secret-free, idempotent merge) |
| `introspect` | Cognition v2 activity: `cognition` (which science subsystems are wired/active, `--json`), `qubo` (experimental QUBO-vs-greedy selection benchmark) |
| `discover` | Find uncompressed commands in shell history; `--card` (shareable "before" SVG) |
| `ghost` | Ghost-token report (hidden waste); `--json` |
| `cheatsheet` (`cheat`) | Workflow cheat sheet |
| `dashboard` | Web dashboard (localhost:3333); `--port`, `--host` |

## Memory & sessions

| Command | Purpose |
|---------|---------|
| `session` | Tasks/findings/decisions + adoption stats: `task`, `finding`, `decision`, `save`, `load`, `status`, `reset` |
| `sessions` (`session-store`) | Manage saved CCP snapshots: `list`, `show`, `delete`, `cleanup`, `doctor` |
| `knowledge` | Project knowledge: `remember`, `recall`, `search`, `export`, `import`, `remove`, `consolidate [--all]`, `status`, `health`, `lifecycle` |
| `overview [task]` | Project overview (task-contextualized) |
| `compress` | Context-compression checkpoint; `--signatures` |
| `control` | Context field manipulation: exclude/pin/priority |
| `plan <task>` | Context planning (Phi-scored); `--budget` |
| `compile` | Context compilation (knapsack + Boltzmann); `--mode`, `--budget` |
| `ledger` | Context-ledger: `status`, `reset`, `evict`, `prune` |

## Code intelligence

| Command | Purpose |
|---------|---------|
| `graph` | Property graph: `build`, `related`, `impact`, `symbol`, `context`, `status`, `export-html` |
| `smells` | Code-smell detection (8 rules): `scan`, `summary`, `rules`, `file` |
| `visualize` | Interactive HTML report (D3); `--output`, `--open` |
| `index` | Index utilities: `status`, `build`, `build-full`, `build-graph`, `watch` |
| `heatmap` | Context heatmap; `--top`, `--by` |
| `cep` | CEP impact report (score trends) |
| `benchmark` | `run`, `report`, `eval`, `compare` |

## Advanced — network / providers / team / plugins

| Command | Purpose |
|---------|---------|
| `serve` | MCP over Streamable HTTP; `--daemon`, `--root PATH[:ALIAS]` (multi-repo), `--rrf-k` |
| `proxy` | API proxy: `start`, `stop`, `status`, `enable`, `disable`, `cleanup` |
| `daemon` | IPC daemon: `start`, `stop`, `status`, `enable`, `disable` |
| `provider` | External provider OAuth (Jira): `auth`, `logout`, `list` |
| `team` | Team server (feature-gated): `serve`, `token create`, `sync` |
| `plugin` (`plugins`) | `list`, `enable`, `disable`, `info`, `init`, `hooks` |
| `rules` | ContextOps governance: `sync`, `diff`, `lint`, `status`, `init` |
| `pack` | Context Package Manager + PR pack (`create`, `install`, `export`, `import`, `pr`, …) |
| `compact [path]` | Compress agent transcripts |
| `learn` | Learned gotchas; `--apply` → AGENTS.md |
| `gotchas` (`bugs`) | Bug memory: `list`, `clear`, `export`, `stats` |
| `buddy` (`pet`) | Token Guardian companion |
| `safety-levels` (`safety`) | Compression safety-level table |

## Lifecycle

| Command | Purpose |
|---------|---------|
| `update` (`--self-update`, `upgrade`) | Self-update; `--check`, `--insecure`, `--skip-rules`, `--schedule [off\|status\|notify\|<h>h]` |
| `stop` | Stop ALL lean-ctx processes (LaunchAgent-safe) |
| `restart` | Restart daemon (apply config.toml) |
| `dev-install` | Build release + atomic install + restart (dev) |
| `uninstall` | Stop processes + remove configs, autostart, data, **and the binary**; `--dry-run`, `--keep-config`, `--keep-binary` |
| `cache` | Read cache: `stats`, `clear`, `reset`, `invalidate`, `prune` |
| `harden` | Harden native read/grep in MCP configs; `--hard`, `--undo` |

## Tools / profiles / config

| Command | Purpose |
|---------|---------|
| `tools` | MCP tool profile: `minimal`, `standard`, `power`, `show`, `list` |
| `allow` | Shell allowlist: add/remove commands in `shell_allowlist_extra`, `--list` shows the effective allowlist + any parse errors |
| `trust` / `untrust` | Workspace trust: gate a cloned repo's project-local `.lean-ctx.toml` security overrides; `trust status`, `trust --list` |
| `profile` | Context profiles: `list`, `show`, `active`, `diff`, `create`, `set` |
| `config` | Config file: dump, `init`, `set <k> <v>`, `schema`, `validate`, `show`, `apply` |
| `theme` | Terminal colors: `list`, `set`, `export`, `import` |
| `terse` / `compression` | Compression level: `off`, `lite`, `standard`, `max` |
| `filter` | Custom compression filters: `list`, `validate`, `init` |
| `tee` | Output tee logs: `list`, `clear`, `show`, `last` |
| `slow-log` | Slow commands: `list`, `clear` |
| `stats` | Raw stats store: summary, `reset-cep`, `json` |

## Cloud

| Command | Purpose |
|---------|---------|
| `login <email>` | Cloud login |
| `register <email>` | Create cloud account |
| `forgot-password <email>` | Password reset email |
| `sync` | Upload local stats to cloud |
| `contribute` | Share anonymized compression data |
| `cloud` | `status`, `pull-models` |

## Internal / hidden (used by agents, not for daily use)

| Command | Purpose |
|---------|---------|
| `mcp` | Explicit stdio MCP server |
| `hook <sub>` | Agent hook entry points (Cursor/Claude/Copilot/Codex) |
| `audit` | Compliance report from audit trail |
| `instructions` | Compile MCP instructions for a client |
| `export-rules` | High-confidence knowledge → rules files |
| `proof` / `verify` | Context-proof artifacts |
| `report-issue` (`report`) | Open a GitHub issue with diagnostics |

## Known help-text drift (tracked for cleanup)

The dispatch exposes ~35 commands not yet in `lean-ctx help all`, including
`provider`, `team`, `heatmap`, `ledger`, `control`, `plan`, `compile`,
`compact`, `learn`, `stats`, `bypass`, `safety-levels`, and several subcommands
(`graph export-html`, `proxy enable/disable`, `cache prune`, full `pack`
actions). These are intentionally advanced/internal but should be surfaced in a
future `help advanced` tier. See [Journey 1 UX notes](01-setup-and-onboarding.md).
