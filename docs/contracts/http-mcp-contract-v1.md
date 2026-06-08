# HTTP-MCP Contract v1

## Goal

A **versioned HTTP API contract** for lean-ctx Context OS, defining the REST + SSE surface
that sits alongside the Streamable HTTP MCP transport. All endpoints listed below are
served by the same `axum` server that handles MCP protocol messages via fallback routing.

- **workspace-aware**: every request is scoped to a `(workspace_id, channel_id)` pair.
- **observable**: tool calls, session mutations, and graph builds emit events to an SSE bus.
- **redaction-safe**: event payloads are stripped by default; full payloads require Audit scope.
- **bounded**: SSE replay is capped at 1 000 events; rate + concurrency limits protect the server.

## Version (SSOT)

- Runtime (local): `rust/src/http_server/mod.rs`
- Runtime (team): `rust/src/http_server/team.rs`
- Events: `rust/src/core/context_os/context_bus.rs`
- Metrics: `rust/src/core/context_os/metrics.rs`
- Redaction: `rust/src/core/context_os/redaction.rs`

---

## Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| GET | `/health` | none | Liveness probe (`200 ok`) |
| GET | `/v1/manifest` | bearer | Full MCP manifest |
| GET | `/v1/capabilities` | bearer | Instance capabilities discovery ([contract](capabilities-contract-v1.md)) |
| GET | `/v1/openapi.json` | bearer | OpenAPI 3.0 spec for this surface |
| GET | `/v1/tools` | bearer | Paginated tool list |
| POST | `/v1/tools/call` | bearer | Execute a single tool |
| GET | `/v1/events` | bearer + `Events` scope | SSE stream with replay |
| GET | `/v1/context/summary` | bearer | Materialized workspace/channel summary |
| GET | `/v1/events/search` | bearer | Full-text search over event payloads (FTS5) |
| GET | `/v1/events/lineage` | bearer | Causal lineage chain for an event |
| GET | `/v1/metrics` | bearer + `Audit` scope | JSON metrics snapshot |
| POST (fallback) | `/*` | bearer | Streamable HTTP MCP transport |

---

## Error Responses

Every REST endpoint returns errors as a JSON envelope with `Content-Type: application/json`:

```json
{ "error": "invalid bearer token", "error_code": "unauthorized" }
```

| Field | Type | Description |
|-------|------|-------------|
| `error` | string | Human-readable message — for logs/UI, **not** for branching |
| `error_code` | string | Stable machine code clients switch on |

### Codes

| `error_code` | HTTP | Raised when |
|--------------|------|-------------|
| `unauthorized` | 401 | Missing/malformed `Authorization` header, wrong scheme, or invalid bearer token |
| `scope_denied` | 403 | Valid token, but its scopes do not grant the requested endpoint/tool (team server) |
| `unknown_workspace` | 400 | `x-leanctx-workspace` / body `workspaceId` names a workspace the server does not serve (team server) |
| `invalid_arguments` | 400 | Tool `arguments` is not a JSON object (team server) |
| `invalid_request` | 400 | Request body could not be read/parsed (team server) |
| `tool_error` | 400 | The tool ran but returned an error |
| `request_timeout` | 504 | The tool call exceeded `request_timeout_ms` |

`GET /health` is exempt — it is a plain-text liveness probe (`200 ok`), never the JSON envelope.
The A2A JSON-RPC surface keeps the standard JSON-RPC `error: { code, message }` shape instead.

---

## Workspaces and Channels

Every HTTP request is associated with a **(workspace_id, channel_id)** pair that determines
session isolation and event routing.

### Tool Call Requests

Include `workspaceId` and `channelId` in the JSON request body of `POST /v1/tools/call`:

```json
{
  "name": "ctx_read",
  "arguments": { "path": "src/main.rs" },
  "workspaceId": "backend-team",
  "channelId": "feature-auth"
}
```

Both fields default to `"default"` when omitted. Sessions are shared per unique
`(workspace_id, channel_id)` pair — two requests with the same pair share caches,
scratchpad, and knowledge state.

### Workspace Header (Team Server)

The team server supports workspace routing via the `x-leanctx-workspace` HTTP header:

```
x-leanctx-workspace: backend-team
```

The header is resolved during authentication. If the header is absent, the
`defaultWorkspaceId` from the team server configuration is used. An unknown workspace
returns `400 Bad Request`.

### Precedence

| Source | Applies to | Priority |
|--------|-----------|----------|
| `workspaceId` in JSON body | `POST /v1/tools/call` | highest |
| `x-leanctx-workspace` header | all endpoints (team server) | fallback |
| `defaultWorkspaceId` config | team server default | lowest |

---

