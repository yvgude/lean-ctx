# Task receipt v1

Status: Experimental · Since: 3.12 · Schema id: `leanctx.task-receipt/v1`

A task receipt is the engine side of the per-task receipt of a finished
board task (LeanCTX Code launch criterion h8e, C7 and C8): which context
LeanCTX delivered to which agent for the task, under which policy, with
references into the local hash chains that a third person can check without
trusting any chat.

The caller (the vault of LeanCTX Code) names the task and the session of
each of its stages. LeanCTX answers per session. The board-side facts of the
receipt — stages, roles, MR and merge commit, fact ids, handover, reviewer
verdicts — stay vault data; this document only adds what the engine
recorded.

Receipts are free for everyone: there is no plan check and no account.

## Request

```
lean-ctx task receipt <task> --json --session <id> [--session <id> …]
GET /v1/local/task/<task>/receipt?session=<id>&session=<id>
```

Both answer the same bytes (local read contract `local-read/1`, kind
`task.receipt`).

| Input | Rule |
|---|---|
| `<task>` | required; 1–128 of `[A-Za-z0-9._:-]` (the board task id), no surrounding whitespace |
| `--session` / `session` | required, repeatable, 1–64 distinct ids of the same shape; duplicates are dropped, order is kept |

A session id is resolved as in `session.value`: the engine session with that
id, or every engine session that carries it as its app session id, and the
savings-ledger and audit-trail entries recorded under it. The engine records
those entries under the engine session id, so a session's evidence is every
entry recorded under the requested id or under the id of one of its engine
sessions, first and last in chain order.

Each store is read once per receipt, whatever the number of sessions: the
saved sessions, one pass and one verification of each chain, and one
verified read of the execution ledger. At most 256 MCP tasks are resolved
per receipt (in request order of the sessions). While this process is still
building a receipt — for example after a socket timeout ended the response
but not the work — a further request fails with `unavailable`; retry later.

## Response

```json
{
  "contract": "local-read/1",
  "kind": "task.receipt",
  "receipt_schema": "leanctx.task-receipt/v1",
  "task": "418",
  "chain_broken": false,
  "sessions": [
    {
      "session_id": "s1",
      "engine_sessions": 1,
      "chain_broken": false,
      "ledger": { "intact": true, "entries": 120, "first_invalid_at": null },
      "audit": { "intact": true, "entries": 40, "first_invalid_at": null },
      "ledger_evidence": { "entries": 12, "first_hash": "…", "last_hash": "…" },
      "audit_evidence": { "entries": 0, "first_hash": null, "last_hash": null },
      "tokens": { "baseline": 52000, "saved": 31000, "bounced": 0 },
      "security": { "…": 0 },
      "agents": [ { "agent_id": "agent-a", "client": "claude-code" } ],
      "mcp_tasks": [
        {
          "task_id": "mcp-task-…",
          "outcome": "accepted",
          "deliveries": [
            {
              "digest": "sha256:…",
              "verified": true,
              "error": null,
              "summary": {
                "outcome": "delivered",
                "delivery_semantics": "returned_to_host",
                "destination": "mcp-host",
                "policy_digest": "sha256:…",
                "inspected": 3, "delivered": 2, "withheld": 1, "redactions": 0,
                "tokens_original": 900, "tokens_delivered": 300,
                "final_context": "sha256:…"
              }
            }
          ],
          "gaps": []
        }
      ],
      "gaps": []
    }
  ],
  "gaps": []
}
```

| Field | Meaning |
|---|---|
| `receipt_schema` | exactly `leanctx.task-receipt/v1` |
| `task` | the task id the caller named |
| `chain_broken` | any session's chain failed verification (C8 fails) |
| `sessions[]` | one per requested session, in request order |
| `.engine_sessions` | engine sessions found for the id |
| `.chain_broken` | the savings ledger or the audit trail failed verification |
| `.ledger`, `.audit` | whole-chain verification: entry count, intact, index of the first invalid entry (the same for every session) |
| `.ledger_evidence`, `.audit_evidence` | count and first/last `entry_hash` of the chain entries recorded under the session id or its engine sessions' ids |
| `.tokens` | savings-ledger sums of the session |
| `.security` | security counts of the session from the audit trail |
| `.agents[]` | distinct agent id / MCP client pairs from the session's evidence, sorted |
| `.mcp_tasks[]` | the engine MCP tasks the session's evidence names, sorted, at most 64 per session and 256 per receipt |
| `..deliveries[]` | governed deliveries from the task's Decision Receipts; `verified` means the stored receipt matched its digest, `error` is `missing`, `unreadable` or `tampered` otherwise |
| `..summary.policy_digest` | digest of the policy the delivery was decided under |
| `..summary.final_context` | digest of the context that left LeanCTX |
| `..gaps` | missing links of the MCP task's lineage (as `lean-ctx inspect --task`) |
| `.gaps`, `gaps` | sorted gap ids per session and for the whole receipt |

Session gaps: `no_engine_session`, `no_ledger_evidence`,
`no_context_delivery`, `delivery_unverified`, `mcp_tasks_truncated`. An MCP
task without a project root carries `no_project_scope`.

* `no_ledger_evidence`: no savings-ledger entry of the session counts; an
  entry with neither saved nor bounced tokens does not count.
* `mcp_tasks_truncated`: the session names more MCP tasks than were
  resolved, by the per-session or the per-receipt cap. Such a session never
  carries `no_context_delivery`, since an unresolved task may hold one.

## Guarantees

* Content-free: ids, counts, digests and chain hashes only. No path, file
  content, prompt or timestamp.
* Deterministic: the bytes are a function of the local stores and the
  request; there is no clock or counter in the body.
* Computed on demand from the local chains and receipt stores; nothing is
  written. A third person checks it by re-walking the named chains (or
  `lean-ctx proof --session <id>`) and looking up the named hashes.
* Agent ids and clients are as recorded in engine evidence; they are not
  attested identities.
