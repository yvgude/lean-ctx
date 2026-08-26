# LeanCTX Complete Implementation Roadmap

> **Status:** Active delivery architecture. This is the one dependency-ordered
> SDK-first program for turning the Engine substrate into a premium Production
> SDK, proving adoption, and only then considering Workspace or Cloud expansion.
>
> **Classification:** Canonical internal delivery sequence for the Engine,
> Production SDK, later Workspace work, and optional Cloud research.
>
> **Authority:** The [public README](../../../README.md) controls public claims;
> the [Master Execution Plan](../execution/MASTER-EXECUTION-PLAN.md) schedules
> approved work; the [P4 extraction report](../P4-SDK-EXTRACTION-AND-ENGINE-REFACTOR-REPORT.md)
> records the current Engine/SDK boundary. Code, tests and released artifacts
> override every plan.

## 1. Outcome and product boundary

```text
Engine contract and evidence
      ↓
BSL 1.1 Production SDK product
      ↓
real customer / embedded adoption
      ↓
SDK hardening and repeatability
      ↓
optional Workspace and Cloud research later
```

LeanCTX succeeds when a capable customer who could build on the Apache Engine
chooses the BSL 1.1 Production SDK, integrates Select → Shape → Reuse → Recover,
keeps it after comparison, and expands it without founder-only work. The host
retains its task, agent, model, prompts, tools, orchestration and product.

The program must never turn LeanCTX into an agent builder, scheduler, sandbox,
generic memory product, vector database, marketplace or mandatory Cloud.

## 2. Rules for every work item

No feature starts without:

1. **Placement:** Engine mechanism, SDK semantic, Cloud coordination, or Host
   responsibility is explicit.
2. **Open-core class:** owner/repository, executes-versus-decides boundary,
   data class and public/private contract are recorded.
3. **Contract:** version, compatibility range, canonical bytes/identity where
   relevant, fixtures, failure behavior and deprecation policy are specified.
4. **Security admission:** path/sandbox posture, identity/scope, policy,
   data mode, signer/trust state and local-degradation behavior are explicit.
5. **Evidence:** named workload, baseline where a result is claimed, quality
   threshold, measured-versus-estimated distinction and acceptance tests.
6. **Rollback:** compatibility/migration path and a bounded reversal mechanism.

An internal module, benchmark, test fixture or published package does not
promote a surface to Available.

> **Build less surface. Make the surface exceptional.** Lean means fewer
> concepts, states, contracts and divergent truths—not minimum LOC. Testing is
> proportional to risk; test count is not a KPI. One real vertical slice is
> more valuable than large amounts of unintegrated infrastructure.

## 3. Cross-cutting gates

| Gate | Required before promotion |
| --- | --- |
| **Local-first** | Engine and SDK work without account, Cloud or private imports. |
| **Protocol** | Strict versioned schema, fixtures, unknown-field policy, compatibility matrix and owner. |
| **Trust** | Explicit integrity, signer trust and authorization states; self-attested or unsigned artifacts never satisfy a governed claim. |
| **Data** | Default payload is bounded metadata/source references; raw source, prompts, secrets and credentials require explicit policy and are never implicitly transported. |
| **Quality** | Same workload/control conditions, declared quality floor and accepted-outcome result; token reduction alone is diagnostic only. |
| **Operations** | A hosted surface has tenant isolation, retention/deletion, audit, incident/support owner, residency/jurisdiction review and a local outage path. |
| **Release** | Rust/Python compatibility fixtures, deterministic tests, docs and a user path are green; migration and rollback are documented. |

## 4. Program sequence

### P0 — Truth, ownership and containment

**Objective:** make one plan and one status language govern all work.

**Deliverables**

- This roadmap, the authority map and a decision ledger.
- A support matrix for local CLI/MCP/proxy, Python Preview, Rust embedding
  substrate and non-public compatibility modules.
- Named owners for protocol, runtime, SDK, evidence, package, security, Cloud
  and release decisions.
- A public-surface inventory that explicitly quarantines broad experimental or
  legacy protocol/control-plane exports until a narrow compatibility policy,
  package namespace/license and support owner are approved.
- A ledger that classifies legacy Cloud/registry, Agent Bus, OCLA and package
  surfaces as Available, Preview, Research, legacy or retired.