## Events API (SSE)

### Endpoint

```
GET /v1/events?workspaceId=<ws>&channelId=<ch>&since=<cursor>&limit=<n>
```

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `workspaceId` | string | `"default"` | Filter events by workspace |
| `channelId` | string | `"default"` | Filter events by channel |
| `since` | i64 | `0` | Cursor — replay events with `id > since` |
| `limit` | usize | `200` | Max events to replay (capped at 1 000) |

### Protocol

Server-Sent Events (SSE) stream with full replay support. The connection starts by
replaying persisted events matching the filter, then switches to live broadcast.

```
HTTP/1.1 200 OK
Content-Type: text/event-stream
Cache-Control: no-cache
Connection: keep-alive

id: 42
event: tool_call_recorded
data: {"id":42,"workspaceId":"ws1","channelId":"ch1","kind":"tool_call_recorded","actor":"agent","timestamp":"2026-05-05T13:00:00Z","payload":{...}}

id: 43
event: session_mutated
data: {"id":43,"workspaceId":"ws1","channelId":"ch1","kind":"session_mutated","actor":"agent","timestamp":"2026-05-05T13:00:01Z","payload":{...}}
```

### Event Types

| Kind | Trigger |
|------|---------|
| `tool_call_recorded` | Any MCP tool invocation completes |
| `session_mutated` | Shared session state is modified |
| `knowledge_remembered` | Knowledge store entry written |
| `artifact_stored` | Artifact persisted to proof store |
| `graph_built` | Dependency/call graph index built or updated |
| `proof_added` | Evidence ledger entry appended |

### Event Schema (`ContextEventV1`)

