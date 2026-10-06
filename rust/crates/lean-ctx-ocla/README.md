# lean-ctx-ocla

Open Context & Token Lifecycle Architecture (OCLA) — the stable, provider-neutral
contract boundary shared between lean-ctx-core (OSS) and lean-ctx-enterprise
(proprietary).

Defines capability traits, canonical types, token envelopes, and shared
production receipt-signature verification.

This crate is an internal dependency of [lean-ctx](https://crates.io/crates/lean-ctx)
and is not intended for direct use.

## Migrating Rust implementers to 2.0

`SavingsLedger` requires `project_savings` in addition to `record_savings`.
Projection updates capability-local summaries and evidence only; it must not
append another accounting event owned by the caller. Direct recording retains
its accounting write and then updates the same projection.

Rebuild implementers against the 2.0 trait and update Cargo requirements.
Do not mix old and new Rust trait objects or implement projection by forwarding
to a ledger-writing record method. JSON types, `ocla/v1`, and envelope schema
versions are unchanged.

## Receipt trust verification in 2.1

`ReceiptSignerAdmissionV1`, `validate_signer_admission`, and
`verify_receipt_signature` are available to the Engine and private consumers.
Resolve signer snapshots and public keys from trusted host configuration, never
from a receipt. Verification checks canonical receipt shape, signer identity,
key digest, admission at issuance and verification, revocation, future issuance,
and the strict Ed25519 signature. Admission is inclusive; expiry/revocation are
exclusive. Existing receipt wire schemas and the 2.0 traits are unchanged.

A successful signature check is not a learning admission token. Consumers must
still verify execution lineage, actual artifact bytes, authenticated tenant and
project scope, replay, and outcome evidence. Missing quality remains Unknown.
The Engine retains its local receipt-store and publication checks. The separate
`leanctx-verify` implementation remains an independent verification oracle.

## Decision authentication in 2.2

`sign_decision_record` and `verify_decision_signature` authenticate the complete
canonical decision using a versioned, decision-specific Ed25519 domain.
`DecisionSignerAdmissionV1` requires an explicitly trusted host grant for the
exact task and stage/kind purpose; receipt-signing permission alone is insufficient.
The signed inputs must include the SHA-256 digest of the complete canonical task.
The protocol crate supplies serialization only; the SDK performs authentication.

Signature success does not establish execution, artifact availability, replay
admission or outcome quality. Production consumers must compare the actual
immutable handoff and final plan before issuing any learning admission token.

## License

Apache-2.0
