# Appendix — Glossary

Every term lean-ctx uses, in one place. If a command or doc uses a word you don't
recognize, it's here.

## Core concepts

**MCP (Model Context Protocol)** — the standard your AI editor uses to call
external tools. lean-ctx registers as an MCP server so your editor can call
`ctx_*` tools instead of its own native file reads/search.

**MCP tool** — one of the 68 `ctx_*` functions lean-ctx exposes (e.g.
`ctx_read`, `ctx_search`). Your AI calls these; you usually don't. See the
[MCP tool map](appendix-mcp-tools.md).

**Shell hook** — a snippet lean-ctx adds to your shell RC file. It lets terminal
commands (run by you or your AI) be compressed automatically without typing
`lean-ctx -c`.

**Data directory** — `~/.lean-ctx/` (or XDG `~/.config/lean-ctx/`). Holds config,
stats, sessions, caches, indexes, and knowledge. Auto-detected; see
[paths reference](appendix-paths-and-config.md).

**Compression** — the heart of lean-ctx: returning the *signal* of a file or
command output while dropping noise, measured in tokens saved. Levels: `off`,
`lite` (default), `standard`, `max`.

## Memory & sessions

**CCP (Cross-Session Context Protocol)** — how lean-ctx saves a session's state
(tasks, findings, decisions) so the next session in the same project can resume
automatically.

**Session** (singular `session` command) — your current working context. Records
*into* the session.

**Session store** (plural `sessions` command, alias `session-store`) — the
collection of saved session snapshots. Managed/repaired with `sessions doctor`.

**Knowledge** — the project-scoped, permanent fact base. Survives across all
sessions; recallable by exact, semantic, or hybrid search.

**Gotcha** — an auto-detected recurring error pattern, stored so the same mistake
isn't repeated. Project-scoped or universal (cross-project).

**Wakeup** — the bundle of relevant prior knowledge injected at session start
(via `ctx_overview` when `enable_wakeup_ctx` is on).

## Read modes

**Read mode** — how `ctx_read` returns a file: `full`, `map`, `signatures`,
`aggressive`, `entropy`, `task`, `reference`, `diff`, `lines:N-M`, or `auto`.
See [Journey 2](02-daily-use.md).

**Session cache** — keeps already-read files so an unchanged re-read costs ~13
tokens instead of the whole file.

## Profiles (two different things!)

**Tool profile** — *how many MCP tools* your AI sees: `minimal` (5), `standard`
(15), `power` (all). Set with `lean-ctx tools`.

**Context profile** — *compression/read-mode behavior* tuning. Set with
`lean-ctx profile`. Different from tool profile despite the similar name.

## Code intelligence

**Property graph** — the in-repo graph of files, symbols, and edges (imports,
calls, references) that powers `graph`, `impact`, `callgraph`, `repomap`,
`architecture`, and `smells`. Built with tree-sitter.

**Impact / blast radius** — everything transitively affected by changing a file
or symbol (`ctx_impact`).

**Repomap** — a PageRank-ranked map of the most important symbols, within a token
budget (`ctx_repomap`). MCP-only.

**Call graph** — who-calls-what relationships (`ctx_callgraph`): callers, callees,
traces, risk scores.

## Network & integrations

**Proxy** — an optional layer between your AI client and the LLM API that
compresses `tool_results` in-flight. Runs on port 4444 by default. The most
powerful and most invasive feature (edits RC files / API base URLs).

**Daemon** — the local IPC service (Unix socket). Background plumbing; rarely
touched directly.

**Serve (HTTP MCP)** — running lean-ctx as an HTTP MCP server (Streamable HTTP),
including multi-repo serving.

**Provider** — an external context source: GitHub, GitLab, Jira, Postgres, or an
MCP bridge. Surfaced via `ctx_provider`.

**RRF (Reciprocal Rank Fusion)** — how multi-repo search merges ranked results
from several repositories.

**Context package / PR pack** — a bundle of curated context (or PR-specific
context) that can be installed or shared (`ctx_pack`).

## Multi-agent

**Handoff** — a deterministic context bundle passed from one agent to another
(Context Ledger Protocol, `ctx_handoff`).

**Diary** — an agent's running log of discoveries/decisions (`ctx_agent diary`),
shareable between agents.

## Lifecycle

**LaunchAgent / systemd unit** — OS autostart mechanism. lean-ctx uses
`com.leanctx.{proxy,daemon,autoupdate}.plist` (macOS) or systemd user units
(Linux). The proxy has `KeepAlive=true`, which is why plain `kill` doesn't stop
it — use `lean-ctx stop`.

**`.bak` backup** — every edit lean-ctx makes to an existing file writes a
`*.lean-ctx.bak` first, so changes are reversible.

**Rewire** — re-applying MCP/rules config after an update (`update --rewire`,
internal `post_update_rewire`), so a new version's tool list reaches your editors.

## Safety

**PathJail** — restricts file access to allowed roots. Extend with `allow_paths`
/ `LEAN_CTX_ALLOW_PATH`.

**Shell allowlist** — the ~200 binaries `ctx_shell` is permitted to run. Replace
the whole set with `shell_allowlist` / `LEAN_CTX_SHELL_ALLOWLIST`, or just add a
few extras with `shell_allowlist_extra` (managed via `lean-ctx allow <cmd>`).

**Secret detection** — redacts secrets from output before they enter context
(`[secret_detection]`, on by default).

**Kill switch** — `LEAN_CTX_DISABLED=1` disables everything for a session; the
`lean-ctx-off` shell alias does the same.