```json
{
  "id": 42,
  "workspaceId": "ws1",
  "channelId": "ch1",
  "kind": "tool_call_recorded",
  "actor": "agent-a",
  "timestamp": "2026-05-05T13:00:00.000Z",
  "version": 17,
  "parentId": null,
  "consistencyLevel": "local",
  "payload": {
    "tool": "ctx_read",
    "action": null,
    "path": "src/main.rs",
    "reasoning": "Reading entry point for auth refactor"
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | i64 | Monotonically increasing event ID (SQLite autoincrement) |
| `workspaceId` | string | Workspace that produced the event |
| `channelId` | string | Channel within the workspace |
| `kind` | string | One of the event types above |
| `actor` | string \| null | Identifier of the agent/user that triggered the event |
| `timestamp` | RFC 3339 | Server-side UTC timestamp |
| `version` | i64 | Monotonic counter per (workspace, channel) pair |
| `parentId` | i64 \| null | Causal link to the triggering event (enables lineage graphs) |
| `consistencyLevel` | string | `"local"`, `"eventual"`, or `"strong"` (see below) |
| `payload` | object | Event-specific data (subject to redaction) |

### Consistency Levels

Each event is classified by how it should be treated in multi-agent coordination:

| Level | Meaning | Event Kinds |
|-------|---------|-------------|
| `local` | Agent-local, informational — never requires sync | `tool_call_recorded`, `graph_built` |
| `eventual` | Shared, eventually consistent — broadcast via bus | `knowledge_remembered`, `artifact_stored` |
| `strong` | Shared, critical — other agents should sync before proceeding | `session_mutated`, `proof_added` |

### Enriched Payloads

Event payloads include contextual metadata when available:

| Field | Included When | Description |
|-------|--------------|-------------|
| `tool` | always | Tool name that triggered the event |
| `action` | tool has action param | Tool action (e.g., `"remember"`, `"save"`) |
| `path` | file-related tools | File path involved |
| `category` | knowledge tools | Knowledge category |
| `key` | knowledge tools | Knowledge key |
| `reasoning` | session has active task | Current task description from session state |

### Staleness Guard

When an agent in shared mode has fallen behind by more than **K events** (default: 10),
the server injects a `[CONTEXT STALE]` prefix into tool responses:

```
[CONTEXT STALE] 15 events happened since your last read. Use ctx_session(action="status") to sync.
```

### Knowledge Conflict Detection

When `ctx_knowledge(action="remember")` writes a fact and another agent recently wrote to
the same `category/key`, a `[CONFLICT]` warning is injected:

```
[CONFLICT] Agent 'agent-b' recently wrote to the same knowledge key 'architecture/auth-strategy'. Review before proceeding.
```

### SSE Backfill on Lag

When a broadcast subscriber falls behind (channel buffer overflow), the server automatically
backfills missed events from SQLite instead of silently dropping them. Clients may
receive a synthetic `event: backfill` SSE message indicating the recovery.

### Reconnect

Use `since=<lastEventId>` to resume from the last received cursor. Events are persisted
in SQLite and survive server restarts. The SSE `id:` field matches `ContextEventV1.id`.

```
GET /v1/events?workspaceId=ws1&channelId=ch1&since=42
```

### Heartbeat

The server sends a keep-alive comment every **15 seconds** to prevent proxy/client timeouts:

```
: keep-alive
```

---

## Context Summary

### Endpoint

```
GET /v1/context/summary?workspaceId=<ws>&channelId=<ch>&limit=<n>
```

Returns a materialized view of the workspace/channel state: active agents, recent decisions,
knowledge delta, conflict alerts, and event counts by kind.

### Response Schema

```json
{
  "workspaceId": "ws1",
  "channelId": "ch1",
  "totalEvents": 142,
  "latestVersion": 142,
  "activeAgents": ["agent-a", "agent-b"],
  "recentDecisions": [
    {
      "agent": "agent-b",
      "tool": "ctx_knowledge",
      "action": "remember",
      "reasoning": "JWT preferred for scaling",
      "timestamp": "2026-05-05T13:00:01Z"
    }
  ],
  "knowledgeDelta": [...],
  "conflictAlerts": [
    { "category": "architecture", "key": "auth-strategy", "agents": ["agent-a", "agent-b"] }
  ],
  "eventCountsByKind": {
    "tool_call_recorded": 120,
    "session_mutated": 10,
    "knowledge_remembered": 8,
    "artifact_stored": 3,
    "graph_built": 1,
    "proof_added": 0
  }
}
```

---

## Event Search (FTS5)

### Endpoint

```
GET /v1/events/search?q=<query>&workspaceId=<ws>&limit=<n>
```

Full-text search over event payloads using SQLite FTS5. Returns matching events
ranked by relevance.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `q` | string | required | FTS5 search query |
| `workspaceId` | string | `"default"` | Filter by workspace |
| `limit` | usize | `20` | Max results (capped at 100) |

---

## Event Lineage

### Endpoint

```
GET /v1/events/lineage?id=<eventId>&depth=<n>
```

Traces the causal chain of an event by following `parentId` links. Returns the event
and all ancestors up to `depth` (default 20, max 50).

### Response Schema

```json
{
  "eventId": 42,
  "chain": [ /* ContextEventV1[] from child to root */ ],
  "depth": 3
}
```

---

## Metrics

### Endpoint

```
GET /v1/metrics
```

Returns a JSON snapshot of Context OS process-level counters. Requires `Audit` scope on
the team server.

### Response Schema (`MetricsSnapshot`)

```json
{
  "eventsAppended": 1234,
  "eventsBroadcast": 1200,
  "eventsReplayed": 560,
  "sseConnectionsActive": 3,
  "sseConnectionsTotal": 47,
  "sharedSessionsLoaded": 12,
  "sharedSessionsPersisted": 8,
  "activeWorkspaceCount": 2
}
```

| Field | Type | Description |
|-------|------|-------------|
| `eventsAppended` | u64 | Total events written to the SQLite event log |
| `eventsBroadcast` | u64 | Total events pushed to live SSE subscribers |
| `eventsReplayed` | u64 | Total events served via replay (`since` queries) |
| `sseConnectionsActive` | u64 | Currently open SSE connections (opened − closed) |
| `sseConnectionsTotal` | u64 | Lifetime SSE connections opened |
| `sharedSessionsLoaded` | u64 | Shared sessions loaded from disk |
| `sharedSessionsPersisted` | u64 | Shared sessions persisted to disk |
| `activeWorkspaceCount` | usize | Distinct workspace IDs seen since process start |

---

## Redaction

Event payloads delivered via SSE are redacted by default to prevent leaking file contents,
session data, or tool arguments to observers.

### Redaction Levels

| Level | Default | Exposed Fields | Requires |
|-------|---------|---------------|----------|
| `refs_only` | **yes** | `tool`, `kind`, `event_kind`, `workspace_id`, `channel_id`, `id` + `"redacted": true` | — |
| `summary` | no | All metadata preserved; sensitive content fields (`content`, `file_content`, `result`, `output`, `session_data`, `knowledge_value`, `arguments`) replaced with `[redacted]` | — |
| `full` | no | Complete payload, no redaction | `Audit` scope |

### Example: `refs_only` (default)

```json
{
  "tool": "ctx_read",
  "kind": "tool_call_recorded",
  "workspace_id": "ws1",
  "redacted": true
}
```

### Example: `summary`

```json
{
  "tool": "ctx_read",
  "kind": "tool_call_recorded",
  "workspace_id": "ws1",
  "content": "[redacted]",
  "arguments": "[redacted]"
}
```

### Example: `full`

```json
{
  "tool": "ctx_read",
  "kind": "tool_call_recorded",
  "workspace_id": "ws1",
  "content": "use std::sync::Arc;\n...",
  "arguments": { "path": "src/main.rs", "mode": "full" }
}
```

---

## Auth / Scopes (Team Server)

The team server enforces scope-based authorization per bearer token. Tokens are configured
in the team server JSON config with SHA-256 hashes.

### Token Configuration

```json
{
  "tokens": [
    {
      "id": "ci-readonly",
      "sha256Hex": "<lowercase hex of SHA-256(token)>",
      "scopes": ["search", "graph"]
    },
    {
      "id": "admin",
      "sha256Hex": "<lowercase hex of SHA-256(token)>",
      "scopes": ["search", "graph", "artifacts", "index", "events", "sessionMutations", "knowledge", "audit"]
    }
  ]
}
```

### Scopes

| Scope | Grants Access To |
|-------|-----------------|
| `search` | `ctx_read`, `ctx_multi_read`, `ctx_smart_read`, `ctx_search`, `ctx_tree`, `ctx_outline`, `ctx_expand`, `ctx_delta`, `ctx_dedup`, `ctx_prefetch`, `ctx_preload`, `ctx_review`, `ctx_response`, `ctx_task`, `ctx_overview`, `ctx_pack` (+ graph), `ctx_semantic_search` |
| `graph` | `ctx_graph`, `ctx_impact`, `ctx_callgraph`, `ctx_refactor`, `ctx_routes`, `ctx_pack` (+ search) |
| `artifacts` | `ctx_semantic_search` with `artifacts=true` |
| `index` | `ctx_graph` with `action=index-build*`, `ctx_semantic_search` with `action=reindex` |
| `events` | `GET /v1/events` SSE stream |
| `sessionMutations` | Shared session write operations |
| `knowledge` | Knowledge store read/write |
| `audit` | `GET /v1/metrics`, full-payload event access, audit log reads |

### Blocked Tools

The following tools are **never allowed** on the team server (no scope grants access):

- `ctx_shell` / `ctx_execute` — arbitrary command execution
- `ctx_edit` — file modification

### Scope Enforcement

1. **Endpoint-level**: `/v1/events` requires `events`, `/v1/metrics` requires `audit`.
2. **Tool-level**: each tool call is mapped to required scopes via `required_scopes()`.
   The request is allowed only if `required_scopes ⊆ token_scopes`.
   - `ctx_session` (mutating actions) → `sessionMutations`
   - `ctx_knowledge`, `ctx_knowledge_relations` (mutating actions) → `knowledge`
   - `ctx_artifacts` (mutating actions) → `artifacts`
   - `ctx_proof`, `ctx_verify` (mutating actions) → `search`
3. **MCP fallback**: `tools/call` JSON-RPC requests on the MCP transport are also
   scope-checked by parsing the request body in the auth middleware.

### Audit Log

Every tool call and endpoint access is logged to the configured `auditLogPath` as
newline-delimited JSON:

```json
{
  "ts": "2026-05-05T13:00:00+02:00",
  "tokenId": "ci-readonly",
  "workspaceId": "ws1",
  "tool": "ctx_read",
  "method": "/v1/tools/call",
  "allowed": true,
  "deniedReason": null,
  "argumentsMd5": "d41d8cd98f00b204e9800998ecf8427e"
}
```

---

## Server Configuration

### Local Server (`HttpServerConfig`)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `host` | string | `127.0.0.1` | Bind address |
| `port` | u16 | `8080` | Bind port |
| `auth_token` | string \| null | none | Bearer token (required for non-loopback) |
| `stateful_mode` | bool | `false` | MCP stateful session mode |
| `max_body_bytes` | usize | `2 MiB` | Max request body size |
| `max_concurrency` | usize | `32` | Max concurrent requests (semaphore) |
| `max_rps` | u32 | `50` | Token-bucket rate limit (requests/sec) |
| `rate_burst` | u32 | `100` | Token-bucket burst capacity |
| `request_timeout_ms` | u64 | `30 000` | Per-request timeout |

### Team Server (`TeamServerConfig`)

Extends the local server with multi-workspace support, token-based auth, and audit logging.
See `rust/src/http_server/team.rs` for the full config schema.

---

## Security

- **Non-loopback binding** requires `--auth-token` (local server) or configured tokens (team server).
- Bearer tokens are compared in **constant time** to prevent timing attacks.
- Team server tokens are stored as **SHA-256 hashes** — raw tokens never touch disk.
- Rate limiting and concurrency guards protect against resource exhaustion.
- Host header validation follows rmcp defaults (loopback-only) unless explicitly overridden.
