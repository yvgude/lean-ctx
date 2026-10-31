# Context Quality v1 — Evidence Tiers, Verdicts, Retention

Status: experimental
Owner: core engine
Code: `rust/src/core/context_quality/`

The engine's evaluation tooling (`lean-ctx eval`, `quality-lab`, `prove speed`,
the A/B testbench) was removed on 2026-10-30. The tier vocabulary and the
retention model below stay in force; paired task evaluations (tiers C and D) are
produced outside the engine.
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
| B | `deterministic_quality` | fidelity, retention, recovery, security checks | a transformation satisfies deterministic invariants |
| C | `recorded_regression` | replay of a captured real-model run | behaviour did not drift from that captured run |
| D | `live_task_evaluation` | live baseline vs treatment run against a model | non-inferiority on the declared suite, model and method |
| E | `production_outcome` | real deployment outcomes / holdouts | the evaluated behaviour holds in that deployment |

The tier of a paired model evaluation is derived, never declared by the caller:
fixture answers are tier A whether replayed or not; a replayed real-model capture
is tier C; a live call is tier D. A run whose caller does not state that it was
live is reported as tier C (the weaker claim).

## Verdicts

A paired evaluation reports one of `improved`, `non_inferior`, `regressed` or
`underpowered`. A run with fewer than 30 pairs (and no regression) is
`underpowered`; only a powered `improved`/`non_inferior` run of tier C or higher
supports a quality claim. The proxy's compression holdout uses the same 30-turn
floor per arm before it reports a measured reduction.

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

- **RETAINED** — the same fact appears as a whole token in the text the model
  receives (`src/lib.rs:42` is not retained by `archive/src/lib.rs:42`; runner
  summaries and statuses match case-insensitively);
- **RECOVERABLE** — absent, but an exact recovery path to the original was
  verified for this delivery;
- **LOST** — neither.

Hard invariants:

- **A lost critical probe fails the check.**
- **Every critical probe is checked.** Up to 4096 critical facts per text are all
  matched (first error, last summary and everything between); beyond that bound the
  check fails closed (`critical_unchecked`) instead of sampling.

Prose may be rewritten (abbreviations, whitespace) without counting as loss; atomic
facts must survive. Reversible rewrites are resolved before matching: the terse
auto-dictionary legend (`[dict: @D0=…]`) is expanded, and the Cargo dictionary's
`FAIL` / `PASS` count as `test result: FAILED` / `test result: ok`.

Security is a separate dimension: lines carrying a detected secret are not probed.
Removing a secret is a security action, never "context loss", and a secret
removed by policy is never reported as recoverable for the model.

Runtime use: the terse compression engine (shell and tool output, which has no
recovery path) checks the *final* text — after line filtering, dictionaries and
the auto-dictionary — and delivers the original when a critical probe would be
lost or could not be checked.

## Context Quality Receipt (`lean-ctx.context-quality-receipt` v1)

Independent dimensions, never one score:

| Dimension | Content | When not measured |
|---|---|---|
| transformation | source type, mode, input/delivered/saved tokens, ratio, engine version | — |
| retention | critical/important counts per RETAINED/RECOVERABLE/LOST, lost critical kinds, probe-limit flag | `UNMEASURED` (no probes) |
| recovery | handles emitted/verified, failures, critical failures | `UNMEASURED` |
| security | probe-bearing lines withheld because they carry a secret | — |
| task quality | always `UNMEASURED` — a single transformation never measures it | — |

Overall state: `FAIL` if a measured dimension failed, `UNMEASURED` if neither
retention nor recovery measured anything, `PASS` only when something was measured
and held. A gate checks "no measured failure"; that is never shown as `PASS` on its
own.

The receipt contains no content and no timestamp; identical inputs render
byte-identically (#498).

## Claims policy

Documentation and release notes may state a quality claim only with its tier,
suite, model population and verdict. Tier A/B results support statements about
mechanisms and invariants only.

Economic figures follow the same rule; their evidence levels per data path are
described in [measurement scope](../concepts/measurement-scope.md).
