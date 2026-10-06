# ADR: v4 Decision Spine consolidation

**Status:** Accepted
**Date:** 2026-09-03
**Scope:** Engine, protocol crate, local runtime, and documented private-service
boundaries

## Authority and evidence

- Governing input: `LEANCTX_V4_ULTIMATE_MASTER_PROMPT_FINAL.md`, SHA-256
  `90fe65b50a80d8a802c066be060ac0771ddf0fa697758a3f09ec79e2ecc62f9b`.
- Engine cut:
  `b4429b80f3bc75bab4c2d7a959c9ae81ba5a2666`.
- Accepted private Phase-0 baseline (not published), SHA-256
  `17da230ccdd8faac41c190f703522d23e5fa0558914cf2421c6eace24b6d588c`.
- Accepted private Phase-0 capability census (not published), SHA-256
  `1300c675fce0c6d980e91add46bc222ac930d312a70edb89b070b6775ed5785d`.
- Companion decision:
  [v4 VIA resolution](ADR-v4-via-resolution.md).

Observed behavior below is evidence from that cut. Statements beginning with
"must", "will", "Phase", or "planned" are target architecture and remain
implementation gates unless the text cites current executable behavior.

## Context

LeanCTX already contains the pieces of an execution architecture, but several
generations can currently appear to own the same decision. The accepted
Phase-0 census records 87 primary capability groups and all 16 OCLA built-ins
at Engine base
`b4429b80f3bc75bab4c2d7a959c9ae81ba5a2666`. This ADR assigns one owner to
each decision without treating source presence as production readiness.

The relevant generations are the Task Spine, Decision Loop,
`DecisionLoopRuntime`, Context Control Kernel, OCLA scheduler and built-ins,
protocol `ControlPlaneContract`, Work Graph, AgentConnector, A2A task and
transport types, Value Gate, Outcome Evaluator, execution/evidence ledgers,
Agent Bus, Scent Field, Delivery Registry, Sub-Agent Contract, and VIA.

Uncoordinated evolution would create multiple task IDs, context plans, policy
decisions, schedulers, receipts, outcome meanings, and learning loops. It would
also make product entitlement and data-boundary enforcement dependent on which
entry point happened to run.

## Decision

LeanCTX v4 has one canonical **Decision Spine**. It is the authority that admits
a task, records the decisions applied to it, coordinates context and execution,
and closes the lineage only when outcome evidence has been evaluated.

"Decision Spine" names an ownership model and canonical flow. This ADR does not
authorize a second orchestration implementation. Existing modules become
components or compatibility adapters under this ownership model.

The canonical flow is:

```text
trusted ingress
  -> TaskEnvelopeV1
  -> task profile / triage
  -> CapabilityManifestV1 catalogue
  -> entitlement and hard-policy admission
  -> ContextPlanV1
  -> ExecutionPlanV1
  -> bounded execution
       -> AgentConnector
       -> OCLA capability invocation
       -> model/provider adapter
       -> optional BoundedWorkGraph
       -> optional A2A transport
  -> ExecutionReceiptV1 + specialized receipt references
  -> AcceptedOutcomeV1
  -> accepted-path cost/value attribution
  -> typed learning observations
```

Each arrow preserves `task_id`, `trace_id`, schema version, applicable
decision references, and evidence references. A child unit of work receives a
new `task_id`, points to `parent_task_id`, and retains the parent trace.
Retries retain the logical task identity but receive attempt-level identity.

### Authority rules

1. `TaskEnvelopeV1` is the only canonical task-lineage envelope.
2. Entitlement and hard policy run before context, routing, scheduling, or
   execution optimization. Unknown or paid capability IDs fail closed. Only
   capabilities explicitly classified Community or Trust Core are accountless.
3. `CapabilityManifestV1` is the canonical schedulable-capability
   description. Registry entries advertise; they never authorize themselves.
4. The Context Kernel owns context candidate gathering, deduplication, scoring,
   policy filtering, budget compilation, selection reasons, and the local
   context receipt.
5. `ExecutionPlanV1` is the canonical execution-plan wire contract.
   Control-plane and scheduler outputs are decision inputs or references, not
   competing plans.
6. AgentConnector and OCLA execute admitted plans. They do not independently
   choose product entitlements, task identity, or outcome acceptance.
7. `BoundedWorkGraph` owns bounded delegation structure and budget cascade.
   It does not replace TaskEnvelope lineage.
