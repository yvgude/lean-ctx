# Tasks: <Feature Name>  (refs #<iid>)

> Atomic, individually testable. Implement one at a time. Each task pairs a change
> with its verification. Cite the spec in commits: `<type>(<scope>): … refs specs/<iid>-<slug>`.

- [ ] **T1 — <name>**
  - Files: `…`
  - Do: <the change>
  - Verify: `cargo test -q <…>`  (the tests impact analysis flagged)
- [ ] **T2 — <name>**
  - Files: `…`
  - Do: <…>
  - Verify: `…`

## Done gate
- [ ] All EARS criteria covered by a task.
- [ ] `cargo fmt --check && cargo clippy --all-features -- -D warnings && cargo test --all-features`
- [ ] `scripts/preflight.sh fast` green.
- [ ] Tracking issue #<iid> status updated; spec referenced in commit(s).
