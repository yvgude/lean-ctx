# lean-ctx Deploy Checklist

Use this checklist for every release. Copy the section below and check off each item.

---

## Pre-Release

- [ ] All tests pass: `cd rust && LEAN_CTX_DISABLED=1 cargo test`
- [ ] Clippy clean: `LEAN_CTX_DISABLED=1 cargo clippy -- -D warnings`
- [ ] Format clean: `LEAN_CTX_DISABLED=1 cargo fmt --check`
- [ ] Version bumped in ALL locations:
  - [ ] `rust/Cargo.toml` → `version = "X.Y.Z"`
  - [ ] `packages/lean-ctx-bin/package.json` → `"version": "X.Y.Z"`
  - [ ] `website/public/version.txt` → `X.Y.Z` (triggers update notification)
- [ ] `CHANGELOG.md` has detailed entry matching release notes format
- [ ] Release build succeeds: `LEAN_CTX_DISABLED=1 cargo build --release`

## Documentation Check

> **For every issue fixed or feature added, check if website docs need updating.**
> **This is MANDATORY for every release — never skip!**

- [ ] Review closed issues since last release: `gh issue list --state closed --since <last-release-date>`
- [ ] Review merged PRs since last release: `gh pr list --state merged --search "merged:>YYYY-MM-DD"`
- [ ] For each issue/feature/PR, check ALL applicable doc pages:
  - [ ] New config options → update `DocsConfigurationPage.astro` config options table
  - [ ] New env variables → update `DocsConfigurationPage.astro` environment variables table
  - [ ] New MCP tools → update `DocsToolsCorePage.astro` (core tools), `DocsToolsSessionPage.astro` (session), `DocsToolsIntelligencePage.astro` (intelligence), or `DocsToolsMemoryPage.astro` (memory)
  - [ ] New CLI commands → update `DocsCliReferencePage.astro`
  - [ ] New hook behavior → update Configuration page "Hook Redirect" section
  - [ ] Changed tool parameters/actions → update the relevant tool's parameter table
  - [ ] New safety/behavior features → update Configuration page (e.g. Loop Detection section)
- [ ] If docs were updated, translate relevant keys in ALL 11 locale JSON files:
  `en.json`, `de.json`, `es.json`, `fr.json`, `ja.json`, `pt.json`, `ru.json`, `zh.json`, `ar.json`, `bn.json`, `hi.json`