8. `ExecutionReceiptV1` is the canonical execution summary. Specialized
   receipts remain valid only when referenced from its evidence/decision chain.
9. `AcceptedOutcomeV1` owns accepted, rejected, or unknown outcome state.
   Process exit zero, HTTP success, or tool return alone is never acceptance.
10. Learning consumes typed observations after outcome evaluation. It cannot
    bypass policy or collapse incomparable rewards into one undocumented score.

## Canonical contracts and owners

| Concern | Canonical v4 contract or owner | Required relationship |
| --- | --- | --- |
| Task identity and lineage | `lean-ctx-protocol::TaskEnvelopeV1` | Created at trusted ingress; every plan, child, receipt, outcome, checkpoint, and evidence artifact joins to it. |
| Triage | Task profile derived from the envelope | Adds intent, complexity, risk, quality, cost, latency, classification, and policy references; it does not mint a second task. |
| Capability description | `CapabilityManifestV1` | OCLA and other providers register manifests; admission filters the catalogue before selection. |
| Hard policy | one typed admission decision plus `DecisionRecordV1` | Entitlement, data, region, model, egress, tenant, and safety policy precede optimization; denial is terminal and auditable. |
| Context decision | Context Kernel `ContextPlanV1` | Current internal plan is authoritative locally. Phase 4 must define its versioned protocol projection rather than creating another semantic plan. |
| Execution decision | `ExecutionPlanV1` | Carries context strategy, capabilities, provider/model, reasoning allocation, retries, fallback, stop, expected cost/quality/latency, and policy/scheduler refs. |
| Execution identity | planned additive agent/attempt fields | Current `ExecutionPlanV1` and `ExecutionReceiptV1` both lack explicit executor agent identity. Phase 4 must add a version-safe executor reference or referenced assignment/receipt decision. |
| Context accounting | `ContextBalanceV1` plus Context Kernel receipt | Original, materialized, delivered, and provider-billed token classes remain distinct. |
| Execution result | `ExecutionReceiptV1` | Summarizes actual execution and links specialized receipts and evidence. |
| Outcome | `AcceptedOutcomeV1` | Tri-state acceptance with explicit signals and evidence. |
| Value | Value Gate projection over accepted outcome and receipt | CPAO, ETPAO, avoided cost, accepted path, and waste remain evidence-based projections, not admission authority. |
| Learning | typed observation bus and model-specific learners | Separate models for context source, read strategy, capability/provider, agent, model, and fallback selection. |

## Existing-system map

| Existing subsystem | Canonical role | Disposition |
| --- | --- | --- |
| `core/task_spine.rs` | trusted local envelope construction and triage bridge | KEEP as ingress adapter; converge all callers on protocol TaskEnvelopeV1. |
| `core/decision_loop.rs` | current task/triage/value composition façade | CONSOLIDATE into Decision Spine orchestration; remove independent ownership after parity. |
| `core/decision_loop_runtime.rs` | tool-start/tool-end bridge and local assessment cache | KEEP temporarily as runtime adapter; emit canonical decisions/receipts, then narrow or delete duplicate state. |
| `core/context_kernel/**` | context planning, receipts, policy, feedback, recovery, and context-specific learning | KEEP as canonical context-decision component. |
| `core/ocla/**` and `lean-ctx-ocla` | capability catalogue, invocation fabric, executor adapters, and conformance | KEEP; OCLA executes capabilities selected by the Spine. |
| `lean-ctx-protocol::ControlPlaneContract` | replaceable strategy provider for model/context resource suggestions | KEEP as a strategy interface below hard policy; its decision is converted into plan fields and a DecisionRecord reference. |
| `core/work_graph.rs` | bounded delegation DAG, fan-out/depth limits, budget cascade, stop propagation | KEEP as the graph executor below one admitted root plan. |
| `core/agent_connector/**` | local child-process execution adapter | KEEP as sole agent-process connector; return attempt result and receipt evidence. |
| `core/a2a/**` and `core/a2a_transport.rs` | interoperable remote message/task transport and reliability policy | KEEP as transport/projection; A2A Task is not canonical task identity or scheduler authority. |
| `tools/ctx_agent.rs` and `core/agents/**` | current local research presence, messaging, diary, and handoff surface | CONSOLIDATE into the paid Local/Workspace Agent Bus after identity, tenancy, durability, and retention gates. |
| `core/scent_field.rs` | decaying Need/Have/Claim/Done/Friction hints | KEEP as advisory coordination only; never authorization or durable task truth. |
| `core/agent_lease.rs` | bounded exclusive resource ownership | KEEP as canonical lease primitive; claims may provide a user-facing advisory façade. |
| OCLA Delivery Registry | content-addressed delivery deduplication and bounded relay cache | KEEP as delivery plane; it does not own work state or general messaging. |
| `core/subagent_contract.rs` | deterministic bounded briefing and structured return projection | KEEP as Work Graph edge payload; wrap in canonical task/checkpoint/evidence references. |
| Value Gate, outcome evaluator, ledgers | post-execution evidence, outcome, cost, and value | CONSOLIDATE behind canonical receipt/outcome chain. |
| VIA | bounded remote context-planning protocol only | KEEP with the exact non-overlapping scope in the companion VIA ADR. |
| OCP adapter and schema mirror | open context/governance/evidence exchange | KEEP as a lower-level open wire projection, not an execution or capability scheduler. |

