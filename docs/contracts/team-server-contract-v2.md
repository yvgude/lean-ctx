# Team Server Contract v2

GitLab: `#2331`, `#387`, `#388`  
Pillar: Context Delivery  
Scope: workspaces, scopes, audit log, dot-path rewrite, hosted-storage quota, ROI webhook, managed connectors

> v2 is **additive** over [v1](team-server-contract-v1.md): every v1 guarantee
> holds unchanged. v2 adds the optional `storageQuotaBytes` and `roiWebhookUrl`
> config keys (the v1 file is frozen; this file is the live, stable surface).

## Config (`TeamServerConfig`)

File is JSON.

- `host` (string, required)
- `port` (number, required)
- `defaultWorkspaceId` (string, required)
- `workspaces` (array, required; must include default workspace)
  - `{ id, label?, root }`
- `tokens` (array, required for serve)
  - `{ id, sha256Hex, scopes?, role? }`
  - `sha256Hex` is lowercase hex SHA-256 of the plaintext token
  - `scopes` and/or `role` (see [RBAC roles](#rbac-roles)); a token must yield at
    least one effective scope
- `auditLogPath` (path, required)
- `disableHostCheck` (bool, default false)
- `allowedHosts` (string[], default [])
- `maxBodyBytes` (number, default 2097152)
- `maxConcurrency` (number, default 32)
- `maxRps` (number, default 50)
- `rateBurst` (number, default 100)
- `requestTimeoutMs` (number, default 30000)
- `statefulMode` (bool, default false)
- `jsonResponse` (bool, default true)
- `storageQuotaBytes` (number, optional, v2 / GL #387) — hosted-storage quota;
  omitted ⇒ Team-tier 5 GiB default, `LEANCTX_TEAM_STORAGE_QUOTA_BYTES`
  overrides both
- `roiWebhookUrl` (string, optional, v2 / GL #388) — https-only
  Slack/Discord/generic webhook; when set, the server posts a weekly team-ROI
  summary (once per ISO week, real reported numbers only; state in
  `savings/roi_webhook_state.json`). A non-https URL is a startup error.
- `connectors` (array, optional, GL #281) — managed source syncs; each entry is a
  `ConnectorConfig` (see [Managed Connectors](#managed-connectors-281)). Omitted ⇒
  the feature is off and the scheduler never starts.

## Workspace selection

Workspace is selected deterministically via:

1. Header `x-leanctx-workspace` (if present and valid)
2. Otherwise `defaultWorkspaceId`

`POST /v1/tools/call` also accepts `workspaceId` in the JSON body (takes precedence over the header for that call).

## Dot-path rewrite (`rewrite_dot_paths`)

For arguments keys `path`, `target_directory`, `targetDirectory`:

- if value is `""` or `"."`, it is rewritten to the workspace root path before executing the tool.

## Scopes

Scope enforcement is tool/action-aware. Tokens must include required scopes for the requested tool.

| Scope | Grants Access To |
|-------|-----------------|
| `search` | `ctx_read`, `ctx_multi_read`, `ctx_smart_read`, `ctx_search`, `ctx_tree`, `ctx_outline`, `ctx_expand`, `ctx_delta`, `ctx_dedup`, `ctx_prefetch`, `ctx_preload`, `ctx_review`, `ctx_response`, `ctx_task`, `ctx_overview`, `ctx_pack`, `ctx_semantic_search`, `ctx_proof`, `ctx_verify` |
| `graph` | `ctx_graph`, `ctx_impact`, `ctx_callgraph`, `ctx_refactor`, `ctx_routes`, `ctx_pack` |
| `artifacts` | `ctx_artifacts`, `ctx_semantic_search` with `artifacts=true` |
| `index` | `ctx_graph` with `action=index-build*`, `ctx_semantic_search` with `action=reindex` |
| `events` | `GET /v1/events` SSE stream |
| `sessionMutations` | `ctx_session` (mutating: `save`, `set_task`, `task`, `checkpoint`, `finding`, `decision`, `reset`, `import`), `ctx_handoff`, `ctx_workflow`, `ctx_share` |
| `knowledge` | `ctx_knowledge` (mutating: `remember`, `feedback`, `remove`, `consolidate`), `ctx_knowledge_relations` (mutating: `relate`, `unrelate`) |
| `audit` | `GET /v1/metrics`, `GET /v1/connectors`, full-payload event access, audit log reads |

Errors:

- `401 unauthorized` (missing/invalid token)
- `403 scope_denied` (token lacks required scopes)
- `400 unknown_workspace`

## RBAC roles

A token's effective scopes are `scopes ∪ role.scopes()`. Roles (EPIC 13.2) are an
ergonomic layer over scopes — assign a coarse role instead of hand-picking
scopes. Enforcement is unchanged (the middleware evaluates effective scopes).

| Role | Effective scopes |
|------|------------------|
| `viewer` | `search` |
| `member` | `search`, `graph`, `index`, `knowledge`, `events` |
| `admin` | all scopes |
| `owner` | all scopes (org/billing authority is a hosted control-plane concern) |

Roles are monotonic: `viewer ⊆ member ⊆ admin = owner` (server scopes). Create
with `lean-ctx team token create --role <viewer|member|admin|owner>` (or
`--scopes <csv>`, or both).

> Additive / Team-Cloud plane only — never gates the local plane. SSO/SCIM,
> org-shared knowledge graph, and audit-retention dashboards build on this role
> model and are tracked on the commercial plane (EPIC 13.2).

## Managed Connectors (#281)

A *connector* is a scheduled, in-process sync from an external source into a
workspace's long-term stores (BM25 + graph + knowledge). Once a connector has
run, `ctx_semantic_search` and `ctx_knowledge` surface the source's issues / PRs
/ pipelines to every seat — no per-call credential transport, no manual
`ctx_provider` invocation.

### `ConnectorConfig`

Each entry of `connectors[]` (camelCase JSON):

- `id` (string, required) — stable, file-safe, unique within the instance.
- `provider` (string, required) — `gitlab` | `github`.
- `resource` (string, required) — gitlab `issues|merge_requests|pipelines`;
  github `issues|pull_requests|actions`.
- `project` (string, optional) — `group/project` (GitLab) or `owner/repo` (GitHub).
- `host` (string, optional) — GitLab host (default `gitlab.com`) or GitHub API
  base (default `https://api.github.com`).
- `state` (string, optional) — provider state filter (e.g. `opened`).
- `limit` (number, optional, default 50) — max items per sync.
- `intervalSecs` (number, optional, default 3600) — sync cadence, **clamped to a
  300 s floor** to protect external APIs.
- `secret` (string, optional) — the provider credential. **Plaintext only inside
  the private `team.json`** (a control-plane-injected env var); it is never
  written to disk by the server and never returned by any endpoint.
- `workspaceId` (string, optional) — target workspace; the instance default when omitted.
- `displayName` (string, optional).
- `enabled` (bool, default true).

### Scheduler

A single background scheduler ticks once a minute, runs each *due* connector
(`now ≥ lastRun + effectiveInterval`) on the blocking pool, and records the
outcome per connector under `<state_dir>/<id>.json` (never the credential). When
the hosted index is **over quota** (GL #282) ingestion pauses (never deletes,
never gates reads); the connector records an `error` status and waits its
interval before retrying.

### `GET /v1/connectors`

Audit-scoped. Returns the **secret-free** roster + per-connector sync status
(`lastRunAt`, `lastStatus` `ok|error`, `lastError`, `lastItemCount`,
`totalRuns`, `totalItems`, `hasSecret`). The credential is never exposed.

## Audit log (JSONL)

Audit log is JSONL; one object per line:

- `ts` (RFC3339)
- `tokenId`
- `workspaceId`
- `tool`
- `method`
- `allowed` (bool)
- `deniedReason` (string|null)
- `argumentsMd5` (string; MD5 of canonicalized arguments JSON)

Raw arguments are never stored in the audit log.

## Implementation

- `rust/src/http_server/team.rs`
- CLI dispatch: `rust/src/cli/dispatch.rs` (`lean-ctx team ...`)
