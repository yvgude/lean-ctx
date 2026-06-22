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
- Read files from your local filesystem (explicit reads and tool-driven scans within the project boundary)
- Execute shell commands (only commands you or your AI tool explicitly invoke)
- Cache file contents in memory during a session
- Store statistics in `~/.lean-ctx/stats.json` (command counts, token savings)
- Store session state in `~/.lean-ctx/sessions/` (task context, findings)

### I/O Boundary (PathJail + Roles)

lean-ctx enforces a **project boundary** for filesystem I/O:

- **PathJail**: all tool path inputs are resolved and jailed under the current `project_root`.
  - If a path would escape, the call fails with a clear hint to explicitly allow additional roots.
- **Explicit allow roots**:
  - Env: `LEAN_CTX_ALLOW_PATH` (or `LCTX_ALLOW_PATH`) — a path list (`:` on Unix, `;` on Windows)
  - Config: `allow_paths` in `~/.lean-ctx/config.toml` (whitelist only); `extra_roots` (whitelist + multi-root scanning)
  - `~`, `$VAR` and `${VAR}` are expanded in these entries (no shell runs for config files)
- **Disabling the jail** (sandboxed environments where the OS is the boundary):
  - Config: `path_jail = false` in `~/.lean-ctx/config.toml`
  - Compile-time: the `no-jail` cargo feature
  - The legacy `LEAN_CTX_NO_JAIL=1` env var was removed in v3.7.4 and has no effect
  - Full reference: `docs/reference/appendix-paths-and-config.md` §5; `lean-ctx doctor` reports the effective jail state
- **Symlink escape protection**: canonicalization ensures that symlinks pointing outside the jail are rejected.

In addition, roles can restrict **unsafe I/O**:

- **Secret-like deny-by-default**:
  - Search skips secret-like files (e.g. `.env`, `*.pem`, `id_rsa`, `.ssh/`, `.aws/`) unless the active role explicitly allows them.
  - Artifact registry resolution rejects secret-like artifact paths unless allowed (artifacts are indexed/shareable by design).
  - Direct reads/edits can warn or error depending on boundary mode.
- **`.gitignore` bypass is policy-gated**:
  - `ctx_search ignore_gitignore=true` requires explicit role permission (typically the `admin` role).
- **Boundary mode**:
  - Roles can set `io.boundary_mode = "warn" | "enforce"`.
  - Env override: `LEAN_CTX_IO_BOUNDARY_MODE=warn|enforce`.
- **Auditability**:
  - Boundary denials/warnings emit local `PolicyViolation` events (no secret content is returned as part of the violation).

### Threat Model (v2)

**Primary risks (local-only, but high impact):**
- **Accidental secret exfiltration to LLMs** via `ctx_read`, `ctx_search`, compressed `ctx_shell`, archives, or exported artifacts.
- **Boundary escapes** via absolute paths, symlinks, linked projects, or artifact path tricks.
- **Amplification / token burn** by scanning large files or returning unbounded outputs.
- **ReDoS** via user-supplied regex patterns in `ctx_search`.
- **Cross-workspace data access** in team server deployments (IDOR).