## OCLA built-in map

All 16 built-in implementation files are mapped. Fifteen correspond to the
registry's discoverable capability kinds; `experiment_executor` is a bounded
execution helper rather than a separate registry kind. None becomes a parallel
Decision Spine.

| Built-in | Canonical v4 responsibility | Migration |
| --- | --- | --- |
| `agent_gateway` | admitted Agent Bus/A2A gateway invocation | Retain façade; route through canonical identity, task, policy, and transport. |
| `compression_provider` | context transformation capability | Delegate algorithm and accounting to canonical compression/Context Kernel path. |
| `config_tuner` | configuration proposal capability | Proposal only; hard policy authorizes any mutation. |
| `connector_scheduler` | executor scheduling adapter | Invoke the Work Graph/scheduler decision; remove independent budget authority. |
| `delivery_registry` | bounded content-addressed delivery plane | Retain dedup/TTL/ack behavior; separate delivery from task/message state. |
| `efficiency_analyzer` | receipt/value projection | Read canonical receipts; do not create a second cost ledger. |
| `experiment_executor` | approved experiment execution | Require admitted variant, budget, stop, evidence, and rollback refs. |
| `experiment_runner` | experiment orchestration façade | Call the canonical evaluator/executor; one experiment receipt chain. |
| `intent_classifier` | task-profile proposal | Populate triage evidence; cannot admit, entitle, or schedule. |
| `metrics_exporter` | allowlisted operational export | Consume the canonical telemetry dictionary; no payload content. |
| ~~`model_router`~~ | removed in v4 | LeanCTX no longer proposes models; outbound model policy stays enforced in the proxy. |
| `observation_hook` | lifecycle observation adapter | Emit typed, content-free stage observations. |
| `outcome_tracker` | outcome-signal adapter | Write signals to the canonical Outcome Evaluator; never infer acceptance from transport success. |
| `response_optimizer` | admitted response transformation | Execute as a capability with classification, budget, and receipt. |
| `savings_ledger` | compatibility value projection | Read the canonical execution/evidence ledger; one writer owns facts. |
| `usage_sink` | authenticated usage-delivery adapter | Emit schema-allowlisted usage only after canonical accounting and policy. |

## Control and execution ordering

The order below is mandatory for every ingress, including CLI, MCP, proxy,
Agent Bus, A2A, and hosted-service continuations:

1. validate schema, bounds, trusted lineage, and tenant/project scope;
2. enumerate the untrusted technical `CapabilityManifestV1` catalogue;
3. resolve explicit product classification/entitlement and apply hard data,
   region, model, egress, safety, budget, lease, and organization policy;
4. materialize the eligible capability catalogue from the filtered manifests;
5. construct the context plan under its token budget;
6. construct the execution plan under cost, latency, quality, and retry budgets;
7. assign an agent/executor and optional Work Graph;
8. execute through bounded adapters;
9. persist canonical and specialized receipts;
10. evaluate outcome from evidence;
11. attribute accepted-path cost and waste;
12. publish typed learning observations.

The raw catalogue is descriptive and untrusted. Only the hard-filtered eligible
catalogue may feed context or execution optimization; a manifest never
authorizes itself.

A missing or invalid admission input cannot be repaired by an optimizer.
Offline/grace behavior must be signed, bounded, and capability-specific.
Community fallback is deterministic and non-personalized. Paid adaptive
learning never becomes a condition for explicitly classified Community
operation.

## Context-plan relationship

