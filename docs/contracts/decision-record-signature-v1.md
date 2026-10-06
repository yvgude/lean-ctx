# DecisionRecordV1 signature profile

Status: Stable, additive v1 profile; production-lineage acceptance is separate.

This additive profile defines authentication of the existing decision record.
It does not change its wire fields or turn generic structural validation into
proof of execution. Older opaque signature strings remain structurally readable,
but do not authenticate under this profile.

## Covered bytes

1. Validate the record's existing structural invariants.
2. Serialize its full object as compact UTF-8 JSON with recursively sorted object
   keys, retaining array order and all extension fields.
3. Omit only the top-level `signature` member. Keep decision ID, task/plan IDs,
   stage/kind, selected result, references, rationale and observation time.
4. Prefix those bytes with the ASCII bytes `lean-ctx:decision-record:v1` and one
   NUL byte. Sign the concatenation with Ed25519.
5. Encode the 64-byte signature using standard padded Base64.

`DecisionRecordV1::signing_bytes` is the normative Rust implementation of this
byte format. Protocol code performs serialization only; signing and strict
Ed25519 verification belong to the OCLA SDK.

The record's `input_refs` must contain `sha256:<64 lowercase hex characters>`
of the complete task's `TaskEnvelopeV1::canonical_bytes`. These bytes use the
same compact recursively sorted JSON format, retaining all task extensions.
Changing project, tenant, policy, session or another task field while retaining
the task ID therefore cannot reuse the old signed task binding.

## External authorization

Resolve the verifying key and `DecisionSignerAdmissionV1` from trusted host
policy, not record/request key material. The grant binds an exact task snapshot
and the authorized decision stage/kind. Receipt-signing permission alone does
not authorize a decision grant; the host must explicitly authorize this purpose.
The grant has no wire deserializer or automatic receipt-admission conversion.

The actual key must match the trusted SHA-256 key digest. Admission is inclusive;
expiry and revocation are exclusive at both the decision's observation time and
the verification time. `observed_at` uses canonical UTC second precision
(`YYYY-MM-DDTHH:MM:SSZ`) for this profile and must not be in the future.
The producer must capture the genuine decision time, not reconstruct it later.

## Production lineage is an additional gate

An authentic signature proves the admitted signer's statement, not that an
action ran or an evaluator's claim is correct. Consumers must also compare the
selected result with the actual immutable task-bound decision; verify exact
context/final-plan/artifact bindings; enforce replay and scope; and preserve
Unknown outcomes as non-training observations.

For Planning/ContextSelection, preserve the complete existing canonical handoff,
not merely its shortened decision ID or lossy wire context projection. Bind the
existing decision ID into the plan before plan finalization, then sign the record
referencing that final plan before invocation. Do not demand future receipt or
outcome inputs, and do not hash the final signed record back into its own plan.

This profile and SDK helpers alone do not establish a production producer,
operator-purpose provisioning, full lineage admission, or private learning flow.

## Host purpose provisioning

The operator-owned receipt host configuration may explicitly set
`allow_context_decision_signing: true`. Omission or `false` preserves receipt-only
authority; request arguments cannot supply this permission. When enabled, host
attempt admission requires the actual immutable context handoff, an already-bound
final plan, and the executed autopilot reference. Missing or mismatched context
fails closed. The host observes, signs and persists the complete planning record
before execution and retains it in the existing attempt. Its artifact reference
is audit metadata, not an outcome or a learning authorization.

Native planner finalization and stage-aware downstream lineage validation must
also be integrated; configuring this permission alone does not complete that flow.
