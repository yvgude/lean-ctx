# Engine provider-execution v1

Status: additive public transport contract; this document does not claim a
provider execution implementation or a release/deployment journey.

## Scope and authority

EngineProviderExecutionRequestV1 is a host-prepared request. It joins an
existing TaskEnvelopeV1, an existing ExecutionPlanV1, and an already
validated EngineContextSourceMaterializationResponseV1 with an explicit
provider query and output-token ceiling.

The request is not caller authorization. The authenticated adapter remains the
authority for tenant and actor identity, source egress grants, source
revocation/classification/validity, provider selection, model policy, budget,
wallet reservation, and dispatch. A task envelope's optional tenant_id is
lineage metadata, not an authentication claim.

The contract deliberately does not create a planner, provider selector,
receipt signer, billing ledger, outcome evaluator, savings calculator, or
artifact-fetch authority. The adapter must reauthorize current source grants
and policy immediately before dispatch; a materialization digest is an
integrity join, not a reusable permission.

## Request v1

The strict envelope is:

- schema_version: 1
- transport_version: 1
- engine_interface_version: "1.0.0"
- attempt_id
- task: TaskEnvelopeV1
- plan: ExecutionPlanV1
- materialization: EngineContextSourceMaterializationResponseV1
- query
- max_output_tokens

The task ID, context-plan ID, and context budget must agree across the task,
provider plan, and materialization. The provider plan must name a concrete
non-local-native provider/model, have max_retries: 0, and have no fallback
references. The initial contract accepts max_output_tokens in 1..=65536,
rejects an empty/NUL-containing query, and bounds the serialized request to the
existing source transport limit plus 64 KiB.

query is the provider query supplied by the host. It is not inferred from
TaskEnvelopeV1.intent.

canonical_bytes() recursively sorts JSON object keys. canonical_digest()
hashes leanctx.engine.provider-execution.request.v1\0 followed by those
bytes. The digest is an adapter correlation key, not a signature or admission
proof.

canonical_intent_digest() excludes only attempt_id and uses the separate domain
leanctx.engine.provider-execution.intent.v1\0. It supports stable pre-admission
deduplication before storage assigns the attempt ID. Every other request field
still binds; neither digest is permission to retry or dispatch.

## Response v1

EngineProviderExecutionResponseV1 repeats attempt_id, task_id, plan_id,
context_digest, provider, model, and the complete canonical request_digest.
validate_for() checks that digest, including query and output-budget changes,
not merely reused task/plan labels. It carries:

- status: succeeded, failed, rejected, timed_out, or dispatch_uncertain;
- acceptance: "unknown" only;
- optional bounded output content plus its SHA-256 digest;
- ViaProviderUsageV1, preserving measured/estimated/unavailable semantics;
- EngineProviderCostV1;
- optional sanitized EngineFailureV1.

A successful response requires output and no failure. Every non-success status
requires a failure and omits output. Host automatic retry is forbidden,
including for dispatch_uncertain; uncertain side effects must be reconciled
by the owning adapter rather than silently redispatched.

EngineProviderCostV1 is one of:

- {"basis":"unavailable"};
- {"basis":"usage_priced_estimate","micros":N};
- {"basis":"observed_charge","micros":N}.

micros is USD micro-units, bounded by the cross-language safe integer
ceiling. Zero is allowed only when the selected basis is factual; absence is
represented by unavailable, never by an invented zero.

Output content is bounded to 256 KiB and the complete response to 1 MiB.
Output content is transport data, not a signed receipt or an implicit artifact
lookup. Provider execution does not imply acceptance, learning eligibility,
savings, or a canonical receipt.

## Existing local-native contracts

EngineContextSourceExecutionRequestV1 and its v1/v2 responses remain
local-native contracts. Their AcceptanceState::Unknown, local capability
checks, and canonical host receipt are not provider execution evidence and must
not be relabeled as provider completion, observed charge, or accepted outcome.

ExecutionReceiptV1 remains the legacy execution summary. Its required
numeric actual/baseline/avoided-cost fields are not used as the provider
result because unknown cost must remain unknown.

External consumers must independently admit any signer and verify any later
canonical receipt. This v1 transport itself carries no signature or key
authority.