- [ ] Build website locally to verify: `cd website && npm run build`
- [ ] Check that page-template files exist on `deploy` branch (they don't exist on `main`!)

## Git + GitHub Release (Automated)

- [ ] Commit all changes
- [ ] Tag: `git tag vX.Y.Z`
- [ ] Push to GitHub: `git push github main --tags`
  - GitHub Actions automatically: builds 6 platform binaries, creates Release with CHANGELOG notes, publishes to crates.io + npm
- [ ] Push to GitLab: `git push origin main`

> **⚠️ CRITICAL: NEVER push the `deploy` branch or `website/` to GitHub!**
> Only push `main` and tags to the `github` remote.

## AUR (Arch Linux)

> AUR directories are SEPARATE git repos (`ssh://aur@aur.archlinux.org/...`).

### lean-ctx (source build)
- [ ] Update `aur/lean-ctx/PKGBUILD`: bump `pkgver`, update `sha256sums`
  ```bash
  curl -sL -o /tmp/lean-ctx-src.tar.gz https://github.com/yvgude/lean-ctx/archive/refs/tags/vX.Y.Z.tar.gz
  shasum -a 256 /tmp/lean-ctx-src.tar.gz
  ```
- [ ] Update `aur/lean-ctx/.SRCINFO`: bump version, update source URL + SHA
- [ ] Push to AUR: `cd aur/lean-ctx && git add -A && git commit -m "X.Y.Z" && git push`

### lean-ctx-bin (pre-built binary)
- [ ] Update `aur/lean-ctx-bin/PKGBUILD`: bump `pkgver`
- [ ] Update `aur/lean-ctx-bin/.SRCINFO`: bump version, update source URLs
- [ ] Push to AUR: `cd aur/lean-ctx-bin && git add -A && git commit -m "X.Y.Z" && git push`

## Homebrew

- [ ] Update formula: `~/Documents/Privat/Projects/homebrew-lean-ctx/Formula/lean-ctx.rb`
  - [ ] Update `url` to new tag
  - [ ] Update `sha256` with GitHub tarball SHA (ALWAYS download to file first!)
  - [ ] Update `assert_match` version in test block
- [ ] Commit + push: `cd ~/Documents/Privat/Projects/homebrew-lean-ctx && git commit -am "X.Y.Z" && git push`

## Website (leanctx.com) — only if changes needed

> Website deploys via GitLab CI on the `deploy` branch.
> **⚠️ The deploy branch is GitLab-ONLY. NEVER push it to GitHub!**

- [ ] Cherry-pick code commits to `deploy` branch
- [ ] Update `website/public/version.txt` with new version
- [ ] Build website: `cd website && npm run build`
- [ ] Commit and push to GitLab: `git push origin deploy`
- [ ] Verify: `curl -sL https://leanctx.com/version.txt` → X.Y.Z

## Post-Release Verification

- [ ] `npm view lean-ctx-bin version` → X.Y.Z
- [ ] `curl -s https://crates.io/api/v1/crates/lean-ctx | python3 -c "import sys,json; print(json.load(sys.stdin)['crate']['newest_version'])"` → X.Y.Z
- [ ] GitHub Release page has 7 assets (6 binaries + SHA256SUMS)
- [ ] Release notes on GitHub match CHANGELOG entry
- [ ] Discord channel has release announcement with full notes (automatic via Captain Hook webhook)
- [ ] `gh issue list --repo yvgude/lean-ctx --state open` → 0 issues (or expected)
- [ ] Close related GitHub issues with fix comment

## Release Notifications (Automatic)

The GitHub webhook triggers Captain Hook on Discord when a release is **created**.
Release notes are extracted from CHANGELOG.md during the GitHub Actions pipeline,
so they must be written BEFORE tagging. No manual action needed.

## Dual-Remote Workflow

```
main branch  → GitHub (github remote) + GitLab (origin remote)
deploy branch → GitLab ONLY (origin remote) — contains website/, cloud/
```

For code-only releases (no website changes):
```bash
git push github main --tags   # triggers GitHub Actions release
git push origin main          # mirrors to GitLab
```

For releases with website changes:
```bash
# 1. Push code to GitHub
git push github main --tags

# 2. Cherry-pick to deploy branch, update website version
git checkout deploy
git cherry-pick <commit>
echo "X.Y.Z" > website/public/version.txt
git commit -am "deploy: vX.Y.Z"
git push origin deploy        # triggers GitLab CI website build

# 3. Back to main
git checkout main
```

## Common Pitfalls

- **SHA256 for Homebrew/AUR: ALWAYS download to file first!** `curl ... | shasum` gives wrong results for GitHub archives.
- **Shell aliases cause recursion**: Always use `LEAN_CTX_DISABLED=1` for ALL cargo/lean-ctx commands during release
- **Discord empty notes**: Release notes must be in CHANGELOG.md BEFORE tagging — the pipeline extracts them at release creation time
- **npm/crates.io "already exists"**: Normal if re-tagging. `continue-on-error: true` prevents pipeline failure.
- **macOS codesign**: After copying binary locally, run `codesign --force -s - <path>`
- **pi-lean-ctx**: Does NOT need update for lean-ctx version bumps (binary is found dynamically)

## Rollback Plan

If critical issues found after publish:
1. `cargo yank --version X.Y.Z` (crates.io — prevents new installs)
2. Fix, bump patch version, re-publish
3. Update AUR/Homebrew/GitHub Release with patch
