# LeanCTX V4 architecture overview

Status: implementation in progress. This is an architecture boundary document,
not a release, availability, licensing, or revenue-readiness claim.

## Product thesis

LeanCTX monetizes two compounding decisions:

1. What context should a task receive?
2. How should that task execute?

The local Rust engine remains the inspectable trust base. Paid tiers add
adaptive intelligence, coordinated shared execution, and organizational
governance without making Community unsafe or unusable.

## Canonical tiers

| Tier | Product boundary | Required invariant | Current evidence class |
|---|---|---|---|
| Community | Context Toolkit + deterministic reference runtime | Useful local-first operation without account or hosted dependency | Production and preview primitives exist; full V4 gate open |
| Pro | Context + Execution Autopilot, Local Agent Bus, bounded local Work Graph | Material adaptive/automation gain; clean deterministic fallback when disabled | Multiple local slices tested; complete paid lifecycle not proven |
| Team | Shared organizational context + distributed workspace Work Graph | Cross-user/device scope, provenance, authority, bounded budgets and conflict handling | Partial isolated work; shared-context product gate open |
| Enterprise | Governed context and agent execution infrastructure | Identity, owner, policy, audit, deployment and compliance controls | Governance slices exist; complete surface and integration open |
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
authorities. Community uses deterministic reference decisions; Pro may learn
within policy and evidence bounds.

### Context Autopilot

Pro selects sources, read modes, budgets, preload, relevance, pressure,
consolidation, memory injection and checkpoints using measured outcomes.
Explicit user choices and policy override learning. Disabled or unavailable Pro
must fall back to deterministic Community behavior without data loss. The
planner contract, safety invariants and learning bounds are in
[`autopilot.md`](autopilot.md).

### Execution Autopilot and capability fabric

Execution selection is bounded by supported connectors, capability manifests,
policy, cost, latency, quality and reversibility. OCLA candidates run in shadow
until evidence justifies promotion. A private optimizer may improve decisions;
the public contract and receipts remain sufficient for audit/interoperability.
The selection and binding contract is in
[`execution-autopilot.md`](execution-autopilot.md); the adapter layer it
executes through is in [`agent-connectors.md`](agent-connectors.md).

### Orchestration

Pro owns live single-user Local Agent Bus and bounded local Work Graph. Team
extends these across users, devices and machines with explicit parent/child
lineage, fan-out/depth/concurrency limits, inherited budgets, leases,
cancellation, resumability, result fusion and accepted-path/waste attribution.
Unlimited agent spawning is outside the product contract.

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

The entitlement model is in [`entitlements.md`](entitlements.md); the telemetry
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

Known open fronts include the complete paid revenue lifecycle, Pro adaptive and
execution lineage, shared Team authority/conflicts, Enterprise integration,
telemetry privacy/offline completion, licensing/legal artifacts, packaging,
cross-platform gates, migrations, repository consolidation and final public
claim verification.

## Completion rule

V4 completes only when every applicable Master Prompt requirement and hard gate
has authoritative evidence at an accepted repository revision and, where
required, deployed runtime state. The final implementation report must bind each
claim to repository, branch/commit, test or external gate, and known rollback.
