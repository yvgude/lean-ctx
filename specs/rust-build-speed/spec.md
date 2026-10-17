# Rust development build speed

## Scope

Reduce repeated development compilation and release CI work without changing
the production release profile or dropping platform-specific builds.

## Acceptance

- When `dev-install` runs without options, it builds and installs the `dev-fast`
  profile with incremental compilation and timing output.
- When `dev-install --release` runs, it uses the existing production profile.
- When help or an invalid option is supplied, no build or install starts.
- When a Cargo target directory is overridden, installation reads the selected
  profile from that actual target directory.
- When sources are unchanged, repeated Cargo builds reuse fresh artifacts.
  Adding an unreferenced contaminated Rust source file still blocks the build.
- Release jobs cache each target variant and run identical native all-feature
  tests once per OS/architecture; musl/CUDA artifact builds remain required.
- Development-host build budgets preserve running jobs, reserve CPU for services and
  do not lower live memory limits or guest memory.
- Record measured builds and required test, clippy and format outcomes; do not
  infer speedup from profile settings alone.

## Flow rollout

- The shared Cargo alias is tracked and works alongside host-level config;
  host profiles also support older worktrees without changing their manifests.
- Make, shell installation and agent instructions use the same development
  flow; compilation failure leaves the running runtime untouched.
- Deploy the optimized installer from current 3.11.4 source without a version
  downgrade. Recognize the fixed fast alias in build-resource admission.
- GitLab cache reuse remains scoped to branch/project/job/runner and lockfile;
  cache ownership changes preserve the source-write guard and both test suites.
