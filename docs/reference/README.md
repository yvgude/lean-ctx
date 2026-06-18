# lean-ctx Reference — Every Function, Every Path

This is the complete, function-by-function reference for lean-ctx, organized
the way you actually meet it: as a sequence of **user journeys**, starting at
setup and walking through everything lean-ctx can do.

Each journey document answers three questions for every feature:

1. **What does it do?** (plain language)
2. **How do I use it?** (the exact command / MCP call)
3. **What happens under the hood?** (which code path runs, what files change)

> New to lean-ctx? Read the journeys in order. Looking for one command? Use the
> index below.

## The journeys

| #  | Journey                                                          | You are…                                             | Covers                                                                                                    |
|----|------------------------------------------------------------------|------------------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| 1  | [Setup & Onboarding](01-setup-and-onboarding.md)                 | installing for the first time                        | `onboard`, `setup`, `install`, `bootstrap`, `init`, `doctor`, `status`                                    |
| 2  | [Daily Use](02-daily-use.md)                                     | coding with your AI every day                        | `read`, `grep`, `find`, `ls`, `-c`/`exec`, `gain`, `tools`                                                |
| 3  | [Memory & Knowledge](03-memory-and-knowledge.md)                 | wanting continuity across sessions                   | `session`, `sessions`, `knowledge`, `overview`, CCP                                                       |
| 4  | [Code Intelligence](04-code-intelligence.md)                     | exploring or refactoring a codebase                  | `graph`, `impact`, `repomap`, `smells`, `visualize`, `index`                                              |
| 5  | [Advanced & Integrations](05-advanced.md)                        | wiring up proxy, providers, plugins                  | `proxy`, `provider`, `serve`, `plugin`, `rules`, `pack`, multi-repo                                       |
| 6  | [Lifecycle & Troubleshooting](06-lifecycle.md)                   | updating, fixing, or removing                        | `update`, `uninstall`, `stop`, `restart`, `cache`, `doctor --fix`                                         |
| 7  | [Context Engineering & Observability](07-context-engineering.md) | actively managing the context window                 | `radar`, `control`, `plan`, `compile`, `ledger`, `preload`, `compose`, `verify`                           |
| 8  | [Multi-Agent Collaboration](08-multi-agent.md)                   | running several agents on one project                | `ctx_agent`, `ctx_task`, `ctx_handoff`, `ctx_share`, diaries, shared knowledge                            |
| 9  | [Team, Cloud & CI](09-team-cloud-ci.md)                          | sharing across a team or running headless            | `team serve`/`token`/`sync`, `login`, `sync`, `contribute`, `bootstrap`, `serve`                          |
| 10 | [Customization & Governance](10-customization-and-governance.md) | tuning behavior & enforcing rules                    | `compression`, `tools`, `profile`, `config`, `theme`, `filter`, `rules`, `harden`                         |
| 11 | [Analytics, Insights & Reporting](11-analytics-and-insights.md)  | measuring savings & finding waste                    | `gain`, `wrapped`, `token-report`, `discover`, `ghost`, `dashboard`, `watch`, `cep`, `stats`              |
| 12 | [Troubleshooting Playbook](12-troubleshooting.md)                | something's not working                              | symptom → diagnosis → fix; `status`, `doctor`, `doctor integrations`, `sessions doctor`, `report-issue`   |
| 13 | [Security & Governance](13-security-and-governance.md)           | putting lean-ctx in front of real code               | PathJail, `shell_allowlist`, `secret_detection`, sandbox, `harden`, role policies                         |
| 14 | [Performance Tuning](14-performance-tuning.md)                   | huge repo / constrained machine                      | `memory_profile`, `bm25_max_cache_mb`, `graph_index_max_files`, `LEAN_CTX_MAX_*`, `slow-log`              |
| 18 | [Adaptive Learning](18-adaptive-learning.md)                     | understanding how lean-ctx tunes itself              | learned thresholds, LITM calibration, scent field, playbook, `learning export/import`, efficacy           |
| 19 | [JetBrains-Plugin](19-jetbrains-plugin.md)                      | using code intelligence from a running JetBrains IDE | `ctx_refactor`: navigation, structure, inspections, symbol-edits, rename/reformat/move/safe_delete/inline |
| 20 | [Hermes Context Engine](20-hermes-context-engine.md)            | embedding lean-ctx as your agent's context engine    | `ctx_transcript_compact`, `serve`, `context.engine`, recall tools, session lifecycle                      |

## Cross-cutting references

| Reference                                                | What's in it                                                                                                                                             |
|----------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|
| [Per-IDE quickstarts](appendix-ide-quickstarts.md)       | Copy-paste setup + verify for Cursor, Claude, Codex, VS Code, JetBrains                                                                                  |
| [CLI command map](appendix-cli-map.md)                   | Every CLI command + alias, one line each                                                                                                                 |
| [MCP tool map](appendix-mcp-tools.md)                    | Every MCP tool, params, and which profile exposes it                                                                                                     |
| [Paths, env vars & config](appendix-paths-and-config.md) | Data dir layout, every `LEAN_CTX_*` var, every config key                                                                                                |
| [Glossary](appendix-glossary.md)                         | MCP, CCP, hooks, modes, profiles, proxy — in one place                                                                                                   |
| [JetBrains-Plugin](appendix-jetbrains-plugin.md)        | Compact agent lookup for the JetBrains plugin — every `ctx_refactor` action, endpoint, guard, error. Full guide: [Journey 19](19-jetbrains-plugin.md)   |

> **Generated, always-current appendices** (rendered directly from the code, so
> they can never drift): [MCP tools](generated/mcp-tools.md) (every registered
> tool + parameters) and [config keys](generated/config-keys.md) (every
> `config.toml` key with type, default, and env override). Regenerate with
> `cargo run --example gen_docs --features dev-tools`; CI fails if they are stale.

## The two mental models you need

lean-ctx has exactly **two ways** of helping your AI, and almost every command
belongs to one of them:

- **MCP tools** — your AI editor calls `ctx_*` tools instead of its native file
  reads/search. lean-ctx returns compressed, cached results. (Journeys 2–5.)
- **Shell hooks** — when you (or your AI's terminal) run `git`, `npm`, `cargo`,
  etc., lean-ctx compresses the output. (Journey 2.)

Everything else — sessions, knowledge, graph, proxy — exists to make those two
paths smarter. If you remember only that, the rest falls into place.

The journeys layer onto this: 1–4 are the core daily loop, 5 wires in external
systems, 6 keeps it healthy, 7 gives you fine-grained control of the window,
8–9 scale it to multiple agents and teams, and 10–11 let you tune behavior and
measure the payoff. **12–14 are the operations track**: a central troubleshooting
playbook, the security/governance surface, and performance tuning for big repos
and constrained machines. Every CLI command and MCP tool appears in at least one
journey and in the appendices below.