- Active internal authorities link only to files that exist in this reduced
  documentation set; removed roadmap/strategy paths are not normative.

**Exit:** no active document can override the public README and the named
authorities above; every planned public object has an owner, status, repository
class and evidence gate.

### P1 — Engine invocation spine

**Objective:** prove one native local mechanism through a stable Engine path.

**Deliverables**

- Versioned Engine invocation, policy-admission, resolved identity,
  observation, failure and receipt-link contracts.
- One native capability (compression is the reference) dispatched through the
  production runtime, not only a test adapter.
- Capability ID/version, input/output/source digests, policy decision,
  measurement classification and recovery reference preserved end-to-end.
- Engine compatibility policy, golden JSON fixtures and a narrow public
  facade/migration stance for existing low-level Engine surfaces.

**Exit:** the same local invocation is deterministic, policy-gated,
observable, recoverable and receipt-linked through a real user path; legacy
dispatch remains compatible until measured parity.

**Status (2026-08-23): exit met.** PRs `#1515` and `#1516` established the
production `ctx_read` v1 path and closed descriptor/handle containment,
failure-atomic publication and cross-platform adversarial coverage. Final CI
run `32662146660` passed native Ubuntu and Windows tests; the latest P1 merge
commit `dd55473302` is the P2 integration base.

### P2 — Canonical local evidence

**Status (2026-08-24): exit met.** PR `#1522` is integrated at
`github/main@8ebf61a21c063a1d0a86be33511588d27d7ca71e`; exact-SHA CI run
`32758619963`, Security Check `32758619955`, and CodeQL `32758619966` passed.
Delivery remains a factual Engine result; accepted quality requires explicit
host/evaluator evidence.

**Objective:** establish one evidence truth before claims, tuning or Cloud.

**Deliverables**

- One owner and additive migration decision for session/run receipts, receipt
  builder, ledger, chain, bundle and verifier records.
- A canonical identity map from Task → Plan → Invocation → Receipt → Outcome;
  projections retain their relationship but no duplicate object becomes truth.
- Immutable receipt construction with task/plan/identity/policy/capability
  links; durable chain storage; explicit measured, estimated, calculated,
  reconciled and accepted-outcome fields.
- Independent offline verifier with strict tamper, wrong-key, missing-field,
  stale and unknown-payload failures.
- One provider-free reproducible fixture and one real local reference workload
  with baseline/treatment, declared quality threshold and methodology.

**Exit:** a third party can reproduce and reject tampered evidence without
trusting the producer; no universal performance or savings claim is needed.

### P3 — Supported SDK Preview vertical slice

**Status (2026-08-25): P3 COMPLETE/INTEGRATED.** Merge
`e47cb432f2e9e2d7ecf13e3c85a0d1cc4fa68f96` passed exact-baseline CI
`32778481164`, Security `32778481373`, and CodeQL `32778481121`. Candidate
`79e3888862659e5ac0623a8fa00e16155aa144da` proves the Python-owned Preview
lifecycle through a strict local Engine-operation transport, native Embed, the
actual OpenAI Agents SDK, exact recovery, factual receipts and explicit
degradation. `LIVE PROVIDER SMOKE: UNVERIFIED`.
P4 subsequently completed through the integrated seam; no P5 delivery or Cloud
work was integrated.

**Objective:** make the existing Python reference path function end-to-end.

**Deliverables**

- A declared support matrix: only the named OpenAI Agents wrapper is Preview;
  compatibility imports and experimental adapters are not silently supported.
- A resolved decision for Python `/v1/sessions/*` expectations: implement
  compatible local runtime routes, or narrow/remove the route-dependent façade
  with migration tests. Do not leave a false lifecycle promise.
- Deterministic `ContextSession → ContextSource/View → ContextPlan →
  ContextReceipt` reference demo for a custom host without internal `ctx_*`
  tool calls.
- Source lineage, budget, recovery, restart/abort, degraded/fail-open and
  no-Cloud tests. Python/Rust protocol fixtures must agree.

**Exit:** a clean user can complete the documented Preview quickstart, recover
exact source, receive a truthful local receipt and roll back without changing
the host agent loop.

### P4 — Separate BSL 1.1 Production SDK repository

