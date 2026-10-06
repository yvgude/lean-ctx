# Local Work Graph observation

Controllers use the existing `ctx_work_graph` execution and authorization path.
Observation does not create a second scheduler, execute a worker, or accept its
result. A registered agent can observe only graphs it belongs to in its project;
the public tool also requires the local Work Graph entitlement.

```json
{"action":"observe","graph_id":"release-v4"}
```

The response includes `schema_version`, `graph_id`, `revision`, `unchanged`,
`max_concurrency`, and compact `nodes`. Nodes expose ownership, parent linkage,
status, execution-start evidence, stop reason, accepted-path membership and
remaining/consumed token and cost budgets. Context capsules, result contents and
execution fences are intentionally omitted.

Retain the returned revision and pass it on the next relevant check:

```json
{"action":"observe","graph_id":"release-v4","if_revision":"<returned 64-character revision>"}
```

If the visible state has not changed, the response omits the node list and returns
`unchanged: true`. Keep the previous display. Otherwise replace it with the new
snapshot and revision. The revision is a content hash, not an access credential;
authorization runs even on cache hits. It covers the observation, not hidden
graph details. Controllers needing those details must use the authorized detail
and receipt APIs.

Poll at task boundaries or a bounded refresh interval, not in a tight loop.
An unchanged response is not a reason to start a new worker or repeat a blocker
message. Respect execution leases and terminal states. The concurrency limit
governs execution, not the number of registered agents.

## Reading status correctly

- A connected MCP transport is not proof of an executing model turn.
- `execution_started` records a start; it does not prove the process is live now.
- Accepted work is distinct from a worker reporting completion.
- Observed token consumption is accounting, not a measured savings claim.
- Dashboard `pending_messages` excludes expired, read and recipient-less history;
  `retained_messages` describes retained history separately.

This API supplies a compact observation surface. A visual controller must still
bind its user/session authority to project and graph membership, and use existing
claim, execute, cancellation and receipt verification paths for mutations.
