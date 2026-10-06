# Operator-owned source execution v1

`lean-ctx engine context-sources-receipt --project-root ABSOLUTE_ROOT --host-stdin`
is an additive local host operation. It is not an unauthenticated remote API.
The existing `context-view-receipt` contract continues to reject caller-supplied
context-plan IDs.

Stdin contains one compact JSON host-settings document followed by a newline
(at most 16 KiB including that newline), then one JSON request (at most 1 MiB).
The first document uses the existing host authority settings and requires the
independent `allow_context_decision_signing` grant. Source data and signing
material never go into command arguments or temporary request files.

An embedding host can append `--ledger-root ABSOLUTE_EXISTING_STATE_ROOT`.
This trusted operator argument restricts the settings' ledger path to that root
through the existing descriptor-pinned `ExecutionLedgerStore::new_verified`.
Outside paths, traversal and symlink roots/parents fail before durable intent.
Omitting the option preserves the existing operator-only local contract.
Multi-tenant hosts must provide their authenticated tenant's private state root;
neither the root nor host settings may come from the untrusted request.

The shared `EngineContextSourceExecutionRequestV1` has exactly `schema_version: 1`, `transport_version: 1`,
`engine_interface_version: "1.0.0"`, `task`, `plan`, and `materialization`.
Task and plan use the existing public v1 contracts. Materialization uses
`EngineContextSourceMaterializationRequestV1`, including its original source
batch, expected binding digest and optional retention evaluation-time echo.
The caller declares a local-native plan with no context-plan ID or scheduler
claim. Its explicit token budget must equal the policy-admitted source plan.
Unresolved model/region policy is rejected; execution remains local-only.

The same process retains the canonical Context Autopilot decision, binds it to
the declared execution plan, and invokes the existing native materialized-input
adapter. It does not fabricate filesystem paths for service/database sources.
The invocation binds the materialized input digest, one descriptor-only
source-plan evidence ref (which canonicalizes and binds all selected source
descriptors), and existing task/plan evidence. It intentionally does not emit
one invocation ref per selected source, so the Engine's bounded lineage remains
valid for the source-plan limit of 64 candidates.

Source policy and materialization are checked before intent and again before
receipt publication/disclosure. A changed snapshot fails closed. Failed attempts
after durable intent remain recorded and cannot silently execute twice; this is
not an atomic policy-change lock. The existing canonical host authority verifies
actual output bytes and signs the receipt. Acceptance remains `unknown`; token
measurements come from native execution, not the wire materialization metric.
No billing or accepted-learning authority is implied.

When the host captures a planning decision, its receipt also binds the exact
signed `DecisionRecordV1` digest as `runtime` evidence at
`artifact://execution/evidence/<digest>`. This is the existing persisted planning
record, not a new store or an acceptance claim. A separately authorized operator
can pass that digest and the original receipt digest to the existing
`context-outcome` command. Its evaluator, independent `allow_outcome_signing`
grant, expiry/replay rules and learning consent remain unchanged. The operation
records operator attestations, not independently observed CI/provider outcomes.
Receipts without captured planning decisions do not fabricate this evidence.
When a receipt contains this runtime reference, outcome evaluation requires the
exact bound digest and canonical URI; another valid signed planning record is
insufficient. Historical receipts without the reference retain their existing
signature/handoff checks.

The shared `EngineContextSourceExecutionResponseV1` has v1 envelope fields, `source_plan`, `execution_plan`, `view`,
`invocation`, `observation`, and `canonical_receipt`. The view contains actual
output text/ref/digest. Receipt identity/ref/digest and `outcome: "unknown"` use
the existing canonical receipt store. Recovery is not disguised as single-file
recovery: this operation does not return a fabricated path or recovery descriptor.

`validate_against(request)` checks the exact declared plan (allowing only the
kernel's context-plan ID and decision-ref additions), original task evidence,
source binding/optional epoch, actual output digest and bounded lineage refs.
It is transport integrity validation, not independent signer verification of
the canonical receipt document. The v1 JSON shape is unchanged.

This contract is an implementation component; release/deployment/user-journey
acceptance remains separate.

## Explicit v2 signed-document delivery

`context-sources-receipt-v2` accepts the same v1 request and host-stdin framing,
including the optional trusted `--ledger-root`. It executes the same authority
once; it is not a historical receipt lookup. The original command and response
remain unchanged. Consumers must explicitly select the new operation.

`EngineContextSourceExecutionResponseV2` has exactly `schema_version: 2`,
`execution` (the unchanged v1 response), and `receipt_document_json`. The latter
is a lossless UTF-8 string of the exact persisted canonical signed document:
after JSON decoding, its UTF-8 bytes must round-trip without normalization or
re-rendering. Hash those exact bytes, not a newly serialized object. The document
is bounded to1 MiB before parsing; the complete serialized response is bounded
to4 MiB, including JSON escaping.

The host uses the existing bounded artifact reader, signer admission/signature
verification and verified ledger membership before disclosure. Protocol
validation joins document ID/digest/ref, task/plan/invocation hashes, actual
observation evidence and the distinct native receipt evidence to the nested v1
response. The source-plan hash remains bound through the signed invocation;
omitting the nested execution loses that source evidence.

DTO validation does not verify a signature or admit a key. External consumers
must provision the Ed25519 public key/admission independently; receipt `key_id`
is not a trust root. This immediate-delivery operation does not define historical
key rotation, delayed retrieval, accepted outcomes or billing. Remote embedding
hosts must retain authenticated tenant/current-source authorization before
disclosure. Enterprise wrapper tenant/revision metadata is not a signed claim.