**Status (2026-08-26): P4 V4 TECHNICAL/INTERNAL EXIT COMPLETE.** The separate private SDK
head is `8c84e224`; ledger `a7afd1f` binds immutable implementation
`11f77debc2` and wheel `b54ac013…`. Independent internal-RC evidence passes
Python 3.9/3.14 (`19/19` each), the exact CPython 3.9 macOS-arm64 offline wheel
closure (`25/25`), fresh Python 3.9 install and
`pip check`, real Engine `context-view`/`recover`, exact recovery, sealed
receipts and provider-free OpenAI Agents 0.8.4 actual Runner success/abort.
Historical `fbfec0b` evidence remains quarantined. No live-provider or public-
release claim is made.

BSL 1.1 is the decided license family. Exact Change Date, Change License,
Additional Use Grant, publication, namespace, OEM/pricing, contributor,
security and release parameters remain pending. The Apache Python Product
lifecycle remains `OSS_TRANSITION_COMPAT`: security/correctness and migration
fixtures continue, but no new canonical lifecycle or framework breadth is
added there.

The Engine integration serially delivers reviewed R4A–R4G work: profiles,
parser, bounded Attach journal, policy separation, explicit field/compiler
inputs, factual kernel evidence and protocol freeze/narrowing. R5-D2 retires
behavioral outcome inference. The proposed detached-store deletion was withheld
and reclassified D4 because those Rust paths remain publicly reachable. R6
clarifies package/embed boundaries without an install break. P3 remains
`COMPLETE/INTEGRATED`; P5 is not integrated and remains quarantined; Cloud has
not begun.

**Objective:** create the premium SDK product around the small proven contract;
remove lifecycle, integration, compatibility and maintenance burden from the
customer without weakening the Engine.

P4 does not exit when a second repository exists. It exits only after the SDK
successor is accepted and the non-preserved Product implementation has been
removed, reduced to an Engine mechanism, frozen as bounded transition
compatibility, deprecated with a named removal gate, or quarantined. The local
architecture meets that closure gate. Final Engine
`5220ad11191e9de012dfedc97479dae6d28d1111` passed authoritative CI, Security,
CodeQL and the exact canonical-wheel clean-install gate. No public SDK release
or live-provider support claim is recorded.

**Deliverables**

- Versioned contracts for `ContextSession`, `ContextSource`,
  `ContextView`, `ContextPlan` and `ContextReceipt`; a root `LeanCTX`
  facade only after those are stable.
- Explicit state machine, typed IDs, budgets, freshness/reuse rules,
  source/recovery references, error taxonomy and compatibility fixtures.
- Reference demo and one or two maintained integrations; adapter breadth is
  demand-gated.
- Approved repository owner/access, namespace, BSL Change Date/License/Grant,
  use boundary, OEM terms, pricing, release/security and contributor parameters
  before production publication or commercial commitments.
- Product metrics: install-to-first-Receipt, integration time/LOC, founder
  assistance, reuse/recovery/evidence coverage, clean upgrade, explicit
  buy-vs-build win, retention, second workflow and embedded deployment.
- Dual-track ownership ledger covering SDK construction, Engine extraction,
  OSS impact, provenance, migration and removal gates.

**Technical/internal exit:** documented API, compatibility matrix, fixture
suite, migration/rollback and clean-machine path pass; Engine ownership seams
are independently reviewed and no duplicate canonical Product lifecycle
remains. **Public/commercial exit remains open:** approved support/release terms,
a stable public release and at least one real customer/design-partner validation
are not evidenced or claimed. Cloud remains absent.

### P4 V4 dual-track ledger

