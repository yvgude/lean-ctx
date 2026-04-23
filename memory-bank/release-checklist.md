# Release Checklist — lean-ctx

## Pre-Release

### 1. Bump Version (3 locations — rest is automatic)

| File | Method |
|------|--------|
| `rust/Cargo.toml` | **Manual**: `version = "X.Y.Z"` |
| `packages/lean-ctx-bin/package.json` | **Manual**: `"version": "X.Y.Z"` |
| `packages/pi-lean-ctx/package.json` | **Manual**: `"version": "X.Y.Z"` |
| `rust/src/main.rs` | Auto: `env!("CARGO_PKG_VERSION")` |
| `rust/src/server/mod.rs` | Auto: `env!("CARGO_PKG_VERSION")` |
| `rust/src/shell.rs` | Auto: `env!("CARGO_PKG_VERSION")` |

> **Important:** The npm packages MUST be bumped before tagging. The release pipeline
> publishes whatever version is in `package.json` at the tagged commit. If forgotten,
> you can fix it with `npm publish` manually in each package directory rather than
> force-pushing the tag (which rebuilds everything).

**Verify no old version remains:**
```bash
rg 'OLD_VERSION' rust/src/ packages/*/package.json
```

### 2. Build & Test
```bash
cd rust
cargo fmt -- --check          # CI will fail without this!
cargo clippy --all-features -- -D warnings  # CI runs this too
LEAN_CTX_ACTIVE=1 cargo build --release
cargo test                    # All tests must pass
./target/release/lean-ctx --version  # Verify
```

### 3. Commit & Tag
```bash
git add -A
git commit -m "release: vX.Y.Z — <summary>"
git tag vX.Y.Z
```

---

## Publish

### 4. Push to Remotes
```bash
# GitHub (triggers CI + Release pipeline)
GIT_SSH_COMMAND="ssh -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes" git push github main --tags

# GitLab
git push origin main --tags
```

### 5. Wait for CI (all must pass before Release pipeline completes)
```bash
gh run list --repo yvgude/lean-ctx --limit 3
# Watch: gh run watch <run-id> --repo yvgude/lean-ctx --exit-status
```

**CI checks:** Format, Clippy, Tests (ubuntu/macos/windows), Adversarial Safety, Security, CodeQL

### 6. Release Pipeline (AUTOMATIC — triggered by tag push)
The `release.yml` GitHub Actions workflow handles all of the following automatically:
- **Binary builds**: 7 targets (x86_64/aarch64 × linux-gnu/linux-musl/macos, x86_64-windows)
- **GitHub Release**: via `softprops/action-gh-release` with CHANGELOG notes
- **crates.io**: via `cargo publish` with `CARGO_REGISTRY_TOKEN`
- **Homebrew**: auto-updates formula with new SHA256
- **npm**: publishes `lean-ctx-bin` + `pi-lean-ctx`

**Do NOT run `cargo publish` or `gh release create` manually** — the pipeline does this.

### 7. Update AUR (lean-ctx — source build)
```bash
# Get SHA256 of GitHub source tarball
curl -sL "https://github.com/yvgude/lean-ctx/releases/download/vX.Y.Z/lean-ctx-X.Y.Z-source.tar.gz" \
  -o /tmp/lean-ctx-X.Y.Z-source.tar.gz
shasum -a 256 /tmp/lean-ctx-X.Y.Z-source.tar.gz

# Update aur/lean-ctx/PKGBUILD: pkgver, sha256sums
# Update aur/lean-ctx/.SRCINFO: pkgver, source URL, sha256sums
# Push to AUR:
cd aur/lean-ctx && git add -A && git commit -m "update to vX.Y.Z" && git push origin master
```

### 8. Update AUR (lean-ctx-bin — pre-built binary)
```bash
# Get SHA256 of both Linux binaries from GitHub Release
curl -sL "https://github.com/yvgude/lean-ctx/releases/download/vX.Y.Z/lean-ctx-x86_64-unknown-linux-gnu.tar.gz" \
  -o /tmp/lean-ctx-linux-x86_64.tar.gz
curl -sL "https://github.com/yvgude/lean-ctx/releases/download/vX.Y.Z/lean-ctx-aarch64-unknown-linux-gnu.tar.gz" \
  -o /tmp/lean-ctx-linux-aarch64.tar.gz
shasum -a 256 /tmp/lean-ctx-linux-x86_64.tar.gz /tmp/lean-ctx-linux-aarch64.tar.gz

# Update aur/lean-ctx-bin/PKGBUILD: pkgver, sha256sums_x86_64, sha256sums_aarch64
# Update aur/lean-ctx-bin/.SRCINFO: pkgver, source URLs, sha256sums
# Push to AUR:
cd aur/lean-ctx-bin && git add -A && git commit -m "update to vX.Y.Z" && git push origin master
```

