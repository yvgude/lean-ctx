# P4 SDK Extraction and Engine Refactor Report

**Status:** ACTIVE — V4 dual-track ledger
**Mode:** `EXECUTION`
**Date:** 2026-08-25
**Scope:** P4 SDK construction and Apache Engine extraction only

This is a live review ledger, not a completion report. P3 remains
`COMPLETE/INTEGRATED`; P4 remains active. P5 Workspace/package/handoff work and
Cloud work have not begun.

## Authority and current truth

The V4 architecture is:

```text
optional private Cloud
        ↓
Production SDK — BSL 1.1 family
        ↓
public versioned Engine contract
        ↓
Engine — Apache-2.0
```

Engine owns factual mechanisms, deterministic execution, security, bounded
IO, Attach, protocol, recovery and OSS Coding-Agent paths. The SDK owns Product
intent, lifecycle, policy, reuse/freshness, integrations, compatibility and
evidence projection. Product lifecycle semantics must not remain canonical in
both places.

| Item | Recorded state | Claim boundary |
| --- | --- | --- |
| Engine integration baseline | `github/main@49a6b2053c` | Current local integration base; not a P4 completion SHA |
| Engine integration candidate | `eb8db0ca23` | R4A–R4E and D1 focused reviews passed; local/unmerged; no final Engine SHA |
| Persistent SDK candidate | `2bafc965d3c599bf568c1070e5e5810ffc0f6ebf` | Private staging, no remote; technical and offline-packaging review passed |
| SDK distribution | `PRIVATE_NOT_RELEASED` | No public package, release or production-support claim |
| License family | `BSL_1_1_DECIDED` | Exact parameters pending; no terms invented |
| SDK provenance | `CLEAN_REIMPLEMENT` | Historical Apache evidence is not an approval to reuse |
| P3 Python Preview | `COMPLETE/INTEGRATED` | Preserve seam; package lifecycle remains `OSS_TRANSITION_COMPAT` |
| P5 / Cloud | `NOT BEGUN` | No Workspace, package, handoff or Cloud implementation started |

Historical SDK identity `fbfec0b` and associated provider assertions are
quarantined. They are not the persistent SDK candidate and must not be used as
the current evidence base. The candidate's recorded provider-free checks are
scoped to that private candidate; live-provider smoke is `UNVERIFIED`.

## Track A — SDK construction

The candidate contains the five intended Product primitives:
`ContextSession`, `ContextSource`, `ContextView`, `ContextPlan` and
`ContextReceipt`. Recorded candidate evidence is: source tests `10/10`, clean
wheel/install pass, real Engine `context-view`/`recover` pass, exact host
exception identity, and provider-free `openai-agents==0.19.4` pass. Focused
technical and packaging review passed; this is not an accepted release or a
live-provider result.

The SDK is a clean reimplementation candidate. No historical Apache module is
approved for BSL reuse. Any future reuse requires one explicit provenance
disposition (`APPROVED_REUSE`, `KEEP_APACHE_COMPAT`, `DEPRECATE_APACHE`, or
`LEGAL_REVIEW_REQUIRED`) before entering a BSL source tree.

| Surface | Current owner | Target owner | SDK successor | Engine extraction status | OSS impact | Transition/removal gate |
| --- | --- | --- | --- | --- | --- | --- |
| Product lifecycle primitives | P3 Python Preview plus private SDK staging | Production SDK, BSL 1.1 family | `2bafc965d3c599bf568c1070e5e5810ffc0f6ebf` (technical review passed) | Canonical lifecycle extraction remains open | Preserve integrated Preview behavior; no new Apache lifecycle | API/compatibility fixtures, release/legal approval and rollback |
| Python Product lifecycle | `packages/python-lean-ctx/**` | `OSS_TRANSITION_COMPAT` until successor release | Same private candidate | Freeze; no new lifecycle or adapter breadth | Security, correctness and migration path remain supported | Released successor, migration window, support owner, rollback and named removal decision |
| Framework reference path | Preview OpenAI Agents wrapper | SDK reference integration after acceptance | Candidate's maintained reference path | Engine remains provider-neutral | Keep host agent/model/provider ownership explicit | Clean-machine compatibility, provider-free fixture and separate live-provider approval |
| SDK evidence projection | P3 receipt seam plus candidate receipt | SDK `ContextReceipt` over factual Engine evidence | Candidate receipt model | Engine remains factual; no task-quality inference | Preserve truthful degradation and offline verification | Evidence/receipt compatibility, independent review and migration tests |

