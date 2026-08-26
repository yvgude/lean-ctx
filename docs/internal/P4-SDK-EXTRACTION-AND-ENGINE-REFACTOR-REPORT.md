# P4 SDK Extraction and Engine Refactor Report

**Status:** TECHNICAL/INTERNAL EXIT COMPLETE — R4A–R6 merged; final Engine/SDK
gate passed; public/commercial release remains gated
**Date:** 2026-08-26
**Scope:** P4 Production SDK extraction and Apache Engine refactor only

P3 and the P4 V4 technical/internal scope are COMPLETE/INTEGRATED. P5 is not
integrated; the out-of-sequence `#1571` candidate is quarantined. Cloud is NOT
BEGUN. R4A–R6 passed
authoritative CI and landed; the accepted Engine and canonical private SDK also
passed the exact final clean-install integration gate.

This is not a public SDK release or commercial-readiness claim. Namespace,
publication/legal terms, support ownership and real customer/design-partner
validation remain explicit later gates.

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

| Item | Final identity | State |
| --- | --- | --- |
| Original V4 Engine baseline | 49a6b2053c08c4d831916f03e01c7b5e6079a372 | Historical start |
| Accepted R5 Engine baseline | efbdb796b9b361a1778545e29e8ec7a96f20bbd5 | R4A–R5 merged through PR 1566 |
| R6 upstream baseline | 0135ef25bd186ac95a1c0423a12cc6ae1479b946 | Main at PR 1573 creation |
| R6 reviewed candidate | b1d530718a7f8285cf726979ca41db30c7817a43 | PR 1573; reviewed patch ID 65f0a36e… |
| Final accepted Engine SHA | 5220ad11191e9de012dfedc97479dae6d28d1111 | PR 1573 squash merge; authoritative CI passed |
| Installed final Engine binary SHA-256 | 9cc619e11965c0f0505b638bc6d5825607f5fb204b72d2d7917e9ed7a2848ded | Atomically installed from the final accepted Engine SHA; final offline SDK rebind passed |
| Production SDK repository | /Users/yvesgugger/Documents/Privat/Projects/leanctx-product-sdk-staging | Separate private staging |
| Production SDK repository head | 8c84e2246bff0f2681dc69ea08d4322519f88050 | Current private docs/governance head |
| Production SDK ledger | a7afd1f | Clean, reviewed internal RC record |
| SDK implementation baseline | 11f77debc2811dfd6569ba3a30bb674ae5e8b5d1 | Immutable reviewed code |
| SDK wheel SHA-256 | b54ac013af1494c00a64feac8ce3eb1910fc32baa3ba9a6d4b52d565942ff141 | Reviewed artifact |
| SDK wheel path | /Users/yvesgugger/Documents/Privat/Projects/leanctx-product-sdk-artifacts/11f77debc2811dfd6569ba3a30bb674ae5e8b5d1/leanctx_product_sdk_local-0.1.0.dev0-py3-none-any.whl | Canonical content-addressed private artifact |
| SDK distribution | PRIVATE_NOT_RELEASED | No public release or namespace claim |
| License family | BSL_1_1_DECIDED | Exact publication parameters remain pending |
| SDK provenance | CLEAN_REIMPLEMENT | No Apache source copied into BSL staging |

Historical SDK identity fbfec0b is quarantined and is not release evidence.
An ignored `dist/` rebuild with SHA-256 `3889f83c…` was also quarantined as
unreviewed; it is not a release artifact and cannot be confused with `b54ac013…`.
The immutable SDK `P4-RELEASE-CANDIDATE.md` ledger's pre-R6 Engine source
`8fc5cfaf13` is historical and superseded for accepted-Engine compatibility by
the exact final gate against `5220ad1119`; SDK implementation and wheel
identities remain unchanged.

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
- final offline rebind against the installed Engine binary: Engine interface
  1.0.0, sealed integrity, exact recovery and succeeded status

Public publication remains intentionally blocked by unresolved BSL parameters,
public namespace, support/release process, pricing/OEM decisions and legal
approval. Those are explicit business/legal gates, not hidden engineering work.

## Track B — Engine refactor

