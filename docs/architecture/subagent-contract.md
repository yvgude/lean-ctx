# Sub-Agent Contract

Status: V4 target contract with a local deterministic briefing/return parser;
full task-lineage, policy, evidence and cross-agent integration remain incomplete.

## Purpose

The Sub-Agent Contract defines the smallest deterministic information exchange
between a parent task and a bounded child task. It gives the child an explicit
objective, budget, relevant facts and return schema, then converts the result
into structured candidate facts instead of retaining an unbounded transcript.

It is not an agent launcher, scheduler, handoff bundle, ContextCheckpoint,
message channel, task identity or outcome evaluator.

## Canonical lifecycle

```text
parent TaskEnvelopeV1 + ExecutionPlanV1
  → bounded child task and outcome contract
  → deterministic briefing pack
  → AgentConnector / Work Graph execution
  → structured child return
  → schema + policy + evidence validation
  → parent result fusion
  → child ExecutionReceiptV1 + AcceptedOutcomeV1
  → approved knowledge/context update
```

Every child receives a new canonical `task_id`, references its
`parent_task_id`, inherits the trace, and receives explicit token, cost,
latency, capability, model/provider, retry and stop bounds. The contract cannot
create identity or enlarge the parent authorization.

## Briefing pack

The V1 local implementation contains:

- `contract_version`;
- the child task text;
- requested and used token counts;
- a project hash;
- deterministically selected structured facts; and
- an explicit return-format instruction.

Facts contain category, key and value. Selection uses current project knowledge
ranked deterministically, then fills greedily within the fact budget. Stable
field and fact ordering makes identical inputs serialize byte-identically for
diffing, caching and replay.

The task text is always present in the current implementation and its tokens
are counted before facts. The canonical builder now rejects a budget smaller
than that mandatory task overhead; a pack never claims to satisfy a total
budget merely because optional facts were omitted.

The production V4 contract additionally binds:

- child, parent and trace identity;
- task class, complexity, risk and outcome contract;
- policy, entitlement, classification and workspace references;
- allowed capabilities, data sources and mutations;
- context/checkpoint and execution-plan digests;
- token, cost, latency, fan-out, retry and deadline bounds;
- expected evidence and receipt schema; and
- cancellation and fallback behavior.

Raw secrets, ambient environment, unrelated conversation history and
unbounded repository content are never implicit inputs.

## Structured return

The current V1 return grammar accepts one self-contained fact per line:

```text
category/key: value
```

Malformed or empty entries are returned as rejects rather than silently lost.
Parsed lines are candidate facts, not trusted knowledge. Before integration the
parent validates:

- schema/version and child task binding;
- category/key/value bounds and allowed classifications;
- evidence and source references;
- contradiction with authoritative state;
- freshness and replay identity;
- secrets, paths, prompt/source leakage and injection content; and
- whether the child outcome was accepted, rejected or unknown.

Only approved, evidence-backed information enters durable knowledge. Raw child
transcripts remain ephemeral unless an explicit retention contract permits
them.

## Distinction from adjacent contracts

| Contract | Owns | Does not own |
| --- | --- | --- |
| Sub-Agent Contract | Deterministic briefing and structured child return. | Execution, durable context state, cross-session transfer or acceptance. |
| Handoff | Explicit context/task transfer between agents or sessions. | Child task decomposition or result fusion. |
| ContextCheckpoint | Versioned resumable context state at a task boundary. | Child instructions or agent scheduling. |
| Work Graph | Bounded decomposition, dependencies, budgets, cancellation and fusion. | Briefing serialization or knowledge acceptance. |
| AgentConnector | Starts and controls a supported agent process. | Task policy, briefing contents or accepted outcome. |
| Agent Bus | Presence, messaging, claims and delivery coordination. | Child execution authority or outcome truth. |
| Outcome Evaluator | Determines accepted/rejected/unknown from required evidence. | Briefing or execution scheduling. |

The Sub-Agent Contract may travel through handoff or Agent Bus transport and
may reference a checkpoint, but those projections cannot alter its task,
budget, policy or lineage.

## Product boundaries

### Community

Community retains manual handoff/export and public task/context/evidence
contracts. It may construct explicit local briefing data, but does not receive
the complete automatic multi-agent execution and synthesis product.

### Pro

Pro includes deterministic local briefing packs, supported connector launch,
bounded Work Graph children, structured return synthesis, child receipts,
outcome evaluation and accepted-path/waste attribution for one account.

### Team

Team adds authenticated workspace-scoped briefing sources, shared checkpoints,
multiple users, roles, seat enforcement, durable child history and
cross-machine synthesis. Personal facts require explicit promotion before they
become shared workspace context.

### Enterprise

Enterprise adds governed identity, RBAC, capability and delegation policy,
classification/region/provider restrictions, retention, audit, offboarding and
supported VPC/on-premise/air-gap execution.

## Security and reliability invariants

- A child can only narrow inherited authority and budgets.
- The briefing is deterministic for the same canonical inputs and policy.
- Every pack and return is bounded by bytes, tokens, facts and field lengths.
- Unknown contract versions fail closed at execution and durable integration.
- Parent/child identity, plan, checkpoint and receipt digests prevent
  substitution and cross-task replay.
- Cancellation stops new work and records whether process termination was
  actually confirmed.
- Child exit zero or a well-formed return does not imply accepted outcome.
- Rejected or unknown child outcomes are not learned from as accepted.
- A malformed return cannot erase valid prior knowledge or partially mutate
  shared state.
- Secrets, raw prompts, source, paths and unrestricted tool output do not enter
  telemetry or shared knowledge.
- Deterministic Community/manual fallback remains available when paid
  orchestration is disabled.

## Current implementation map

| Surface | Evidence and limitation |
| --- | --- |
| `core::subagent_contract::SubAgentContractV1` | Local pack fields and deterministic pretty-JSON ordering; crate-private, not a complete public/wire contract. |
| `build_briefing_pack` | Deterministic relevance selection, total-budget rejection and fact filling; lacks full task/policy/receipt bindings. |
| `parse_return_lines` | Structured facts plus explicit rejects; does not itself validate evidence, policy or acceptance. |
| `ctx_agent` briefing/return actions | Reachability adapter into local knowledge; tier and end-to-end authorization still require proof. |
| Work Graph and AgentConnector | Intended execution owners; integration must be proven rather than inferred from coexisting modules. |

## Acceptance gates

1. Schema includes canonical child/parent/trace, policy, budget, plan,
   checkpoint, evidence and outcome bindings.
2. Two supported local agents complete a real bounded child task through
   briefing, connector execution, structured return, fusion, receipt and
   accepted outcome.
3. Oversized task, insufficient mandatory overhead budget, malformed return,
   stale/replayed pack, identity mismatch, cancellation, timeout, connector
   failure and contradictory facts fail safely.
4. Team proves authorized cross-user/cross-machine briefing and synthesis with
   no personal-to-workspace or cross-tenant leakage.
5. Enterprise proves delegation denial, revocation, retention, audit and
   governed deployment behavior.
6. Capability/entitlement, telemetry, migration, packaging, cross-platform and
   security gates pass.

Until these gates pass, the local V1 implementation is a useful deterministic
primitive, not proof of a complete automatic sub-agent product.

## Related architecture

- `docs/architecture/decision-spine.md`
- `docs/architecture/agent-bus.md`
- `docs/architecture/work-graph.md`
- `docs/architecture/context-checkpoint.md`
- `docs/architecture/cross-agent-delivery.md`
- `docs/architecture/agent-identity.md`