### 9. Commit AUR changes to main repo
```bash
cd /path/to/lean-ctx
git add aur/ && git commit -m "chore: update AUR packages to vX.Y.Z"
GIT_SSH_COMMAND="ssh -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes" git push github main
git push origin main
```

---

## Website Deployment (deploy branch → GitLab CI → leanctx.com)

### 10. Update Website Version
The website lives on the `deploy` branch (worktree at `lean-ctx-deploy/`):
```bash
cd /path/to/lean-ctx-deploy

# Update version.txt (source of truth for live version)
echo "X.Y.Z" > website/public/version.txt

# Update softwareVersion in Schema.org JSON-LD
# File: website/src/layouts/BaseLayout.astro → softwareVersion: 'X.Y.Z'

# Rebuild
cd website && PATH="/opt/homebrew/bin:$PATH" npm run build

# Commit & push (triggers GitLab CI deploy pipeline)
cd .. && git add -A && git commit -m "deploy: bump version to X.Y.Z" && git push origin deploy
```

**GitLab CI** (`.gitlab-ci.yml`) automatically:
1. Builds website (Node 22)
2. Rsyncs `dist/` to server
3. Rebuilds Docker image
4. Restarts `leanctx-web` container

---

## Post-Release Verification

### 11. Verify All Platforms
```bash
# crates.io
curl -s https://crates.io/api/v1/crates/lean-ctx | python3 -c "import sys,json; print(json.load(sys.stdin)['crate']['max_version'])"

# GitHub Release
gh release view vX.Y.Z --repo yvgude/lean-ctx

# AUR source
curl -s "https://aur.archlinux.org/cgit/aur.git/plain/PKGBUILD?h=lean-ctx" | grep pkgver

# AUR bin
curl -s "https://aur.archlinux.org/cgit/aur.git/plain/PKGBUILD?h=lean-ctx-bin" | grep pkgver

# Website
curl -s https://leanctx.com/version.txt

# Open issues (should be 0)
gh issue list --repo yvgude/lean-ctx --state open
```

### 12. Close Related GitHub Issues
```bash
gh issue close <number> --repo yvgude/lean-ctx --comment "Fixed in vX.Y.Z. <description>"
```

---

## Quick Reference

| Platform | Automation | Manual Step |
|----------|-----------|-------------|
| crates.io | Release pipeline | — |
| GitHub Release | Release pipeline | — |
| Homebrew | Release pipeline | — |
| npm | Release pipeline | — |
| AUR lean-ctx | Manual | Update PKGBUILD + .SRCINFO, push |
| AUR lean-ctx-bin | Manual | Update PKGBUILD + .SRCINFO, push |
| Website | GitLab CI on `deploy` push | Update version.txt + BaseLayout.astro |

## Hotfix: npm Version vergessen?

Falls die npm-Pakete vor dem Tag nicht gebumpt wurden:

```bash
# 1. Version in package.json korrigieren
# packages/lean-ctx-bin/package.json → "version": "X.Y.Z"
# packages/pi-lean-ctx/package.json  → "version": "X.Y.Z"

# 2. Manuell publizieren (KEIN Tag force-push noetig!)
cd packages/lean-ctx-bin && npm publish --access public
cd ../pi-lean-ctx && npm publish --access public

# 3. Commit & push
cd ../..
git add packages/*/package.json
git commit -m "chore: bump npm packages to X.Y.Z"
git push origin main && git push github main
```

> **Nie den Tag force-pushen** nur fuer npm — das loest einen kompletten Rebuild
> aller Binaries, crates.io, Homebrew usw. aus.

---

## SHA256 Sources

| Package | SHA256 Source |
|---------|-------------|
| AUR lean-ctx (source) | GitHub source tarball: `.../vX.Y.Z/lean-ctx-X.Y.Z-source.tar.gz` |
| AUR lean-ctx-bin (x86_64) | GitHub binary: `.../vX.Y.Z/lean-ctx-x86_64-unknown-linux-gnu.tar.gz` |
| AUR lean-ctx-bin (aarch64) | GitHub binary: `.../vX.Y.Z/lean-ctx-aarch64-unknown-linux-gnu.tar.gz` |
