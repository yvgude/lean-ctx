# Operator-attested Engine outcome v1

`lean-ctx engine context-outcome-receipt --host-stdin [--ledger-root ROOT]`
is an additive operator-only local process boundary. It is not an authenticated
remote endpoint. The existing `context-outcome --json FILE --host-stdin` wire
shape and explicit local-learning consent remain unchanged.

Stdin uses the same framing as source execution: one compact host-settings
line (at most16 KiB including newline), then one request (at most1 MiB).
Only operator settings can grant `allow_outcome_signing`; an execution signing
grant is insufficient. An embedding service must choose settings and the
existing private ledger root from its authenticated tenant, never request data.
The request cannot carry signing material, paths, caller evidence or timestamps,
completion/retry claims, or a learning toggle.

`EngineOutcomeRequestV1` carries version1 schema/transport and interface1.0.0,
the original unknown receipt digest, its exact planning-decision digest,
expected task/tenant/agent binding and1–16 explicit operator attestations.
The restricted signal DTO is converted to existing local signal values; only
the existing outcome evaluator decides accepted/rejected/unknown semantics.
Identity fields bind the signed task; they do not authenticate a caller.

Before mutation the host verifies the original receipt and planning signatures,
their exact Runtime evidence link, signed task identity, existing execution
protocol and host ledger. Existing grant, expiry, replay and conflict rules
remain authoritative. The framed operation requires the explicit planning
reference; it does not weaken legacy receipt checks or create another store.
Personal learning is always disabled for this operation, including local calls.

`EngineOutcomeResponseV1` contains schema1, original receipt digest, successor
ID/digest, acceptance, already-recorded flag and exact `receipt_document_json`.
The canonical UTF-8 document is bounded to1 MiB; serialized response to4 MiB.
An exact retry returns the same signed successor. Conflicting attestations fail
closed. Post-publication delivery failures are not silently reexecuted; retry
uses the existing receipt-idempotent authority.

DTO validation checks document shape/digest, outcome and task/planning joins;
it does not admit signing keys or independently authenticate the outer original
receipt-digest echo. Consumers must separately provision trusted keys and verify
signatures and original-to-successor lineage. These are operator attestations,
not automatic proof from a provider or CI system. Enterprise HTTP authorization,
durable accounting admission and released-artifact acceptance remain separate.
