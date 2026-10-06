# Stigmergic Coordination

Status: V4 target architecture with a local Scent Field substrate; product
tiering, durable Team projection and failure-mode acceptance remain incomplete.

## Purpose

The Scent Field lets agents coordinate through small, decaying signals instead
of reading message payloads. It reduces duplicate work and exposes contention,
completion, friction and avoidance hints without becoming a second message bus,
task store or lease authority.

This is one of five distinct Agent Bus coordination styles. Messages carry
explicit communication; scents carry ambient hints; leases authorize temporary
ownership; Delivery Registry coordinates context reuse; Work Graph owns bounded
execution.

## Signal model

The current local implementation defines five signal kinds:

| Signal | Meaning | Current half-life |
| --- | --- | --- |
| `claimed` | Agent is actively working on a target. | 10 minutes |
| `hot` | Target is receiving active attention. | 10 minutes |
| `stuck` | Agent encountered friction or a blocker. | 30 minutes |
| `done` | Work related to the target completed. | 60 minutes |
| `avoid` | Target should not currently be touched. | 60 minutes |

Signals are keyed by agent, kind and normalized target. Repeated deposits
superpose with a bounded intensity. Effective intensity decays exponentially;
entries below the garbage-collection threshold disappear lazily. The sync view
groups equivalent signals, sorts strongest first and emits a bounded top set.

These constants describe the present local implementation, not an immutable
wire contract. A versioned Team or Enterprise projection must declare its own
bounds and compatibility rules.

## Claims versus leases

A scent claim is a coordination hint with conflict detection. It answers
"someone appears to be working here" and decays automatically. It does not
grant permission, fence writes, prove identity or replace a lease.

An Agent Lease is an explicit bounded ownership record over a typed resource.
It requires an owner and lease reference and enforces acquisition/release
semantics. Mutation safety uses leases or stronger repository/worktree controls;
Scent Field hints improve scheduling around that authority.

Rules:

- a conflicting active scent should stop speculative duplicate work;
- a valid lease is still required where the operation needs ownership;
- losing a scent never transfers a lease;
- lease expiry does not create a `done` signal;
- completion or abandonment releases the lease and updates scent state
  independently; and
- Team/Enterprise authorization is never inferred from local agent text or PID.

## Storage and identity

The current local field is a schema-versioned JSON document under the LeanCTX
agent data directory, protected by the same file-lock mechanism as the local
agent registry. Garbage collection occurs during locked operations; there is no
timer or daemon requirement.

When durable agent identity is unavailable, local scent attribution adds the
process ID to distinguish concurrent local processes. This is intentionally
ephemeral and must never become Team or Enterprise identity. Hosted/shared
scents reference authenticated durable agent and workspace IDs while retaining
separate ephemeral presence.

Current local persistence and lock behavior are implementation substrate. They
still require crash-consistency, corruption, capacity, multi-process and
cross-platform evidence before product acceptance.

## Product boundaries

### Community

Community retains manual handoff and open interoperability contracts. It does
not receive the complete live Scent Field product merely because historical
experimental code exists under Apache licensing.

### Pro

Pro includes the local Scent Field for one paying account using multiple local
agents. It may expose bounded claim, done, stuck, hot and avoid signals through
Agent Bus sync and dashboards. It must work without transmitting message or
source payloads.

### Team

Team adds durable workspace-scoped scents across users, devices and machines.
The shared projection requires membership, role and seat enforcement; bounded
retention; deterministic conflict resolution; reconnect/resume behavior; and no
cross-workspace leakage.

### Enterprise

Enterprise adds governed identity, RBAC, classification, region, retention,
audit and deployment policy. Organization policy may restrict which agents can
deposit or observe signals for a target. Audit records remain payload-free and
must not claim hardware attestation without measured evidence.

## Privacy and telemetry

Targets are metadata and may themselves be sensitive. Before any remote or
telemetry projection, normalize them to an approved opaque task/resource
reference or privacy-safe aggregate. Never send raw paths, prompts, message
payloads, source text, secrets or arbitrary task labels.

Allowed value reporting is aggregated and bounded, for example:

- rejected claims or duplicate work prevented;
- contention and friction counts by approved class;
- time-to-resolution buckets; and
- accepted-path coordination contribution.

These metrics cannot establish accepted outcome without the Outcome Evaluator.

## Reliability invariants

- Scent operations never break the primary read, execution or handoff path.
- All stores and rendered views have explicit size and output bounds.
- Ordering is deterministic for equal effective intensity.
- Corrupt, expired or unknown-version records fail safely and do not authorize
  ownership.
- Clock skew cannot create indefinite authority; Team protocols use bounded
  server-authoritative expiry or a declared reconciliation rule.
- Restart, crash and partition do not turn a hint into a permanent lock.
- `avoid` and `stuck` are advisory unless a separate policy/lease decision says
  otherwise.
- Every remote operation is authenticated and workspace-scoped.
- Telemetry and sync outputs remain content-free.

## Current implementation map

| Surface | Current role |
| --- | --- |
| `core::scent_field` | Local signal types, exponential decay, superposition, lazy GC, claim conflict and bounded sync rendering. |
| `ctx_agent claim/release/sync` | Agent-facing integration and side effects. |
| `core::agent_lease` | Separate typed ownership authority. |
| `core::agents` | Presence, messages, scratchpad and diary; not scent authority. |
| Delivery Registry | Separate content-reference and dedup coordination. |
| Work Graph | Separate execution ownership and scheduling. |

## Acceptance gates

1. Pro proves two supported local agents avoiding duplicate work through scent
   signals while leases continue to fence mutation.
2. Crash, corrupt-file, concurrent writer, stale PID, clock shift, capacity and
   decay behavior are tested on supported platforms.
3. Team proves authenticated shared scents across two users and machines with
   no cross-workspace access and deterministic reconnect behavior.
4. Enterprise proves policy denial, identity revocation, retention and
   payload-free audit.
5. Capability and entitlement gates match Community/Pro/Team/Enterprise
   packaging and every CLI/MCP/HTTP entry point.
6. Telemetry fixtures prove no raw target, path, prompt, message, source or
   secret leakage.
7. Downgrade and offline behavior preserve safe local operation and export
   without leaking hosted state.

Until these gates pass, `core::scent_field` proves a useful local primitive,
not a finished commercial coordination surface.

## Related architecture

- `docs/architecture/agent-bus.md`
- `docs/architecture/work-graph.md`
- `docs/architecture/cross-agent-delivery.md`
- `docs/architecture/subagent-contract.md`
- `docs/architecture/agent-identity.md`
