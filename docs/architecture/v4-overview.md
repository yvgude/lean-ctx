# LeanCTX V4 architecture overview

Status: implementation in progress. This is an architecture boundary document,
not a release, availability, licensing, or revenue-readiness claim.

## Product thesis

LeanCTX is the Context Gateway for AI systems: it selects the context a task
may receive, controls its use and destination, and proves what was delivered
(Select → Control → Prove). The host keeps the model, the agent loop and the
decision how a task executes.

The local Rust engine remains the inspectable trust base. Paid tiers add
adaptive context intelligence, shared and coordinated team context, and
organizational governance without making Community unsafe or unusable.

Coordination context is part of the gateway: agent presence, leases, the agent
bus and the work graph tell agents who works on what, what is held, what was
handed over and what is done. LeanCTX delivers that state as governed context;
it does not start, schedule, assign or retry agent work.

## Canonical tiers

| Tier | Product boundary | Required invariant | Current evidence class |
|---|---|---|---|
| LeanCTX OSS | The complete local engine: Context Toolkit, context ranking, Context Autopilot planner, memory and consolidation, local coordination context (Local Agent Bus, presence, leases, bounded local Work Graph), local Cockpit | Free and complete; useful local-first operation without account, licence or hosted dependency; no paid-better variant of a local behaviour | Production and preview primitives exist; full V4 gate open |
| LeanCTX Hive Personal | Control and proof for one person: filter packs, personal protection rules, code-security hints, semantic detectors | Adds governance on top of OSS; removing it leaves OSS behaviour unchanged | Runtime slices tested; complete paid lifecycle not proven |
| LeanCTX Hive Team | Authorized shared context + team coordination context (workspace bus, presence, leases, shared Work Graph), Cockpit across engines | Cross-user/device scope; a shared artifact is released only against the receiver's own checkout of its source; provenance, authority and conflict handling | Team journeys pass against a real Hub; release gate open |
| LeanCTX Hive Enterprise | Governed context and governed coordination for agents | Identity, owner, org policy, audit, deployment and compliance controls | Governance slices exist; complete surface and integration open |
| SDK/OEM | Stable commercial integration channel | Public contracts and trust boundary remain interoperable | Requires final licensing/package/release evidence |

An implementation file, schema, test or dashboard does not by itself promote a
capability to a product tier. Availability requires its public route,
entitlement, failure behavior, migration, documentation and end-to-end gate.

## Architectural layers

### Trust Core

Community-owned, deterministic, local-first primitives: bounded reads/searches,
compression, recovery, caches, indexes, redaction, explicit modes, manual
knowledge/session/handoff/export operations, budgets, capability inspection and
public OCLA/scheduler contracts. Trust Core must not depend on Team or
Enterprise services.

### Decision Spine

One canonical lineage connects task admission to ContextPlan, ExecutionPlan,
capability/connector execution, receipts, outcomes, Value Gate and learning.
Legacy planners, ledgers and policy engines are inputs or adapters, not parallel
authorities. The OSS engine uses deterministic reference decisions and learns
only locally, within policy and evidence bounds; no licensed runtime changes
a planning decision.

### Context Autopilot

The OSS planner selects sources, read modes, budgets, preload, relevance,
pressure, consolidation, memory injection and checkpoints locally. Explicit user
choices and policy override learning. An adaptive variant that learns from
measured outcomes is off until an A/B result against the deterministic planner
justifies it; it is never a paid tier, and disabling it must fall back to
deterministic behaviour without data loss. The
planner contract, safety invariants and learning bounds are in
[`autopilot.md`](autopilot.md).

### Execution Autopilot and capability fabric (maintenance only)

An existing contract, kept compatible and not expanded: execution, model choice
and task scheduling belong to the host. Execution selection is bounded by supported connectors, capability manifests,
policy, cost, latency, quality and reversibility. OCLA candidates run in shadow
until evidence justifies promotion. A private optimizer may improve decisions;
the public contract and receipts remain sufficient for audit/interoperability.
The selection and binding contract is in
[`execution-autopilot.md`](execution-autopilot.md); the host launches agents,
and how LeanCTX binds and controls a host-launched child is in
[`work-graph-host-start.md`](work-graph-host-start.md).

### Coordination context

LeanCTX OSS owns the live single-user Local Agent Bus and bounded local Work
Graph. LeanCTX Hive Team extends these across users, devices and machines with explicit parent/child
lineage, leases, presence, handoff and accepted-path attribution. A message
leaves a machine only through the egress admission; what a member wrote is
foreign content to its readers. LeanCTX records and serves the coordination
state; agents and their hosts act on it. Spawning, scheduling and executing
agents is outside the product contract.

### State and synchronization

Local artifacts remain content-addressed, bounded and recoverable. Hosted or
shared state requires authenticated device/workspace identity, encryption,
versioned checkpoints, conflict semantics, retention and deletion. Personal,
Team and authoritative context are distinct promotion states, not implicit
copies of one another.

### A2A and governance

The A2A layer uses authenticated, scoped, replay-protected transport. Its
detailed boundary is in [`a2a-transport.md`](a2a-transport.md). Enterprise adds
durable Ed25519 identity, accountable ownership, lifecycle, attestation/drift,
RBAC/SSO/SCIM, signed policy/evidence, budgets, residency and deploy controls.
Revoked or missing authority must fail closed.

### Commerce and telemetry

Entitlements gate paid intelligence; cached plan labels or UI claims are not
authority. The revenue acceptance chain is checkout → public signed webhook →
paid signed entitlement → real paid product effect → cancellation/downgrade.
Trial conversion, Team seat billing and revenue-dashboard correctness are
separate gates.

Telemetry defaults and migration must be explicit, privacy-bounded and
inspectable. Content, paths and secrets never enter telemetry. Explicit opt-out
must survive upgrades; deletion needs a real remote or documented admin path.

Plans and their rights live in LeanCTX Hive; the telemetry
contract is in [`telemetry.md`](telemetry.md), with per-field definitions in
[`../privacy/TELEMETRY-DATA-DICTIONARY.md`](../privacy/TELEMETRY-DATA-DICTIONARY.md).

## Cross-cutting invariants

- Every external input is bounded before allocation or persistence.
- Tenant/project/workspace authority comes from authenticated policy, never
  caller-controlled labels alone.
- Decisions, execution, receipts and outcomes share stable lineage.
- Unknown effects remain pending until reconciliation; retries never assume a
  failed response means no effect.
- Local/manual fallback stays available when paid or hosted services fail.
- Private keys and secrets never enter configuration reports, logs or telemetry.
- Public claims follow executable evidence and deployed state.
- Schema and data migrations preserve prior explicit privacy/security choices
  and provide tested rollback or fail-closed recovery.

## Current acceptance state

The authoritative capability census records source-level maturity and intended
V4 decisions. The central V4 status records tested/deployed slices and their
limits. Neither shows all hard gates complete.

Known open fronts include the complete paid revenue lifecycle, adaptive
planning evidence and execution lineage, shared Team authority/conflicts, Enterprise integration,
telemetry privacy/offline completion, licensing/legal artifacts, packaging,
cross-platform gates, migrations, repository consolidation and final public
claim verification.

## Completion rule

V4 completes only when every applicable Master Prompt requirement and hard gate
has authoritative evidence at an accepted repository revision and, where
required, deployed runtime state. The final implementation report must bind each
claim to repository, branch/commit, test or external gate, and known rollback.
