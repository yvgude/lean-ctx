# Tasks: AI Dev-Workflow Hardening  (refs #899)

> Atomic, individually testable. Cite the spec in commits:
> `<type>(<scope>): … refs specs/899-dev-workflow-hardening`.

- [x] **T1 — impact-first rule (#900)**
  - Files: `.cursor/rules/impact-first.mdc`
  - Do: scoped rule (globs `rust/**`) mandating `ctx_impact` + affected-test verify.
  - Verify: `.mdc` frontmatter valid; rule shows in Cursor for `rust/**`.
- [x] **T2 — SDD templates + first spec (#901)**
  - Files: `specs/README.md`, `specs/_template/{spec,plan,tasks}.md`,
    `specs/899-dev-workflow-hardening/{spec,plan,tasks}.md`
  - Do: templates + conventions; dogfood with this initiative's spec.
  - Verify: structure present; commit convention documented.
- [x] **T3 — plan loop (#904)**
  - Files: `.cursor/plans/README.md`
  - Do: `.cursor/plans/` convention linking plan ↔ spec ↔ ticket.
  - Verify: README present; CONTRIBUTING references it.
- [ ] **T4 — entrypoint smoke gate (#902)**
  - Files: `rust/tests/entrypoints_wired.rs`, `scripts/preflight.sh`
  - Do: assert every MCP tool has a dispatch arm and every CLI subcommand routes.
  - Verify: `cargo test -q entrypoints_wired`; gate runs in preflight.
- [ ] **T5 — rules SSOT drift gate (#903)**
  - Files: `rust/tests/rules_drift.rs` (+ generator path), CONTRIBUTING note.
  - Do: regenerate agent-instruction artifacts from SSOT; fail on drift.
  - Verify: `cargo test -q rules_drift`.
- [ ] **T6 — docs glue (#901/#904)**
  - Files: `CONTRIBUTING.md`
  - Do: add "Spec-driven workflow" section describing the loop.
  - Verify: links resolve to `specs/README.md`, `.cursor/plans/README.md`.

## Done gate
- [ ] All EARS criteria covered.
- [ ] `cargo fmt --check && cargo clippy --all-features -- -D warnings && cargo test --all-features`
- [ ] `scripts/preflight.sh fast` green.
- [ ] GitLab #899–#904 statuses updated; spec referenced in commits.
