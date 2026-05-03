# **FAQ — Downgrading to an Older Version**

---

**Q: How do I downgrade lean-ctx to a previous version?**
Depends on how you installed it:

**Option 1 — Direct binary download (recommended)**
Download a specific version from GitHub Releases and replace the binary:
```bash
# Example: downgrade to v3.2.5 on macOS (Apple Silicon)
curl -fsSL https://github.com/yvgude/lean-ctx/releases/download/v3.2.5/lean-ctx-aarch64-apple-darwin.tar.gz \
  | tar -xz -C /usr/local/bin lean-ctx
chmod +x /usr/local/bin lean-ctx

# Verify
lean-ctx --version
```

**Option 2 — Cargo (Rust)**
```bash
cargo install lean-ctx --version 3.2.5
```

**Option 3 — npm**
```bash
npm install -g lean-ctx-bin@3.2.5
```

**Option 4 — Homebrew**
Homebrew doesn't support easy version pinning. Use the direct binary download instead.

After downgrading, run `lean-ctx setup` to re-sync hooks and aliases.

**Q: Which binary do I need?**
| Platform | Binary |
|----------|--------|
| macOS Apple Silicon (M1/M2/M3/M4) | `lean-ctx-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `lean-ctx-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 (glibc) | `lean-ctx-x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86_64 (musl/Alpine) | `lean-ctx-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64 (glibc) | `lean-ctx-aarch64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 (musl/Alpine) | `lean-ctx-aarch64-unknown-linux-musl.tar.gz` |
| Windows x86_64 | `lean-ctx-x86_64-pc-windows-msvc.zip` |

**Q: Where do I find all releases?**
All releases with changelogs and binaries: <https://github.com/yvgude/lean-ctx/releases>

**Q: How do I prevent auto-updates?**
If you want to stay on a specific version, set this in your config:
```toml
# ~/.lean-ctx/config.toml
auto_update = false
```

**Q: When should I downgrade?**
- A new version introduces a regression that breaks your workflow
- Your system doesn't support a dependency in the latest version (e.g. GLIBC version mismatch on older Linux)
- You need to stay on a stable version for CI/Docker images

Always check the changelog first — many issues are fixed in patch releases within hours: <https://github.com/yvgude/lean-ctx/blob/main/CHANGELOG.md>
