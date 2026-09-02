# API Guide

> **Status: local implementation reference.** Routes and schemas describe only
> the installed Runtime's discoverable capability; they do not create a hosted
> API, generic agent platform, or support commitment. LeanCTX is **The Context
> SDK for AI Agents**; see `docs/internal/README.md` (internal, not in this repository).

This guide summarizes the stable integration surfaces. Discover a running
instance before calling tools: `GET /v1/capabilities` and `GET /v1/openapi.json`
report the enabled features and published HTTP schema.

## REST APIs

lean-ctx exposes related APIs on different runtime surfaces. Do not substitute
one for another merely because their paths look similar.

| Surface | Endpoint | Response | Purpose |
|---|---|---|---|
| HTTP server | `GET /health` | plain-text `ok` | Liveness probe. |
| HTTP server | `GET /v1/metrics` | JSON; `?format=prometheus` gives Prometheus text | Context OS metrics. |
| Dashboard | `GET /metrics` | Prometheus text | Dashboard telemetry endpoint. |
| Proxy | `GET /v1/quality-lab` | JSON report | Empty-input Quality Lab assessment. |
| Proxy | `POST /v1/quality-lab` | JSON report | Assess supplied `original`, `compressed`, and optional `ext`. |

The HTTP server's stable `/v1` API also includes manifests, capabilities, tool
listing and calls, SSE events, context summaries, event search, and lineage.
The authoritative machine-readable description is served at:

```text
GET /v1/openapi.json
```

The checked-in API snapshot is [openapi-v1.snapshot.json](../reference/openapi-v1.snapshot.json).
For endpoint authentication, scopes, payload examples, and SSE semantics, use
the [HTTP-MCP contract](../contracts/http-mcp-contract-v1.md).

### Calling a tool over REST

`POST /v1/tools/call` accepts a tool name and an argument object. The optional
workspace and channel identify shared HTTP-server state and default to
`"default"` when omitted.

```json
{
  "name": "ctx_read",
  "arguments": { "path": "README.md", "mode": "signatures" },
  "workspaceId": "backend",
  "channelId": "api-work"
}
```

`GET /v1/tools` returns the paginated tool surface; do not hard-code a list
because profiles, plugins, and compiled features can change it.

## MCP protocol

For editor integrations, the editor launches lean-ctx as an MCP server over
stdio. Standard MCP discovery and invocation apply:

```text
tools/list
tools/call { name, arguments }
```

The returned tool schema is the contract for the supplied arguments. Read-only
tools carry `readOnlyHint: true`, `destructiveHint: false`, and
`idempotentHint: true`; examples include `ctx_read`, `ctx_search`, `ctx_glob`,
`ctx_tree`, `ctx_compose`, and `ctx_callgraph`. `ctx_shell`, `ctx_execute`, and
`ctx_patch` are marked destructive because they may change the environment.

Tool visibility is profile-dependent:

```bash
lean-ctx tools show
lean-ctx tools minimal
lean-ctx tools standard
lean-ctx tools power
```

Use [the MCP tool map](../reference/appendix-mcp-tools.md) for human-oriented
tool descriptions; use `tools/list` for the actual schema available to a client.

## OCLA Wire v1

The [OCLA Wire v1 schema](../contracts/ocla-wire-v1.schema.json) defines the
Canonical Token Envelope. It is a strict, payload-free record of a token
decision, not a container for source text.

Required top-level fields are:

| Field | Meaning |
|---|---|
| `schema_version` | Must be `1`. |
| `context` | Correlation context. |
| `surface` | `mcp`, `proxy`, `shell`, or `agent`. |
| `direction` | `input` or `output`. |
| `provider`, `model` | Provider-neutral routing identity. |
| `token_balance` | Original, materialized, delivered, and provider-billed token counts. |
| `idempotency_key` | Stable key for deduplicating the decision. |

`context` carries `request_id`, `session_id`, `agent_id`, `content_ref`, and
nullable `tenant_id`. The runtime request context can additionally carry a
`trace_id` for correlation. `route_ref` and `policy_ref` are optional references.