| Wave | Candidate commit(s) | Disposition |
| --- | --- | --- |
| R4A Profiles | PR 1547 / bf9ae4be05 | Narrow Engine-consumed profile configuration |
| R4B Kits/parser | PR 1550 / 6fb8acb907 | Validated open parser/integrity mechanism |
| R4C Session | PR 1553 / 77aff10c3f | Bounded Attach continuity journal |
| R4D Policy | PR 1556 / 1c1f433532 | Security defaults separated from Product policy |
| R4E Field/compiler | PRs 1559, 1561 / 336fc8d237, d8137af554 | Explicit field input and deterministic bounded compiler |
| R4F Kernel | PR 1563 / 5cedbda3f3 | Product delivery semantics detached; factual measurements retained |
| R4G Protocol | PR 1564 / 73bca34676 | Historical Product-shaped types frozen with a named major-version gate |
| R5 D1 | PR 1565 closed unmerged; reclassified D4 | Detached public receipt/shadow stores retained pending a compatibility gate |
| R5 D2 | PR 1566 / efbdb796b9 | Behavioral outcome inference/learning retired; DTO compatibility retained |
| R6 | PR 1573 / 5220ad1119 (candidate b1d530718a) | Engine/package/embed boundaries clarified without install breakage |

The Engine changes were delivered serially through independent review and
authoritative GitHub CI. PR 1546 removed the earlier unreachable memory branch.

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

- Behavioral infer_outcome acceptance/rejection heuristics
- Inferred-outcome provider learning hook
- Inferred OutcomeTracker quality/degradation model
- Duplicate post-dispatch Product receipt construction
- Context-gate Product plan/receipt/shadow delivery logging
- Earlier unreachable memory_branch module from PR 1546

R5 D2 removed 244 lines of behavioral inference research. R4F first removed the
active duplicate delivery path. The proposed 459-line D1 deletion was withheld
because its detached stores and generator remain syntactically public.

The final direct-caller audit found no production inbound caller for the public
`context_kernel::feedback`, `context_kernel::learning`, or activation outcome
helpers. Production activation uses only `load_config` and `supplement_budget`.
These dormant Product-shaped helpers are `RESEARCH_QUARANTINED`, non-canonical
compatibility surface and may receive no new Product semantics.

### Frozen transition compatibility

| Surface | Why retained | Successor | Exact removal gate |
| --- | --- | --- | --- |
| packages/python-lean-ctx | Integrated P3 Preview users need migration/rollback | Production SDK | Released successor, migration window, support owner and rollback decision |
| OutcomeSignal/InferredOutcome DTOs | ProxyKernelResult exposed serialized/source fields | Explicit evaluator observations | Next public API major after consumers migrate |
| protocol knowledge_routing Product-shaped DTOs | Historical published compatibility | Narrow factual Engine contracts | Protocol major after ControlPlaneRequest migration and full compatibility window |
| context_kernel public path | External Rust source compatibility | engine::ContextEngine and experimental lean-ctx-sdk façade | Separately approved public-major change |
| Detached context_kernel and MCP receipt/shadow stores | Public Rust source compatibility despite no active supported consumer | Factual Engine evidence contracts | Explicit public-major/deprecation decision or compatibility shim |
| Dormant context_kernel feedback/learning and activation outcome helpers | Accidental public source compatibility; no production inbound caller | Explicit SDK/evaluator outcomes and Product feedback strategy | Named public-major removal after consumer audit, migration and compatibility window |

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
  /private/tmp/leanctx-p4-v4-current/sdk-final-8c84/RESULT.md
- R4F factual-evidence review:
  /private/tmp/leanctx-p4-v4-current/r4f-factual-review/RESULT.md
- R4G protocol review:
  /private/tmp/leanctx-p4-v4-next/r4g-protocol-review.md
- R5 D1 review:
  /private/tmp/leanctx-p4-v4-next/r5-d1-review.md
- R5 D2 review:
  /private/tmp/leanctx-p4-v4-next/r5-d2-review.md
- R6 review:
  /tmp/r6-review.md — PASS on patch-equivalent commit 0787066753; stable
  patch ID 65f0a36ec60599c435c221c78021245cc512988b
