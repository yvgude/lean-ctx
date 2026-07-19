# Multi-Agent Efficiency Benchmark V1

`core::multi_agent_efficiency_benchmark` is the canonical OSS contract for a
hermetic single-agent control versus multi-agent treatment replay.

## Evidence contract

- 30–256 paired workloads; every pair carries identical opaque workload, task,
  expected-outcome, acceptance-criteria, policy, and coverage references.
- Both arms carry a canonical `ContextReceiptV1`, a uniquely correlated
  `OutcomeRecorded` event, an explicit experiment assignment, and all ten
  `EffectiveTokenCostV1` components.
- Control is a single-agent arm with no handoff. Treatment has at least one
  `SignedContextCapsuleV1` handoff whose pinned Ed25519 key, allowed recipient,
  relay ID, a process-bound `AgentRelayDeliveryEvidenceV2` shape, receipt entry,
  and canonical bounded A2A byte/token recount reconcile exactly. Unsigned V1
  `delivered_tokens` are never accepted as efficiency evidence.
- Every per-workload capsule graph has one chain identity, exactly one root,
  and unique capsules, relays, edges, and recipients; the root starts at the
  receipt owner with hop 1,
  while descendants bind parent and delta to the same prior capsule, increment
  hop exactly, and transfer ownership to the prior recipient.
- ETPAO and quality/outcome parity are computed by the production
  `reconcile_accepted_outcome_etpao` and
  `evaluate_experiment_outcome_parity` APIs. Duplicate context and configured
  local fan-out are reported from explicit receipts plus signed-capsule/delivery
  evidence.

## Fail-closed semantics

`missing_evidence`, `incomplete_evidence`, `ambiguous_evidence`,
`reference_drift`, `quality_outcome_regression`, and `overflow` never expose arm
metrics or an efficiency delta. Corpus bytes are counted through a non-allocating
bounded writer; top-level and nested cardinalities fail before sorting,
reconciliation, or cryptographic work. Pairs, signers, handoffs, token
components, arithmetic, identities, and evidence cardinality remain bounded.
Equivalent input in the same process produces byte-identical report JSON and
integrity references.

The report is local reconciled evidence only. `live_traffic_observed`,
`provider_billing_observed`, and `commercial_savings_eligible` are always
`false`; the contract cannot authorize billing, settlement, causal savings, or
production-performance claims. Process-bound V2 evidence is a local integrity
binding, not a durable attestation, transport-commit proof, observation of
actual fan-out execution, or proof of external delivery.
