# Explicit Engine source planning v1

`lean-ctx engine context-plan-sources --project-root /absolute/project --json-file request.json`
adds an operator-process source batch to the public Engine interface. It reuses the
canonical Context Kernel and fixed reference weights. It does not mix in local stores,
write source content to indexes/caches, invoke models or require private intelligence.
The existing strict `context-plan` local-store request is unchanged.

For either planning operation, `--json-file -` reads a bounded UTF-8 JSON request
from stdin instead of a regular file. It uses the same decoder, size limits and
validation, and does not create a request file. This lets a trusted service pipe
authorized source content to the public Engine without persisting the request.

The request has `planning` (the complete [context-plan request](engine-context-plan-v1.md))
and `sources`. Each source has `content` and `descriptor`: `object_ref`, `source_id`,
`source_type` (`filesystem`, `issue_tracker`, `relational_database`, `other`),
`content_digest` (`sha256:` plus lowercase hex), optional `revision`, `owner`,
`observed_at`, `valid_until`, `classification` and `permission`.
Classification uses the existing public enum: `Public`, `Internal`, `Confidential`,
`Restricted`. Permission is `permitted`, `denied`, or `unknown` (default).

The Engine verifies each content hash and rejects duplicate object references.
Unknown/denied permission, unknown classification, expired sources and future observed
times cannot become candidates. Unknown observation time stays unknown; host retention
policy can reject it. These checks precede canonical host source/sensitivity/budget
policy. Producer metadata is not authenticated authority or a remote authorization grant:
the local operator owns this process. A multi-tenant host must authenticate the caller,
derive the tenant and restrict sources before crossing this boundary. Caller labels
cannot substitute for those checks.

Input is strict bounded JSON: 1MiB total, 64 sources, 64KiB per nonempty content body,
plus the existing planning bounds. Candidate enumeration is ordered by object reference
before applying the requested candidate limit. Only full views have real token costs;
the adapter offers no fictitious compressed view. The canonical compiler excludes a
candidate when no declared positive-cost view fits; even pinned items cannot invent
or truncate a handle cost to consume the remaining budget. Canonical content-address deduplication
retains one selected source binding for identical content. When multiple eligible
sources contributed identical content, `result.plan.source_lineage_v1` preserves
their descriptors without adding duplicate selected candidates or token charges.

This additive projection extension has `schema_version: 1` and ordered `groups`,
each containing `selected_ref` and `equivalent_sources` (the original descriptors,
sorted by object reference, including the selected source). It is emitted only for
groups with at least two admitted sources. Admission comes from the canonical
kernel after candidate enumeration, identity checks and host policy—not from hash
equality alone. Pre-candidate denied/expired inputs, host-policy exclusions and
unenumerated inputs cannot appear in lineage. Original revisions, owners and
classification are retained; no source body or verified source authority is inferred.
The selected-only `source_bindings` contract is unchanged. Older extension-aware
V1 readers retain the field; callers must not assume older producers supply it.
Historical internal origin records default to unadmitted, never retroactively
granting lineage. New semantic plan identities include that admission marker.
The extension uses existing bounded V1 storage (64KiB serialized value); overflow
fails with `source_lineage_too_large`, never a silently truncated lineage claim.

The response has `result` (the existing plan response), selected `source_bindings`, and
`binding_digest`: SHA-256 of compact JSON encoding of `[result, source_bindings]` with
recursively sorted object keys and bindings ordered by object reference. Public DTO validation
checks version headers, exact selected-source/provider/content-digest joins and changed
result/binding bytes. The canonical projector uses the kernel's bound content reference,
not the shape of an arbitrary object ID; legacy plans without origin bindings retain
their historical projection behavior. This unsigned integrity digest is not an authentication
signature, source authority attestation, admission, execution receipt or outcome.
Both the projection digest and outer binding digest cover the lineage extension.
No source body is returned; rejected pre-candidate inputs have no selection entry.
Other exclusions are the existing canonical plan's policy/budget metadata.

## Materialization handoff (additive v1)

`EngineContextSourceMaterializationRequestV1` carries the complete bounded
`EngineContextSourcePlanRequestV1` again together with the previously returned
`expected_binding_digest`. Retention-enabled plans additionally expose a
`context_plan_evaluation_v1` projection extension containing the canonical
second-precision `evaluation_time`; the materialization request may echo it as
`planning_evaluation_time`. This epoch is an unsigned identity-replay hint only:
it is not a signature, server provenance, authorization grant, or freshness proof.
The response is produced only after the public source
planner re-runs its existing policy, sensitivity and freshness checks; a changed
selection, source descriptor, expiry decision or binding bytes fails closed with
`source_plan_changed`. The digest is an integrity join, not an authorization
grant, signature or receipt.

Exact replay reconstructs the original retention-aware identity with the echoed
epoch, while a fresh host-time admission pass independently rechecks current
source observation/expiry, sensitivity/source policy, and retention. Missing or
future epochs fail closed when retention is active; a manufactured epoch or
matching unsigned digest cannot bypass current policy or freshness. The exact
expected binding remains mandatory; the implementation never normalizes or
weakens it. Historical retention plans without the additive marker are not
backfilled and cannot be materialized through this replay path.

The materializer matches every canonical selected `source_bindings` descriptor to
the original validated descriptor and retains the complete corresponding body.
It joins selected bodies in object-reference order using deterministic v1 framing:
`## leanctx-source-v1`, `object_ref`, `source_id`, `content_digest`, then the body.
It applies the existing sensitivity/redaction/input-filter chokepoints and returns
the final `materialized_digest`; blocked or warning-producing filters fail closed.
The final rendered token count must be at or below the plan budget. Materialization
never performs a second selection, silently truncates, writes a cache/store, or
claims compression, execution, acceptance or a receipt. `materialized_digest` is
the SHA-256 of the exact returned policy-enforced content, while `binding_digest`
continues to cover the canonical plan and raw selected descriptors.

`materialized_token_count` is an Engine-reported metric, not signed or
independently verified cost evidence. The Engine producer counts the final text
with its canonical tokenizer and enforces the effective budget before returning
it. The protocol DTO's `validate()` checks the envelope, content digest and claim
bounds; it does not contain a tokenizer and cannot certify a remote count.
Consumers must recompute tokens with the canonical tokenizer before using the
content for budget admission, billing, cost learning or receipt accounting.
A self-consistent digest and a claimed zero count do not grant that authority.

This operation is only the immutable-input handoff. A later execution adapter may
pass its content to the existing materialized native Engine path and canonical
host receipt authority; it must not add a second ledger or infer accepted outcome.

Request/transport errors follow the existing context-plan contract; invalid source
manifests use `invalid_source_manifest`. The focused `engine_context_sources_transport`
tests exercise the actual CLI with typed fixtures, policy changes and tampering.
They do not prove deployed connectors, an Enterprise HTTP server, persistent source
catalog/revocation, or release-artifact acceptance; those remain separate journeys.

The bounded CLI entry point is:
`lean-ctx engine context-materialize-sources --project-root /absolute/project --json-file -`.
It reads one JSON request from stdin (or the existing JSON-file input), rejects a
whole request above 1 MiB before planning, and emits only the versioned response;
it does not create a receipt or persist a ledger row. The operation is governed
by the existing `ctx_read` policy allow/deny and `max_context_tokens` budget;
project policy resolution is bound to the supplied project root.
