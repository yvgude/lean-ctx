# OSS Release Checklist

> **Status: active OSS release gate, not a completion record.** Every unchecked
> item remains pending; this checklist does not authorize website, Cloud,
> managed-service, control-plane, or Research capability deployment. Delivery
> order and capability status are governed by the
> [OSS Vision Delivery Plan](../internal/execution/OSS-VISION-DELIVERY-PLAN.md)
> and the [internal canonical entry point](../internal/README.md). This document
> is the public local Runtime release boundary; maintainer-only operator notes do
> not replace or weaken any gate below.

## Overview

Use this checklist to prepare, publish, verify, and if needed roll back a
lean-ctx release. It complements [Release Integrity v1](../contracts/release-integrity-v1.md)
and the [Security Audit Schedule](../contracts/audit-schedule-v1.md).

## Prerequisites

- A clean release commit on protected `main`.
- Permission to create tags, publish releases, and update repository secrets.
- Rust, Cargo, Python 3, and the repository release scripts available locally.
- Access to both configured Git remotes.

Use one intended version consistently in the changelog, Cargo metadata, tag,
release assets, and verification commands.

## Steps

### 1. Pre-release gate

- [ ] All library tests pass.

  ```bash
  cargo test --manifest-path rust/Cargo.toml --lib
  ```

- [ ] Clippy reports zero warnings.

  ```bash
  cargo clippy --manifest-path rust/Cargo.toml --all-features -- -D warnings
  ```

- [ ] Formatting is clean.

  ```bash
  cargo fmt --manifest-path rust/Cargo.toml --check
  ```

- [ ] The standalone W1 customer-proof verifier passes its own contract tests.

  ```bash
  cargo test --manifest-path packages/leanctx-verify/Cargo.toml
  cargo clippy --manifest-path packages/leanctx-verify/Cargo.toml --all-targets -- -D warnings
  ```

  If a release contains a customer-facing V2 proof, verify the assembled
  document through the standalone binary with its external trust store and
  bounded artifact root. A self-attested key, an engine-side check, or schema
  validation alone is not proof.

  ```bash
  cargo run --manifest-path packages/leanctx-verify/Cargo.toml -- \
    v2 <customer-proof.json> --trust-store <customer-trust.json> \
    --artifact-root <proof-directory> --json
  ```

- [ ] Python remains labelled **Preview** and passes its package test suite;
  it must not be released as a broad framework or agent-runtime guarantee.

  ```bash
  cd packages/python-lean-ctx && python3 -m pytest
  ```

- [ ] The committed provider-free fixture boundary remains reproducible and
  fails closed for drift/path violations.

  ```bash
  cargo test --manifest-path rust/Cargo.toml --lib benchmark_spec::types
  ```

- [ ] If a release exposes a profile/Kit selection or rollback path, rehearse
  the corresponding local rollback tests before publication. Profiles and
  first-class Context Kits remain **Research** until their separate W4/W5 exit
  criteria are met.

  ```bash
  cargo test --manifest-path rust/Cargo.toml --lib calibrator::selection
  ```

- [ ] Narrative and claim language matches the internal authority.

  ```bash
  python3 scripts/check-narrative-governance.py
  ```

- [ ] `CHANGELOG.md` describes the new version and user-visible changes.
- [ ] `Cargo.toml` contains the intended version.
- [ ] The security audit is clean.

  ```bash
  cargo audit
  ```

- [ ] The history policy gate passes.

  ```bash
  python3 scripts/history-policy-gate.py gate --root . \
    --policy security/history-policy-v1.json \
    --output /tmp/leanctx-history-delta-evidence.json
  ```

- [ ] Branch protection is verified.

  Export a GitHub token with repository-administration read access as
  `GITHUB_TOKEN` before running this check and the secret-expiry check below.

  ```bash
  python3 scripts/verify-branch-protection.py verify
  ```

- [ ] Secret expiry is checked.

  ```bash
  python3 scripts/check-secret-expiry.py check \
    --policy security/secret-rotation-policy-v1.json \
    --repo yvgude/lean-ctx \
    --output /tmp/leanctx-secret-rotation-report.json
  ```

- [ ] The working tree contains only intentional release changes.

  ```bash
  git status
  ```

Do not tag a release with a failing gate. Correct the failure, commit it, and
repeat the relevant checks.

