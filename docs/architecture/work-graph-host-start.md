# Work Graph: the host starts, LeanCTX controls

LeanCTX does not launch agents. Starting, scheduling and supervising agent
processes belongs to the host (Claude Code, Codex, Cursor, a CI runner, …).
LeanCTX binds a host-launched child agent to one Work Graph node, controls what
context that child may receive and records how the node ended.

Until 3.11.5 `ctx_work_graph action=execute` started a Codex CLI child itself
(process supervision, cancellation signals, one retry, usage parsed from CLI
output). It was removed; `action=execute` now answers with this path.

## Lifecycle

```text
create ─► delegate ─► start ─► host launches child ─► receipt | cancel | lease expiry
                        │                                  │
                        └─ launch env (delivery profile) ──┘ every end revokes delivery
```

1. **delegate** queues a child node for a registered agent, with budget,
   task lineage and optional `path_claims`.
2. **start** (caller: the node's owner or the owner of its direct parent):
   - checks the project policy (`ctx_work_graph` must be allowed);
   - claims the node for a bounded window (`timeout_ms`, 1 ms–30 min, default
     5 min) and returns a new `execution_fence` for this incarnation; an
     expired earlier incarnation is stopped first;
   - acquires the node lease and every path lease in the shared lease
     registry (owner: the child agent, request reference: the fence);
   - reserves the signed context capsule queued for the child, verifies it
     against the root agent's pinned key and against the node (task, policy,
     outcome, chain, parent capsule, allowed agent, token/cost budget, window),
     then acknowledges it as handed over;
   - opens delivery attempt 1 and, when the host is configured as delivery
     issuer, issues the child's delivery profile (below).

   Any failure after the claim stops the node, releases its leases and returns
   the capsule to the queue.
3. The **host launches the child** with `launch.env` applied and every name in
   `launch.unset_env` removed from the child's environment.
4. **receipt** (node owner or delegating host) ends the node with the fence,
   an outcome and the consumed tokens/cost. These figures are reported by the
   caller (`usage_source: "reported"`) and stay bounded by the node and chain
   budgets. Reported figures never free budget: the parent keeps the child's
   full allocation reserved, so under-reporting cannot be delegated again.
   **cancel** stops the node and its descendants; exhausting the node budget
   stops it too. A lease that expires is stopped on the next claim.

Every end revokes delivery at once and frees the node's concurrency slot.
LeanCTX cannot see whether the host has ended the child process, so:

- after a **receipt** (the child reports it is done) the node and path leases
  are released immediately;
- after a **stop** (cancel, operator cancel, budget exhaustion, lease expiry)
  the child's paths stay exclusive and its leases stay held until the window
  granted at start ends. The host must end the child process on a stop; the
  window bounds how long a child it fails to end can keep its paths.

A start that fails before it returns a launch stops the node and releases its
leases at once: no child process exists.

`claim` remains the lightweight path for coordination without a capsule or
delivery profile; a claimed node finishes with a fenced receipt the same way.

## Delivery profile

The child reaches shared context through the scoped delivery endpoint
(`/ocla/v1/delivery/scoped`). It signs each request with the key of its own
agent identity and carries a `DeliveryDelegationV1` certificate that the host
signed. The certificate binds graph, node, fence, task id and attempt, the
project root, tenant/project scope, privacy, read/write grants, the child key
and an expiry no later than the node lease.

The server derives authority only from the persisted Work Graph
(`WorkGraphStore::child_delivery_authority`): the node must be active, started
and inside its lease, and fence and attempt must match. A receipt, cancel or
lease expiry therefore revokes the child's delivery immediately, regardless of
whether the child process is still running.

The host enables issuance by setting `LEAN_CTX_DELIVERY_ISSUER_PROFILE` for its
own LeanCTX process (host agent, host key and grant ids, recipient, tenant,
project, privacy, write permission, authority file). Without it,
`launch.env.LEAN_CTX_DELIVERY_PROFILE` is `{}`: an explicit empty profile that
disables delegated delivery, so a child never inherits the parent's profile.
`LEAN_CTX_DELIVERY_ISSUER_PROFILE` is always listed in `unset_env`. A `start`
from any agent other than the configured host agent is refused while that
variable is set, so a child whose host ignored `unset_env` cannot sign
delivery profiles as the host.

## Start response

```json
{
  "graph": { "…": "full graph state" },
  "launch": {
    "graph_id": "release",
    "node_id": "fix-tests",
    "agent_id": "worker-7",
    "execution_fence": "fence:…",
    "task_id": "work-execution:…:attempt-1",
    "lease_expires_epoch_ms": 1760000000000,
    "delegated_delivery": true,
    "env": { "LEAN_CTX_DELIVERY_PROFILE": "{…}" },
    "unset_env": ["LEAN_CTX_DELIVERY_ISSUER_PROFILE"],
    "finish": "action=receipt with this execution_fence, or action=cancel; the lease ends at lease_expires_epoch_ms"
  }
}
```

## Evidence

- `tools::ctx_work_graph::lifecycle_tests::host_started_child_gets_scoped_delivery_until_the_receipt`
  launches a real child process with the returned environment; the child
  obtains scoped context over HTTP, and after the receipt the certificate no
  longer authorizes delivery and the node lease is free.
- `core::work_graph_delegation::tests` cover issuance (live grant, current
  persisted attempt, privacy, write grant, project binding, revocation by stop)
  and capsule scope.
- `http_server::scoped_delivery` tests reject a stale attempt, a foreign
  certificate and tampered requests.
