# Task Spine v1 fixture cohort

This directory is a **SYNTHETIC, PUBLIC (Class A/B)** benchmark cohort for
comparing context-planned execution with a fabricated without-optimization
baseline. It contains no customer traces, production identifiers, secrets, or
copied repository content.

Each of the ten directories under `tasks/` contains the same five artifacts:

| Artifact | Purpose |
| --- | --- |
| `task_envelope.json` | TaskEnvelopeV1 admission and lineage metadata |
| `context_plan.json` | Planned context sources, views, and token budget |
| `execution_receipt.json` | ExecutionReceiptV1 actual usage and cost |
| `baseline.json` | Synthetic without-optimization observation |
| `outcome.json` | AcceptedOutcomeV1 acceptance and quality signals |

The task IDs are stable (`task-spine-v1-001` through `task-spine-v1-010`) and
are repeated consistently in every artifact in a task directory. The cohort
covers a small Python bug fix, a large Rust refactor, TypeScript tests,
Markdown documentation, a critical Go security fix, a React feature, Rust
performance work, CI YAML/Shell repair, a Python/FastAPI endpoint, and a
SQL/Rust database migration.

## ETPAO

ETPAO means effective tokens per accepted outcome. This fixture cohort uses

`(fresh_input_tokens + cached_input_tokens + output_tokens + reasoning_tokens)`
`/ accepted_outcomes`.

All ten outcomes are accepted, so each fixture has one accepted outcome. The
numbers are realistic but deliberately fabricated for reproducible comparison
only.

The cohort is read by the V3 receipt migration tests
(`rust/src/core/task_receipt_migration.rs`,
`rust/tests/v3_receipt_fixture_migration.rs`), the round-trip test
`rust/tests/suite/execution_receipt_roundtrip.rs` and the fixture matrix gate
`scripts/check-v3-fixture-migrations.py` (`docs/migration/v3-fixture-matrix-v1.json`).

The receipt signatures use one deterministic Ed25519 key reserved for this
public fixture set. The verification key is not a customer credential and is
only for reproducibility in `rust/tests/suite/execution_receipt_roundtrip.rs`.
