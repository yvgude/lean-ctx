# Tech Context

## Runtime / Language
- **Rust 2021**, Single-binary CLI + library (`lean_ctx`).
- **MCP** via `rmcp`.

## Wichtige Dependencies
- `rmcp` (server + stdio transport + streamable http server transport)
- `tokio` (async runtime)
- `axum` (HTTP server host für Streamable HTTP Service)
- `rusqlite` mit `bundled` (cross-platform SQLite)
- `tree-sitter-*` (optional, aktiviert via default features)

## Website
- Astro SSG (separater Deploy-Worktree/Branch)
- Tool-Counts/Read-Modes werden aus `website/generated/mcp-tools.json` gerendert.

## Build/Tests
- `cargo test` muss grün sein (Unit + Integration).
- Manifest drift wird durch `mcp_manifest_is_up_to_date` verhindert.

# Tech Context

## Stack
- **Language**: Rust (edition 2021)
- **Protocol**: MCP via `rmcp`
- **Storage**
  - JSONL/JSON stores in `~/.lean-ctx/*`
  - SQLite via `rusqlite` (bundled) für Property Graph / Impact / Architecture
- **Parsing/Indexing**: Tree-sitter optional, BM25, Embeddings optional (`rten`)

## Dev Commands
- `cargo test`
- `cargo fmt`
- `cargo run --bin gen_mcp_manifest` (regeneriert `website/generated/mcp-tools.json`)

## Constraints
- local-first / zero telemetry
- Cross-platform (macOS/Linux/Windows)
- deterministische Output-Formate (stable ordering)

# Tech Context — lean-ctx

## Technology Stack

### Rust Binary (core)
- **Language**: Rust 2021 Edition
- **MCP Framework**: `rmcp` (server, transport-io)
- **Token Counting**: `tiktoken-rs` (o200k_base encoding)
- **Async Runtime**: `tokio` (full)
- **Serialization**: `serde` + `serde_json` + `toml`
- **AST Parsing**: `tree-sitter` 0.26 + 14 language grammars (optional feature)
- **Regex**: `regex` crate
- **File System**: `walkdir`, `dirs`
- **Hashing**: `md-5`
- **Logging**: `tracing` + `tracing-subscriber` (env-filter)
- **Time**: `chrono` (serde)
- **Error Handling**: `anyhow`

### Release Profile
```toml
[profile.release]
opt-level = "z"    # size optimization
lto = true         # link-time optimization
codegen-units = 1  # single codegen unit
strip = true       # strip debug symbols
panic = "abort"    # smaller binary
```

### Website (Astro)
- **Framework**: Astro 6.0.8
- **Styling**: Tailwind CSS 4.2.2
- **Fonts**: Inter (300-800), JetBrains Mono (400-600) via Google Fonts
- **Node.js**: >= 22.12.0 required
- **Build**: Static HTML output

### Infrastructure
- **Server**: Hetzner VPS at `185.142.213.170`
- **SSH User**: `administrator`
- **SSH Key**: `~/.ssh/pounce_server`
- **Container Runtime**: Docker
- **Reverse Proxy**: Traefik (on `coolify` network)
- **TLS**: Let's Encrypt via Traefik certresolver
- **Domains**: `leanctx.com`, `www.leanctx.com`, `lean-ctx.pounce.ch`, `leanctx.tech`

### CI/CD
- **GitHub Actions**: `.github/workflows/release.yml` — triggered by `v*` tags
  - Matrix build: 5 targets (Linux x64/ARM, macOS x64/ARM, Windows x64)
  - Produces `.tar.gz` (Unix) and `.zip` (Windows) archives
  - Creates GitHub Release with SHA256SUMS
- **GitLab CI**: `.gitlab-ci.yml`
  - `check-rust` — cargo check on main branch or tags
  - `deploy-website` — rsync + Docker rebuild on main branch
  - CI Variables: `DEPLOY_SSH_KEY`, `DEPLOY_HOST`, `DEPLOY_USER`, `DEPLOY_SUDO_PASSWORD`

### Package Registries

| Registry | URL | Auth |
|----------|-----|------|
| **crates.io** | https://crates.io/crates/lean-ctx | `cargo login` token in `~/.cargo/credentials.toml` |
| **Homebrew** | https://github.com/yvgude/homebrew-lean-ctx | GitHub SSH key |
| **AUR lean-ctx** | https://aur.archlinux.org/packages/lean-ctx | SSH key registered on AUR |
| **AUR lean-ctx-bin** | https://aur.archlinux.org/packages/lean-ctx-bin | SSH key registered on AUR |
| **GitHub Releases** | https://github.com/yvgude/lean-ctx/releases | Automatic via CI |

## Development Setup

```bash
# Clone
git clone git@github.com:yvgude/lean-ctx.git
cd lean-ctx

# Build
cd rust && cargo build --release

# Run
./target/release/lean-ctx --version

# Website
cd ../website && npm install && npm run build

# Local dev server
npm run dev
```

## Git Remotes

| Name | URL | Usage |
|------|-----|-------|
| `origin` | `https://gitlab.pounce.ch/root/lean-ctx.git` | GitLab (primary for CI) |
| `github` | `git@github.com:yvgude/lean-ctx.git` | GitHub (public, releases) |

### Push Authentication
- **GitLab**: HTTPS with credentials
- **GitHub**: SSH key (`~/.ssh/id_ed25519`) — required for workflow file pushes
  - OAuth App tokens lack `workflow` scope
  - Use: `GIT_SSH_COMMAND="ssh -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes" git push github main`

## Key File Locations

| Path | Purpose |
|------|---------|
| `~/.lean-ctx/stats.json` | Persistent token statistics |
| `~/.lean-ctx/config.toml` | User configuration |
| `~/.lean-ctx/tee/` | Error output logs (redacted) |
| `~/.cursor/mcp.json` | Cursor MCP configuration |
| `~/.cargo/credentials.toml` | crates.io auth token |
