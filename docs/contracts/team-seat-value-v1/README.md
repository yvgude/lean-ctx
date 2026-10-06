# LeanCTX Team Seat and Value Contract v1

This directory is an independent, immutable public-contract family. It does not
extend or alter `team-context-v1`; consumers must negotiate this capability
separately. `frozen-hashes.json` freezes every normative schema, this README,
and every fixture.

The imported Team v1 primitive semantics and offline schema engine are pinned
to these immutable bytes; a mismatch fails the frozen-pack gate:

- `docs/contracts/team-context-v1/common.schema.json`:
  `f193647f2652fff7553240e2cf9ee692f4fd4883deee2ac1d3295703ae5405a0`
- `scripts/verify-team-context-v1.py`:
  `5df932374f44c32bdbf6598825498f54e30a2dee42eecc9bfbe79015525f74ee`

## Canonical bytes and signatures

Records use RFC 8785 JCS. A payload digest is
`sha256:<hex(SHA-256(JCS(record without signature_envelope_digest)))>`.
The envelope signs the ASCII bytes
`leanctx-team-seat-value-v1:<payload_kind>:<payload_digest>`.
The envelope itself has no self-digest field; references to it use SHA-256 over
its complete JCS bytes. Every record in the positive fixture has its own
materialized envelope binding, and the fixture trust store binds issuer,
organization, purpose, minimum epoch, validity and revocation state.

## Authorities and flow

Billing authority issues entitlement facts and their signed successor/revocation
chain. Workspace policy authority decides a fully embedded allocation request.
An allowed decision authorizes exactly one allocation request; an allocation
references that decision, and immutable usage references the allocation. This
request -> decision -> allocation -> usage graph is acyclic. Value aggregation
authority creates dashboard evidence only from accepted usage and may correct a
prior aggregate through CAS lineage.

Mutable logical heads (`seat-allocation`, `team-value-aggregate`) use `version`:
version 1 forbids `parent_digest`; version >1 requires the immediate predecessor
digest and the same logical id/scope. Immutable facts (`seat-entitlement`,
`seat-limit-decision`, `seat-usage`) instead use monotonic `sequence` plus an
optional predecessor/correction reference where their schema requires it.
Identical idempotency scope, action and key must bind identical canonical request
bytes. Replay returns the prior result; drift is rejected.

## Time, limits, money and privacy

Entitlement activity is derived from `effective_at`, `expires_at`, `grace_until`
and a signed revocation successor; no independently asserted state exists.
Allocation never exceeds its allowed decision or the active entitlement at the
decision instant. Usage uses half-open `[period_start, period_end)` intervals and
an inclusive `event_watermark`; late events require a correcting usage fact.
Overage and invite-reserved seats are not representable in v1.

Money is `{currency, coefficient, scale}`: uppercase three-letter currency;
coefficient is normalized signed base-10 integer text (`0`, never `-0`, no
leading zero); value is coefficient * 10^-scale, scale 0..18. Arithmetic uses
checked integers and round-half-to-even. Currency mixing requires explicit
`fx_evidence_digest`; aggregates remain partitioned by currency otherwise.

Actor/customer identity is represented only by tenant-keyed,
domain-separated HMAC pseudonyms. No email, name, provider identity, prompt,
path, secret or free text is admitted. Retention class is explicit in the
verifier evidence model; deletion erases the identity mapping, preserves only
unlinkable finance tombstones, and never rewrites signed facts.

## Cross-record invariants

1. Every reference resolves inside the same organization/workspace scope.
2. Signer key, action, membership and role are valid at the record instant.
3. Entitlement predecessor has same id, sequence - 1 and scope.
4. Allowed decision request digest equals its embedded request and capacity is
   within the active entitlement; denied decisions cannot authorize allocation.
5. Allocation decision is allowed, request fields match exactly, and summed
   active allocation quantity never exceeds the applicable seat limit.
6. Usage allocation is active for the whole period, quantities are nonnegative,
   and correction chains are acyclic with identical scope and period.
7. Aggregate inputs resolve to accepted usage, are sorted/unique/bounded, share
   scope/currency/window, and exclude rejected or superseded usage.
8. CAS lineage is immediate, identity-preserving and acyclic.
9. Monetary derivations are exact, overflow checked and provenance-complete.
10. Idempotency replay is byte-identical; key reuse with drift is rejected.

Run `python3 scripts/verify-team-seat-value-v1.py`; this rejects duplicate JSON
names, verifies frozen bytes, seven schemas, materialized record/envelope
bindings, trust/authority, lifecycle, money, privacy, race models and cross-
record rules. It is an offline contract gate; PostgreSQL isolation and runtime
authorization remain deployment gates.
