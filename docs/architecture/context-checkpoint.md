# Context Checkpoint

Status: canonical V4 live-state contract is specified here; existing session
snapshot and shadow-Git checkpoint implementations are partial projections, not
the complete `ContextCheckpointV1`.

## Purpose

`ContextCheckpointV1` is the portable semantic state needed to continue work
across process, session, device or authorized workspace boundaries. It preserves
meaning and lineage, not a machine image or raw working directory.

It is distinct from:

| Format | Canonical purpose |
| --- | --- |
| Session Bundle | Bounded transfer of one session. |
| Handoff Bundle | Point-to-point agent/task handoff. |
| `.ctxpkg` | Installable, versioned context asset. |
| Context Snapshot | Signed point-in-time evidence/timeline projection. |
| Context Checkpoint | Mutable-lineage live continuation state. |
| Shadow-Git checkpoint | Local code-change snapshot used for diff/restore; not semantic continuation state. |

Adapters may project shared typed fields, but these formats do not become
synonyms and do not maintain independent copies of the same semantic authority.

## Canonical contract

A checkpoint contains or references:

- schema version, checkpoint ID and optional parent checkpoint ID;
- device identity and monotonic device sequence;
- project, optional workspace and tenant identity;
- canonical active task, parent/trace lineage and outcome contract;
- progress, accepted decisions, findings and next steps;
- bounded handoff and relevant logical-session state;
- knowledge and gotcha object references/deltas;
- typed learning-state references/deltas;
- Context IR and compiled context references;
- execution-plan, evidence and receipt references;
- signed context-snapshot and hosted-index digest/reference;
- exact policy/profile and entitlement identity;
- engine/schema versions and bounded creation/expiry times;
- encryption envelope metadata; and
- canonical digest and signature.

Large content is content-addressed and referenced. A reference never grants
read authority by itself. Every dereference rechecks task, identity, workspace,
classification, region, entitlement and retention policy.

## Forbidden state

A portable checkpoint does not sync:

- credentials, API keys, cookies or private signing keys;
- raw provider cache handles or opaque provider request state;
- PIDs, process handles, locks, sockets or local leases;
- absolute machine paths where avoidable;
- temporary directories or process-local live-zone state;
- transient flush/heartbeat timestamps; or
- raw repository, prompt, response or tool content by default.

Machine-local data is reconstructed through trusted adapters. If an unavoidable
path-like reference is required, it is an approved opaque project-relative or
content-addressed reference, never ambient authority.

## Lineage and state transition

Every checkpoint binds to `TaskEnvelopeV1` and the applicable ContextPlan,
ExecutionPlan and receipt chain. A new checkpoint has one parent in its lineage
and a strictly advancing device sequence. Identical canonical content produces
an identical digest but does not permit checkpoint-ID reuse across incompatible
identity or policy scopes.

Suggested lifecycle:

```text
captured → validated → encrypted/signed → stored
       → pulled → verified → resumable | degraded | inspect-only | corrupt
```

Only `resumable` or explicitly policy-approved `resumable_with_degradation`
may continue execution. `inspect_only` and `corrupt` never become executable by
dropping invalid fields.

## Conflict and merge model

Semantic state never uses silent whole-document last-writer-wins. When two
devices branch from the same parent, both checkpoints remain addressable.

Typed merge rules are:

| State | Merge rule |
| --- | --- |
| Knowledge/gotchas | Stable object IDs plus version, validity and provenance merge; surface contradictions. |
| Learning | Only mathematically valid, typed and idempotent merge operations. |
| Receipts/evidence | Append-only, verify first, deduplicate by canonical ID/digest. |
| Hosted index | Immutable digest-addressed bundles; rebuildable projections never override source truth. |
| Decisions/policy | Preserve both when authority or version differs; current hard policy decides admissibility. |
| Active task/session | Preserve divergent branches; never field-merge incompatible active work. |
| Next steps/progress | Deterministic union only when IDs and task lineage agree; otherwise require explicit selection. |

The resolver records chosen parents, merge algorithm/version, rejected inputs,
conflicts, policy decision and resulting digest. User or authorized agent choice
is required when no safe deterministic merge exists.

## Encryption and device sync