| Surface | Current owner | Target owner | SDK successor | Engine extraction status | OSS impact | Transition/removal gate |
| --- | --- | --- | --- | --- | --- | --- |
| Product primitives and lifecycle | P3 Python Preview plus private SDK staging | Production SDK, BSL 1.1 family | ledger `a7afd1f`; implementation `11f77debc2`; internal-RC PASS | Engine lifecycle ownership closed by removal/reduction/freeze | Preserve P3 Preview; no new canonical Apache lifecycle | Public license/namespace/support approval and rollback |
| Python package lifecycle | `packages/python-lean-ctx/**` Apache Preview | `OSS_TRANSITION_COMPAT` during migration | Same private SDK candidate | Frozen transition candidate; no new lifecycle semantics | Retain security, correctness and migration path | Released successor, migration window, rollback and named removal decision |
| Engine session | Apache Engine session state | Bounded Attach continuity mechanism | SDK `ContextSession` | Bounded journal and limits integrated | Preserve supported local Attach and persisted state | Serial PR CI and revert path |
| Engine Kits/formats | Apache Engine parser/Kits | Open parser, validation and integrity in Engine | SDK `ContextSource` / `ContextPlan` | Validated parser integrated | Preserve format interoperability and safe admission | Serial PR CI and migration |
| Engine transport/protocol | Apache Engine public boundary | Versioned bounded Engine contract | SDK public `context-view` / `recover` client | Bounds accepted upstream; Product-shaped protocol schema frozen with named gate | Preserve security, PathJail, bounded IO and recovery | v1 compatibility and authoritative CI |
| Profiles/policy/compiler/kernel | Mixed Engine mechanisms and Product policy | Engine mechanisms; SDK intent/policy | SDK `ContextPlan` | R4A–R4G integrated; Product delivery/inference removed | Preserve deterministic OSS mechanisms and security | Serial PR CI and named public-major compatibility gates |
| R5 decommission | Detached receipt/shadow stores and behavioral outcome research | Behavioral inference removed; public stores frozen | Explicit SDK/evaluator outcomes | D2 removes 244 lines; D1 PR 1565 closed unmerged and reclassified D4 | Factual accounting and Rust source compatibility remain | D2 revert; public-major/deprecation gate or shim for D4 |
| Orphan `memory_branch` module | Unreachable private Engine code | Removed | None | Merged in PR 1546 | No public, persistence, security or OSS Core reachability | Exact commit revert |
| Workspace/Cloud | P5 candidate quarantined; Cloud not begun | Later private repositories/services | None in P4 | Not integrated | No OSS impact; no Cloud dependency | P4 exit and separately authorized P5/P8 work |

Final Engine integration state: R4A–R5 accepted baseline `efbdb796b9`; R6
candidate `b1d530718a` merged through PR `#1573` as
`5220ad11191e9de012dfedc97479dae6d28d1111`.

### P5 — Local Workspace contract

**Status: NOT INTEGRATED.** Out-of-sequence PR `#1571` was closed and its remote
branch deleted; the local candidate remains quarantined pending separate
authorization. No promoted Workspace contract exists.

**Objective:** introduce `ContextWorkspace` only after the session/evidence
path has earned P4.

**Deliverables**

- RFCs for Workspace identity, SourceAnchor, storage boundary, policy,
  session attachment and state transitions.
- One local state owner that adapts existing SessionState, ProjectKnowledge,
  snapshot, bus and cache stores without declaring any legacy store the
  Workspace truth.
- Append-only typed lifecycle events, deterministic projections and immutable
  state references; mutable latest pointers remain an implementation cache,
  not the authoritative history.
- Local create/open/status plus bounded source catalog and session index.
- Explicit scope model: project, workspace, agent principal and tenant remain
  distinct identities; credentials and transcripts stay outside state.

**Exit:** restart-safe local Workspace can reopen bounded source-backed state
and run multiple sessions with clear lineage, without an agent scheduler,
remote service or hidden global fallback.

### P6 — Checkpoint, package and continuation

**Objective:** make durable state portable and recoverable without collapsing
snapshot, Kit, package and evidence semantics.

**Deliverables**

- Additive `ContextCheckpointV2` with immutable state references,
  checkpoint ID distinct from state digest, SourceAnchor/recovery references,
  versioned parents and restore policy.
- Explicit adapter/migration from current Git-anchored SnapshotV1; no silent
  reinterpretation of old artifacts.
- Strict local `.ctxpkg` profile: bounded data-only content, manifest,
  canonical identity, lock pins, trust state, capability/policy references and
  package-to-receipt references.
- Package admission hardening before any Kit bridge: path jail, bounded reads
  and decompression, verify-before-write, atomic rollback, canonical signed
  manifest scope and a policy-owned external trust anchor. Integrity,
  signature validity and trusted signer remain distinct states.
- A versioned ContextKit↔`.ctxpkg` composition decision; legacy TOML Kits
  remain readable until a proven migration exists.