- Final accepted-Engine/SDK integration gate:
  /private/tmp/leanctx-p4-v4-current/final-engine-sdk-gate/RESULT.md — PASS on
  Engine 5220ad1119 and canonical wheel b54ac013…
- Final installed-Engine/SDK rebind:
  /private/tmp/leanctx-p4-v4-current/final-installed-engine-sdk-gate/RESULT.md —
  PASS on installed binary SHA-256 9cc619e1… and canonical wheel b54ac013…
- Pre-R6 architecture/security review:
  /tmp/p4-final-review.md — historical PASS on its examined scope; it explicitly
  left final CI/Windows/merge pending and is not the final closeout evidence
- Final technical/internal closeout review:
  /private/tmp/leanctx-p4-v4-current/final-docs-review/RESULT.md — PASS on all
  20 technical V4 criteria; public/commercial release remains gated
- Final bounded architecture/security review:
  /Users/yvesgugger/.leanctx-p4-orchestration/agents/P4-FINAL-REVIEW-V10.md —
  PASS with no P0-P3 findings; direct-caller evidence independently confirmed
- R4F focused kernel gate: 510/510
- R5 D1 focused kernel gate: 499/499
- R5 D2 focused gate: 3/3
- Protocol narrowing gate: 4/4
- R6 authoritative gates: CI 32936915240, Security 32936915221 and CodeQL
  32936915253 passed before PR 1573 merged
- Final Layer-3 gate: cargo test --lib passed 10,473/10,473; cargo clippy
  --all-features -- -D warnings passed; cargo fmt --check passed

D1 review found no P0/P1 but identified public Rust source-compatibility risk.
The deletion was therefore withheld and reclassified as D4: the detached stores
remain frozen until an explicit public-major/deprecation gate or compatibility
shim authorizes removal.

## P4 exit criteria

| # | Criterion | Final status |
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
| 11 | OSS Coding-Agent core remains strong | PASS — authoritative CI 32936915240, including Ubuntu and Windows |
| 12 | Engine security remains strong | PASS — Security 32936915221 and CodeQL 32936915253 |
| 13 | SDK is materially more than a thin wrapper | PASS |
| 14 | No Cloud required | PASS |
| 15 | P5+ did not begin | PASS on governed delivery — no P5 merged; out-of-sequence PR 1571 was closed, remote branch deleted and local work quarantined |
| 16 | Unresolved BSL parameters explicit | PASS |
| 17 | Provenance disposition exists | PASS |
| 18 | Clean-machine SDK integration passes | PASS — accepted Engine 5220ad1119, CPython 3.9.6, canonical wheel b54ac013…, sealed exact recovery |
| 19 | Preview-to-SDK migration/rollback documented | PASS |
| 20 | Independent architecture/security review has no blocker | PASS — final closeout review plus PR 1573 CI/security/CodeQL found no blocker |

Criterion 15 is evaluated against the accepted delivery state: the
out-of-sequence local draft never became an integrated P5 workstream and remains
quarantined pending separate authorization.

P4's technical/internal V4 scope satisfies the architecture, SDK, Layer-3 and
authoritative Layer-4 gates.
Final accepted Engine SHA is 5220ad11191e9de012dfedc97479dae6d28d1111.
Public/commercial release remains open and is not claimed by this exit.

## Rollback

- SDK: keep the `a7afd1f` ledger private; rollback to immutable implementation
  baseline `11f77debc2811dfd6569ba3a30bb674ae5e8b5d1` or retain P3 Preview.
- Engine: each wave is a separate commit; revert one owned slice at a time.
- R5 D1: no rollback is needed because PR 1565 closed without merging.
- R5 D2 recovery: revert PR 1566's squash merge to restore heuristic research
  code while retaining factual Engine evidence.
- R6 recovery: revert squash merge 5220ad11191e9de012dfedc97479dae6d28d1111;
  no binary or install path changes.
- Never replace accepted main with the entire local stack at once; deliver and
  validate serially.

## Stop line

P4 exit is complete. Do not automatically begin P5 Workspace/package/handoff,
Cloud, Continuous Optimization, Selection Intelligence, Fleet, marketplace or
A2A expansion from this work.
Stop after this closeout.
