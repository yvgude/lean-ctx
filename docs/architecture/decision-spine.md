# Decision Spine

Status: canonical V4 target architecture; consolidation and full end-to-end
acceptance remain incomplete.

## Purpose

The Decision Spine is the single lineage and decision loop for context and
execution. It consolidates the Task Spine, Decision Loop, Context Control
Kernel, OCLA scheduler, protocol control plane, Work Graph, AgentConnector,
A2A, Value Gate and Outcome Evaluator. None of these may remain a competing
source of execution truth.

```text
TaskEnvelopeV1
  → profile / triage
  → capability catalogue
  → hard policy filter
  → ContextPlan
  → ExecutionPlanV1
  → bounded execution / Work Graph
  → ExecutionReceiptV1
  → AcceptedOutcomeV1
  → value and accepted-path attribution
  → typed learning
```

## Canonical identities

### Task

`TaskEnvelopeV1` is the execution-lineage identity. Every child task, plan,
invocation, receipt, outcome, checkpoint and evidence artifact references it or
a versioned projection that preserves the same identity. Surface-specific task
types are adapters, never new lineage roots.

The envelope carries or binds identity, parent/trace lineage, task class,
complexity, risk, quality requirement, cost/latency budget, classification,
region/model policy and the outcome contract.

### Context decision

There is one ContextPlan authority. The Context Control Kernel gathers and
deduplicates candidates, scores them using explicit signals, applies policy,
compiles under a token budget and records selected, excluded and deferred
sources with reason codes. Pro may choose adaptive strategies around that
authority; it does not create a second ContextPlan model.

A context receipt binds candidate inputs, policy, budget, decisions and the
resulting context artifact to the task. Outcome evidence feeds future decisions
only after accepted evaluation.

### Execution decision

`ExecutionPlanV1` is the canonical wire contract when version compatibility is
preserved. It covers context strategy, capabilities, agent identity,
model/provider, reasoning/token/cost/latency budgets, retries, fallbacks, stop
conditions, policy decision and scheduler decision.

Hard policy runs before learned ranking. A private scheduler can propose a
choice behind the public contract, but cannot bypass identity, entitlement,
classification, egress, budget or audit policy.

## Execution boundary

Execution occurs through a supported AgentConnector or capability invocation.
A bounded Work Graph may decompose the task only within explicit fan-out,
depth, concurrency, cost/token inheritance, retry and stop limits. Every node
keeps parent/trace identity, its own context checkpoint, mutation leases,
receipt and outcome.

Fallbacks are explicit plan decisions, not hidden retries. Unknown external
effects remain unknown/pending until reconciled. Cancellation requests and
confirmed process stops are distinct states.

## Receipt chain

Specialized receipts may remain, but project into one coherent chain. The
canonical execution receipt answers:

- task, parent and trace;
- context and execution plan versions/digests;
- selected agent, model, provider and capabilities;
- actual token classes, reported cost, latency and retries;
- policy/scheduler/fallback decisions;
- evidence, baseline and outcome reference;
- accepted-path contribution, avoided cost and waste.

Receipt success does not mean value acceptance. HTTP 200, tool return or agent
exit zero is execution evidence only.

## Outcome and value

`AcceptedOutcomeV1` and OutcomeEvaluator form the single outcome direction.
Outcome is tri-state:

- `accepted`: required evidence proves the outcome contract;
- `rejected`: evidence contradicts the contract;
- `unknown`: required evidence is absent or inconclusive.

Value Gate, CPAO/ETPAO, cost-per-outcome and contribution attribution consume
accepted outcome evidence. They never manufacture acceptance from transport or
process status. Accepted-path attribution separates useful execution from
retry, abandoned and redundant branch waste.

## Learning boundary

One typed learning subsystem consumes comparable, privacy-safe observations.
Its models remain separate for context-source, read strategy,
provider/capability, agent, model and fallback selection. Common observation
contracts are allowed; incomparable rewards are not collapsed into one scalar
without evidence.

Learning updates require stable task/plan/outcome lineage and cannot override
hard policy. Explicit user choices take precedence where permitted. With Pro
disabled or unavailable, deterministic Community planning and execution remain.

## Existing implementation families

The accepted capability census identifies current inputs rather than declaring
consolidation complete:

- context compiler, Context Field, relevance, budgets and Context Kernel;
- task spine, decision-loop runtime and protocol execution/outcome contracts;
- OCLA registry/runtime/reference scheduler and capability manifests;
- AgentConnector, Work Graph and A2A transport;
- execution/context/evidence/savings ledgers and receipt documents;
- Value Gate, CPAO/ETPAO and outcome evaluators;
- mode predictors, provider bandits, adaptive routing and feedback loops.

Each family must either own one step, adapt to the owner, or be archived after
dependency and migration proof. Merely renaming duplicates is not consolidation.

## Required invariants

- One task identity and one owner per decision stage.
- Versioned schemas and deterministic canonical signing where authority crosses
  a process, device or trust boundary.
- Stable digests connect task, inputs, policies, plans, receipts and outcomes.
- No learning from rejected/unknown outcomes as if accepted.
- No paid/private implementation weakens public audit/interoperability contracts.
- No telemetry includes task content, paths, prompts, secrets or raw evidence.
- Replays, retries and restarts preserve idempotency and lineage.
- Migration keeps legacy readers/adapters until compatibility evidence permits
  removal; rollback is tested or fails closed.

## Open acceptance gates

1. Prove the runtime has one authoritative producer for each task, ContextPlan,
   ExecutionPlan, receipt and outcome contract.
2. Map every legacy planner/ledger/scheduler to owner, adapter or deletion with
   migration and dependency evidence.
3. Run a real bounded supported agent path from admission through accepted
   outcome and learning, with exact lineage assertions.
4. Prove Community deterministic fallback after Pro is disabled mid-flow.
5. Prove policy denial, budget exhaustion, unknown outcome, cancellation,
   retry/fallback and rollback without false acceptance or double effects.
6. Demonstrate accepted-path versus waste attribution across a bounded local and
   distributed Work Graph.
7. Complete schema, security, privacy, cross-platform, packaging and release
   gates at an accepted revision.

Until these gates pass, the Decision Spine is the governing target and partial
implementation boundary, not a finished product claim.
