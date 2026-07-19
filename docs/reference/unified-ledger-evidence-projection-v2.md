# Unified Ledger Evidence Projection v2

Status: experimental implementation reference; canonical contract registration deferred

## Purpose

This contract maps a verified local savings-ledger snapshot and already
reconciled, payload-free source references into the attribution role of
`settlement-evidence-v2`.

It is an adapter, not a second evidence manifest. The canonical settlement
schema, bounded offline verifier, evidence classes, correction lineage,
exclusive-attribution rules and out-of-band trust store remain defined by
`settlement-evidence-v2`.

## Compatibility boundary

- `SavingsEvent` v1-v4 wire and hash verification remain unchanged.
- `SignedSavingsBatchV1` remains unchanged and keeps its current consumers.
- Existing ledger events gain no inferred quality, approval, contract or
  settlement state.
- A v1 mechanism label is a technical observation, not an exclusive commercial
  attribution claim.
- The adapter emits only `SettlementEvidenceClaimV2::Attribution` items. All
  other required settlement roles come from their own evidence producers.

## Inputs

### Verified ledger snapshot

`read_verified_snapshot_v2` opens one regular file without following symlinks,
takes the ledger writer's shared advisory lock, reads at most 4 MiB from that
same handle, rejects malformed UTF-8/JSON or a file changed during the read, and
then constructs `VerifiedLedgerSnapshotV2`. The snapshot accepts at most 1,000
ordered `SavingsEvent` values.
Construction requires:

- the first `prev_hash` is `genesis`;
- every later `prev_hash` equals the preceding `entry_hash`;
- every entry hash is lowercase SHA-256 hex and unique;
- every event verifies under one of the supported v1-v4 ledger hash schemes;
- monetary floats are finite.
- every event string is at most 256 bytes;
- mechanism-specific counter invariants hold: compression tokens reconcile
  exactly, routing/caching preserve token count, prices are nonnegative, and
  bounce records use the explicit negative-adjustment form.

The snapshot identifier is BLAKE3 over the domain separator and ordered event
hashes. It commits to chain order without exposing prompts, responses, paths,
repository names or user identities.

The lock coordinates with lean-ctx writers. An external process that ignores the
lock is outside that coordination boundary; length/mtime drift is nevertheless
checked before the snapshot is accepted.

### Signed batch binding

Projection requires a valid `SignedSavingsBatchV1`. Its Ed25519 signature,
`chain_valid`, event count, first entry hash and last entry hash must exactly
match the verified snapshot. The adapter content-addresses the complete signed
batch, including public key and signature. This proves a local signer asserted
the bound snapshot head; it does not establish independent trust in that signer.

### Attribution links

Every positive ledger observation requires exactly one
`LedgerAttributionLinkV2` containing:

- the ledger entry hash;
- a content-addressed reconciled source-evidence ID;
- a content-addressed exclusive-attribution group ID;
- explicitly supplied attributed tokens and currency minor units;
- an evidence trust tuple.

The adapter does not calculate settlement money from `SavingsEvent.saved_usd`.
That field is a local technical estimate and may use a different price basis.
Minor units therefore require separate reconciled pricing evidence.

For compression, attributed tokens cannot exceed net saved tokens. For routing
and caching, they cannot exceed the measured baseline tokens affected by the
observation. All quantities must be positive and use checked integer sums.

## Projection rules

Projection is deterministic under input-link permutation.

1. Verify the complete ledger snapshot.
2. Validate bounded, content-addressed links.
3. Require one link for every positive observation.
4. Reject repeated ledger entries, source evidence or attribution groups.
5. Reject unknown mechanisms.
6. Reject negative or bounce adjustments until a separate reconciliation has
   assigned them explicitly; never drop them from a positive claim.
7. Aggregate by the closed mechanism set: `compression`, `routing`, `caching`.
8. Require one identical trust tuple per aggregate mechanism.
9. Derive a projected source ID that binds snapshot ID, ledger entry hash,
   signed-batch ID, reconciled source ID and attribution group ID.
10. Emit one exclusive Settlement Evidence V2 attribution item per mechanism.

The complete projection has its own `projection_id`: canonical BLAKE3 over
schema, kind, signed-batch ID, signer key, snapshot head/count, sorted bindings
with integer amounts, and sorted emitted settlement items. `verify()` recomputes
every projected source, aggregate and content address and requires the same
`VerifiedLedgerSnapshotV2`; it checks every bound entry is a member of that
snapshot with the exact mechanism and token ceiling. A v1 batch head alone is
not a membership proof for intermediate events. `load_projection_artifact_v2`
therefore also requires the verified snapshot and performs a bounded 4 MiB
no-follow/nonblocking regular-file read before verification.

The emitted item is content addressed by Settlement Evidence V2. It is still
untrusted unless an operator-provided `SettlementEvidenceTrustStoreV2` contains
the exact evidence-ID, trust-decision-ID and trust-anchor-ID tuple.

## Fail-closed outcomes

Projection returns a typed error and no partial attribution item for:

- broken, duplicate or malformed chain entries;
- missing, duplicate or unknown links;
- duplicate source evidence or attribution groups, including cross-mechanism
  reuse;
- zero quantities or attribution exceeding its observation;
- non-finite money values;
- negative/bounce adjustments without reconciliation;
- unknown mechanisms;
- conflicting trust tuples inside one mechanism;
- arithmetic overflow or evidence-bound violations.

Missing, ambiguous or unsupported evidence is therefore `unknown` for
attribution purposes and contributes zero eligible settlement amount.

## Offline verification sequence

Consumers perform the following sequence:

1. obtain one stable bounded ledger snapshot;
2. construct `VerifiedLedgerSnapshotV2`;
3. create the deterministic attribution projection;
4. export or load it only through its batch-and-snapshot-bound
   `canonical_json`/`verify` API;
5. combine its items with independently produced baseline, price, contract,
   quality, period-completion and customer-approval evidence;
6. build `SettlementEvidenceManifestV2`;
7. call `reconcile_settlement_evidence_v2` with an operator-pinned trust store.

A manifest's embedded `trusted` flag cannot replace the external trust store.
The projection is locally re-verifiable where the bounded ledger snapshot is
available; it is not a portable privacy-preserving Merkle membership proof.

## Privacy

The projection carries hashes, content-addressed evidence references, mechanism
IDs and integer aggregates only. It does not carry prompt or response content,
file paths, repository names, model names, tool names, raw timestamps, personal
identifiers or secrets.

## Claim boundary

A successful projection proves only:

- the supplied local event snapshot was hash-chain consistent;
- a valid local Ed25519 batch signer bound the exact snapshot head and count;
- every projected positive observation had one exclusive reconciled source;
- projected source references bind the exact ledger snapshot;
- integer aggregation followed deterministic no-double-count rules.

Even a structurally eligible Settlement Evidence V2 result does not prove:

- provider billing accuracy or a provider invoice;
- causal equivalence outside the supplied evidence;
- quality or outcome truth;
- contract validity or dispute resolution;
- customer-approval authority;
- settlement completion or invoice authority;
- external certification, deployment readiness or full G2 completion.

The three authority flags in `SettlementEligibilityV2` remain false.