## Track B — Engine extraction and refactor

The Engine integration candidate `eb8db0ca23` starts from
`github/main@49a6b2053c` and is not closed or merged.

| Surface | Current owner | Target owner | SDK successor | Engine extraction status | OSS impact | Transition/removal gate |
| --- | --- | --- | --- | --- | --- | --- |
| `rust/src/core/session/**` | Apache Engine session state | Bounded Engine Attach continuity | SDK `ContextSession` | Bounded journal in `eb8db0ca23`; focused review passed | Retain required local Attach continuity and persisted user state | Consolidated gate, serial merge and revertable migration |
| Engine Kits/document parser | Apache Engine Kits | Open parser, validation and integrity mechanisms | SDK `ContextSource` / `ContextPlan` | Validated parser in `eb8db0ca23`; focused review passed | Preserve `.ctxpkg`/format interoperability and safe admission | Compatibility fixture, consolidated gate and rollback |
| Request/path boundary | Apache Engine transport | Bounded public Engine contract | SDK public `context-view` / `recover` client | `a20b98c2e0` request/path bounds, `57bcb8db58` shared-seam bounds; both review candidates | Preserve PathJail, bounded IO, failure atomicity and recovery | v1/legacy compatibility, adversarial checks, independent review and serial integration |
| Profiles, policy, context field/compiler and kernel | Mixed Engine mechanisms and Product policy | Engine mechanisms; SDK intent and Product policy | SDK `ContextPlan` | R4A/R4D/R4E in `eb8db0ca23`; focused reviews passed; closure remains open | Preserve deterministic optimization, security and OSS Attach behavior | Remaining ownership, no duplicate canonical lifecycle, consolidated gate and rollback |
| Orphan `memory_branch` module | Unreachable private Engine code | Removed | None | D1 deletion in `eb8db0ca23`; independent reachability/rollback review passed | No public, persistence, security or OSS Core reachability | Consolidated gate and exact commit revert |
| Public protocol and evidence links | Apache Engine public boundary | Versioned factual Engine contract | SDK public client and receipt projection | Keep open; narrow Product semantics, no removal claim | Preserve Invocation, Observation, Measurement, Failure, Artifact and recovery | Contract fixtures, compatibility matrix and independent security review |
| Workspace/Cloud coordination | Not begun | Later private repositories/services | None in P4 | Not started | No OSS impact and no Cloud dependency | P4 exit plus separately authorized P5/P8 work |

### Current Engine integration stack

The current integration candidate serializes seven focused-review-passing
commits plus one reviewed deletion from `github/main@49a6b2053c`:

| Commit | Purpose | Status |
| --- | --- | --- |
| `3fd4a62d02` | Expose narrow Engine-consumed profile configuration | Focused review passed |
| `bd33dc4b07` | Expose validated document parser | Focused review passed |
| `e66570f3d9` | Add minimal Attach journal | Focused review passed |
| `3c36aa2215` | Bound Attach journal input and serialization | Focused review passed |
| `c7b3418f01` | Separate security and transition policy defaults | Focused review passed |
| `89cadca80e` | Inject explicit ContextField while preserving compatibility | Focused review passed |
| `25f0091698` | Add pure explicit compiler selection | Focused review passed |
| `eb8db0ca23` | Remove unreachable private `memory_branch` module | Independent reachability/rollback review passed |

No refactor PR or final Engine SHA is claimed. One consolidated Engine gate is
intentionally deferred until the remaining bounded slices are ready.

## Extraction closure status