### Claim promotion gate

Do not turn a local measurement, receipt, cache diagnostic, fixture pass, or
code path into a public quality or savings claim. A customer-facing claim must
name its metric, matched workload, quality threshold, methodology, limitations,
and evidence state. “Verified” additionally requires the independently runnable
W1 verifier with external signer trust. A release may ship without such a claim.

### 2. Release process

1. Create and push the version tag from the approved release commit.

   ```bash
   git tag vX.Y.Z && git push github vX.Y.Z
   ```

2. Confirm CI builds binaries for:

   - macOS arm64 and x86_64
   - Linux x86_64 and arm64
   - Linux CUDA

3. Confirm CI attaches platform archives and binaries, `SBOM.txt`,
   `SHA256SUMS`, and `release-manifest.json` to the release.

4. Confirm `SHA256SUMS` is generated and signed where the release process has
   a signing key. Do not publish if a checksum or manifest artifact is missing.

The required chain is source commit → build artifacts → `SHA256SUMS` →
`SBOM.txt` → `release-manifest.json`. The integrity contract defines its
schema and failure behavior.

### 3. Post-release gate

- [ ] Download and verify each platform artifact from the published release.

  ```bash
  sha256sum -c SHA256SUMS
  ```

- [ ] Verify the complete published release directory.

  ```bash
  python scripts/verify-release-integrity.py verify --tag vX.Y.Z --dir ./release-files
  ```

- [ ] Update the compatibility matrix if platform, agent, or protocol support changed.
- [ ] Preserve Python’s **Preview** status in release notes and compatibility
  material; do not infer broader support from package tests.
- [ ] Record the exact offline fixture and rollback commands/results when the
  release changes those boundaries. A green unit test does not establish a
  customer-proof or a capability promotion.
- [ ] Close GitHub issues associated with the completed milestone.
- [ ] Update `install.sh` if a platform or installation flag changed.
- [ ] Push final protected-branch state to both remotes.

  ```bash
  SKIP_PREFLIGHT=1 git push github main && SKIP_PREFLIGHT=1 git push origin main
  ```

### 4. Rollback procedure

1. Stop promotion and mark or yank the faulty release in the release system.
2. Preserve failed artifacts and verification output for investigation.
3. Fix the critical defect on `main`, repeat the full pre-release gate, and tag
   a new patch release. Do not reuse or move the original tag.
4. Direct users to install the latest patched release.

   ```bash
   curl -fsSL https://leanctx.com/install.sh | sh
   lean-ctx status
   ```

The installer retrieves the latest published release. Do not tell users to
disable checksum verification as a rollback workaround.

### 5. Enterprise release follow-up

- [ ] Bump the Helm chart version and update its compatibility matrix.
- [ ] Regenerate the air-gap bundle from verified release assets.
- [ ] Verify digest-pinned deployment references before promotion.
- [ ] Coordinate the enterprise deployment window and rollback path.

Enterprise Helm, air-gap, and HA procedures are in the private deployment
repository; this OSS checklist deliberately does not reproduce them.

## Verification

A release is complete only when:

- Required CI builds and metadata assets are present.
- `sha256sum -c SHA256SUMS` succeeds for every downloaded platform artifact.
- The release-integrity verifier accepts the published release directory.
- The compatibility matrix and milestone state are current.
- Both remotes contain the final `main` commit and version tag.

It may describe only the capability status that is actually shipped: local
Runtime/CLI/MCP paths are Available, Python SDK v1 is Preview, and Profiles,
first-class Context Kits, benchmark/performance claims, managed operation, and
control-plane capabilities remain Research unless their explicit delivery gates
are complete.

Record the tag, source commit, verification output, and approved exceptions with
release evidence. Reject a release for any manifest, checksum, size, or tag
mismatch.

## Troubleshooting

- If `cargo audit` fails, remediate or document an approved exception before tagging.
- If history or branch-protection checks fail, correct repository state rather
  than bypassing the gate.
- If a checksum fails, discard the downloaded asset set and fetch every asset
  again; see [Release Integrity v1](../contracts/release-integrity-v1.md).
- If CI omits an expected platform, correct the build matrix before publishing.
- For a post-publication integrity incident, use
  [RB-05](runbook-index.md#rb-05-release-integrity-verification-failed).
