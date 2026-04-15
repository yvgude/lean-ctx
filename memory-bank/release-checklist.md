# Release Checklist — lean-ctx

## Pre-Release

### 1. Bump Version (7 locations!)
All must be updated to the new version `X.Y.Z`:

| File | Location | Pattern |
|------|----------|---------|
| `rust/Cargo.toml` | Line 3 | `version = "X.Y.Z"` |
| `rust/src/main.rs` | `--version` branch | `println!("lean-ctx X.Y.Z")` |
| `rust/src/main.rs` | `run_mcp_server()` | `tracing::info!("lean-ctx vX.Y.Z MCP server starting")` |
| `rust/src/main.rs` | `print_help()` | `"lean-ctx X.Y.Z — The Cognitive Filter..."` |
| `rust/src/server.rs` | `get_info()` | `Implementation::new("lean-ctx", "X.Y.Z")` |
| `rust/src/shell.rs` | `interactive()` | `eprintln!("lean-ctx shell vX.Y.Z (wrapping ...)")` |
| `rust/src/dashboard/dashboard.html` | footer | `<span class="version">vX.Y.Z</span>` |
| `rust/src/core/stats.rs` | `format_gain()` | `lean-ctx vX.Y.Z  |  leanctx.com` |

**Verify no old version remains:**
```bash
rg 'OLD_VERSION' rust/src/
```

### 2. Build & Test
```bash
cd rust
LEAN_CTX_ACTIVE=1 cargo build --release
./target/release/lean-ctx --version  # Verify
```

### 3. Commit & Tag
```bash
git add -A
git commit -m "chore: bump version to vX.Y.Z"
git tag vX.Y.Z
```

---

## Publish

### 4. Push to Remotes
```bash
# GitHub (triggers CI for binary builds)
GIT_SSH_COMMAND="ssh -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes" git push github main --tags

# GitLab
git push origin main --tags
```

### 5. Publish to crates.io
```bash
cd rust
cargo publish
```

### 6. Wait for GitHub Actions CI
```bash
gh run list --repo yvgude/lean-ctx --limit 1
# Wait until status: completed + success
```

### 7. Create GitHub Release
```bash
gh release create vX.Y.Z --title "vX.Y.Z — <title>" --notes "<release notes>"
```
**Note**: CI already creates a release if `softprops/action-gh-release` is configured. Check if one already exists before creating manually.

### 8. Update Homebrew
```bash
# Get SHA256 of GitHub source tarball
curl -sL "https://github.com/yvgude/lean-ctx/archive/refs/tags/vX.Y.Z.tar.gz" \
  -o /tmp/lean-ctx-vX.Y.Z.tar.gz
shasum -a 256 /tmp/lean-ctx-vX.Y.Z.tar.gz

# Update formula
cd /tmp/homebrew-lean-ctx  # or clone fresh
# Edit Formula/lean-ctx.rb: update url, sha256, test version
git add -A && git commit -m "update lean-ctx to vX.Y.Z"
GIT_SSH_COMMAND="ssh -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes" git push origin main
```

### 9. Update AUR (lean-ctx — source build)
```bash
# Get SHA256 of crates.io crate
curl -sL "https://crates.io/api/v1/crates/lean-ctx/X.Y.Z/download" \
  -o /tmp/lean-ctx-X.Y.Z.crate
shasum -a 256 /tmp/lean-ctx-X.Y.Z.crate

# Clone, update, push
cd /tmp && git clone ssh://aur@aur.archlinux.org/lean-ctx.git lean-ctx-aur
# Edit PKGBUILD: pkgver, sha256sums
# Edit .SRCINFO: pkgver, source URL, sha256sums
git add -A && git commit -m "update to vX.Y.Z" && git push origin master
```

### 10. Update AUR (lean-ctx-bin — pre-built binary)
```bash
# Get SHA256 of Linux binary from GitHub Release
curl -sL "https://github.com/yvgude/lean-ctx/releases/download/vX.Y.Z/lean-ctx-x86_64-unknown-linux-gnu.tar.gz" \
  -o /tmp/lean-ctx-linux.tar.gz
shasum -a 256 /tmp/lean-ctx-linux.tar.gz

# Clone, update, push
cd /tmp && git clone ssh://aur@aur.archlinux.org/lean-ctx-bin.git lean-ctx-bin-aur
# Edit PKGBUILD: pkgver, sha256sums
# Edit .SRCINFO: pkgver, source URLs, sha256sums
git add -A && git commit -m "update to vX.Y.Z" && git push origin master
```

---

## Post-Release

### 11. Update Website
Update version references in website docs (if tracked):
- `website/src/pages/docs/cli.astro` — meta description, terminal examples, `--version` output
- `website/src/components/TerminalShowcase.astro` — footer version
- `website/src/pages/changelog.astro` — add new release entry
- `README.md` — version references

### 12. Build & Deploy Website
Follow [deployment.md](deployment.md) steps 1-5.

### 13. Verify All Platforms
```bash
# crates.io
curl -s https://crates.io/api/v1/crates/lean-ctx | jq '.crate.max_version'

# GitHub Release
gh release view vX.Y.Z --repo yvgude/lean-ctx

# Homebrew
brew update && brew info lean-ctx

# AUR (may be cached for hours)
curl -s "https://aur.archlinux.org/cgit/aur.git/plain/PKGBUILD?h=lean-ctx" | grep pkgver

# Website
curl -s https://leanctx.com/docs/cli/ | grep 'X.Y.Z'
```

---

## Quick Reference: SHA256 Sources

| Package | SHA256 Source |
|---------|-------------|
| Homebrew | GitHub source tarball: `https://github.com/yvgude/lean-ctx/archive/refs/tags/vX.Y.Z.tar.gz` |
| AUR lean-ctx | crates.io crate: `https://crates.io/api/v1/crates/lean-ctx/X.Y.Z/download` |
| AUR lean-ctx-bin | GitHub binary: `https://github.com/yvgude/lean-ctx/releases/download/vX.Y.Z/lean-ctx-x86_64-unknown-linux-gnu.tar.gz` |
