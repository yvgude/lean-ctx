# lean-ctx Deploy Checklist

Use this checklist for every release. Copy the section below and check off each item.

---

## Pre-Release

- [ ] All tests pass: `cd rust && LEAN_CTX_ACTIVE=1 cargo test`
- [ ] Clippy clean: `LEAN_CTX_ACTIVE=1 cargo clippy -- -D warnings`
- [ ] Format clean: `LEAN_CTX_ACTIVE=1 cargo fmt --check` (fix with `cargo fmt` if needed)
- [ ] Version bumped in ALL locations (replace `X.Y.Z` with new version):
  - [ ] `rust/Cargo.toml` → `version = "X.Y.Z"`
  - [ ] `rust/src/main.rs` → 3 occurrences (version print, help text, gain footer)
  - [ ] `rust/src/server.rs` → 2 occurrences (`Implementation::new`)
  - [ ] `rust/src/shell.rs` → 1 occurrence (shell hook eprintln)
  - [ ] `rust/src/dashboard/dashboard.html` → 1 occurrence (version badge)
  - [ ] `README.md` → 1 occurrence (example output)
  - [ ] `CHANGELOG.md` → new entry at top with date
- [ ] CHANGELOG.md has detailed entry for this release
- [ ] Release build succeeds: `LEAN_CTX_ACTIVE=1 cargo build --release`

## Local Install + Smoke Test

- [ ] Remove old binary: `rm -f ~/.cargo/bin/lean-ctx`
- [ ] Copy new binary: `cp rust/target/release/lean-ctx ~/.cargo/bin/lean-ctx`
- [ ] Verify version: `LEAN_CTX_ACTIVE=1 lean-ctx --version`
- [ ] Smoke test shell hook: `LEAN_CTX_ACTIVE=1 lean-ctx -c echo hello`
- [ ] Smoke test config: `LEAN_CTX_ACTIVE=1 lean-ctx config`

## Git

- [ ] Stage all changes: `git add -A`
- [ ] Commit: `git commit -m "release: vX.Y.Z — <summary>"`
- [ ] Tag: `git tag vX.Y.Z`
- [ ] Push to GitLab: `git push origin main --tags`
- [ ] Push to GitHub: `git push github main --tags`

## crates.io

- [ ] Publish: `cd rust && LEAN_CTX_ACTIVE=1 cargo publish`
- [ ] Verify: https://crates.io/crates/lean-ctx shows new version
- [ ] Get crates.io SHA256 (for AUR):
  ```bash
  curl -sL https://crates.io/api/v1/crates/lean-ctx/X.Y.Z/download -o /tmp/lean-ctx-crate.tar.gz
  shasum -a 256 /tmp/lean-ctx-crate.tar.gz
  ```

## GitHub Release

- [ ] Create release:
  ```bash
  gh release create vX.Y.Z --title "vX.Y.Z — <title>" --notes "<notes>" --repo yvgude/lean-ctx
  ```
- [ ] Upload macOS ARM binary:
  ```bash
  cp rust/target/release/lean-ctx /tmp/lean-ctx-darwin-arm64
  gh release upload vX.Y.Z /tmp/lean-ctx-darwin-arm64 --repo yvgude/lean-ctx
  ```
- [ ] Get GitHub tarball SHA256 (for Homebrew):
  ```bash
  # IMPORTANT: Always download to file first, never pipe!
  # `curl | shasum` gives WRONG results for GitHub archives.
  curl -sL -o /tmp/lean-ctx-src.tar.gz https://github.com/yvgude/lean-ctx/archive/refs/tags/vX.Y.Z.tar.gz
  shasum -a 256 /tmp/lean-ctx-src.tar.gz
  ```

## npm (lean-ctx-bin)

- [ ] Update version: `packages/lean-ctx-bin/package.json`
- [ ] Publish: `cd packages/lean-ctx-bin && npm publish`
- [ ] Verify: `npm view lean-ctx-bin version` shows new version

## AUR (Arch Linux)

### lean-ctx (source build from crates.io)
- [ ] Update `aur/lean-ctx/PKGBUILD`: bump `pkgver`, update `sha256sums` with crates.io SHA
- [ ] Update `aur/lean-ctx/.SRCINFO`: bump version, update source URL + SHA
- [ ] Push: `cd aur/lean-ctx && git add -A && git commit -m "X.Y.Z" && git push`

