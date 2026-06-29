# Journey 5 — Advanced & Integrations

> You've mastered daily use and want more: compress the LLM API stream itself,
> pull in GitHub/GitLab/Jira context, share context across repos or agents, and
> govern rules across your team. This journey covers the power-user surface.

Source files referenced here:
- `rust/src/cli/dispatch/network.rs` — `serve`, `proxy`, `daemon`, `provider`, `team`
- `rust/src/cli/profile_cmd.rs` — context `profile`
- `rust/src/cli/plugin_cmd.rs`, `rules_cmd.rs`, `pack_cmd.rs`
- `rust/src/tools/registered/ctx_provider.rs`, `ctx_pack.rs`, `ctx_multi_repo.rs`,
  `ctx_agent.rs`, `ctx_handoff.rs`
- `rust/src/core/gateway/` (`client.rs`, `catalog.rs`, `router.rs`, `config.rs`),
  `rust/src/tools/ctx_tools.rs` — the MCP Tool-Catalog Gateway

---

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

**Plays nice with provider prompt caching.** Anthropic's `cache_control` and
OpenAI's automatic prompt caching bill cached prefix tokens at a fraction of
the base rate — but only for *byte-identical* prefixes. The proxy therefore
mutates history exclusively in cache-stable ways: tool-result compression is
content-deterministic (the same result compresses identically on every turn),
and old tool results are summarized only at **frozen compaction boundaries**
that advance in large deterministic strides instead of a per-turn rolling
window. Between boundary jumps your request prefix stays byte-identical, so
cache reads keep hitting; a jump costs one re-write and then caching resumes
on the smaller history. Tune via `[proxy].history_mode` (or
`LEAN_CTX_PROXY_HISTORY_MODE`):

| Mode | Behaviour | Use when |
|------|-----------|----------|
| `cache-aware` *(default)* | Prune at frozen 16-message strides, ≥8 recent messages always intact | You use prompt caching (Claude Code, Cursor, most clients) |
| `rolling` | Legacy: summarize everything older than the last 6 messages, every turn | Maximum raw-token reduction, no prompt caching in play |
| `off` | Never prune history (compression still applies) | Debugging, or the client manages history itself |

> **Heads-up (community-reported):** `proxy enable` modifies your shell RC. If a
> base URL "defaults to the wrong provider," check the exported `*_BASE_URL`
> values in your RC and `lean-ctx proxy status`. The unmodified RC is preserved
> as a `*.lean-ctx.bak` backup.

> **Claude Pro/Max subscriptions need an API key for the proxy.** The proxy
> forwards your credential upstream but never *injects* one. A Claude Pro/Max
> subscription authenticates via OAuth directly against `api.anthropic.com`, and
> that token is rejected by any custom `ANTHROPIC_BASE_URL` — routing it through
> the proxy produces a login loop / 401. Therefore `proxy enable` **skips the
> Claude redirect when no `ANTHROPIC_API_KEY` is detected** (env or
> `~/.claude/settings.json`) and leaves Claude Code talking to Anthropic directly.
> `lean-ctx doctor` flags the conflict if a stale redirect remains.
>
> - **On a subscription?** Keep the proxy disabled for Claude and get savings from
>   the lean-ctx MCP tools instead (`ctx_read` / `ctx_search` / `ctx_shell`).
>   Other providers (OpenAI/Codex, Gemini, Ollama) are still routed through the
>   proxy.
> - **Pay-as-you-go?** Export `ANTHROPIC_API_KEY=…`, then run
>   `lean-ctx proxy enable` (or `--force` to override detection). Claude traffic is
>   then compressed by the proxy.

### Codex in front of the proxy (native WebSocket + HTTP/SSE)