The current Context Kernel `ContextPlanV1` is an internal semantic plan with
selected, excluded, and deferred entries and explicit budget accounting. The
protocol `ExecutionPlanV1` currently carries only a context strategy, token
budget, and knowledge references.

The v4 relationship is:

```text
Context Kernel ContextPlanV1
  -> stable versioned wire projection/reference
  -> ExecutionPlanV1.context_strategy + context_budget_tokens + knowledge_refs
  -> ExecutionReceiptV1.context_balance + evidence refs
```

The internal plan may evolve faster than the wire projection. The projection
must preserve its ID/digest, selected/excluded/deferred reason codes, budget,
policy decision reference, and schema version. It must never embed unrestricted
source content merely to make a remote plan self-contained.

## Work Graph, Agent Bus, A2A, and delivery boundaries

These layers solve different problems:

| Layer | Owns | Must not own |
| --- | --- | --- |
| Work Graph | DAG topology, node state, delegation bounds, chain budgets, cancellation, accepted-path projection | user/workspace messaging, transport protocol, task identity, entitlement |
| Agent Bus | authorized messages, handoff, acknowledgement, presence, diary/workspace coordination | scheduling policy, arbitrary content cache, model routing |
| A2A | signed bounded remote envelope, delivery/retry/DLQ/health, interoperable task projection | canonical internal task store, product entitlement, Work Graph planning |
| Scent Field | expiring advisory hints and collision avoidance | durable leases, authorization, completion truth |
| Agent Lease | exclusive bounded resource ownership | human-readable coordination history |
| Delivery Registry | content-addressed deduplication, bounded relay, delivery acknowledgement | workflow state, identity, accepted outcome |

A bus message or A2A task that requests work must reference a canonical task.
A Work Graph node must reference its TaskEnvelope and admitted plan. A delivery
record may satisfy an authorized content reference but cannot authorize access
to that content.

## Identity model

"Agent ID" is no longer an overloaded phrase. V4 uses distinct namespaces:

| Identity | Lifetime and owner | Canonical use |
| --- | --- | --- |
| User/account or service principal | durable; Cloud/organization identity authority | accountability, subscription, role, tenant membership |
| Durable agent identity | durable until revoked; governed identity registry | public key, owner, type, lifecycle, capability claims, attestation refs, allowed workspace/org scope |
| Agent presence registration | ephemeral; Agent Bus presence registry | current process/session, project/workspace, status, heartbeat, expiry |
| Process identity | operating-system lifetime; local runtime | PID/start-time/executable identity used only for liveness and PID-reuse defense |
| Logical session identity | session lifetime; source/editor/workspace | groups tasks and presence without becoming process or task identity |
| Task identity | logical unit of work; trusted Runtime ingress | canonical task/parent/trace lineage |
| Device identity | durable installation/device scope | signed installation or fleet association; never substituted for user or agent |

`core/agent_registry.rs` already provides the public durable lifecycle
registry: owner/role/public-key records, status transitions, attestation,
heartbeat, suspension/resume, decommissioning, and owner offboarding.
`core/agent_identity.rs` provides its local signing-key substrate.
`core/agents/registry.rs` is the separate ephemeral presence and research
messaging registry, despite persisted crash-recovery state. Protocol identity
wrappers provide bounded wire values, not ownership records.

Phase 18 must promote, harden, and reconcile the existing durable registry and
signing substrate with ephemeral presence; it must not invent a replacement
registry or rename ephemeral entries into durable principals.

Presence expires; durable identity does not. Offboarding, suspension, key
rotation, revocation, attestation, and scope changes belong to the durable
registry. Liveness, heartbeat, and status belong to presence.

## OCP and OCLA relationship

OCP is the lower-level open exchange format for context representation,
capability grants/checks, policy packs, evidence-chain entries, and governance
events. OCLA is the capability description and invocation fabric used by the
Decision Spine. OCP does not select or invoke an OCLA capability; OCLA does not
replace OCP governance/evidence formats.

