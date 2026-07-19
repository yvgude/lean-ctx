# Context Candidate Admission V1

`core::candidate_admission` is the shared, payload-free decision contract for
checking candidate provenance, freshness, sensitivity, policy, and target
eligibility before materialization.

The evaluator accepts at most 256 candidates and bindings, covers all eight
`CandidateDomainV1` values, validates every `ContextObjectV1`, and requires exact
content-, freshness-, and policy-reference agreement. Missing, unknown, or
duplicate bindings; duplicate objects; schema/reference drift; denied
sensitivity or targets; and every `StalenessReasonV1` produce explicit
fail-closed decisions. Structurally invalid input exposes no per-candidate
partial result. Canonical reports are sorted, permutation-stable, and contain no
source path or object payload.

The accompanying store primitives provide exact, bounded, idempotent
invalidation for semantic-cache paths, search roots, and workspace files.
Search invalidation advances a generation so an in-flight stale build cannot
reinstall invalidated state.

This contract and its hermetic gate prove local decision and invalidation
semantics only. They do not yet prove enforcement by every read, search, graph,
knowledge, provider, or materialization hotpath; live adoption, cross-process
durability, customer policy, and production SLO claims remain open.