The proxy serves the OpenAI Responses API on both `/v1/responses` and the bare
`/responses` path over **two transports**: native **WebSocket**
(`ws://127.0.0.1:4444/responses`) — Codex's default — and **HTTP/SSE** for clients
that prefer it ([#440](https://github.com/yvgude/lean-ctx/issues/440)). Point Codex
at the proxy and it connects over WebSockets out of the box; the proxy bridges the
WS frames to the upstream and compresses them like any other request:

```toml
# ~/.codex/config.toml — point Codex at the proxy (WebSockets work as-is)
[model_providers.lean-ctx]
name = "lean-ctx"
base_url = "http://127.0.0.1:4444/v1"
```

> Prefer HTTP/SSE instead? Set `supports_websockets = false` in the provider block
> to force Codex onto the `/v1/responses` HTTP transport.

**Non-loopback HTTP upstreams (e.g. `codex-lb`).** By default an upstream must be
HTTPS unless it is loopback (`127.0.0.1` / `localhost` / `[::1]`). To put the proxy
in front of a *trusted local-network* plaintext service such as
`http://host.docker.internal:2455`, opt in deliberately — otherwise the upstream is
rejected:

```bash
# env (any value) — wins over config.toml
export LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM=1
export LEAN_CTX_OPENAI_UPSTREAM="http://host.docker.internal:2455"
```

```toml
# or persist it in config.toml
[proxy]
openai_upstream = "http://host.docker.internal:2455"
allow_insecure_http_upstream = true
```

> ⚠ This downgrades the upstream hop to plaintext HTTP. Use it **only** on a trusted
> local network (loopback, a container host, a private LAN service you control) —
> never for traffic that crosses an untrusted network. The proxy prints a warning at
> startup whenever a non-loopback HTTP upstream is active.

**Custom HTTPS upstream hosts (e.g. a corporate gateway).** By default the upstream
host must be one of the provider defaults (`api.anthropic.com`, `api.openai.com`,
`chatgpt.com`, `generativelanguage.googleapis.com`). To route through a custom HTTPS
host you control — such as `https://gw.corp.example/anthropic` — opt in deliberately
([#590](https://github.com/yvgude/lean-ctx/issues/590)):

```bash
# env (any value) — works for a foreground `lean-ctx proxy start`
export LEAN_CTX_ALLOW_CUSTOM_UPSTREAM=1
```

```toml
# persist it in config.toml — REQUIRED for the service-managed proxy
[proxy]
anthropic_upstream = "https://gw.corp.example/anthropic"
allow_custom_upstream = true
```

> The env var only reaches a proxy you start **in the foreground** (`proxy start`),
> because it inherits your shell. A proxy started by `lean-ctx proxy enable` /
> `restart` runs as a LaunchAgent / systemd service that never sees your shell env,
> so it would otherwise fall back to the provider default. `enable`/`restart`
> therefore **auto-persist** `allow_custom_upstream = true` when you run them with
> `LEAN_CTX_ALLOW_CUSTOM_UPSTREAM` set and a custom upstream configured — or set the
> flag yourself with `lean-ctx config set proxy.allow_custom_upstream true`.

**Live upstream — `config.toml` is the source of truth for a running proxy**
([#449](https://github.com/yvgude/lean-ctx/issues/449)). A long-lived proxy
(LaunchAgent / systemd / IDE-spawned) re-reads its upstreams from `config.toml`
every ~2s, so a change takes effect **without a restart**:

```bash
lean-ctx config set proxy.openai_upstream https://api.openai.com   # live in ≤2s
lean-ctx proxy status                                              # shows the active upstreams
```

- **`LEAN_CTX_*_UPSTREAM` env vars are a *start-time* override only.** An
  environment variable cannot reach a process that is already running, so for a
  service-managed proxy use `config.toml` (or `lean-ctx proxy restart`, which
  re-reads `config.toml` and drops any start-time env override). This is the
  common trap with MCP hosts: **Codex (and other MCP clients) launch the lean-ctx
  MCP server with a stripped, allowlisted environment** that omits
  `LEAN_CTX_*_UPSTREAM`, so the proxy that server spawns never sees it — even
  though `lean-ctx` *invoked directly as a CLI* does. Put the upstream in
  `config.toml` and it applies to every proxy regardless of how it was started.
- An **invalid** value (typo, unreachable scheme) keeps the last good upstream —
  a live proxy is never silently rerouted to the provider default.
- `lean-ctx doctor` warns when the running proxy's live upstream **drifts** from
  what `config.toml` resolves to (typically an env override masking a later edit)
  and points you at `lean-ctx proxy restart`.
- Tune the reload cadence with `LEAN_CTX_PROXY_RELOAD_SECS` (default `2`).

---

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

---

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
everything else — `execute()` → `consolidate()` → BM25 chunks + graph edges +
knowledge facts. That's why a GitHub issue can show up as a cross-source hint
when you read a related file.

---

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

---

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

---

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

---

## 7. Governing rules — `lean-ctx rules` / `ctx_rules`

Keeps the lean-ctx rule blocks in sync across every agent's rule file
(`.cursor/rules`, `AGENTS.md`, `CLAUDE.md`, …).

```bash
lean-ctx rules status        # what's installed where
lean-ctx rules sync          # re-sync all agents
lean-ctx rules diff          # show drift
lean-ctx rules lint          # validate
```

Scope via `rules_scope` (`both`/`global`/`project`). Promote high-confidence
knowledge into rules with `lean-ctx export-rules`.

---

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

---

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
yet, or to inspect exactly what guidance lean-ctx injects. `--client <id>` selects
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

---

## 10. MCP Tool-Catalog Gateway — `ctx_tools` (downstream MCP servers)

**The problem it solves:** every MCP server you connect injects its *entire* tool
catalog into the system prompt — on every request. Ten servers can mean dozens of
tool schemas the model must read and disambiguate before it does anything. More
tools measurably *lowers* tool-selection accuracy and raises cost. lean-ctx only
ever shrank its **own** surface; the gateway extends that to *external* catalogs.

**What it does:** lean-ctx becomes an **MCP gateway** in front of any number of
downstream MCP servers. Instead of registering all their tools, it exposes one
meta-tool, `ctx_tools`:

| Action | What it does |
|--------|--------------|
| `find` | Rank the aggregated downstream catalog against your query (BM25, the same engine as `ctx_search`) and return the top-N as compact **ChoiceCards** |
| `call` | Proxy a `server::tool` call to its owning server and return the result |
| `list` | Show configured servers + how many tools each contributes |
| `refresh` | Drop the catalog cache and re-aggregate |

Net effect: **unlimited downstream tools at roughly constant context cost** — the
model only ever sees the handful that matter for the task in front of it.

**How to use it (config is global-only, off by default):**

```toml
# ~/.lean-ctx/config.toml
[gateway]
enabled = true
top_n = 5              # tools returned per `find`
cache_ttl_secs = 300  # catalog cache lifetime
call_timeout_secs = 30

[[gateway.servers]]
name = "fs"                              # becomes the namespace: fs::read_file
transport = "stdio"                      # spawn a local server as a child process
command = "mcp-server-filesystem"
args = ["/path/to/project"]

[[gateway.servers]]
name = "linear"
transport = "http"                       # connect to a remote server
url = "https://mcp.linear.app/mcp"
headers = { Authorization = "Bearer ${LINEAR_TOKEN}" }
```

Then, from the agent:

```jsonc
// 1) Discover — "what can touch issues?"
ctx_tools {"action":"find","query":"create an issue with a title and assignee"}
// 2) Invoke the chosen handle
ctx_tools {"action":"call","tool":"linear::create_issue",
           "arguments":{"title":"Fix login","assignee":"me"}}
```

**Golden output — `ctx_tools find`** returns a ranked, citation-style shortlist
plus the size of the full catalog it is shielding you from:

```text
gateway: 3 tool(s) for "create an issue" (catalog: 47 tool(s) across 4 server(s))

1. linear::create_issue — Create a Linear issue
   params: title*, assignee, team
2. linear::update_issue — Update fields on an existing issue
   params: id*, title, state
3. github::create_issue — Open a GitHub issue
   params: repo*, title*, body

Invoke one with:
  ctx_tools {"action":"call","tool":"<server::tool>","arguments":{ ... }}
```

**What happens under the hood:**
- `rust/src/core/gateway/client.rs` — a real MCP client built on the official
  `rmcp` SDK. `stdio` spawns the server as a child process; `http` uses the
  streamable-HTTP transport with custom headers. Every connect/list/call is
  bounded by `call_timeout_secs`; sessions are opened per operation and shut down
  cleanly (no stale child processes).
- `rust/src/core/gateway/catalog.rs` — aggregates each enabled server's tools
  into a namespaced `server::tool` catalog behind an in-process **TTL cache**.
  Per-server fetch errors are *surfaced*, never hidden, so a misconfigured server
  is visible to the agent.
- `rust/src/core/gateway/router.rs` — builds an **ephemeral BM25 index** over the
  catalog per query and returns the top-N. Deterministic for a fixed catalog.
- `rust/src/tools/ctx_tools.rs` — gates on config, routes the action, and proxies
  the call; downstream results flow back through the same ephemeral firewall and
  sensitivity floor as native tools.

**Security:** `[gateway]` is **global-only** — it is never merged from a
project-local `.lean-ctx.toml`, so cloning an untrusted repo can never point the
gateway at an arbitrary command or endpoint. It is a complete no-op until you set
`enabled = true`.

---

## UX notes captured during this walkthrough

- The proxy is the most powerful and the most invasive feature (it edits RC files
  and redirects API base URLs). The community-reported "defaults to wrong
  provider" issue is called out inline with the recovery path (check `*_BASE_URL`,
  `proxy status`, `.bak` backup).
- "profile" is overloaded: tool profile (Journey 2) vs. context profile (here).
  Both journeys cross-reference each other to defuse the confusion.