**OPEN — P4 cannot complete yet.** The V4 closure gate audits every
`SDK_BSL_TARGET`, `SDK_MIGRATION_INPUT`, `SPLIT_REQUIRED` and
`OSS_NOT_PRESERVED` source group. Each Product-semantic implementation must
end as one of `REMOVED_FROM_ENGINE`, `REDUCED_TO_ENGINE_MECHANISM`,
`FROZEN_TRANSITION_COMPAT`, `DEPRECATED_WITH_NAMED_REMOVAL_GATE`, or
`RESEARCH_QUARANTINED`.

Current dispositions:

- `packages/python-lean-ctx/**`: intended `FROZEN_TRANSITION_COMPAT`; removal
  gate is not yet satisfied.
- `rust/src/core/session/**`: candidate `REDUCED_TO_ENGINE_MECHANISM`;
  focused review passed; consolidated integration gate remains.
- Kits/parser: integrated candidate Engine mechanism; compatibility gate remains.
- Profiles/policy/compiler seams: focused reviews passed; remaining ownership
  and closure work stays open.
- No source has been declared removed, no BSL reuse has been approved, and no
  duplicate-canonical-logic gate has passed.

OSS parity remains a preservation requirement: install/uninstall, doctor and
status, Claude/Codex/Cursor Attach, MCP startup, read/search/shell compression,
public Engine recovery, local evidence/offline verification, PathJail,
redaction and bounded execution must remain available through the extraction.

## Compatibility and release matrix

| Consumer/path | Current state | Successor/target | Gate |
| --- | --- | --- | --- |
| P3 Python Preview | Integrated; Preview only | SDK candidate after review | Preserve exact recovery, receipt truth and degradation |
| `packages/python-lean-ctx` | `OSS_TRANSITION_COMPAT` | Private SDK candidate | Successor release, migration window and rollback |
| Engine `context-view` / `recover` | Public local boundary | Versioned Engine contract | Compatibility fixtures and bounded transport review |
| OpenAI Agents `0.19.4` | Provider-free candidate check recorded | SDK reference integration | No live-provider or broad adapter claim; smoke remains unverified |
| Other providers/frameworks | Not supported by this README | Demand-gated SDK work later | Separate contract, support and provider review |
| Cloud/Workspace | Not begun | Later private work | P4 exit; no dependency in local path |

## Licensing, distribution and provenance

BSL 1.1 is the selected license family for the Production SDK. Change Date,
Change License, Additional Use Grant, namespace, publication/distribution,
OEM/pricing, contributor, security and release parameters are pending. The
candidate remains private and unreleased with no remote.

The SDK candidate is `CLEAN_REIMPLEMENT`. Historical evidence is retained only
as quarantined review material; `fbfec0b` is not a current identity. The Apache
Engine refactor remains Apache-side work. Any code crossing into the BSL SDK
requires a recorded provenance disposition and legal gate before merge.

## Rollback and recovery

- Keep `github/main@49a6b2053c` as the integration baseline; do not treat
  `eb8db0ca23` as merged or final.
- If consolidated review fails, retain P3's integrated seam and revert or repair
  one owned Engine candidate at a time; do not merge a stacked alternative
  blindly.
- Keep the SDK staging candidate private and recoverable at `2bafc965…`; no
  remote publication or destructive migration is authorized.
- Keep `packages/python-lean-ctx` as the transition path until a released
  successor, migration fixtures, support owner and bounded rollback exist.
- Preserve exact Engine recovery and receipt truth during every migration;
  fail closed on compatibility, identity, digest, policy or transport errors.

## P4 exit blockers and stop line

P4 remains blocked from completion by exact BSL parameters,
publication/release/security decisions, remaining Engine ownership slices, the
consolidated Engine gate, compatibility fixtures and extraction closure.
Live-provider smoke is unverified. No final SHA is recorded for either product.

P5 Workspace/package/handoff, P8 Cloud Receipt Board and later Cloud/optimization
work are explicitly **NOT BEGUN**. Stop at P4; do not scaffold or infer those
tracks from this report.
