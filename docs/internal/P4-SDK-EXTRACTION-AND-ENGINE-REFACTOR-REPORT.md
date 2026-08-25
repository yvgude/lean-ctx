# P4 SDK Extraction and Engine Refactor Report

**Status:** ACTIVE — final integration candidate; Layer-4 PR CI pending
**Date:** 2026-08-26
**Scope:** P4 Production SDK extraction and Apache Engine refactor only

P3 is COMPLETE/INTEGRATED. P5 Workspace/package/handoff and Cloud work are
NOT BEGUN. This report records the completed local P4 candidate and remains
ACTIVE until the serial Engine PR stack passes authoritative CI and lands.

## Final architecture

The ownership direction is:

| Layer | License/state | Owns |
| --- | --- | --- |
| Optional private Cloud | Not part of P4 | Hosted services only |
| Production SDK | BSL 1.1 family; private/non-published staging | Product intent, lifecycle, policy, reuse/freshness, adapters, compatibility and evidence projection |
| Public Engine contract | Versioned and factual | Identity, source/view/shape, search, recovery, capability execution and evidence references |
| Engine | Apache-2.0 | Deterministic mechanisms, bounded execution, Attach, MCP/proxy/CLI, security and OSS Coding-Agent paths |

The Engine may optimize explicit inputs. It does not own Product context intent
or infer task acceptance from delivery, retries or response length.

## Identities and release state

| Item | Final candidate | State |
| --- | --- | --- |
| Original V4 Engine baseline | 49a6b2053c08c4d831916f03e01c7b5e6079a372 | Historical start |
| Accepted upstream baseline | github/main at 9aba08d40796c15de6775ae0161176e2b9b13ef6 | PR 1544 merged |
| Engine code candidate | e495b61f24a50012e90f419577d3cabda57fc50a | R4A–R6 integrated locally |
| Engine report candidate | Integration branch after this report commit | Final SHA assigned by serial Layer-4 merge |
| Production SDK repository | /Users/yvesgugger/Documents/Privat/Projects/leanctx-product-sdk-staging | Separate private staging |
| Production SDK repository head | 8c84e2246bff0f2681dc69ea08d4322519f88050 | Current private docs/governance head |
| Production SDK ledger | a7afd1f | Clean, reviewed internal RC record |
| SDK implementation baseline | 11f77debc2811dfd6569ba3a30bb674ae5e8b5d1 | Immutable reviewed code |
| SDK wheel SHA-256 | b54ac013af1494c00a64feac8ce3eb1910fc32baa3ba9a6d4b52d565942ff141 | Reviewed artifact |
| SDK distribution | PRIVATE_NOT_RELEASED | No public release or namespace claim |
| License family | BSL_1_1_DECIDED | Exact publication parameters remain pending |
| SDK provenance | CLEAN_REIMPLEMENT | No Apache source copied into BSL staging |

Historical SDK identity fbfec0b is quarantined and is not release evidence.

## Track A — Production SDK

The private staging candidate implements the five Product primitives:

- ContextSession
- ContextSource
- ContextView
- ContextPlan
- ContextReceipt

It is materially more than a transport wrapper: it owns lifecycle, bounded
reuse/freshness, policy, framework mapping, evidence projection, compatibility,
migration/rollback support, safe defaults and embedded/OEM-oriented integration.

Independent release review passed:

- Python 3.9 and 3.14: 19/19 tests on both interpreters
- certified offline dependency closure: 25/25 exact wheel hashes for CPython
  3.9 on macOS 11+ arm64; other platforms require separate reviewed manifests
- real Engine 3.9.20 context-view and recover
- exact recovery and sealed receipt evidence
- fresh Python 3.9 offline install and `pip check`
- provider-free OpenAI Agents 0.8.4 actual Runner success and abort paths,
  exact host object identity and secret-redacted receipts
- no credentials, provider call, Cloud dependency or live-provider claim
- clean wheel/install and immutable artifact digest

Public publication remains intentionally blocked by unresolved BSL parameters,
public namespace, support/release process, pricing/OEM decisions and legal
approval. Those are explicit business/legal gates, not hidden engineering work.

## Track B — Engine refactor