**Core mitigations:**
- **PathJail** + explicit allow roots (`LEAN_CTX_ALLOW_PATH` / `allow_paths`).
- **Role-gated unsafe I/O** (`ignore_gitignore`, secret-like allow).
- **Secret path check on all MCP read paths** — `.env`, SSH keys, etc. blocked by default.
- **Shell CWD jail enforcement** — explicit `cwd` parameters are jail-checked, `cd` targets validated.
- **Deterministic redaction** on tool outputs (non-admin roles, and for persisted archives).
- **Hard caps** on reads and outputs to limit DoS/token burn.
- **Regex guards** — pattern length (1024 chars) and DFA size (1 MiB) limits on `ctx_search`.
- **MCP message size limit** — 32 MiB cap on JSON-RPC message size.
- **Constant-time token comparison** in all auth paths (dashboard, HTTP server, team server).
- **Team server tenant isolation** — workspace enforced from authenticated token, not query parameters.
- **JSON-RPC batch rejection** — batch requests rejected on team server to prevent scope bypass.
- **Event payload redaction** — REST API responses redacted to `Summary` level by default.
- **Pipeline archive redaction** — shell output archives redacted before storage.
- **UDS socket permissions** — `0o600` enforced on Unix domain sockets after bind.
- **Error response sanitization** — internal details logged server-side, generic codes returned to clients.

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
| `rust/src/shell/` | Shell execution | Wraps your shell, executes commands |
| `rust/src/server/` | MCP protocol | Handles all tool calls from AI editors/agents |
| `rust/src/hooks/` | Editor integration | Installs hooks/config into Claude Code, CodeBuddy, Cursor, etc. |
| `rust/src/core/cache.rs` | File caching | Reads and stores file contents |
| `rust/Cargo.toml` | Supply chain | Dependency manifest |
| `.github/workflows/*.yml` | CI/CD | Release pipeline integrity |

---

## Dependency Security

All dependencies in `Cargo.toml` meet these criteria:

- **Established crates**: All 29 dependencies are well-known, widely-used Rust crates
- **License**: Apache-2.0 compatible
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

## Known Residual Risks

### TOCTOU (Time-of-Check to Time-of-Use)

**Status:** Mitigated on Unix; residual risk on Windows.

A race window exists between `jail_path` validation and the subsequent file operation. Mitigations in place: standard reads open with `O_NOFOLLOW` (Unix) and reject symlinks; `ctx_edit` additionally rejects symlinks on both its read and write paths (lstat pre-check on all platforms, `O_NOFOLLOW` on Unix) and re-verifies the file fingerprint (size/mtime/md5) immediately before writing. On Windows there is no `O_NOFOLLOW` equivalent, so the lstat pre-check is the only guard — it rejects symlinks **and all NTFS reparse points (junctions, mount points)** via `pathutil::is_symlink_or_reparse`, and `read_file_nofollow` applies the same lstat check before opening. The residual risk is the unavoidable check→open race window.

**Recommendation for regulated environments:** Run lean-ctx inside a container or VM where the filesystem is controlled and no untrusted processes can modify symlinks concurrently.

### ctx_execute Sandbox Naming

**Status:** Documented limitation.

The `ctx_execute` tool provides **timeout enforcement** and **output capping** but does **not** provide OS-level sandboxing (no containers, namespaces, or seccomp filters). The term "sandbox" in tool descriptions refers to the execution boundary, not kernel-level isolation.

**Recommendation for regulated environments:** Disable `ctx_execute` via role configuration (`denied: ["ctx_execute"]`) or run lean-ctx in a pre-existing container sandbox.

### Shell Command Validation (REQ-57177, GH #391)

**Status:** Defense in depth — the agent's permission model remains the primary boundary. **`ctx_shell` is not a sandbox.**

`ctx_shell` and `lean-ctx -c` enforce, in both allowlist and blocklist-only mode:

- a deny-by-default executable allowlist when configured (AST-segmented: every segment of a pipeline/compound command must be allowed),
- `eval`/`exec`/`source` unconditionally blocked; `$()`/backticks blocked at command position,
- **interpreter inline-code blocking**: `bash -c`, `sh -c`, `python -c`, `node -e`, … are rejected (including via delegation wrappers like `env`, `timeout`, `xargs`) — quoting a payload inside `bash -c '…'` does not bypass the file-write or allowlist checks because the interpreter call itself is refused,
- file-write detection (`>`, `>>`, `tee`, heredoc-to-file, `dd of=`, `curl -o/-O`, `wget` to file) — writes belong to the editor's native Write/Edit tools where the agent's permission UI governs them,
- dangerous-flag blocking (`git --upload-pack`, `tar --to-command`, `find -exec`, `awk system()`), inline env hijack blocking (`PATH=`, `LD_PRELOAD=`, `GIT_SSH_COMMAND=`, …), and dangerous env-key filtering on the MCP `env` parameter.

