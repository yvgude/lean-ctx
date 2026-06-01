You've mastered daily use and want more: compress the LLM API stream itself,
pull in GitHub/GitLab/Jira context, share context across repos or agents, and
govern rules across your team. This journey covers the power-user surface.

## 1. The proxy — compress the LLM stream itself

**What it does:** Everything so far compresses *before* your AI calls a tool. The
proxy goes one level deeper: it sits between your AI client and the LLM API and
compresses `tool_results` in-flight, before they reach the model.

```bash
lean-ctx proxy enable        # set up env + autostart (writes RC + LaunchAgent)
lean-ctx proxy status
lean-ctx proxy start         # start now
lean-ctx proxy stop
lean-ctx proxy disable       # remove env + autostart
lean-ctx proxy cleanup       # clear proxy state
```

**Golden output — `lean-ctx proxy status`** tells you, at a glance, whether the
proxy is configured, on which port, and whether the process is currently up:

```text
lean-ctx proxy:
  Config:  enabled
  Port:    4444
  Process: not running
```

`Config: enabled` with `Process: not running` means it is wired up but not
started — run `lean-ctx proxy start` (or rely on the LaunchAgent/systemd unit).

**Under the hood:** runs on `LEAN_CTX_PROXY_PORT` (default 4444), auth via
`session_token`. `proxy enable` writes `*_BASE_URL` exports into your shell RC,
`~/.claude/settings.json` (`ANTHROPIC_BASE_URL`), and Codex `config.toml`
(`OPENAI_BASE_URL`), and installs `com.leanctx.proxy.plist` (macOS) or a systemd
user unit (Linux). Upstreams are configurable in `[proxy]`.

> **Heads-up (community-reported):** `proxy enable` modifies your shell RC. If a
> base URL "defaults to the wrong provider," check the exported `*_BASE_URL`
> values in your RC and `lean-ctx proxy status`. The unmodified RC is preserved
> as a `*.lean-ctx.bak` backup.

## 2. HTTP MCP & multi-repo — `lean-ctx serve`

For clients that speak Streamable HTTP instead of stdio, or to serve several
repos at once:

```bash
lean-ctx serve --daemon                       # background HTTP MCP server
lean-ctx serve --root ~/work/api:api \
               --root ~/work/web:web           # multi-repo, with aliases
lean-ctx serve --status
lean-ctx serve --stop
```

Multi-repo search fuses results across roots with Reciprocal Rank Fusion
(`--rrf-k`). The MCP equivalent is `ctx_multi_repo` (`add_root`, `list_roots`,
`search`, `save_config`).

The **daemon** (`lean-ctx daemon`) is the local IPC service (Unix socket in
`~/.local/share/lean-ctx/`); most users never touch it directly.

## 3. External context providers — `ctx_provider`

**What it does:** Brings issues, PRs/MRs, pipelines, tickets, and DB schema into
context so `ctx_semantic_search` and `ctx_knowledge` can find them.

Supported: GitHub, GitLab, Jira, Postgres, and arbitrary MCP bridges.

```text
ctx_provider action=list
ctx_provider action=gitlab_issues state=opened labels=bug
ctx_provider action=gitlab_mrs
ctx_provider action=query provider=jira resource=PROJ-123
```

**Auth:** via env tokens — `GITHUB_TOKEN`/`GH_TOKEN`, `GITLAB_TOKEN`/`CI_JOB_TOKEN`,
`JIRA_URL`+`JIRA_EMAIL`+`JIRA_TOKEN`, `DATABASE_URL`. Jira also supports OAuth via
`lean-ctx provider auth jira`. Configure under `[providers]` in `config.toml`.

**The pipeline:** provider data flows through the same consolidation path as
everything else — the raw provider results are consolidated into BM25 chunks,
graph edges, and knowledge facts. That's why a GitHub issue can show up as a
cross-source hint when you read a related file.

## 4. Context profiles — `lean-ctx profile`

