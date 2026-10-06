# A2A transport architecture

Status: partial V4 implementation; not a release or product-availability claim.
This document consolidates the transport boundary required by Master Phase 17.
The older [`a2a-contract-v1`](../contracts/a2a-contract-v1.md) remains a research
contract for local coordination and compatibility; it is not evidence that the
managed cross-machine lifecycle is complete.

## Scope

The transport moves bounded, authenticated envelopes between configured peers.
Phase 17 requires signed task transport, authentication, send/get/cancel,
task/context transfer, replay protection, relay, rate limits, DLQ, remote
transport, tenant/project isolation, and an end-to-end proof.

## Components

| Boundary | Runtime responsibility |
|---|---|
| `core/a2a_transport.rs` | Versioned envelope, content type, sender/recipient and hop integrity |
| `core/a2a/relay.rs` | Origin and hop records, route/hop bounds, scope and peer policy |
| `core/a2a/remote_transport.rs` | Bounded remote HTTP delivery and unverified task-response carriage |
| `core/a2a/task.rs` | Signed send authority, task scope/state and trust policy |
| `core/a2a/task_control.rs` | Separate signed get/cancel request transcript |
| `core/a2a/task_response.rs` | Request-bound signed status response and origin-side verification |
| `http_server/handlers.rs` | Authentication, policy, replay reservation, local handoff or forwarding |
| `http_server/relay_replay*.rs` | Durable bounded replay proof and exact task-response cache outside reactor threads |
| `http_server/relay_rate.rs` | Peer/origin/tenant/project quota enforcement |
| `core/a2a/dlq.rs` | Bounded retry evidence for eligible non-task forwarding failures |

## Authority order

An inbound relay request is processed in this order:

1. Resolve configured peer and verify bearer credentials.
2. Parse the bounded envelope and relay record.
3. Verify envelope/hop signature, origin signature, final recipient, content
   type, tenant/project scope, classification, route and payload policy.
4. Charge quota only after authentication and policy checks.
5. Derive a path-safe storage ID and an origin-bound fingerprint.
6. Reserve the durable replay proof before any local or downstream effect.
7. Execute local handoff outside the async reactor or forward to the configured
   next peer.
8. Commit completion only after a successful outcome. For task responses, cache
   the exact non-empty UTF-8 bytes (maximum 64 KiB) atomically with completion.

Authentication metadata alone never grants task authority. Send, get and cancel
use signed task-specific transcripts and configured trust/grant checks.

## Replay and response semantics

The SQLite replay store binds each proof to storage ID, origin, tenant, project,
fingerprint, signed expiry, random lease and state. Pending reservations never
expire automatically: after an ambiguous effect, retry must not create a second
effect. Recovery requires explicit reconciliation.

Completed task retries return cached bytes only after the same authenticated
scope and fingerprint match. A legacy Completed proof without response returns
an availability error and never re-executes the task. Relays preserve the final
recipient's response bytes and do not re-sign or claim to verify them.

The origin verifies the cached `SignedTaskStatusV1` against the exact current
request, including nonce and digest, plus current peer key, grant, scope,
revocation and expiry. Cache retention does not extend signature validity.

The database uses a bounded page count, private owner-only creation, no-follow
opens, rollback journal with full synchronization, exact schema validation and
an external establishment marker. Schema v2 migrates v1 proof rows and metadata
atomically while adding response ownership through a foreign key.

## Failure policy

- Invalid authentication, signatures, scope or policy fail before mutation.
- Capacity returns a bounded retry hint without deleting proofs.
- A task transport error or unusable downstream task response retains Pending,
  because the downstream effect may already exist.
- Existing non-task evidence/context behavior keeps its release and DLQ path;
  task ambiguity rules do not silently change that contract.
- SQLite, body-stream and blocking-worker failures return generic public errors;
  internal paths, leases and stored bodies are not disclosed.
- Cancellation of an HTTP waiter does not release the concurrency permit while
  its blocking handoff worker is still running.

## Current verification

- Response-store schema/migration/security review: scoped runtime/security PASS;
  reported test-lint issue subsequently corrected.
- Store gate: 31 tests plus strict Clippy for all three store test targets,
  formatting and diff checks passed (`c72ee6f40e580959`).
- Integrated transport gate: 49 HTTP and 31 store tests, all-features Clippy,
  formatting and diff checks passed (`a70ad83ebf533d69`).
- A later full-library run passed 10,625 tests and failed one unrelated RTK
  shadow-adapter assertion; therefore it is not a green release gate.

These are local worktree results. They do not prove an accepted commit, merge,
deployment, cross-platform package, or full Phase 17 lifecycle.

## Open acceptance requirements

1. Integrate an accepted existing-only P18 identity signer. The receiver must
   hold its admission lease through signing and reject missing, legacy-schema or
   tenant-mismatched identities; it must never bootstrap a key for remote use.
2. Wire managed callers through signed send, authenticated get, authorized
   cancel and a later get. Carry the signed response through HTTP, transport and
   gateway to the origin verifier.
3. Connect the project-scoped task store to the actual executor. Distinguish
   `cancel_requested` from confirmed execution stop.
4. Prove real HTTPS origin→relay→recipient behavior with first-response loss,
   retry, process restart and byte-identical origin verification without a
   second effect.
5. Cover foreign owner/scope, revoked key/grant, tampered/expired request,
   changed payload, missing signer and capacity/recovery paths without data
   disclosure or unauthorized mutation.
6. Complete independent integration review, full library/Clippy/format gates,
   cross-platform coverage and release integration.

Phase 17 is complete only when all eleven master-prompt items and this end-to-end
acceptance chain are proven together. Passing replay-store tests is necessary,
not sufficient.