Enforcement applies to the MCP path, hook-child mode and every non-interactive CLI invocation; an interactive human terminal gets a warning instead (`LEAN_CTX_ALLOWLIST_WARN_ONLY=1` opts out explicitly). Cloud/infra mutation CLIs (terraform, kubectl, aws, …) are excluded from the default allowlist and require per-tool opt-in (`lean-ctx allow <cmd>`). `shell_strict_mode = true` upgrades the warn-only heuristics (command substitution in arguments, pipe-to-bare-interpreter) to hard blocks.

**Explicitly out of scope:** commands run with the invoking user's full privileges. Anything the user can do, an allowed command can do — `cp`/`rsync` can copy any file the OS lets the user read, package managers execute arbitrary install scripts, `npm test` runs project code. A blocklist cannot enumerate these; pretending otherwise would be security theater.

**Rationale:** Shell filters can be bypassed by a sufficiently creative attacker, so the agent's permission model (Claude Code allowlists, Cursor approval dialogs) remains the primary boundary — lean-ctx's allowlist is a second, independent layer, not a replacement.

**Mitigation for untrusted agents:** Use role-based `denied: ["ctx_shell"]` to disable shell access entirely, enable the deny-by-default allowlist with a minimal command set, and/or run the whole agent stack inside an OS sandbox (container, gVisor/Firecracker, bwrap, seccomp/AppArmor) — that is the correct layer for kernel-grade isolation.

### PathJail TOCTOU Race (REQ-57178)

**Status:** Mitigated on Unix; residual risk on Windows.

A race window exists between `jail_path` validation and the subsequent file operation. Mitigations: symlink-following canonicalization before access, `O_NOFOLLOW` + symlink rejection on read paths (Unix), and symlink rejection on `ctx_edit` write paths (all platforms, lstat-based; on Windows this includes NTFS junctions and other reparse points). Home-level IDE config dirs (`~/.cursor`, `~/.claude`, …) are excluded from the jail's allow-list by default (`allow_ide_config_dirs` opts in).

**Windows file permissions:** the Unix `0o600`/`0o700` tightening (cloud credentials, crash log) has no direct Windows equivalent; protection relies on the default NTFS ACL of the user profile (`%USERPROFILE%` is not readable by other non-admin users). Machines with custom ACLs on the profile directory should verify `%USERPROFILE%\.lean-ctx` inherits owner-only access.

**Mitigation:** For regulated environments: run lean-ctx inside a container where no untrusted processes can modify symlinks concurrently.

### Cloud Server Database TLS (REQ-57188)

**Status:** Accepted risk — localhost-only by default.

The cloud server's PostgreSQL connection does not enforce TLS by default. This is acceptable because the cloud server is designed for localhost/loopback deployment where DB traffic does not traverse a network.

**Mitigation for production:** Set `DATABASE_URL` with `?sslmode=require` or use a connection string that enforces TLS. When deployed behind a reverse proxy (nginx/Caddy), ensure TLS terminates before the DB.

### HuggingFace Model Downloads

**Status:** Documented risk.

Embedding models for semantic search are downloaded from HuggingFace Hub. Verification is size-based heuristic only, not cryptographic (no SHA256 pinning for model files).

**Recommendation for regulated environments:** Pre-provision models manually from an internal mirror with signature verification. Set `LEAN_CTX_EMBEDDING_MODEL_DIR` to point to the pre-provisioned directory to skip downloads entirely.

### Project-Scope Config Influences Injected Context (external audit, finding 4)

**Status:** Mitigated by Workspace Trust (selective gating + content-hash pin).

A cloned repository's `.lean-ctx.toml` is merged over the global config by `Config::merge_local`. That merge can raise **security-sensitive** settings — replace the shell allowlist, widen the path jail (`allow_paths` / `extra_roots`), repoint the proxy upstream, define command aliases, change `rules_scope` / `rules_injection`. Opening an untrusted clone with an agent would let the repo silently weaken lean-ctx's own boundaries.

