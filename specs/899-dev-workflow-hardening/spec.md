# Spec: AI Dev-Workflow Hardening — Cursor-Loop + Dogfooding  (refs #899)

> SDD spec-anchored. Source of truth for the *intent* of this initiative.
> This is the first spec authored under `specs/` — it documents (and dogfoods)
> the very workflow it introduces.

## Problem / Why
lean-ctx is ahead of most 2026 teams on dev *infrastructure* (specs/contracts,
drift gates, evidence ledger, `ctx_impact`), but the day-to-day **Cursor loop**
does not consistently use it, and the project's own primitives are under-dogfooded.
Symptoms: a released binary with un-wired entrypoints (v3.4.7), instruction sprawl
across AGENTS.md / CLAUDE.md / .kiro / .cursor/rules with no drift gate, and a
manual plan/spec practice that is not anchored in the editor loop.

## Goal
A repeatable, review-gated loop (`spec → plan → tasks → implement → verify →
evidence`) that is wired into Cursor and enforced by deterministic gates.

## Acceptance Criteria (EARS)
- **#900** WHEN the agent edits `rust/src/**`, THE workflow SHALL require running
  impact analysis (`ctx_impact`) and verifying the affected tests beforehand.
- **#901** THE repo SHALL provide `specs/_template/{spec,plan,tasks}.md` and THE
  workflow SHALL create `specs/NNN-<slug>/` per non-trivial feature; commits SHALL
  cite the spec (`refs specs/NNN-<slug>`).
- **#902** THE test suite SHALL fail if any registered MCP tool lacks a dispatch
  arm, or any CLI subcommand falls through to help (no un-wired entrypoints).
- **#903** WHEN the agent-instruction SSOT changes, THE generated rule artifacts
  SHALL be regenerable and a drift test SHALL fail on divergence.
- **#904** THE repo SHALL establish `.cursor/plans/` with a convention linking
  plan ↔ `specs/NNN` ↔ GitLab ticket, documented in CONTRIBUTING.

## Out of Scope
- Migrating existing `docs/superpowers/**` or `docs/specs/**` into `specs/`.
- Changing any runtime behavior or `docs/contracts/*-v1.md`.
- Touching the in-progress `rust/src/core/addons/**` work.

## Verification (deterministic first)
- `cargo test -q entrypoints_wired` · `cargo test -q rules_drift`
- `scripts/preflight.sh fast`
- Rules/specs render in Cursor; `.mdc` frontmatter valid.

## Links
- GitLab: #899 (epic) → #900 #901 #902 #903 #904
- Plan: ./plan.md · Tasks: ./tasks.md
