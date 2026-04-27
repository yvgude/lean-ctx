# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in lean-ctx, please report it privately:

- **Email**: yves@pounce.ch
- **GitHub**: [Create a private security advisory](https://github.com/yvgude/lean-ctx/security/advisories/new)
- **Response time**: We aim to acknowledge reports within 48 hours
- **Disclosure**: We follow responsible disclosure practices (90-day embargo)

**Please do NOT:**
- Open public GitHub issues for security vulnerabilities
- Disclose vulnerabilities on social media or forums before we've had a chance to address them

---

## What lean-ctx Does (and Doesn't Do)

lean-ctx is a **local-only CLI tool and MCP server**. Understanding its scope helps assess risk:

**Does:**
- Read files from your local filesystem (only files you explicitly request)
- Execute shell commands (only commands you or your AI tool explicitly invoke)
- Cache file contents in memory during a session
- Store statistics in `~/.lean-ctx/stats.json` (command counts, token savings)
- Store session state in `~/.lean-ctx/sessions/` (task context, findings)

**Optional network activity (fully disableable):**
- **Update check**: a lightweight daily GET to `leanctx.com/version.txt` to notify you of new versions. Sends only the current version as User-Agent. Disable with `update_check_disabled = true` in `~/.lean-ctx/config.toml` or `LEAN_CTX_NO_UPDATE_CHECK=1`.
- **Anonymous stats sharing** (opt-in, off by default): if you enable `contribute_enabled` in setup, anonymized compression statistics (token counts, compression ratios — no file names, no code, no PII) are periodically sent to `api.leanctx.com`.

**Does NOT:**
- Collect tracking analytics, fingerprints, or PII
- Access files outside of requested paths
- Store or transmit credentials, API keys, or secrets
- Require elevated privileges (runs as your user)

---

## Automated Security Checks

Every push and pull request triggers our CI security pipeline:

1. **`cargo audit`** — Scans dependencies for known CVEs
2. **`cargo clippy`** — Enforces Rust safety lints (warnings = errors)
3. **Dangerous pattern scan** — Detects potentially unsafe code patterns:
   - Shell injection vectors (`Command::new("sh")` with user input)
   - Network operations (`reqwest::`, `std::net::`, `hyper::`)
   - Unsafe code blocks (`unsafe {`)
   - Environment manipulation (`.env("LD_PRELOAD")`)
   - Hardcoded secrets or obfuscated strings
4. **`cargo test`** — Full test suite must pass

---

## Critical Files Requiring Enhanced Review

Changes to these files receive extra scrutiny:

| File | Risk | Why |
|------|------|-----|
| `src/shell.rs` | Shell execution | Wraps user's shell, executes commands |
| `src/server.rs` | MCP protocol | Handles all tool calls from AI editors |
| `src/hooks.rs` | Editor integration | Installs hooks into Claude Code, Cursor, etc. |
| `src/core/cache.rs` | File caching | Reads and stores file contents |
| `Cargo.toml` | Supply chain | Dependency manifest |
| `.github/workflows/*.yml` | CI/CD | Release pipeline integrity |

---

## Dependency Security

All dependencies in `Cargo.toml` meet these criteria:

- **Established crates**: All 29 dependencies are well-known, widely-used Rust crates
- **License**: MIT or Apache-2.0 compatible
- **Active maintenance**: Recent commits within 6 months
- **Minimal network**: `ureq` (lightweight HTTP client) used only for version check and opt-in cloud sync

Key dependencies and their purpose:

| Crate | Purpose | Downloads |
|-------|---------|-----------|
| `rmcp` | MCP protocol (stdio transport only) | Rust MCP reference impl |
| `tiktoken-rs` | Token counting (o200k_base) | OpenAI's tokenizer |
| `tree-sitter` + grammars | AST parsing for 18 languages | Mozilla's parser |
| `tokio` | Async runtime (for MCP server) | 200M+ downloads |
| `serde` / `serde_json` | JSON serialization | 400M+ downloads |
| `similar` | Myers diff algorithm | Well-established |
| `walkdir` | Directory traversal | 100M+ downloads |

---

## VirusTotal False Positives

Rust binaries are frequently flagged by ML-based antivirus engines (particularly Microsoft Defender's `Wacatac.B!ml` classifier). This is a **known issue** affecting many Rust projects:

- [Rust lang discussion on false positives](https://users.rust-lang.org/t/rust-programs-flagged-as-malware/49799)
- 1/72 engines flagging = definitively a false positive
- The `!ml` suffix in `Wacatac.B!ml` means "Machine Learning detection" (heuristic, not signature-based)

**Why it happens:**
- Statically linked binaries (~30 MB) are unusual for Windows
- `strip = true` + `lto = true` optimizations alter binary structure
- New/unsigned executables trigger ML classifiers trained on known-good signed software

**How to verify lean-ctx yourself:**
1. Build from source: `cargo install lean-ctx` (compiles on your machine)
2. Compare SHA256 checksums against our [GitHub Releases](https://github.com/yvgude/lean-ctx/releases)
3. Audit the source code: the entire codebase is open source

---

## Build Reproducibility

To verify that a release binary matches the source code:

```bash
# Clone and build
git clone https://github.com/yvgude/lean-ctx.git
cd lean-ctx/rust
cargo build --release

# Compare with installed version
lean-ctx --version
./target/release/lean-ctx --version
```

SHA256 checksums for all release binaries are published in each [GitHub Release](https://github.com/yvgude/lean-ctx/releases).

---

## Disclosure Timeline

When vulnerabilities are reported:

1. **Day 0**: Acknowledgment sent to reporter
2. **Day 7**: Severity assessment completed
3. **Day 14**: Patch development begins
4. **Day 30**: Patch released + CVE filed (if applicable)
5. **Day 90**: Public disclosure

Critical vulnerabilities (RCE, data exfiltration) are fast-tracked.

---

## Contact

- **Security issues**: yves@pounce.ch
- **General questions**: [GitHub Discussions](https://github.com/yvgude/lean-ctx/discussions)
- **Discord**: [Join our server](https://discord.gg/leanctx)

---

**Last updated**: 2026-04-27
