# Reference scheduler estimate semantics

The public reference scheduler consumes technical capability information. Task
quality requirements and latency budgets express requested bounds, not measured
performance or a capability's predicted ability to satisfy them.

Generated candidates therefore retain unknown cost, quality and latency estimates.
Their `ExecutionPlanV1.estimates` explicitly contains null entries, with zero only
in the legacy compatibility scalars. The synthetic manual/passthrough fallback
retains its zero model-call cost; its quality and latency remain unknown.

Ranking consumes the candidate-level nullable estimates, never the compatibility
scalars: known cost and latency precede unknown values (ascending), known quality
precedes unknown values (descending), and equal estimates are ordered by candidate
identity and then plan ID. Thus unknown values cannot rank as zero, while the
historical fallback wire representation remains unchanged.
This also applies to legacy plans without `estimates`: a missing candidate-level
estimate remains unknown even if the legacy plan contains a nonzero scalar.

A hard minimum-quality or maximum-latency policy rejects unknown predictions,
including the manual fallback. Raising a requested target cannot manufacture an
estimate that satisfies the same policy. Without these hard prediction requirements,
ordinary deterministic recommendations remain available. Recommendations never
execute a provider or attest model success, cost settlement or learning authority.

This is an additive use of the existing versioned estimates projection, not a new
planner, pricing catalogue or protocol generation. Existing serialized V1 plans
remain readable; legacy zero scalars must not be interpreted as observed results.

The native context adapter retains the historical wire representation of its
unselectable manual fallback in persisted scheduler evidence. It still requires
exactly one admitted native candidate, whose estimates remain explicitly unknown.
This narrow compatibility projection preserves old native-plan revalidation;
it does not permit an unproved fallback or weaken comparison of stored decisions.