**Mitigation (shipped):** lean-ctx now applies a VS-Code-style **Workspace Trust** gate. For a workspace the user has not trusted, the security-sensitive overrides above are **withheld** (comfort knobs like `compression`/`theme` still apply) and a `[SECURITY]` warning is logged; `lean-ctx doctor` shows the state. Trust is granted explicitly with `lean-ctx trust` and pinned to BOTH the workspace path AND a content hash of `.lean-ctx.toml`, so editing the file after trust re-gates it (a "trust once, modify later" change cannot take effect silently). Headless/fleet use can opt in via `LEAN_CTX_TRUST_WORKSPACE=1` or `LEAN_CTX_TRUSTED_ROOTS`.

**Residual:** Model-visible *content* lean-ctx injects (the static `<!-- lean-ctx -->` rules block, hook `additionalContext`, `[VERIFY]`/`[HINT]` suffixes) is itself auditable and not repo-controlled. Review a clone's `.lean-ctx.toml` before `lean-ctx trust`.

### LLM Proxy is a Full MITM on API Traffic (external audit, finding 6)

**Status:** By design; loopback-bound and disabled by default.

When enabled, the optional LLM proxy (`lean-ctx proxy`) reads and rewrites every request body (compression, history pruning) and sees `Authorization` headers in cleartext — a concentrated sensitive-data surface. Any process able to reach the port, or a forwarding bug, would expose prompts, completions and API keys.

**Mitigation:** The proxy is disabled by default and binds to loopback (`127.0.0.1`) with an auto-generated auth token when enabled; keep it loopback-bound. The MCP `ctx_*` tools deliver compression savings without routing API traffic through any proxy — leave it disabled unless you need pay-as-you-go key forwarding.

---

## Security Architecture for Enterprise Deployments

### Recommended Configuration (Bank / Regulated)

```toml
# ~/.lean-ctx/config.toml

# Disable all outbound network
update_check_disabled = true
contribute_enabled = false

# Enforce strict I/O boundary
[io]
boundary_mode = "enforce"
allow_secret_paths = false

# Use restrictive role
[roles.bank]
denied = ["ctx_execute"]
io.boundary_mode = "enforce"
io.allow_secret_paths = false
io.allow_ignore_gitignore = false
```

### Network Surface

| Endpoint | Purpose | Disable |
|----------|---------|---------|
| `leanctx.com/version.txt` | Update check (daily GET) | `update_check_disabled = true` |
| `api.leanctx.com` | Opt-in anonymous stats | `contribute_enabled = false` (default) |
| `huggingface.co` | Embedding model download | Pre-provision models, set `LEAN_CTX_EMBEDDING_MODEL_DIR` |
| `localhost:PORT` | Dashboard (local TCP) | Don't start dashboard, or bind to loopback only |
| UDS socket | Daemon IPC | Permissions `0o600`, owner-only access |

### Team Server Hardening

When running the team server (`lean-ctx team-server`):

1. **Token rotation**: Rotate workspace tokens periodically. Tokens are stored in the team config.
2. **Scope minimization**: Grant only necessary scopes per workspace token (e.g., `read` only, no `shell`).
3. **Network isolation**: Bind the team server to an internal network interface, not `0.0.0.0`.
4. **Audit log monitoring**: Team server writes audit logs for all tool calls. Monitor for denied requests.
5. **JSON-RPC batch requests**: Rejected by default to prevent scope bypass.

### Supply Chain

- **`cargo audit`** runs on every CI push (zero known CVEs tolerated).
- **`cargo deny`** checks licenses and advisories.
- **npm `postinstall.js`** verifies SHA256 of downloaded binaries against `SHA256SUMS` release asset.
- **GitHub Actions** uses pinned action versions with hash verification.

---

## Contact

- **Security issues**: yves@pounce.ch
- **General questions**: [GitHub Discussions](https://github.com/yvgude/lean-ctx/discussions)
- **Discord**: [Join our server](https://discord.gg/pTHkG9Hew9)

---

**Last updated**: 2026-06-21