> Not to be confused with **tool profiles** (`lean-ctx tools`, Journey 2). Tool
> profiles pick *which MCP tools* exist. **Context profiles** tune *compression
> and read-mode behavior*.

```bash
lean-ctx profile list
lean-ctx profile show [name]
lean-ctx profile active
lean-ctx profile diff A B
lean-ctx profile set <name>
```

Set the active profile with `LEAN_CTX_PROFILE`; project overrides live in
`<repo>/.lean-ctx/profiles/`.

## 5. Packaging & sharing context — `lean-ctx pack` / `ctx_pack`

**Context packages** bundle curated context (and PR-specific "PR packs") so it
can be installed elsewhere or shared with teammates.

```bash
lean-ctx pack pr                         # build a PR pack for the current diff
lean-ctx pack create --name my-context
lean-ctx pack list
lean-ctx pack install <name>
lean-ctx pack export / import
```

Packages live under `packages/` with a `package-index.json`. `ctx_pack` exposes
the same actions to your AI.

## 6. Multi-agent coordination — `ctx_agent`, `ctx_handoff`, `ctx_share`

For workflows where several AI agents collaborate:

| Tool | Purpose |
|------|---------|
| `ctx_agent` | Register agents, post/read messages, `handoff`, `sync`, shared diaries |
| `ctx_handoff` | Deterministic handoff bundles (Context Ledger Protocol) |
| `ctx_share` | Push/pull cached file contexts between agents |
| `ctx_task` | A2A task orchestration (create/update/cancel) |

State lives under `agents/` (registry, diaries, shared knowledge) with per-agent
identity keys in `keys/`. Handoff bundles are written to `handoffs/`.

## 7. Governing rules — `lean-ctx rules` / `ctx_rules`

Keeps the LeanCTX rule blocks in sync across every agent's rule file
(`.cursor/rules`, `AGENTS.md`, `CLAUDE.md`, …).

```bash
lean-ctx rules status        # what's installed where
lean-ctx rules sync          # re-sync all agents
lean-ctx rules diff          # show drift
lean-ctx rules lint          # validate
```

Scope via `rules_scope` (`both`/`global`/`project`). Promote high-confidence
knowledge into rules with `lean-ctx export-rules`.

## 8. Plugins — `lean-ctx plugin`

```bash
lean-ctx plugin list
lean-ctx plugin enable <name>
lean-ctx plugin info <name>
lean-ctx plugin init          # scaffold a new plugin
lean-ctx plugin hooks         # show hook points
```

Plugins live under `<config-dir>/lean-ctx/plugins/`. `ctx_plugins` exposes
list/enable/disable/info/hooks to your AI.

## 9. Client integration internals — `instructions` & `hook`

These are the low-level building blocks `setup`/`init` (Journey 1) wire up for
you. You rarely call them by hand, but they're documented for anyone integrating
a new client or debugging an integration:

```bash
lean-ctx instructions --client cursor          # compile guidance for one client
lean-ctx instructions --client claude --profile standard --crp tdd
lean-ctx instructions --client codex --json --include-rules
lean-ctx instructions --list-clients           # which client IDs are supported
```

`instructions` renders the system-prompt/tool-instruction block a given client
should receive — useful when adding support for an editor `setup` doesn't know
yet, or to inspect exactly what guidance LeanCTX injects. `--client <id>` selects
the target (see `--list-clients`); `--profile` and `--crp off|compact|tdd` tune
the tool surface and output style; `--unified` emits one combined block; `--json`
adds metadata and, with `--include-rules`, the rules-file contents. Output is
**deterministic** for the same inputs, which is what lets the docs-drift CI gate
diff it reliably.

```bash
lean-ctx hook <rewrite|redirect|observe|copilot|codex-pretooluse|codex-session-start|rewrite-inline>
```

`hook` exposes the agent hook entry points that editors call automatically
(Cursor/Claude/Copilot/Codex). They are invoked by the editor's hook mechanism,
not typed manually — listed here so the integration surface is fully accounted
for.
