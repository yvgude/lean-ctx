# Verification evidence

The measurements and gate results below describe the initial 3.10.5 change.
The subsequent current-runtime rollout starts from public source `5006595751`
(3.11.4); its activation and verification are recorded separately.

## Build measurements

Measured in the same macOS worktree with the shared Cargo target directory:

| Scenario | Cargo elapsed time |
| --- | ---: |
| Existing production release build | 8m 25s |
| First dev-fast build | 8m 03s, including 3m 30s waiting for the compiler gate |
| Repeated dev-fast builds before invalidation fix | 17.40–19.17s |
| First dev-fast build after invalidation fix | 24.55s |
| Unchanged dev-fast build after fix | 0.43s, then 0.39s |
| One displayed help phrase changed | 32.02s |
| Help phrase restored | 29.56s |

The first two runs have different cache and scheduling conditions and do not
establish a cold-build speedup. The release run also reported an existing
rust-objcopy/libLLVM stripping warning; production profile settings were not
changed. Source-edit and unchanged-build timings measure different work.

Cargo verbose output identified the always-dirty input as the removed
`src/dashboard/dashboard.html`. The build script now watches existing `src`
recursively and retains the source-contamination guard.

## Behavioral and static checks

- Linux `cargo test --lib --locked`: 11,344 passed, zero failed, 24 ignored
  (162.92s). The source-only test copy initially lacked Git project-root
  metadata; initializing its temporary Git root resolved the path-dependent
  regression test, without changing product code.
- Linux `cargo clippy --all-features --locked -- -D warnings` and
  `cargo fmt --check` pass. macOS production/binary/example Clippy and format
  checks also pass; macOS test Clippy passes with the hook's test-only style
  allowances.
- The hook encountered 313 existing `assert_is_empty` pedantic style findings.
  It now preserves the repository's boolean empty-assertion style only in the
  test pass, while production stays strict. Capability detection avoids an
  unknown-lint failure with toolchains predating the
  [Clippy 1.98 rule](https://clippy.rust-lang.org/rust-1.99.0/index.html#assert_is_empty).
  Bash syntax and independent review of the hook change pass; no test assertions
  were rewritten.
- Actual dev-fast CLI: help and invalid/conflicting arguments never invoke
  Cargo; default and release modes select the expected profile. A failed build
  exits before any runtime stop or install operation.
- Offline real-Cargo regression: initial command execution, unchanged fresh
  artifacts with no command execution, and rejection of a new nested,
  unreferenced contaminated Rust file all pass. The fixture owns and removes
  its isolated target directory. The test also passes with CI RUSTFLAGS and
  forced color output.
- Narrative governance, internal-artifact and SDK-surface checks pass.
- `git diff --check` passes.
- Actionlint comparison with the base workflows adds zero diagnostics;
  existing workflow diagnostics remain.
- Independent fast-installer, build-script and CI reviews completed. The
  cached `build-script-executed` JSON event was rejected as an execution signal;
  the regression uses verbose command output with a positive control instead.

## Deployment boundary

Host changes retain the compiler memory gate, set conservative Cargo defaults
and reuse shared target directories. VM and interactive-build slice quotas
were verified active; no guest memory was lowered and no live job was stopped.
Existing Docker executor and aggregate CI slice limits were checked separately
from runner-manager limits and preserved. These independent limits do not
constitute a strict machine-wide reservation of spare CPUs.

The installed runtime reports 3.11.4 while this checkout is 3.10.5. No runtime
downgrade or release publication was performed. Host resource and Cargo cache
configuration changes are applied independently of installing this source.

## Current-runtime rollout (3.11.4)

The rollout starts from public source `5006595751`. Development entrypoints use
`dev-fast`; explicit release builds retain production settings. Global Cargo
aliases and profile fallbacks support older worktrees on both development hosts.
Aliases are strings: Cargo concatenates array aliases across configuration layers,
which otherwise duplicates the `build` subcommand. The original working checkout
also receives the tracked alias and Make/shell entrypoints without importing a
different source version into its branch.

- Five behavioral build/installer tests pass, including a real offline Cargo
  freshness/contamination check and installer failure/order/argument checks.
  Source builds respect the configured shared target directory. Installed binaries
  are independent copies, and build-only mode never stops or installs a runtime.
- The Linux GitLab permission fixture executes the actual source/symlink guard
  and cache-ownership blocks. Restored artifacts become writable to the test
  identity; protected source remains read-only; nested symlinks are not followed;
  symlink cache roots are rejected. Cache keys separate project, branch, job,
  runner and lockfile. Both runtime suites and the jobs=1 memory budget remain.
- YAML parsing passes; actionlint comparison adds no diagnostics to the base
  GitHub workflows. Existing diagnostics remain. Narrative
  governance, internal-artifact, SDK-surface and team-seat-value verifiers pass.
  Only the two changed CI-file hashes in the frozen manifest are refreshed;
  normative contract schemas, fixtures and verifier code are unchanged.
- Independent installer/classifier and CI reviews found no remaining blocker.
  Review caught and resolved first-install user-local directory creation and
  no-checkout build-only handling.

The Linux library suite exposes an existing test cost:
`concurrent_cache_hits_are_lossless` drives 16,000 synchronous event observations.
It completes successfully; persistence and concurrency assertions are preserved.

The final 3.11.4 macOS source passes `cargo check --locked`, all 12,918 library
tests (zero failures, 31 ignored; 196.07s), strict all-feature Clippy and fmt.
The final edited fast build takes 2m37s; an unchanged `cargo fast` takes 0.63s,
and the installer's cached build takes 0.17s. The supported atomic installer
successfully installs 3.11.4 and restores autostart. A standalone crate with no
repository config/profile builds successfully with the global fast alias on
both hosts. These are warm-cache measurements, not a cold-build comparison.

The initial Linux full suite reports 12,816 passed and 57 permission-related
failures. Its login umask 0002 makes tempfile 3.27 directories group-writable;
permission-sensitive fixtures correctly refuse them. The exact same entitlement
test binary fails under subprocess umask 0002 and passes under 0077. The runtime
CI test shells, Make and preflight now use a local umask 077; login defaults and
explicit insecure-permission assertions are unchanged. The final Linux rerun
passes 12,873 tests, zero failures and 32 ignored (328.61s), followed by strict
all-feature Clippy (1m20s) and fmt. Source hashes for the changed Rust files,
manifest, lockfile and build script match between the two hosts.

The real pre-commit hook passes production and test Clippy and fmt. The final
CI-parity preflight passes all eight applicable checks, including strict
rustdoc, generated-doc/registry drift and Windows compilation; the unrelated
CCR regression is skipped by its existing change classification.

Canonical license, provenance and third-party-notice audits pass. The new Cargo
config is classified by its exact path in the Apache rule; source headers are
present in the two new Python build tests. The independent policy review confirms
that no wildcard allowance or private-source boundary changed.

Named temporary fixture targets and the completed Linux test checkout are
removed. The source tree contains no task transcripts or binary backups; the
active integration worktree and its owned external evidence remain until CI and
the requested final installation finish.