**Exit:** sealed local artifact and checkpoint round trips are deterministic,
tamper-rejecting, credential-free, offline-capable and restore exact source
references without implicit project-state mutation.

### P7 — Bounded parallel context

**Objective:** safely reuse verified work across local sessions/agents.

**Deliverables**

- Authenticated AgentPrincipal and scope/policy checks on every shared read,
  write, relay, import/export and graph operation.
- Privacy-on-read, project/workspace/tenant isolation, persisted directed
  target/privacy fields and negative spoofing/replay tests.
- One signed, redacted, idempotent handoff contract with delivery versus
  acceptance states. Handoff precedes any broad merge.
- Copy-on-write fork lineage and source-backed ContextDelta only after P6.
  Any three-way merge is explicit, mode-limited and conflict-visible; caches
  are derived, never truth.

**Exit:** two local sessions continue bounded verified work without transcript
transfer, cross-project leakage or false “handoff complete” status.

### P8 — Optional Cloud Receipt Board

**Status: NOT BEGUN.** Cloud work remains unopened and is not required for local
Engine or SDK operation.

**Objective:** add the smallest credible organization surface in a private
service, never inside OSS Engine.

**Deliverables**

- Named private repository/service owner and a Board projection contract—not a
  second receipt truth.
- Metadata-only receipt/evaluation ingestion: IDs/digests, scope, measured/
  estimated/accepted facts, trust/admission state and provenance links.
- Authenticated tenant→project→workspace authorization, strict idempotency,
  retention/deletion, append-only audit, export, support and incident controls.
- Explicit data-mode state machine:
  `metadata_only → derived_context → synchronized_project_context →
  managed_context`. Each escalation needs policy, consent, residency and
  local-degradation approval.
- Legacy Personal Cloud/client/sync paths are classified, quarantined or
  retired; they are not reused as compliant Board ingestion.

**Exit:** a customer can inspect declared cross-run behavior with no raw source
upload; Board outage leaves local execution, receipt and offline verification
usable. Shared context, registry and fleet remain separate later decisions.

### P9 — Governed optimization and enterprise operation

**Objective:** only after P8/customer evidence, add governed decision services.

**Deliverables**

- Manual, reversible profile recommendation from evaluated local evidence.
- Private learned ranking/continuous optimization only behind the D/E boundary;
  no customer data or weights enter OSS.
- Customer-validated policy distribution, approved package/Workspace sharing,
  deployment controls and operational runbooks.

**Exit:** repeated paid workloads, support/security ownership and explicit
quality/budget/rollback evidence justify each automation step. Fleet, SSO,
SCIM, managed execution and regional deployment remain independent funded
commitments—not implied milestones.

## 5. Contract/RFC backlog

Create each RFC before implementation and promote it only with fixtures:

1. Engine interface compatibility and native invocation lineage.
2. Receipt/evidence ownership and verifier compatibility.
3. SDK Session/Source/View/Plan/Receipt public contract.
4. Workspace identity, SourceAnchor and local storage boundary.
5. ContextCheckpointV2 and SnapshotV1 migration.
6. `.ctxpkg`/ContextKit composition, trust and lock semantics.
7. Bounded handoff, identity/scope/privacy and delivery semantics.
8. Cloud Receipt Board projection, data modes and tenant/audit model.
9. Protocol-family public surface, compatibility matrix and deprecation policy.

## 6. Definition of done

Each milestone is complete only when:

- implementation is linked to its RFC and owner;
- deterministic unit, compatibility, negative-security and recovery tests pass;
- relevant Rust/Python quality gates and formatting pass;
- public docs name the exact capability and status;
- source/protocol/version migration is tested;
- the claim can be reproduced from a named fixture or is labelled Research;
- no private data, service dependency, secret or proprietary decision logic
  enters the OSS repository.

## 7. Stop conditions

Stop and return to the preceding gate when source lineage is missing, evidence
cannot be independently verified, a policy/trust/data-mode decision is
fail-open, Cloud becomes necessary for local work, an object duplicates an
existing owner, or the feature broadens LeanCTX into agent orchestration.
Also stop when a public contract lacks a version, owner, compatibility fixture,
support decision or explicit migration path.
