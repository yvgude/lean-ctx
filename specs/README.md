# specs/ — Spec-Driven Development (SDD)

lean-ctx develops non-trivial features **spec-anchored** (2026 SDD): the spec is the
source of truth for *intent*; code + tests enforce it. Never jump spec → code —
review the plan, then the tasks, then implement in small, verified steps.

## Loop
```
spec → plan → tasks → implement (impact-first) → verify → evidence
```
See `CONTRIBUTING.md` → "Spec-driven workflow".

## Layout
- `specs/_template/` — copy these into a new feature dir.
- `specs/NNN-<slug>/` — one directory per feature; `NNN` = tracking issue iid.
  - `spec.md`  — problem, goal, EARS acceptance criteria, out-of-scope.
  - `plan.md`  — architecture, constraints, file structure, impact.
  - `tasks.md` — atomic, individually testable steps with verification.

## How this relates to existing docs (no duplication)
- `docs/contracts/*-v1.md` — **versioned runtime contracts** (SSOT for behavior).
  Unchanged; a spec may *touch* a contract but does not replace it.
- `docs/superpowers/` — prior design notes/plans; kept as history, not migrated.
- `specs/NNN-*` — **canonical home for new feature work** going forward.

## Conventions
- One feature = one `specs/NNN-<slug>/` dir; keep the spec to 1–3 pages.
- Acceptance criteria in **EARS** (`WHEN … THE system SHALL …`).
- Cite the spec in commits: `refs specs/NNN-<slug>`.
- Link the tracking issue (#NNN); the approved approach lives in `plan.md`.
- Skip the full loop for trivial fixes; use it for features, contracts, and
  anything touching the tool/CLI surface.
