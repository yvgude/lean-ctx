# Developer Guide

> **Status: historical implementation guide.** It must not be read as a public
> builder, extension-marketplace, or multi-agent product promise. LeanCTX is
> **The Context SDK for AI Agents**; current scope and status are governed by
> [`docs/internal/README.md`](../internal/README.md).

This guide explains how to integrate with and extend lean-ctx. For installation
and daily operation, start with the [reference guides](../reference/README.md).

## How lean-ctx works

lean-ctx turns high-volume development inputs into compact, recoverable context.
The normal path is:

```text
Read or run command → classify and compress → cache → serve to the agent
```

- **Read:** MCP tools access workspace files, indexes, and approved commands.
- **Compress:** language-aware read modes and command-output patterns retain the
  structure needed for the current task.
- **Cache:** unchanged reads can be represented by a compact cache reference;
  use `fresh: true` when the caller needs a new disk read.
- **Serve:** the tool response includes the compressed result and, where needed,
  an identifier that `ctx_expand` can recover without loss.

### MCP server and shadow mode

lean-ctx is an [MCP](https://modelcontextprotocol.io/) server. Editors normally
launch it over stdio and discover its tools through `tools/list`. The active
tool profile determines the advertised surface; inspect or change it with:

```bash
lean-ctx tools show
lean-ctx tools standard
```

Shadow mode is enabled by default. For agents that support tool-deny hooks,
native Read, Grep, Glob, and Shell calls are denied so calls use `ctx_*` tools.
This prevents a native fallback from bypassing compression. Set
`shadow_mode = false` only when native tools must remain available.

`hook_mode` in `~/.config/lean-ctx/config.toml` controls integration behavior:

| Mode | Behavior |
|---|---|
| `replace` | Denies native file/search/shell tools; MCP is the access path. |
| `hybrid` | Provides MCP plus shell hooks; suitable where deny hooks are unavailable. |
| `mcp` | Provides the MCP server only; no shell hooks. |

`replace` is selected automatically for supported agents, including Codex,
Cursor, Claude Code, Windsurf, and OpenCode. See the [advanced reference](../reference/05-advanced.md)
for operational configuration.

## MCP tool catalog

The following core tools form the developer-facing integration surface. Tool
profiles and complete parameter schemas are in the [MCP tool map](../reference/appendix-mcp-tools.md).

| Tool | Purpose |
|---|---|
| `ctx_read` | Read files with compression modes such as `auto`, `map`, `signatures`, and `diff`. |
| `ctx_compose` | Orient to a task with ranked files and symbols; call it before broad exploration. |
| `ctx_search` | Search code with `action=grep`, `action=symbol`, or `action=semantic`. |
| `ctx_shell` | Run an allowed shell command with structured output compression. |
| `ctx_glob` | Find files by pattern, respecting `.gitignore` by default. |
| `ctx_tree` | Return a compact directory tree with metadata. |
| `ctx_expand` | Recover archived or compressed response content without loss. |
| `ctx_callgraph` | Find function callers, callees, traces, and call-graph risk. |
| `ctx_session` | Store and retrieve task, finding, decision, and session state. |
| `ctx_knowledge` | Persist project knowledge across sessions. |
| `ctx_call` | Invoke a named lean-ctx tool through the lazy-loading dispatcher. |

Use `ctx_search` with its supported action rather than parsing large files
yourself. `grep` is the normal code-search path; symbol and semantic operations
are available when exposed by the active profile. `ctx_call` is useful for a
client that keeps its initial MCP tool list small, not a replacement for normal
MCP calls.

`ctx_read` supports ten working modes: `auto`, `full`, `map`, `signatures`,
`diff`, `aggressive`, `entropy`, `task`, `reference`, and `lines:N-M`. Choose
`signatures` for an API surface, `map` for structure, `diff` after an edit, and
`full` or `anchored` when the client will make an edit. See [context engineering](../reference/07-context-engineering.md)
for selection guidance.

## Custom compression rules

Built-in shell patterns handle common development commands. For a tool with
stable, noisy output, add a custom filter before asking for a new built-in
pattern. lean-ctx loads TOML filter files from `~/.lean-ctx/filters/`.

```bash
lean-ctx filter init
lean-ctx filter list
lean-ctx filter validate ~/.lean-ctx/filters/example.toml
```

Each rule can match a command and output pattern, replace matching text, or
retain selected lines. Keep rules narrow: match the command name first, preserve
errors and summaries, and validate the rule before relying on it in CI.

```toml
[[rules]]
command = "^acme-cli test"
keep_lines = ["^(PASS|FAIL|ERROR|Summary:)"]
```

Custom filters run before generic compression, so they are appropriate for
organization-specific CLIs. Do not place compression rules in an undocumented
`patterns/` directory; the supported configuration location is the filter
directory above. The [customization guide](../reference/10-customization-and-governance.md)
has the command reference.

## SDK and protocol-client integration

Starting with SDK 1.1.0, the supported Python Agent SDK consumes the versioned
Engine tool-session protocol; it does not embed or reimplement the compression
engine. Lower-level HTTP and OCLA consumers should first discover the running
server:

```text
GET /v1/capabilities
GET /v1/openapi.json
```

Then use the returned capabilities and tool schemas instead of assuming a
particular tool profile or compiled feature set.

| Surface | Current package | Integration role |
|---|---|---|
| Python Agent SDK | `thinkery-leanctx-sdk` (external repository) | `AgentContext`, Engine lifecycle, context tools, and receipts. |
| Rust protocol client | `lean-ctx-client` | HTTP client, OCLA envelope validation, and verification tooling. |
| Rust embedding facade | `lean-ctx-embed` | In-process compression integration; not an Agent SDK. |

Consult the [SDK surface](../reference/sdk-surface.md) before selecting a
package; a protocol client or embedding facade is not a second SDK.

The canonical wire schema is [OCLA Wire v1](../contracts/ocla-wire-v1.schema.json).
Validate envelopes before sending or accepting them; unknown fields are rejected
by this schema. Verify the contract-pack content digests in
[ocla-contract-pack-v1.json](../contracts/ocla-contract-pack-v1.json) when
pinning an integration to a reviewed contract set.

## OCLA: Open Context Layer Architecture

OCLA defines portable context-engineering boundaries. The registry has fourteen
capability kinds:

1. observation hook, usage sink, metrics exporter, and savings ledger;
2. intent classifier, outcome tracker, compression provider, and response optimizer;
3. model router, efficiency analyzer, config tuner, experiment runner,
   connector scheduler, and agent gateway.

An implementation reports which are available, degraded, or unavailable; a
client should discover that state instead of inferring it from a version string.
The [capabilities contract](../contracts/capabilities-contract-v1.md) defines
runtime discovery for the broader lean-ctx server surface.

The **Canonical Token Envelope v1** is OCLA's payload-free record of a token
decision. It identifies the request context (`request_id`, `session_id`,
`agent_id`, `content_ref`, and optional tenant), surface, direction, provider,
model, token balances, routing and policy references, and idempotency key.
The runtime may add `trace_id` to its request context for correlation.

A **Context Receipt** records delivered tokens, cache hits and misses, outcome,
quality signals, and feedback attribution after a context plan is delivered.
Together with the plan and source references, it shows what local context was
delivered and how it was accounted for. It is evidence for local analysis, not
proof of remote delivery, billing, or commercial savings. The [multi-agent efficiency contract](../contracts/multi-agent-efficiency-benchmark-v1.md)
defines the stronger evidence requirements for benchmarked handoffs.

## Extending lean-ctx

### Providers

Providers make external sources available to the same consolidation pipeline as
local context. Built-ins support GitHub, GitLab, Jira, and PostgreSQL when their
credentials are configured. Config-based REST providers live in
`~/.config/lean-ctx/providers/` or `.lean-ctx/providers/`; MCP bridges can use
HTTP or stdio transports.

Provider results are consolidated into search chunks, graph edges, knowledge
facts, and session-cache entries. Use `ctx_provider` to inspect or query the
configured source. The [Provider Framework Contract](../contracts/provider-framework-contract-v1.md)
defines authentication, indexing, and safety expectations.

### Custom MCP workflows and the agent bus

Build a thin custom MCP tool around a stable workflow, then dispatch its
lean-ctx operations through `ctx_call` or regular MCP `tools/call` requests.
Keep the custom tool's input schema small and return references rather than
duplicating large contexts.

For multi-agent work, register each worker and inspect active identities:

```bash
lean-ctx agent register --id "worker-123" --role coder --owner team@example.com
lean-ctx agent list
```

Use `ctx_session` for durable task state and the agent-bus tools for messages,
handoffs, and shared knowledge. See the [multi-agent guide](../reference/08-multi-agent.md)
for coordination and conflict-handling patterns.

## Development workflow

Build and test the repository without stopping the installed runtime:

```bash
cd rust && cargo build --release
cargo test --lib
cargo clippy --all-features -- -D warnings
cargo fmt --check
```

Install a locally built release only after checks pass:

```bash
lean-ctx dev-install
```

`dev-install` performs the short, atomic stop/install/restart sequence. Do not
run `lean-ctx stop` before building or testing: those commands use the worktree
target directory and do not require the installed proxy or daemon to be stopped.
