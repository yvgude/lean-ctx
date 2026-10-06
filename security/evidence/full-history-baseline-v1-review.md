# Full-history baseline review

## Scanner-source re-attestation for the v4 merge

The v4 merge changes pinned scanner sources, so the attestation moves with it
(same chain as 7bf1ac8595). The audited history does not change.

- Audited commit, findings, counts and current-tree IDs are unchanged
  (`bff280bfc8eba6959854fafcbf104b5745d794c3`).
- `rust/src/proxy/anthropic.rs` changes digest (merged v4 and #1912 edits).
- Four v4 detector sources join the pinned scanner sources, mirroring the
  existing `anthropic.rs` + `anthropic_tests.rs` pair: they hold credential
  prefixes as rejection rules or synthetic rejection fixtures, never live
  credentials. Pinning makes every later edit to them a deliberate
  re-attestation.
  - `rust/crates/lean-ctx-protocol/src/credential_redaction.rs`
  - `rust/crates/lean-ctx-protocol/src/context_checkpoint.rs`
  - `rust/crates/lean-ctx-protocol/src/context_checkpoint/tests_core.rs`
  - `rust/src/core/execution_ledger/host/checkpoint_transfer/content.rs`
- Policy fingerprint `ed08cf77` → `b5057df6`; report digest `5e78496c` →
  `e4853cbb`. The gate reports 0 new findings on the merged tree.
- Forbidden paths, the secret rule and the limits are unchanged. No path
  exemption is added. The regeneration reproduces unchanged `main`
  byte-identically before it is applied.

## Baseline audit

- Audit target: `bff280bfc8eba6959854fafcbf104b5745d794c3`
- Audit source: clean post-rewrite clone, all public branches and tags
- Scanner contract: `commit-path/v2`, `diff-pickaxe/v2`, `public-tree/v2`
- Coverage: 16,291 commits, 99,593 objects, 558 metadata-only findings
- Current tree: 11 accepted finding IDs

The previous 473 findings remain present with identical IDs. The 85 additional
object-specific IDs introduce zero new `(scanner, rule, path)` combinations.
They therefore represent already-audited path classes across later or rewritten
objects, not a newly accepted finding class.

No rule, forbidden-path entry, source-binding requirement, or allowlist was
weakened. The pending audit status remains intentional because remediation of
other historical classes is outside this baseline rotation.