Protected checkpoints use a versioned device-aware envelope. An account/vault
key is wrapped independently for registered devices; plaintext key material is
never sent to the server. Device revocation and rotation prevent future access
and trigger explicit rewrap/recovery policy without pretending already-copied
plaintext can be recalled.

The server stores ciphertext for protected content. Signatures cover identity,
lineage, sequence, policy/profile refs, ciphertext digest and encryption
metadata. Tamper, rollback, replay, wrong-device, revoked-device and
cross-workspace cases fail closed.

## Product boundaries

### Community

Community retains local/manual snapshots, session export/import and handoff.
Local shadow-Git code checkpoints remain a developer safety tool; they are not a
hosted continuity promise.

### Pro

Pro adds automatic local semantic checkpoints and encrypted personal
cross-device push/pull, knowledge/gotcha/learning continuity, history, conflict
handling and recovery for one account.

### Team

Team adds `WorkspaceCheckpoint`, authenticated shared authority, membership,
roles, seat enforcement, personal-to-workspace promotion and shared conflict
resolution. A personal checkpoint never becomes shared implicitly.

### Enterprise

Enterprise adds governed identity, RBAC, classification, region, retention,
legal hold, audit, customer-controlled deployment and offline/air-gap policy.

## Current implementation map

| Surface | Current role and limitation |
| --- | --- |
| `lean-ctx-protocol::ContextSessionSnapshotV1` | Portable task/session identity, pinned configuration and lifecycle/recovery projection. It lacks the complete checkpoint contents, ancestry/device sequence, signature and encryption envelope. |
| `ContextSessionStateV1` | Revision, next event sequence, plan/receipt/checkpoint digest and recovery state; explicitly does not replace the append-only event log. |
| `ctx_checkpoint` and `core::git::shadow` | Local code snapshot/log/diff/restore in isolated shadow Git; not `ContextCheckpointV1`. |
| Session/Handoff/Context Snapshot/`.ctxpkg` | Adjacent conversion sources/targets that retain their own purposes. |
| Personal Cloud | Candidate encrypted storage/sync substrate; full semantic checkpoint push/pull and conflict proof are still required. |

No exact `ContextCheckpointV1` type exists in the currently searched Rust
sources. `ContextSessionSnapshotV1` must be reused or extended through canonical
typed projections rather than duplicated blindly.

## Reliability and security invariants

- Serialization and signatures are deterministic for canonical inputs.
- IDs, counts, byte/token sizes, history, ancestry depth and timestamps are
  bounded.
- Parent ancestry and device sequence prevent silent rollback and replay.
- Save is atomic; interrupted writes leave the prior valid checkpoint readable.
- Push/pull is idempotent and deduplicated by ID/digest.
- Current policy and identity are re-evaluated on resume.
- Missing referenced content produces degraded/inspect-only state, not invented
  content.
- Unknown schema, invalid signature, digest mismatch or unauthorized scope fails
  closed.
- Telemetry carries only approved aggregates and never raw checkpoint content,
  paths, prompts, secrets or evidence.
- Community/manual fallback remains usable when paid sync is disabled.

## Migration and acceptance gates

1. Define the versioned canonical type in the protocol source of truth with
   schema, fixtures and deterministic signing bytes.
2. Map every existing session, handoff, `.ctxpkg`, snapshot and shadow
   checkpoint field to canonical owner, adapter or explicit non-equivalence.
3. Prove atomic local capture/resume and encrypted two-device push/pull.
4. Prove branch preservation, typed merge, deterministic conflict selection and
   rollback/replay rejection.
5. Prove device registration, rotation, revocation, recovery and wrong-device
   denial without plaintext server storage.
6. Team proves personal-to-workspace promotion and shared checkpoint access with
   membership, roles and seats.
7. Enterprise proves policy, retention, legal hold, audit and supported offline
   deployment behavior.
8. Migration from representative v3 state is idempotent, backed up, reversible
   where practical and explicit about non-migratable data.
9. Packaging, cross-platform, security, telemetry, entitlement and licensing
   gates pass.

Until those gates pass, current session snapshots and shadow checkpoints are
partial substrate, not a complete continuity or synchronization product.

## Related architecture

- `docs/architecture/decision-spine.md`
- `docs/architecture/subagent-contract.md`
- `docs/architecture/agent-identity.md`
- `docs/architecture/cross-agent-delivery.md`
- `docs/architecture/team-context.md`