| Wave | Candidate commit(s) | Disposition |
| --- | --- | --- |
| R4A Profiles | 21451c3ce9 | Narrow Engine-consumed profile configuration |
| R4B Kits/parser | 8af44ff21c | Validated open parser/integrity mechanism |
| R4C Session | dee0c24b86, 2dfdd0f3f0 | Bounded Attach continuity journal |
| R4D Policy | f3d75a915c | Security defaults separated from Product policy |
| R4E Field/compiler | b3eda793a0, 785817defa | Explicit field input and deterministic bounded compiler |
| R4F Kernel | 15f12e05d5, 8df206c820 | Product delivery semantics detached; factual measurements retained |
| R4G Protocol | 618cc2d2cc | Historical Product-shaped types frozen with a named major-version gate |
| R5 D1 | d83ef94882 | Detached MCP receipt/shadow stores removed |
| R5 D2 | 581d488477 | Behavioral outcome inference/learning retired; DTO compatibility retained |
| R6 | e495b61f24 | Engine/package/embed boundaries clarified without install breakage |

R4A is represented by PR 1547. At the last recorded check every job passed
except the Windows rerun, which remained pending. Remaining commits stay local
until branch hygiene permits serial PR delivery.

## Ownership before and after

| Surface | Before P4 | Final P4 owner/disposition |
| --- | --- | --- |
| Product lifecycle primitives | Mixed Preview/Engine experiments | Production SDK canonical; Python Preview frozen transition compatibility |
| Session continuity | Mixed lifecycle/session semantics | Engine keeps bounded Attach journal; SDK owns Product session lifecycle |
| Sources and plans | Parser plus Product-shaped planning | Engine keeps validated formats/mechanisms; SDK owns source and plan intent |
| Context selection | Engine candidate creation and learned policy | Engine keeps explicit deterministic bounded optimization; SDK owns construction, weights and Product feedback strategy |
| Receipts/outcomes | Delivery facts mixed with inferred quality | Engine records factual observations; SDK/evaluator owns accepted outcomes |
| Protocol | Product-shaped candidates could expand | Legacy schema frozen; no new Product semantics; named public-major removal gate |
| Rust lean-ctx-sdk crate | Misleading stable-SDK wording | Experimental Apache in-process Engine façade, not Production SDK |
| context_kernel export | Accidental broad documented surface | Public path retained for source compatibility but hidden from generated docs |

## Decommission and transition ledger

### Removed from Engine

- Detached context_kernel shadow event store
- Detached MCP receipt/accounting store
- Orphan MCP ContextReceipt generator and timestamp identifier
- Behavioral infer_outcome acceptance/rejection heuristics
- Inferred-outcome provider learning hook
- Inferred OutcomeTracker quality/degradation model
- Duplicate post-dispatch Product receipt construction
- Context-gate Product plan/receipt/shadow delivery logging
- Earlier unreachable memory_branch module from PR 1544

R5 removed 703 lines in two coherent batches: 459 lines in D1 and 244 lines in
D2. R4F removed the active duplicate delivery path before those deletions.

### Frozen transition compatibility

| Surface | Why retained | Successor | Exact removal gate |
| --- | --- | --- | --- |
| packages/python-lean-ctx | Integrated P3 Preview users need migration/rollback | Production SDK | Released successor, migration window, support owner and rollback decision |
| OutcomeSignal/InferredOutcome DTOs | ProxyKernelResult exposed serialized/source fields | Explicit evaluator observations | Next public API major after consumers migrate |
| protocol knowledge_routing Product-shaped DTOs | Historical published compatibility | Narrow factual Engine contracts | Protocol major after ControlPlaneRequest migration and full compatibility window |
| context_kernel public path | External Rust source compatibility | engine::ContextEngine and experimental lean-ctx-sdk façade | Separately approved public-major change |

No transition surface may receive new Product semantics.

## Compatibility matrix

| Consumer/path | P4 status | Compatibility proof |
| --- | --- | --- |
| Native Engine embed | PASS | Real context-view/recover, exact recovery and receipt lineage |
| OpenAI Agents | PASS, provider-free | Version 0.8.4 Runner success/abort |
| Python 3.9 | PASS | 19/19; clean offline wheel/Agents gate |
| Python 3.14 | PASS | 19/19 source suite |
| P3 Python Preview | PRESERVED/FROZEN | Existing seam and migration/rollback documentation |
| Engine CLI/MCP/proxy/Attach | PRESERVED | Focused Engine tests plus PR CI evidence |
| Rust Engine embedding | PRESERVED | engine::ContextEngine remains public; experimental crate remains opt-in |
| Cloud | NOT REQUIRED | All proof is local/provider-free |
| Live provider | NOT CLAIMED | Separate future approval and credentials required |

