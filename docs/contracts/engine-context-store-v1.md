# Engine Context Store reads v1

`lean-ctx engine context-lineage --project-root ROOT --json-file FILE` and
`lean-ctx engine context-policy-evidence --project-root ROOT --json-file FILE`
are additive, read-only local process boundaries over the Context Store
(`core::context_store`). They are not authenticated remote endpoints and they
never write, plan, learn or contact a runtime.

## Request

`ContextStoreRequestV1` (at most 16 KiB, unknown fields rejected):

| Field | Meaning |
|---|---|
| `schema_version` | `1` |
| `transport_version` | `1` |
| `engine_interface_version` | `"1.0.0"` |
| `project_id` | optional; defaults to `--project-root` as text, the same default `lean-ctx autopilot` uses |
| `tenant_id` | optional tenant of the scope |
| `task_id` | required for `context-lineage`, refused for `context-policy-evidence` |

Every read is confined to the tenant/project scope the request names. Equal
task IDs in another project or tenant are never joined; a ledger entry whose
task envelope proves another scope is reported, not returned.

## Response

Both operations answer `schema_version`, `transport_version` and
`engine_interface_version` (echoed), plus exactly one body:

- `context-lineage` → `lineage`: the task's execution-ledger steps joined with
  the Decision Receipts indexed for that task in that scope, its outcome, and
  `gaps` naming every missing link in plan → delivery → outcome. An incomplete
  record never reads as complete; an unreadable ledger is `ledger_error`.
- `context-policy-evidence` → `evidence`: `ContextPolicyEvidenceV1`
  (`lean_ctx_protocol::context_policy_evidence`), the scope's content-free
  read-strategy evidence per workload and UTC day. Quality, security and
  runtime signals are `unmeasured` wherever any delivery of a task was not
  measured; saved strategy evaluations are attached under `evaluations`.
  Evaluations are suite results of this machine, not of the scope: every
  scope's evidence carries the same ones.

The project root is checked like every other operation's (`unsafe_root`)
even though the scope is named by `project_id`.

Neither body carries source text, prompts, paths of delivered content or
credentials. Errors use the engine CLI's stable codes (`invalid_request`,
`unsupported_*_version`, `context_store_unavailable`).

## Status

Experimental with the v4 Engine. The bodies follow their own schema versions
(`LINEAGE_SCHEMA_VERSION`, `CONTEXT_POLICY_EVIDENCE_VERSION`); additive fields
may appear before the release that publishes this surface.
