# Plan: AI Dev-Workflow Hardening  (refs #899)

> Implementation plan for `./spec.md`. Review before tasks.

**Goal:** Wire a review-gated SDD loop into the Cursor workflow and enforce it with
deterministic gates, dogfooding lean-ctx's own primitives.
**Architecture:** Mostly additive config/docs (rules, templates, plans) plus two
Rust integration tests that mirror existing drift gates
(`mcp_manifest_up_to_date.rs`, `contracts_md_up_to_date.rs`).
**Tech Stack:** Cursor `.mdc` rules, Markdown templates, Rust integration tests.

## Global Constraints
- Additive only; do **not** touch `rust/src/core/addons/**` (in-progress WIP).
- No runtime behavior change; no edits to `docs/contracts/*-v1.md`.
- New rules stay <500 lines, reference files instead of copying.
- No mock data / placeholders. Output determinism preserved (#498).

## File Structure
| File | Responsibility | New/Modify |
|------|----------------|------------|
| `.cursor/rules/impact-first.mdc` | #900 impact-first rule (globs `rust/**`) | new |
| `specs/README.md`, `specs/_template/*` | #901 SDD templates + conventions | new |
| `specs/899-dev-workflow-hardening/*` | #901 first spec (this) | new |
| `.cursor/plans/README.md` | #904 plan convention | new |
| `CONTRIBUTING.md` | #901/#904 "Spec-driven workflow" section | modify |
| `rust/tests/entrypoints_wired.rs` | #902 entrypoint smoke gate | new |
| `rust/tests/rules_drift.rs` (+ generator) | #903 rules SSOT drift gate | new |
| `scripts/preflight.sh` | run new gates in fast path | modify |

## Impact (run impact analysis first)
- #900/#901/#904 are config/docs → no Rust tests affected.
- #902/#903 add new integration tests → verify in isolation, then preflight.
- Affected modules to inspect for #902: `rust/src/cli/dispatch/**`,
  `rust/src/server/dispatch/**`, `rust/src/tool_defs/**`, `rust/src/core/mcp_manifest.rs`.
- For #903: `rust/src/core/instruction_compiler.rs` + existing drift-gate tests.

## Self-Review (fill before implementing)
- Spec coverage: #900↔T1, #901↔T2, #904↔T3, #902↔T4, #903↔T5.
- Placeholder scan: rules/templates contain only real, verified commands.
- Determinism: drift gates compare generated artifacts byte-for-byte.
- Cleanup: no scratch files committed.
