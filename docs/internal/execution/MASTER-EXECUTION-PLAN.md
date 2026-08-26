# Master Execution Plan

**Status:** Active — single source of truth for all execution  
**Owner:** Yves Gugger  
**Created:** 2026-08-20  
**Updated:** 2026-08-26
**Tracking:** GitLab issues on `origin` (project ID 5, `gitlab.pounce.ch/root/lean-ctx`)

---

## Workstreams (parallel)

```text
WS-1: CODEBASE CLEANUP          ██████████  Cleanup and P0 operational delivery/security gate complete
WS-2: SDK V1 + EVIDENCE         ██████████  Phase-A substrate complete; public SDK remains Preview
WS-3: WEBSITE REBUILD           ████████░░  v2 live, Nav/Footer/SEO/Docs pending
WS-4: OCLA + COMPOSABLE (B)     ██████████  P1 Engine runtime exit complete; P2 evidence integration active
WS-5: PARTNERSHIP (C)           —— DEFERRED (no near-term partner execution)
WS-6: BENCHMARK + CALIBRATOR    ██████████  Technical v0 complete; P2 evidence gate remains open
WS-7: REPRODUCIBLE EVIDENCE (C) ██████░░░░  Manifest and signed-bundle foundations exist; provenance replay and independent verification remain
WS-8: MANUAL SELECTION (D)      ██████░░░░  Signed evidence is revalidated at apply time; independent verification and cross-implementation conformance remain
WS-9: THINKERY CONTROL PLANE(E) ░░░░░░░░░░  Private commercial work; separate repository/infrastructure
```

**Status rule:** A completed implementation task or published package does not
promote a product surface to Available. The cross-product dependency order,
target gates and stop conditions are in the
[`Complete Implementation Roadmap`](../vision/08-COMPLETE-IMPLEMENTATION-ROADMAP.md);
the P4 boundary and evidence are in the
[`P4 extraction report`](../P4-SDK-EXTRACTION-AND-ENGINE-REFACTOR-REPORT.md).
This file schedules approved work; it does not supersede those authorities.

## Cross-product execution state

| Roadmap phase | Current state | Next exit |
| --- | --- | --- |
| **P0 — delivery/security** | **Operational exit complete (2026-08-23):** regular `main` CI run `32639983816`, Security Check `32639983788`, and CodeQL `32639983823` passed; declared GitHub `main` protection was applied and independently verified drift-free with required `CI Green` and `Security Scan`, admin enforcement, and force-push/deletion disabled. | Keep the declarative policy and drift verification green; release/publishing remains transitively gated by these checks. |
| **P1 — Engine runtime spine** | **Exit complete (2026-08-23):** PRs `#1515` and `#1516` merged; latest merge commit `dd55473302`. Explicit `ctx_read` `engine_interface="v1"` dispatch, legacy omission compatibility, policy/rejection receipts, deadline behavior, descriptor/handle-rooted source and artifact containment, failure-atomic publication and adversarial swap/relocation coverage passed final CI run `32662146660`, including native Ubuntu and Windows tests. | Preserve the accepted v1/legacy behavior and feed its invocation, observation and receipt link into the single P2 evidence chain. |
| **P2 — canonical evidence** | **Exit complete (2026-08-24):** PR `#1522` merged to `github/main@8ebf61a21c063a1d0a86be33511588d27d7ca71e`; exact-SHA CI `32758619963`, Security Check `32758619955`, and CodeQL `32758619966` passed. The chain keeps delivery factual and host/evaluator outcome explicit. | Preserve the accepted Task → Plan → Invocation → Observation → Receipt → explicit Outcome/Quality → independently verified Evidence Bundle chain. |
| **P3 — Python Preview** | **P3 COMPLETE/INTEGRATED (2026-08-25):** merge `e47cb432f2e9e2d7ecf13e3c85a0d1cc4fa68f96`; exact-baseline CI `32778481164`, Security `32778481373`, and CodeQL `32778481121` passed. Candidate `79e3888862659e5ac0623a8fa00e16155aa144da` runs the Python-owned Session/Plan/View/Receipt lifecycle through the real local Engine operation transport, actual OpenAI Agents SDK `0.19.4`, native Embed, exact recovery and truthful degradation. `LIVE PROVIDER SMOKE: UNVERIFIED`. | Preserve the integrated P3 seam as transition compatibility under the completed P4 boundary. |
| **P4 — production SDK + Engine extraction** | **TECHNICAL/INTERNAL EXIT COMPLETE (2026-08-26):** final Engine `5220ad1119` merged through PR `#1573`; CI `32936915240`, Security `32936915221` and CodeQL `32936915253` passed. Private SDK head `8c84e224` and ledger `a7afd1f` bind implementation `11f77debc2` and wheel `b54ac013…`; the exact accepted-Engine clean-install gate passed. BSL/publication/support and customer-validation gates remain open; provenance is `CLEAN_REIMPLEMENT`. D1 detached-store deletion remains withheld as D4; Apache Python remains `OSS_TRANSITION_COMPAT`. | STOP; preserve the boundary and do not automatically begin P5/Cloud. |
| **P5–P7 — Workspace, package, handoff** | **NOT INTEGRATED:** out-of-sequence PR `#1571` was closed and its remote branch deleted; the local candidate remains quarantined. | Start only through separate authorization after P4; never promote quarantined work or legacy stores/artifacts as the new truth. |
| **P8–P9 — Cloud/optimization** | **NOT BEGUN:** private work remains unopened. | Separate private repository only after local proof, trusted evidence and designated owners. |