### lean-ctx-bin (pre-built binary from GitHub)
- [ ] Update `aur/lean-ctx-bin/PKGBUILD`: bump `pkgver`
- [ ] Update `aur/lean-ctx-bin/.SRCINFO`: bump version, update source URL
- [ ] Push: `cd aur/lean-ctx-bin && git add -A && git commit -m "X.Y.Z" && git push`

## Homebrew

- [ ] Update formula: `~/Documents/Privat/Projects/homebrew-lean-ctx/Formula/lean-ctx.rb`
  - [ ] Update `url` to new tag
  - [ ] Update `sha256` with GitHub tarball SHA (from file download, NOT pipe!)
  - [ ] Update `assert_match` version in test block
- [ ] Commit + push: `cd ~/Documents/Privat/Projects/homebrew-lean-ctx && git commit -am "X.Y.Z" && git push`

## Commit package updates

After all bundles are published, commit the updated package files back to both remotes:
```bash
git add -A
git commit -m "chore: update AUR + npm packages to X.Y.Z"
git push origin main
git push github main
```

## Website (leanctx.com) — only if website changes needed

Website deploys via GitLab CI on the `deploy` branch:

```bash
# 1. Create deploy branch from main
git stash
git branch -D deploy 2>/dev/null
git checkout -b deploy
git stash pop

# 2. Restore deploy files from last deploy
git checkout origin/deploy -- .gitlab-ci.yml Dockerfile.web website/ cloud/

# 3. Apply any website edits (re-apply if checkout overwrote them)

# 4. Commit and push
git add -f .gitlab-ci.yml Dockerfile.web website/ cloud/
git commit -m "deploy: <description>"
git push origin deploy --force

# 5. Switch back to main
git checkout main
git checkout -- .
```

- [ ] Verify: `curl -sL https://leanctx.com/ -o /dev/null -w "%{http_code}"` → 200

## Post-Release Verification

- [ ] `curl -s https://crates.io/api/v1/crates/lean-ctx | python3 -c "import sys,json; print(json.load(sys.stdin)['crate']['max_version'])"` → X.Y.Z
- [ ] `npm view lean-ctx-bin version` → X.Y.Z
- [ ] GitHub Release page has binary + notes
- [ ] `gh issue list --repo yvgude/lean-ctx --state open` → 0 issues (or expected)
- [ ] Close related GitHub issues with fix comment

## pi-lean-ctx — only if pi integration changed

`pi-lean-ctx` does NOT need an update for lean-ctx version bumps. It finds the binary dynamically. Only update if the extension code in `packages/pi-lean-ctx/extensions/` changed:
```bash
cd packages/pi-lean-ctx
# bump version in package.json
npm publish
```

---

## Cross-Compile Notes

### Windows (x86_64-pc-windows-gnu)
```bash
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64
LEAN_CTX_ACTIVE=1 cargo build --release --target x86_64-pc-windows-gnu
gh release upload vX.Y.Z rust/target/x86_64-pc-windows-gnu/release/lean-ctx.exe --repo yvgude/lean-ctx
```

### Linux (requires cross-compile toolchain or CI)
Best done via GitHub Actions or a Linux VM/Docker.

## Rollback Plan

If critical issues found after publish:
1. `cargo yank --version X.Y.Z` (crates.io — prevents new installs)
2. Fix, bump patch version, re-publish
3. Update AUR/Homebrew/GitHub Release with patch

## Common Pitfalls

- **SHA256 for Homebrew: ALWAYS download to file first!** `curl ... | shasum` gives wrong results for GitHub archives because streaming decompression differs from file-based. Always use:
  ```bash
  curl -sL -o /tmp/file.tar.gz <url>
  shasum -a 256 /tmp/file.tar.gz
  ```
- **Shell aliases cause recursion**: Always use `LEAN_CTX_ACTIVE=1` for ALL cargo/lean-ctx commands
- **Node.js version**: Website needs Node.js >= 22.12.0 (`/opt/homebrew/opt/node@22/bin`)
- **Binary corruption on copy**: Remove old binary before copying (`rm -f` then `cp`)
- **Browser cache**: After website deploy, test with `curl` not browser
- **Website deploy branch**: `git checkout origin/deploy` overwrites local edits — apply changes AFTER restoring deploy files
- **pi-lean-ctx**: Does NOT need update for lean-ctx version bumps (binary is found dynamically)