The upstream
[`open-context-protocol`](https://github.com/yvgude/open-context-protocol)
repository is accessible. Its `main` resolved to
`d68f85c3632464dc6f67711311382eb755fdbcda` during this review. Upstream is
`v0.1-draft — pre-review`; its prose is CC-BY-4.0, its schemas are
Apache-2.0, and changes follow the upstream RFC/governance process. Four
vendored schemas match that cut byte-for-byte, but the Engine's vendored
`policy-pack.schema.json` contains additional `filters` and `egress`
fields that are absent upstream. Therefore the mirror is not currently exact.

The divergence must not be normalized as an implicit OCP fork. Before the next
protocol release, either submit those fields through the upstream RFC process
and re-vendor an accepted version, or move them to an explicitly namespaced
LeanCTX extension contract. Until then, the existing file remains compatibility
evidence, not proof that those fields are upstream OCP.

## Learning architecture

Learning shares a versioned observation envelope containing task, decision,
plan, receipt, outcome, data class, policy version, and feature-schema
references. Each learner owns a typed target and reward:

- context-source selection learns from selected/excluded context and accepted
  outcomes;
- read-strategy selection learns from delivered context, latency, and outcome;
- capability/provider selection learns reliability, cost, and outcome by
  admitted capability version;
- agent selection learns task-class fit under identity and policy;
- model selection learns quality/cost/latency under comparable task cohorts;
- fallback selection learns recovery effectiveness and waste.

Context Kernel weights, provider bandits, mode prediction, adaptive scheduler,
routing feedback, and value feedback become adapters to these typed models.
They may share observations and storage primitives, but no learner can update a
different target's policy through an undocumented scalar reward. Rejected and
unknown outcomes remain distinct.

## Migration and deletion map

Deletion occurs only after additive writers, compatibility readers, replay
fixtures, migration telemetry, retention handling, and rollback evidence prove
parity.

| # / old or overlapping concept | Canonical owner | Compatibility and data action | Deletion / acceptance gate |
| --- | --- | --- | --- |
| 1. Task Spine and request/session task IDs | `TaskEnvelopeV1` | Generate at trusted ingress; retain bounded legacy aliases without deriving IDs from content. | Lineage replay proves root/child/retry joins; old task-shaped joins have no writers. |
| 2. Decision Loop and `DecisionLoopRuntime` | Decision Spine with runtime lifecycle adapter | Dual-write decision, receipt, and outcome refs; bound old assessment cache retention. | All ingress paths pass parity and failure-injection tests; rollback reader remains. |
| 3. Context Kernel orchestrator/compiler as peer authority | canonical context-plan stage | Retain candidate, dedup, policy, budget, and receipt behavior; route authority through Spine. | Selected/excluded/deferred and receipt golden fixtures match. |
| 4. legacy compiler, adaptive modes, mode predictor | ContextPlan producers/advisers | Convert outputs to typed proposals; retain model/version provenance, not raw prompt copies. | One ContextPlan writer; shadow comparison meets quality/cost threshold. |
| 5. OCLA reference scheduler | deterministic Community/reference scheduler | Keep public deterministic behavior and fixtures; label it non-adaptive and non-authoritative. | Conformance and Community offline tests pass; no deletion planned. |
| 6. OCLA adaptive scheduler/client/service | paid scheduler executor below policy | Preserve versioned request/result adapter; store decision refs and bounded observations. | Entitlement, policy, rollback, learning, and scheduler E2E pass before old authority is removed. |
| 7. protocol `ControlPlaneDecision` | governance/strategy projection feeding ExecutionPlan | Preserve wire version and context-bundle compatibility; add decision/policy refs. | Local deterministic and Enterprise adapter conformance pass; no unsupported service claim. |
| 8. Work Graph | sole bounded delegation DAG | Add TaskEnvelope/plan refs to nodes; retain graph budgets, stops, fusion, and outcome refs. | Fan-out/depth/concurrency/cost/cancel/replay and accepted-path tests pass. |
| 9. A2A `Task`, store, and `ctx_task` | TaskEnvelope/Work Graph projection | Map transport state to canonical task/node; retain TTL messages/artifacts for compatibility. | No A2A path issues lineage; state migration and round-trip fixtures pass. |
| 10. A2A relay and remote transport | bounded transport adapter | Keep signed envelope, retries, DLQ, rate, health, and scoped retention; exclude raw secrets. | Auth, replay, size, retry, DLQ deletion, and offline tests pass. |
| 11. AgentConnector and OCLA external-process/sidecar paths | one AgentConnector execution layer | Adapt process requests/results; allowlist environment and keep transcripts bounded/private. | Connector parity, timeout/cancel, credential, and receipt tests pass before duplicate launchers archive. |
| 12. `ctx_agent` scratchpad/task/handoff | Local/Workspace Agent Bus | Add authenticated versioned envelope, task refs, tenant/project scope, TTL, ack, and retention migration. | Local durability plus Team tenancy/auth/export/delete E2E pass; old handlers remain during window. |
| 13. OCLA event bus and Context OS events | component lifecycle observation stream | Consolidate bridge/storage and bounded content-free fields; never migrate payloads into paid bus. | Event ordering, bounds, privacy dictionary, and legacy bridge parity pass. |
| 14. Scent claims | derived advisory coordination | Preserve decay/GC and migrate displayed hints from canonical bus/graph events where possible. | Tests prove scent cannot authorize, lock, or mark work complete; no required deletion. |
| 15. AgentLease and ad hoc claims/locks | `AgentLeaseRegistryV1` | Normalize resource refs; migrate live owners with expiry; do not extend stale leases. | Owner/release/expiry/capacity/crash-recovery tests pass; ad hoc authoritative locks removed. |
| 16. Delivery Registry, cache delivery, handoff refs | one content-addressed delivery plane | Preserve digest, recipient, TTL, ack, and authorized relay metadata; migrate content by reference. | Dedup, authorization, eviction, expiry, corruption, and restore tests pass. |
| 17. Sub-Agent Contract, handoff, CCP, A2A transfer | typed Work Graph edge plus checkpoint/evidence refs | Keep published format readers; converge on one content-addressed payload linked to TaskEnvelope. | Determinism, budget, rejection, transfer round-trip, and checkpoint replay pass. |
| 18. durable `agent_registry` versus ephemeral `core/agents` | durable principal registry plus presence projection | Map aliases once; retain key/lifecycle/audit durably while expiring presence/messages by policy. | Rotation, revocation, offboarding, PID reuse, TTL, and migration tests pass. |
| 19. process, session, user, device, workspace, tenant, task IDs | typed identity model | Rename ambiguous fields/add typed projections; preserve bounded old wire values as aliases. | Cross-scope confusion tests fail closed; logs/exports contain no forbidden identity data. |
| 20. kernel, OCLA, Enterprise, and local policies | one ordered hard-policy decision receipt | Translate old rules with source/version; retain audit/retention classification; deny unresolved conflicts. | Cross-engine policy corpus, precedence, rollback, and unknown-rule tests pass. |
| 21. kernel, connector, OCLA, execution, savings receipts | `ExecutionReceiptV1` chain with specialized refs | Dual-write task/plan/agent/decision/evidence refs; retain signed old receipts immutably. | Accounting, signature, replay, migration, and one-factual-writer tests pass. |
| 22. OutcomeEvaluator, Value Gate, OCLA outcome tracker | `AcceptedOutcomeV1` plus value projections | Normalize signals to accepted/rejected/unknown; retain evidence and confidence, not inferred success. | Outcome corpus proves transport/process success is insufficient; value recomputation matches. |
| 23. context/provider/model/agent/fallback learners | typed shared observations with distinct reward models | Version features/rewards; isolate cohorts and retain only policy-allowed bounded observations. | Offline replay, shadow, rollback, drift, and cross-model contamination tests pass. |
| 24. VIA bus/network/mesh meanings | narrow Edge↔remote context-planning protocol | Apply companion ADR; retain accepted protocol adapter and archive generic claims/unmerged seams. | ContextPlan/receipt conformance, auth, egress, fallback, and naming audit pass. |
| 25. OCP vendor mirror | upstream-authoritative OCP plus named LeanCTX extension | Preserve current bytes for compatibility; upstream RFC/re-vendor or move drift to namespaced schema. | Five-schema comparison and OCP conformance pass with no silent Engine-only fields. |
| 26. telemetry, usage, savings, and cost writers | canonical event/evidence ledger and projections | Define allowlisted event dictionary; migrate aggregates with retention/deletion state, never content. | One factual writer, schema/privacy, opt-out, deletion, and projection parity tests pass. |
| 27. archived orchestration generations | explicit archive | Preserve provenance and explanatory history; exclude archived modules/docs from live authority navigation. | Replacement evidence and rollback reference are published before live code is removed. |

## Compatibility sequence

For every migrated contract:

1. define a schema-versioned canonical type and owner;
2. add bounded compatibility readers for the old representation;
3. dual-write canonical references without changing accepted old bytes;
4. run replay, conformance, migration, and failure-injection tests;
5. move all decision authority to the canonical owner;
6. publish deprecation and rollback windows;
7. delete or archive only after observed old-reader/writer use reaches the
   approved threshold and rollback evidence exists.

No migration may silently enable a paid capability, upload content, change an
outcome from unknown to accepted, or drop unknown forward-compatible fields.

## Product and license boundaries

The Decision Spine is architecture, not a tier. Individual capabilities are
classified separately.

- Community and Trust Core provide deterministic, explicitly classified local
  capabilities and open protocols.
- Pro adds personal adaptive Context/Execution Autopilot and bounded local
  Agent Bus/Work Graph capabilities.
- Team adds tenant-scoped durable Workspace Bus and distributed Work Graph.
- Enterprise adds governed identity, policy, fleet control, audit, and
  deployment choices.

Physical locality does not determine price or license. Existing Apache code
remains Apache; new private service intelligence remains behind explicit
interfaces. Unknown capability classification fails closed.


## Validation evidence

- Source signatures confirm one protocol `TaskEnvelopeV1`, one Rust
  `ContextPlanV1`, and the current `ExecutionPlanV1`,
  `ExecutionReceiptV1`, and `AcceptedOutcomeV1` contracts.
- `ExecutionPlanV1` and `ExecutionReceiptV1` were inspected field-for-field;
  neither currently carries explicit executor agent identity.
- The OCLA registry and `builtin/mod.rs` census account for 15 discoverable
  capability kinds and all 16 built-in implementation files.
- Work Graph bounds, Agent Lease ownership, Scent decay, Agent Bus presence,
  A2A transport, Delivery Registry, and Sub-Agent Contract were mapped from the
  executable sources named above.
- Upstream OCP `main` was fetched at
  `d68f85c3632464dc6f67711311382eb755fdbcda` and all five schemas were
  compared against the Engine mirror; four match and policy-pack differs.
- The companion VIA decision was derived from the accepted private evidence cut
  and intentionally excludes private operational topology.

## Unresolved implementation gates

This ADR assigns ownership; it does not claim the target pipeline is already
wired. Later phases must still implement and test:

- the machine-readable product capability/entitlement registry and fail-closed
  admission path in Phase 2;
- the ContextPlan wire projection, executor agent references in plan and
  receipt, canonical decision/receipt chain, and compatibility fixtures in
  Phase 4;
- one executable Decision Spine integration across every ingress;
- durable governed agent identity and its mapping to presence/process/session;
- Agent Bus, Work Graph, A2A, and VIA product/security gates;
- typed learning observations and one factual evidence/usage writer;
- OCP RFC/re-vendoring or a namespaced LeanCTX policy extension;
- migrations, shadow/replay evidence, rollback, and deletion thresholds for
  every row above.

## Consequences

The system gains one auditable task-to-outcome lineage while retaining useful
specialized components. Policy, entitlement, scheduling, execution, outcome,
and learning have explicit order and ownership. Work Graph, Agent Bus, A2A,
Scent, leases, and delivery no longer compete for the word "orchestration."

The migration requires additive contract work before module deletion.
`ExecutionPlanV1` needs a version-safe executor assignment relationship, the
ContextPlan wire projection must be stabilized, governed agent identity remains
unimplemented, and OCP policy-schema drift must be resolved upstream or
namespaced.

## Alternatives rejected

- A new v4 orchestration crate beside existing systems was rejected because it
  would add another authority before removing any duplicate.
- Making OCLA the entire control plane was rejected because a capability fabric
  must not authorize itself or own task/outcome semantics.
- Making Work Graph the task system was rejected because graph-node lifecycle
  and durable task lineage have different scopes.
- Treating Agent Bus, A2A, and OCLA Event Bus as one bus was rejected because
  authorization, durability, transport, and event semantics differ.
- Reducing acceptance to successful transport or process exit was rejected
  because it cannot prove user value.
- Combining every learning signal into one reward was rejected because cost,
  quality, latency, and accepted outcome are not interchangeable.

## Phase-1 acceptance gate

Phase 1 is complete only when this ADR and the VIA companion ADR are reviewed
together and the reviewer confirms:

- every Phase-1 subsystem and all 16 OCLA built-ins have one disposition;
- the canonical task, context, execution, receipt, outcome, and evidence chain
  has one owner at every stage;
- identity namespaces and bus/graph/transport/delivery boundaries are explicit;
- OCP/upstream drift and VIA are resolved without silent forks;
- every duplicate has an additive migration, deletion, or archive path;
- Phase 2 implementation is constrained by these decisions.