**Governance state:** P3 and P4 V4 technical/internal scope
COMPLETE/INTEGRATED; P4 public/commercial release remains gated; P5 not
integrated and quarantined; Cloud not begun.

**Execution order:** P0 operational CI/branch-protection verification (**met**) → P1 runtime Engine caller
(**met**) → P2 canonical evidence (**met**) → P3 real Python Preview (**COMPLETE/INTEGRATED**) → P4 SDK repository (**TECHNICAL/INTERNAL EXIT COMPLETE**).
P5 is not integrated; its out-of-sequence candidate is quarantined. Cloud and
other later work remain unopened.
No later workstream may bypass an earlier exit.

## P4 V4 dual-track execution ledger

P4 delivered two mandatory tracks: the canonical Production SDK and extraction
of non-preserved Product implementation from the Apache Engine. Both tracks
were independently reviewed. The SDK remains deliberately private/unreleased;
Engine delivery used serial PR CI and retains public
compatibility where removal lacks an approved gate.

| Surface | Current owner | Target owner | SDK successor | Engine extraction status | OSS impact | Transition/removal gate |
| --- | --- | --- | --- | --- | --- | --- |
| `ContextSession`, `ContextSource`, `ContextView`, `ContextPlan`, `ContextReceipt` | P3 Python Preview plus private SDK staging | Production SDK, BSL 1.1 family | ledger `a7afd1f`; implementation `11f77debc2`; independent internal-RC PASS | Canonical Product lifecycle removed/reduced/frozen at every audited Engine seam | Preserve the integrated Preview path; add no new Apache Product semantics | Public release namespace/terms/support approval; Preview migration window and rollback |
| `packages/python-lean-ctx/**` Product lifecycle | Apache Preview package | `OSS_TRANSITION_COMPAT` only until successor release | Same private SDK candidate | Freeze candidate; no new lifecycle or adapter breadth | Security, correctness and migration compatibility remain supported | Released successor, migration window, rollback, support owner and named removal decision |
| `rust/src/core/session/**` | Apache Engine session state | Bounded Engine Attach continuity | SDK `ContextSession` | Bounded journal and serialization limits integrated; focused review passed | Retain required local Attach continuity and persisted user state | Serial PR CI and revertable migration |
| Engine Kits and document formats | Apache Engine Kits | Open parser/validation/integrity in Engine; lifecycle activation in SDK | `ContextSource` / `ContextPlan` | Validated parser integrated; focused review passed | Preserve `.ctxpkg`/format interoperability and safe admission | Serial PR CI and migration fixtures |
| Engine request/path transport | Apache Engine public boundary | Bounded public Engine contract | SDK consumes public `context-view` / `recover` path | Request/path bounds are integrated in accepted upstream baseline | Preserve PathJail, bounded IO, failure atomicity and recovery | v1 compatibility and authoritative CI |
| Profiles, policy, compiler and kernel | Mixed mechanisms and Product policy | Engine facts/mechanisms; SDK Product intent/policy | SDK `ContextPlan` and policy layer | R4A–R4G integrated; Product delivery semantics detached; protocol narrowed/frozen | Keep deterministic optimization, security and OSS Attach behavior | Serial PR CI; named public-major gates for retained DTOs |
| R5 decommission | Detached stores and heuristic Product outcome research | Behavioral inference removed; public detached stores frozen | Explicit SDK/evaluator outcomes | D2 removes 244 lines; D1 PR 1565 closed unmerged and reclassified D4 | Factual Engine measurements and public Rust source compatibility retained | D2 revert; explicit public-major/deprecation gate or shim for D4 |
| Orphan `memory_branch` module | Unreachable private Engine code | Removed | None | D1 deletion merged in PR 1546 | No public, persistence, security or OSS Core reachability | Exact commit revert |
| Workspace/Cloud coordination | P5 candidate quarantined; Cloud not begun | Later private work only | None in P4 | Not integrated | No OSS impact; local Engine/SDK must remain complete without Cloud | P4 exit plus separately authorized P5/P8 work; no scaffolding now |