## Provenance

Production SDK source disposition is CLEAN_REIMPLEMENT. The reviewed SDK does
not reuse Apache implementation source. Engine changes remain Apache-side.
Therefore no APPROVED_REUSE or legal exception is claimed.

Any future source entering the BSL tree still requires one recorded disposition:
CLEAN_REIMPLEMENT, APPROVED_REUSE, KEEP_APACHE_COMPAT, DEPRECATE_APACHE or
LEGAL_REVIEW_REQUIRED.

## Validation and review evidence

- SDK independent release report:
  /private/tmp/leanctx-p4-v4-current/sdk-release-review/RESULT.md
- R4F factual-evidence review:
  /private/tmp/leanctx-p4-v4-next/r4f-factual-review.md
- R4G protocol review:
  /private/tmp/leanctx-p4-v4-next/r4g-protocol-review.md
- R5 D1 review:
  /private/tmp/leanctx-p4-v4-next/r5-d1-review.md
- R5 D2 review:
  /private/tmp/leanctx-p4-v4-next/r5-d2-review.md
- R6 review:
  /tmp/r6-review.md
- Final architecture/security review:
  /tmp/p4-final-review.md — PASS, no P0–P3 blocker
- R4F focused kernel gate: 510/510
- R5 D1 focused kernel gate: 499/499
- R5 D2 focused gate: 3/3
- Protocol narrowing gate: 4/4
- R6: cargo fmt --check, git diff --check and cargo metadata passed
- Final Layer-3 gate: cargo test --lib passed 10,473/10,473; cargo clippy
  --all-features -- -D warnings passed; cargo fmt --check passed

D1 review found no P0/P1 and correctly recorded a potential source-compatibility
risk because the deleted paths were syntactically public. The integrated
decision accepts the removal as D1: there are no in-repository or supported
runtime/embed consumers, the public OSS contract does not name those stores,
and R6 preserves the supported embedding façade. This decision and one-commit
rollback remain explicit.

## P4 exit criteria

| # | Criterion | Candidate status |
| --- | --- | --- |
| 1 | P3 recorded integrated COMPLETE | PASS |
| 2 | Separate Production SDK staging exists | PASS |
| 3 | Five Product primitives exist | PASS |
| 4 | SDK uses public Engine contracts only | PASS |
| 5 | Native Embed works | PASS |
| 6 | OpenAI Agents reference integration works | PASS |
| 7 | Compatibility matrix exists | PASS |
| 8 | Apache Python Preview is transition compatibility | PASS |
| 9 | Major ownership seams separated or bounded | PASS |
| 10 | Safe decommission candidates removed meaningfully | PASS |
| 10a | No duplicate canonical Product lifecycle | PASS |
| 11 | OSS Coding-Agent core remains strong | PASS locally; final Layer-4 CI pending |
| 12 | Engine security remains strong | PASS locally; final Layer-4 CI pending |
| 13 | SDK is materially more than a thin wrapper | PASS |
| 14 | No Cloud required | PASS |
| 15 | P5+ did not begin | PASS |
| 16 | Unresolved BSL parameters explicit | PASS |
| 17 | Provenance disposition exists | PASS |
| 18 | Clean-machine SDK integration passes | PASS |
| 19 | Preview-to-SDK migration/rollback documented | PASS |
| 20 | Independent architecture/security review has no blocker | PASS; final PR review/CI pending |

The local candidate satisfies the architecture, SDK and Layer-3 gates. Formal
P4 status remains ACTIVE until the serial Layer-4 PR CI/merge sequence records
the final authoritative main SHA.

## Rollback

- SDK: keep the `a7afd1f` ledger private; rollback to immutable implementation
  baseline `11f77debc2811dfd6569ba3a30bb674ae5e8b5d1` or retain P3 Preview.
- Engine: each wave is a separate commit; revert one owned slice at a time.
- R5 D1 recovery: revert d83ef94882 to restore both detached stores and the
  legacy generator.
- R5 D2 recovery: revert 581d488477 to restore heuristic research code.
- R6 recovery: revert e495b61f24; no binary or install path changed.
- Never replace accepted main with the entire local stack at once; deliver and
  validate serially.

## Stop line

Do not begin P5 Workspace/package/handoff, Cloud, Continuous Optimization,
Selection Intelligence, Fleet, marketplace or A2A expansion from this work.
After authoritative P4 merge evidence is recorded, stop.