At the transport boundary, requests and responses carry the operation result;
streaming responses may carry `StreamChunk`, while agent-facing payloads include
`Messages`, `ToolCall`, and `Usage`. These are separate typed payloads around
the envelope and do not weaken the envelope schema's no-payload rule. Validate
data against the schema before acceptance; unknown fields and invalid
token-balance ordering are rejected.

### OCLA HTTP endpoints

An OCLA router may expose its independent endpoints under `/ocla/v1/`, including
health, capabilities, envelope submission, agents, metrics, ledger summary,
budgets, dead-letter queue management, and capsules. Treat this as a distinct
surface from `/v1`; discover it from its own capabilities response.

### Contract integrity

[ocla-contract-pack-v1.json](../contracts/ocla-contract-pack-v1.json) contains
the contract-pack artifact paths and SHA-256 content digests. Verify those
digests before pinning a schema or fixture set in an integration. This validates
the reviewed local contract pack; it does not prove remote delivery or billing.

## CLI reference

Run `lean-ctx help` for everyday commands and `lean-ctx help all` for the full
reference. The most common integration and lifecycle commands are:

| Command | Purpose |
|---|---|
| `onboard`, `setup` | Configure detected clients and integrations. |
| `wrap`, `unwrap` | Legacy names are not a current CLI surface; use `setup`, `-c`, or `bypass`. |
| `-c` / `exec`, `bypass` | Run with compressed output or explicitly without compression. |
| `serve` | Start Streamable HTTP MCP and the `/v1` API. |
| `proxy start`, `proxy stop`, `proxy status` | Manage the model API proxy. |
| `daemon start`, `daemon stop`, `daemon status` | Manage the IPC daemon. |
| `restart`, `dev-install` | Apply runtime changes or atomically install a development build. |
| `status`, `doctor`, `stats`, `dashboard` | Inspect health, diagnose setup, view raw stats, or open the dashboard. |
| `agent register`, `agent list` | Register an agent and inspect active identities (`agent list --all` shows lifecycle history). |
| `ledger status`, `ledger reset`, `ledger evict` | Inspect and manage the context ledger. |
| `quality-lab` | Run a local quality assessment on original and compressed text. |

`start` and `stop` alone are lifecycle commands, but proxy and daemon use their
own subcommands. `ledger export` and `ledger verify` are not current public CLI
subcommands; use the MCP/contract evidence surfaces for machine verification.
See the [CLI command map](../reference/appendix-cli-map.md) for the authoritative
command inventory.

## Error responses

The HTTP `/v1` API returns JSON error envelopes:

```json
{ "error": "human-readable explanation", "error_code": "machine_code" }
```

Branch on `error_code`, not the message.

| Code | HTTP | Meaning |
|---|---:|---|
| `unauthorized` | 401 | Missing, malformed, or invalid bearer token. |
| `scope_denied` | 403 | Token lacks required team-server scope. |
| `unknown_workspace` | 400 | Requested workspace is not served. |
| `invalid_arguments` | 400 | Tool arguments are not a JSON object. |
| `invalid_request` | 400 | Request body cannot be parsed or read. |
| `tool_error` | 400 | Tool ran and returned an error. |
| `request_timeout` | 504 | Tool call exceeded the configured timeout. |

`GET /health` is deliberately different: it returns the plain-text liveness
response, not an error envelope.

## Limits and quotas

Use server configuration and capability discovery to set client behavior; do
not assume the defaults apply to every deployment.

| Limit | Local HTTP-server default | Notes |
|---|---:|---|
| Request body | 2 MiB | `max_body_bytes`. |
| Concurrent requests | 32 | `max_concurrency` semaphore. |
| Request rate | 50/s | `max_rps`, with a default burst of 100. |
| Tool-call timeout | 30,000 ms | `request_timeout_ms`. |
| `ctx_read` cache | 2,000,000 tokens | `cache_max_tokens = 0` selects this built-in default. |
| SSE replay | 1,000 events | Server-side replay cap. |

OCLA budgets can be configured by scope through the OCLA budget API. Session
and context budgets are policy and configuration inputs, not universal fixed
quotas; use capabilities and the active policy to determine the effective limit.
For config field definitions and security requirements, see the
[HTTP-MCP contract](../contracts/http-mcp-contract-v1.md).
