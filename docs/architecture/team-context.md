# Team Shared Context Architecture

Status: V4 target architecture. The contract is not evidence that Team is shipped.

## Boundary

Team adds a durable multi-user control plane above the Personal Cloud and Pro Local Agent Bus. Local agent identities, local SQLite coordination and caller-supplied workspace strings are never Team authorization sources. The Team service authenticates the human/service principal, resolves membership server-side and applies tenant-scoped policy before accessing encrypted or metadata state.

## State model

`Organization` owns `Workspace`; `Membership` binds a member to one workspace role; an `Invite` creates membership through a single-use expiring nonce whose preimage is at least 128 bits from a CSPRNG and is never stored. Shared objects use the same state primitives as personal context while adding immutable scope, classification, provenance, authority and validity metadata.

The lifecycle is:

`personal → team_candidate → authoritative → historical`

Promotion is an append-only request and authority decision. Supersession never deletes lineage. Concurrent or contradictory heads create an explicit conflict record whose winner is one of the two conflicting branches, never a third object; unresolved conflicts are excluded from authoritative retrieval. A workspace checkpoint commits a bounded, canonical set of object and receipt digests for recovery and audit, under its own `checkpoint_state` vocabulary that is deliberately disjoint from object authority.

Two lineage relations exist and are never mixed: `parent_digest` is the compare-and-swap predecessor of the same logical record, and `supersedes`/`superseded_by` express cross-object authority supersession. `version = 1` is the genesis record and carries no `parent_digest`; every later version carries its predecessor's record digest. No sentinel digest exists, so bootstrapping a workspace needs no implementation-invented constant and digest determinism holds across implementations.

Object/provenance backlinking deliberately avoids a mutual-hash fixed point: `Provenance.object_digest` hashes the complete validated `ContextObject` with only `provenance_digest` omitted, while `ContextObject.provenance_digest` hashes the complete validated `Provenance` record. Validators must recompute both directions and reject any mismatch.

## Trust and authorization

The server derives organization, workspace, member and seat entitlement from authenticated state. Every storage key and query includes tenant and workspace. Every write binds canonical request bytes, version, scope, actor, role, policy, parent digest and idempotency key to a signature or authenticated encryption context. Unknown fields, unknown versions, unavailable policy and unverifiable signatures fail closed.

Authority decisions, policies and receipts carry a detached Ed25519 signature envelope — algorithm, key id, key epoch, verification key, signed-payload digest and raw signature — referenced by record digest. A hash of a signature is not treated as evidence of one. An object reaches `authoritative` only with an approved, signed, same-scope authority decision linked from the object itself, and an approval whose decider equals the requester is rejected: non-self-approval is enforced as cross-record validation, since JSON Schema cannot express it.

Role minimums are owner/admin for membership administration, approver for authority decisions, member for candidate submission and viewer for reads. Role grants are scoped, timed and revocable records rather than a static table. Access leases are explicit records derived from a specific membership version and role-binding version; revoking either takes effect before subsequent reads or writes and invalidates the derived leases by appending a revoked lease version, so history is preserved rather than erased.

Canonical timestamps are UTC RFC 3339 with the `Z` designator only, asserted by pattern rather than by the annotation-only `format` keyword. Clock skew bounds are numeric: 300 s ahead for server-authored and client-supplied values, 900 s behind for client-supplied values. Deployments may narrow, never widen them.

## Privacy and observability

Team telemetry contains only versioned event kinds, tenant-safe pseudonymous IDs, bounded counts, latency classes, outcome classes and digests. It excludes context bytes, prompts, paths, credentials and invite tokens. Pseudonymous identity digests are tenant-keyed HMAC-SHA-256 values over normalized subject identifiers with a per-organization pepper, carried under a distinct `hmac-sha256:` prefix so an unsalted, dictionary-recoverable identity hash cannot occupy a pseudonym field. Users retain explicit opt-out, inspection, export and deletion controls; administrative retention changes are versioned policy decisions with receipts.

Redaction and deletion have an object-side representation: a context object's storage lifecycle (`active`, `redacted`, `deleted`) is separate from its authority state, and a redacted or deleted object retains its lineage and points at the receipt that authorized the change. Workspaces and organizations carry the same tombstone lifecycle at tenant scope.

## Durability and recovery

The hosted or self-hosted backend stores append-only events, current heads and immutable receipts transactionally. Compare-and-swap plus idempotency prevents lost updates and duplicate billing/seat effects; every mutable record carries an idempotency key and every receipt echoes the key of the operation it evidences. Backups, restore tests and workspace checkpoint replay must reproduce the same canonical heads and conflicts. Feature disablement stops Team egress and mutation while retaining encrypted data for rollback.

## Delivery sequence

1. Publish and independently review `team-context-v1` schemas and canonicalization fixtures.
2. Add tenant-scoped organization, workspace, membership and invite migrations with real PostgreSQL isolation/replay tests.
3. Add authority, promotion and provenance enforcement with signed immutable decisions, including the twelve cross-record validations the schemas cannot express.
4. Add checkpoint, conflict, supersession, recovery, retention, export, redaction and deletion flows.
5. Bind seats and signed Team entitlements; expose accepted-path versus waste attribution.
6. Prove two-user/two-device/machine shared context, revocation of memberships and active leases, contradiction and recovery E2E.
7. Only then connect Phase 16B distributed Workspace Bus/Work Graph and claim Team release readiness.

## Rollback

Before runtime shipment, rollback removes these unreferenced contract documents. After persistence ships, rollback is feature-off and no-egress: additive schema remains, encrypted tenant data and immutable audit history are preserved, and downgrade tooling must be proven against V3 fixtures before release.