**Final P4 Engine integration:** R4A–R5 accepted baseline `efbdb796b9`; R6
candidate `b1d530718a` merged through PR `#1573` as final Engine
`5220ad11191e9de012dfedc97479dae6d28d1111`. The required evidence and exit
matrix are in
`docs/internal/P4-SDK-EXTRACTION-AND-ENGINE-REFACTOR-REPORT.md`.

---

## WS-1: Codebase Cleanup

**Goal:** Repo reflects ONE vision. Remove confusion, consolidate duplicates.  
**Timeline:** This week  
**Safety:** Each wave = 1 branch → 1 PR → gates green → merge

| # | Task | Branch | Status |
|---|------|--------|--------|
| 1.1 | Archive `py-sdk/`, `python-sdk/`, `clients/python/` | `cleanup/wave-1-python` | **done** (f615c8d) |
| 1.2 | Archive `marketing/`, `email-templates/`, `demo/`, `lab/`, `blog/`, `lean/` | `cleanup/wave-2-unused` | **done** (6441f73) |
| 1.3 | Consolidate `bench/` + `benchmark/` → `benchmarks/` | `cleanup/wave-3-bench` | **done** (6679c89) |
| 1.4 | Remove `test-results/`, `tmp/` from tracking | `cleanup/wave-3-bench` | **done** (6679c89) |
| 1.5 | Update README.md links after cleanup | main | **done** (2026-08-21: Pi-install lines removed) |
| 1.6 | Prio 1: DELETE ts-sdk, MERGE contracts/examples, .gitignore fix | main | **done** (8ea700f) |
| 1.7 | Prio 2+3: Archive editors/go-sdk/integrations/specs/benchmarks | main | **done** (6ff1e48) |
| 1.8 | Prio 4+5: Archive stale scripts, keep bin/lctx | main | **done** (235d135) |
| 1.9 | Sync GitLab WS-1 issues + `_archive/` restoration README | — | **done** (2026-08-21: #1242–#1244 closed, milestone closed) |

**Gates per wave:**
```bash
cargo build --release
cargo test --lib
cargo clippy --all-features -- -D warnings
cargo fmt --check
```

---

## WS-2: SDK v1 + Evidence (Phase A)

**Goal:** One workload, baseline/treatment, quality gate, and offline-verifiable Receipt; commercial conversion runs in parallel, not as an engineering prerequisite.
**Timeline:** 2-3 weeks after cleanup  
**Exit criterion:** Reproducible local Receipt with offline verification

**Interpretation:** This is an implementation/proof-loop milestone. Python SDK
v1 and its OpenAI Agents reference wrapper remain **Preview** until their
contract, compatibility and support gates are explicitly promoted.

| # | Task | Status |
|---|------|--------|
| 2.1 | Consolidate Python types from `py-sdk/` + `clients/python/` into `packages/python-lean-ctx/` | **done** (pre-existing) |
| 2.2 | Implement `ctx.wrap()` — OpenAI Agents SDK adapter through proxy | **done** (wrap.py 297 LOC) |
| 2.3 | Implement `ContextSession` with correlated task identity | **done** (session.py 321 LOC) |
| 2.4 | Implement Receipt generation on every wrap() call | **done** (receipt.py 410 LOC) |
| 2.5 | Implement local Performance Benchmark (baseline vs treatment) | **done** (evidence realworld) |
| 2.6 | Implement Quality Gate (automated assertions + human gate) | **done** (--quality-gate flag) |
| 2.7 | Implement offline Receipt verification | **done** (verify() local+Ed25519) |
| 2.8 | Write Quickstart documentation | **done** (README 342 LOC) |
| 2.9 | Publish Apache Preview v1.0 on PyPI | **done** (`lean-ctx-python` 1.0.0; not the private Production SDK) |
| 2.10 | Run first Thinkery Agent Tuning Sprint (CHF 7,500) | **ON HOLD** (commercial track; #1254; does not block vision engineering or public Research releases) |
| 2.11 | Freeze Workspace, checkpoint and `.ctxpkg` target architecture | **done** (internal plan; remains Research—no Workspace API or Cloud claim promoted) |

---

## WS-3: Website Rebuild

**Goal:** New website reflecting the professional brand narrative.  
**Timeline:** Parallel to WS-1/WS-2, 1-2 weeks  
**Branch:** `deploy` (GitLab only, NEVER push to GitHub)  
**Tech:** Astro (existing stack)  
**Copy source:** `docs/internal/execution/WEBSITE-REDESIGN.md` (839 lines, complete)

| # | Task | Status |
|---|------|--------|
| 3.1 | Design system: implement Visual Brand Guidelines (colors, typography, grid) | **done** (4579d29, deploy) |
| 3.2 | Homepage rebuild: Hero + Problem + SDK + Integration + Evidence + Enterprise | **done** (82ebb95, deploy) |
| 3.3 | `/sdk` page: Developer-focused, code examples, progressive disclosure | **done** (7a9fa82, deploy) |
| 3.4 | `/enterprise` page: Performance-first enterprise narrative | **done** (7a9fa82, deploy) |
| 3.5 | `/benchmark` page (replaces old Dyno language) | **done** (7a9fa82, deploy) |
| 3.6 | `/docs` restructure: align with new terminology | pending |
| 3.7 | Remove old pages that don't align (old pricing, old cloud references) | pending |
| 3.8 | Navigation update: LeanCTX / SDK / Enterprise / Benchmark / Docs / GitHub | pending |
| 3.9 | Footer: Open source · Local-first · Model-agnostic | pending |
| 3.10 | SEO: meta titles/descriptions with new messaging | pending |
| 3.11 | Deploy to production (GitLab CI → origin only) | pending |

**Design principles:**
- Black ground, minimal orange (= intervention), Aeonik + Mono
- Routing lines, grids, data panels, receipt visualizations
- No fake metrics — only show real data or clearly labeled illustrations
- Product status labels: Available / Preview / Research

---

## WS-4: OCLA + Composable Architecture (Phase B)

**Goal:** First native capability through the full contract path.  
**Timeline:** Engineering now (owner decision 2026-08-22); public claims require reproducible, independently verifiable evidence, not a paid run.
**Prerequisite for code:** none — dogfood internally as Preview/Research.
**Open-core:** Class A/B only (manifest, registry, native CompressionProvider, conformance). No marketplace, no learned ranking, no Control Plane (Class D/E).

| # | Task | Status |
|---|------|--------|
| 4.1 | OCLA audit verified (D4 report already done) | done |
| 4.2 | Define CapabilityManifest v0 schema in `lean-ctx-protocol` | **done** (634dedc) |
| 4.3 | Normalize `CompressionProvider` as first v0 capability | **done** (634dedc) |
| 4.4 | Add capability ID + version to Performance Profile format | **done** (634dedc) |
| 4.5 | Add capability ID + version to Receipt schema | **done** (634dedc) |
| 4.6 | Run Benchmark through capability path (same result, new plumbing) | **done** (634dedc) |
| 4.7 | Write conformance test for `compression_provider` | **done** (634dedc) |
| 4.8 | Sample external local-process capability (trivial example) | **done** (discovery, fixed executable boundary, bounded stdio, timeout/disable, registry + conformance; 10,199 Rust tests and 3 cookbook tests) |

---

## WS-5: Partnership + Ecosystem — Deferred

No near-term partner execution is planned. The bounded external-capability
path remains a local technical compatibility reference; it is not a
marketplace, a public ecosystem strategy or a reason to broaden the product
boundary. Revisit only after the local SDK evidence path and contract gates
have passed.

## WS-6: Benchmark + Calibrator (Research Track)
**Goal:** Validate the Calibrator concept: one agent, two profiles, controlled benchmark, quality preserved, correct recommendation.
**Timeline:** After WS-4 OCLA dogfood; Research status until Phase D evidence gate
**Prerequisite:** WS-4 AdapterRegistry production-wired; PerformanceProfileV1 with capabilities field
**Vision:** [Calibration & Performance Platform Vision](../vision/14-LEANCTX-CALIBRATION-PERFORMANCE-PLATFORM-VISION.md)
**Gap Analysis:** [Benchmark & Calibrator Gap](../reference/BENCHMARK-CALIBRATOR-GAP.md)
**Open-core:** Manual calibration = OSS; automated candidate generation + Selection Intelligence = commercial

| # | Task | Status |
|---:|---|---|
| 6.1 | Consolidate 6 benchmark engines under unified BenchmarkSpecV1 | **done** (BenchmarkSpecV1, BenchmarkRunner trait, report formatters; 7 tests) |
| 6.2 | Extend Profile with constraints (quality_floor, max_cost, max_latency) | **done** (ConstraintsConfig + CapabilitiesConfig in Profile; 64 profile tests pass) |
| 6.3 | Extend Profile with capabilities section (surface to provider) | **done** (CapabilityBinding + CapabilitiesConfig) |
| 6.4 | Create performance-profile-v1 contract and JSON schema | **done** (docs/contracts/performance-profile/) |
| 6.5 | Implement benchmark with profile selection (wire profile to benchmark) | done |
| 6.6 | Implement Calibrator v0: fixed candidate set, Pareto frontier, recommendation | **done** (calibrator module: config, candidate, pareto, recommendation, report; 13 tests) |
| 6.7 | Implement calibrate CLI command | done |
| 6.8 | Agent Connector v0: programmatic invocation of one agent for benchmark | **done** (AgentConnector trait + Codex/Claude/Cursor connectors + detection; 3 tests) |
| 6.9 | LocalRunner wiring and named-profile propagation for live calibration | **done** (LocalRunner, timeout, and `LEAN_CTX_PROFILE` propagation) |
| 6.10 | Local verified comparison artifact: explicit quality evaluator + Receipt linkage | **done** (deterministic evaluator + `--spec` gate + canonical locally signed connector receipt from explicit provider usage/cost; a connector without explicit cost remains correctly OBSERVED) |

**Anti-scope:** No gamification, social profiles, achievements, badges, community platform, marketplace. V1 = one agent, two profiles, controlled benchmark, Receipt, recommendation.

## WS-7: Reproducible Evidence (Phase C, OSS)

**Goal:** Make the Phase A/B primitives proveable over named, evaluated workloads without
turning a local benchmark into a hosted ranking service.
**Status:** Research. A paid run is not required; public claims need a reproducible workload,
predeclared quality gate, complete provenance, and independently runnable verifier.
**Open-core:** Class A/B/C: workload and evidence contracts, local runner, reference fixtures,
report formatter, offline verifier. No learned ranking, fleet telemetry, customer data, or
hosted history.

**Promotion gate:** a governed or shared claim additionally requires explicit
enforced/degraded sandbox status, resolved identity/scope, monotonic policy,
declared data mode, external signer trust, licensing/repository approval and a
named jurisdiction/export review owner. Missing inputs block promotion.

| # | Task | Exit criterion | Status |
|---:|---|---|---|
| 7.1 | Versioned evaluated-workload manifest | Stable identity, declared QA/code evaluator, bounded code-test fixture, deterministic validation | in progress — versioned source-probe and code-repair manifests exist; hardened bounded fixture execution remains required |
| 7.2 | Local suite loader and named-suite CLI | `benchmark-run --profile NAME --spec PATH` executes an evaluated manifest with explicit profile/agent identity | **done** (strict JSON loading, evaluator gate, deterministic profile binding, no `--suite`/`--repeats` overrides) |
| 7.3 | Reproducible evidence bundle | Baseline/treatment outputs, evaluator result, receipt refs, artifact redaction classification, environment and verifier command are linked offline | in progress — signed local spec/result/receipt bundle and explicit redaction classification exist; invocation binding, output replay, and independent receipt/evidence verification remain required |
| 7.4 | Capability coverage matrix | Native and bounded external capability paths show success, policy rejection, timeout, and disable behavior | in progress — deterministic, payload-free test matrix covers each state; a consumable evidence surface remains required |
| 7.5 | Public research fixture pack | Redacted/self-contained fixtures pass on a clean checkout and make no universal-performance claim | in progress — manifests/assets are portable and provider-free; isolated code-repair proof currently requires the macOS sandbox, so cross-platform proof remains open |

## WS-8: Manual Selection (Phase D, OSS)

**Goal:** Convert evaluated local evidence into an explainable, reversible manual recommendation.
**Status:** Research/Preview only after WS-7 evidence exists.
**Open-core:** Class A/B/C deterministic candidate generation, Pareto calculation, explicit
operator selection, exported profile, and rollback. Learned rankings, customer priors, and
automatic promotion stay private Class D/E.

**Promotion gate:** recommendations must preserve the same security, identity,
policy, data-mode and trusted-verifier admission record as their evidence;
unsigned/self-attested artifacts may remain local observations but cannot
justify a governed or Cloud promotion.

| # | Task | Exit criterion | Status |
|---:|---|---|---|
| 8.1 | Evidence-qualified candidate input | Unevaluated or incomplete-cost runs cannot feed a recommendation | in progress — creation validates evaluated receipt-linked runs; apply replays the pinned bundle's signed inventory, exact specs/results, and receipt artifacts, while independent verification remains required for public claims |
| 8.2 | Deterministic recommendation record | Candidate set, constraints, evidence refs, rationale, and profile hash serialize canonically | in progress — canonical serialization exists; its linked evidence requires independent validation |
| 8.3 | Explicit apply/rollback CLI | Operator approves a named profile; prior profile is preserved and restorable | in progress — a later apply requires an explicit bundle path and rejects unavailable, changed, unsigned, semantically mismatched, stale, or profile-hash-mismatched evidence; independent verification remains required |
| 8.4 | Manual-selection conformance suite | Stable result across reordered inputs; all rejection paths are covered | in progress — ordering, verified later apply, stale state, immediate rollback, and tampered/unavailable/mismatched-evidence rejections are covered; full cross-implementation conformance remains |

## WS-9: Cloud Coordination (Phase E, Commercial, private)

**Goal:** Start only with a metadata-first Receipt/Workspace Board after local
SDK proof; govern later organization context without exporting raw source or
private workloads into the OSS repository.
**Repository boundary:** This work belongs in a separate private Thinkery repository and
private infrastructure. It must not be scaffolded as a hidden feature in LeanCTX OSS.

| # | Capability | Boundary | Status |
|---:|---|---|---|
| 9.1 | Metadata-only Receipt/Workspace Board: tenant, project, retention and audit | Class D; private | blocked — local SDK proof and private project not designated |
| 9.2 | Opt-in package/handoff metadata and policy distribution | Class D; private | blocked — Workspace/Package contracts not promoted |
| 9.3 | Shared context, raw-source sync and organization governance | Class D/E; private | blocked — explicit privacy, conflict and customer-evidence gates |
| 9.4 | Learned ranking, fleet rollout and SLA operation | Class E; private | blocked — governed data and private infrastructure not designated |


## Tracking Rules

1. **Each task gets a GitLab issue** with label `status: ready` or `status: in-progress`
2. **Each workstream gets a GitLab milestone** (WS-1, WS-2, WS-3, WS-4, WS-5)
3. **PR links to issue** — close on merge (or when published, for SDK/website)
4. **Weekly status update** in this document
5. **No task without exit criterion** — what does "done" look like?

### GitLab sync (2026-08-21)

| Milestone | Issues | State |
|---|---|---|
| WS-1 Codebase Cleanup | #1242–#1244 | **closed** + milestone closed |
| WS-2 SDK v1 + Evidence | #1245–#1253 | **closed** (historical Apache Preview code + PyPI 1.0.0; not Production SDK publication) |
| WS-2 | #1254 first paid pilot | **open — ON HOLD** (sales/pilot) |
| WS-3 Website Rebuild | #1255–#1259 | **closed** (v2 pages on `deploy`) |
| WS-3 | #1260 old pages / nav / footer / SEO | **open** |
| WS-3 | #1261 production deploy | **open** |
| WS-4 | #1262–#1266 | **open** — engineering unblocked 2026-08-21; do not market |
| WS-5 | #1267–#1268 | **cancelled** — monopoly strategy, close issues |

---

## What we are NOT doing (Anti-Roadmap reminder)

- No marketplace
- No 10 partner integrations
- No partner ecosystem or "composable optimizer" marketing
- No RTK/Headroom/Caveman integrations or joint experiments
- No OptimizationProvider interop promotion
- No control plane or dashboard
- No AutoTune (Continuous Optimization is Phase E, later)
- No hosted platform claims
- No new package formats
- No agent builder

---

## Historical success sketch (not the P4 Production SDK exit)

This 2026-08-21 sketch describes the Apache Preview/website program. It does
not assert Production SDK publication or paid-customer validation.

```text
✓ Repo is clean (one Python SDK, no dead dirs)
✓ Website reflects the new narrative
✓ Apache Preview v1 on PyPI with ctx.wrap() + Receipt
✗ Paid-customer Receipt validation remains unverified and on hold
✓ CompressionProvider runs through OCLA v0 contract
✗ WS-5 cancelled — monopoly strategy, no external partnerships
```

When all six are true, we have earned the right to talk about
"The Context SDK for AI Agents" publicly.
