# Context Quality v1 — Evidence Tiers, Verdicts, Retention

Status: experimental
Owner: core engine
Code: `rust/src/core/context_quality/`, `rust/src/core/eval_ab/report.rs`, `rust/src/core/quality_lab/`
Related: [quality-loop-v1](quality-loop-v1.md) (runtime edit feedback), issue #1905

## Why

LeanCTX produces several kinds of quality evidence. They answer different
questions, and a weak kind must never be presented as a strong one: a pipeline
check on two fixture tasks shows that the evaluation runs; it does not show that
compression keeps answer quality. This contract fixes the vocabulary every
quality report uses.

Token reduction is never quality evidence. A cheaper failed task is not a success.

## Evidence tiers

Every quality report states its tier (`evidence_tier`). Tiers order by claim
strength; only tiers C and above can back a statement about task quality on a
real model.

| Tier | Code | Produced by | Shows |
|---|---|---|---|
| A | `mechanism` | fixture answers (`provider = recorded` at capture) | the evaluation pipeline runs and is reproducible |
| B | `deterministic_quality` | fidelity, retention, recovery, security checks; `lean-ctx quality-lab` | a transformation satisfies deterministic invariants |
| C | `recorded_regression` | replay of a captured real-model run (`eval ab --replay`) | behaviour did not drift from that captured run |
| D | `live_task_evaluation` | live baseline vs treatment run (`eval ab` against a model) | non-inferiority on the declared suite, model and method |
| E | `production_outcome` | real deployment outcomes / holdouts | the evaluated behaviour holds in that deployment |

The tier of a paired model evaluation is derived, never declared by the caller:
fixture answers are tier A whether replayed or not; a replayed real-model capture
is tier C; a live call is tier D. A run whose caller does not state that it was
live is reported as tier C (the weaker claim).

## Eval verdicts (`lean-ctx.eval-ab-report` v2)

Report schema v2 is additive over v1 (`evidence_tier`, `power`, new verdict).
v1 reports still parse; they render `EVIDENCE: UNSPECIFIED` and back no claim.

| Verdict | Condition |
|---|---|
| `regressed` | bootstrap CI lower bound of the paired delta < −margin — at **any** sample size |
| `inconclusive` | no pairs, or fewer than `MIN_POWERED_PAIRS` (30) pairs without a regression |
| `improved` | powered run, CI lower bound > 0 |
| `non_inferior` | powered run, CI lower bound ≥ −margin |

- The CI gate (`--gate`) fails only on `regressed`. An `inconclusive` run passes
  the gate as a mechanism check and backs no quality claim.
- `supports_quality_claim()` is true only for a powered `improved`/`non_inferior`
  run of tier C or higher. `eval ab`, `eval verify` and the testbench findings
  print the tier and whether a claim is supported.
- Several verdicts combine to the most conservative one:
  `regressed` > `inconclusive` > `non_inferior` > `improved`; an empty set is `inconclusive`.
- `eval footprint` recommends pruning an injected element only on evidence
  (`non_inferior` or `regressed`). An inconclusive run keeps the element: uncertainty
  moves toward more context, never less.

30 pairs is a floor, not a sufficiency proof. Whether a suite can detect a given
effect depends on its variance and the declared non-inferiority margin; reports
expose `n`, means, delta, CI and margin so a reader can judge.

## Retention model

A transformation (compression, filtering, summarising) is checked with atomic,
deterministically extracted probes of the original:

| Probe kind | Example | Criticality |
|---|---|---|
| `test_result` | `test result: FAILED`, `2 failed` | critical |
| `status` | `FAILED`, `panicked`, `OOMKilled`, `CVE-2024-1234` | critical |
| `error_code` | `E0308`, `TS2345`, `ERROR-503` | critical on a problem line, else important |
| `location` | `src/handlers/order.rs:42:17` | critical on a problem line, else important |
| `path`, `hash`, `url` | `src/lib.rs`, `3f9c2a7b81d4`, `https://…` | important |

Each probe resolves to:

- **RETAINED** — present verbatim in the text the model receives;
- **RECOVERABLE** — absent, but an exact recovery path to the original was
  verified for this delivery;
- **LOST** — neither.

Hard invariant: **a lost critical probe fails the check.** Prose may be rewritten
(abbreviations, whitespace) without counting as loss; atomic facts must survive.

Security is a separate dimension: lines carrying a detected secret are not probed.
Removing a secret is a security action, never "context loss", and a secret
removed by policy is never reported as recoverable for the model.

Runtime use: the terse compression gate (shell and tool output, which has no
recovery path) rejects a compressed result that loses a critical probe and
delivers the original instead.

## Context Quality Receipt (`lean-ctx.context-quality-receipt` v1)

Independent dimensions, never one score:

| Dimension | Content | When not measured |
|---|---|---|
| transformation | source type, mode, input/delivered/saved tokens, ratio, engine version | — |
| retention | critical/important counts per RETAINED/RECOVERABLE/LOST, lost critical kinds, probe-limit flag | `UNMEASURED` (no probes) |
| recovery | handles emitted/verified, failures, critical failures | `UNMEASURED` |
| security | probe-bearing lines withheld because they carry a secret | — |
| task quality | always `UNMEASURED` — a single transformation never measures it | — |

The receipt contains no content and no timestamp; identical inputs render
byte-identically (#498). `lean-ctx quality-lab --original A --compressed B`
prints it; `--gate` fails on a critical loss.

## Quality Lab

`lean-ctx.quality-lab` v2 renames the former `overall_quality_grade`
(`Premium/Good/…`) to `representation_grade` (`Excellent/Good/Acceptable/BelowThreshold`).
It grades savings, cache reuse and structural fidelity of a representation — not
task quality — and states `Task Quality: UNMEASURED`. v1 JSON (including
`"Premium"`) still parses.

## Claims policy

Documentation and release notes may state a quality claim only with its tier,
suite, model population and verdict. Tier A/B results support statements about
mechanisms and invariants only.
