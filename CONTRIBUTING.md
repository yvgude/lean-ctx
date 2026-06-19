# Contributing to lean-ctx

Thanks for your interest in lean-ctx — contributions are welcome.

## Quick start (core Rust binary)

### Prerequisites

- Rust (stable) via [rustup](https://rustup.rs/)
- Git

### Setup

```bash
git clone https://github.com/yvgude/lean-ctx.git
cd lean-ctx/rust

cargo build
cargo test
```

### Quality bar (required)

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo test --release
```

## Building & disk usage

### Native-dependency prerequisites

A cold `cargo build` compiles C/C++ sources for several bundled dependencies
(tree-sitter language grammars, bundled SQLite via rusqlite, jemalloc on Linux/macOS).
You need a working C toolchain before your first build:

- **macOS**: `xcode-select --install` (provides `clang` and `make`)
- **Linux**: `sudo apt install build-essential` or the equivalent for your distro

No other system packages are required for the default feature set.

### Worktree disk multiplier

Each git worktree gets its own `target/` directory. With N worktrees you accumulate
N independent build caches. A full debug build with all features can reach 10-20 GB
per worktree; incremental rebuilds compound this over time.

Check how much `target/` space you are using:

```bash
du -sh rust/target/
```

To check across all worktrees at once:

```bash
git worktree list | awk '{print $1"/rust/target/"}' | xargs du -sh 2>/dev/null
```

### sccache - shared build cache across worktrees

[sccache](https://github.com/mozilla/sccache) caches compiled artifacts globally so
that switching between worktrees does not re-compile identical crates:

```bash
# Install
cargo install sccache

# Enable for the current shell (or add to ~/.bashrc / ~/.zshrc)
export RUSTC_WRAPPER=sccache
```

> Note: sharing a single `CARGO_TARGET_DIR` across worktrees is an alternative but
> serializes all builds (Cargo holds a lock on the target directory). sccache avoids
> that bottleneck while still deduplicating compilation work.

### Reclaiming disk space

Remove artifacts older than 7 days with [cargo-sweep](https://github.com/holmgr/cargo-sweep):

```bash
cargo install cargo-sweep
cargo sweep --time 7        # run inside rust/
```

Or wipe the entire cache for a worktree:

```bash
cargo clean                 # run inside rust/
```

### Reducing baseline debug-build size (optional)

Adding the following to `rust/Cargo.toml` cuts debug artifact size by 30-50% with
minimal impact on debug quality:

```toml
[profile.dev]
debug = "line-tables-only"
```

This keeps line-number information for backtraces while skipping the heavier per-variable
DWARF data that is rarely needed during day-to-day development.

## Cookbook / SDK / extensions (optional)

If you contribute to `cookbook/` or `packages/`, you’ll also need:

- Node.js (>= 22.12.0)
- npm

```bash
cd cookbook
npm ci
npm test
```

## Repo structure

```text
lean-ctx/
├─ rust/                 # core binary (CLI + MCP server + shell hook)
│  ├─ src/
│  │  ├─ main.rs         # CLI entry point
│  │  ├─ lib.rs          # library entry point (shared core)
│  │  ├─ mcp_stdio.rs    # MCP stdio transport
│  │  ├─ server/         # MCP server state + dispatch
│  │  ├─ tools/          # MCP tool handlers (ctx_read, ctx_shell, ...)
│  │  ├─ core/           # cache, compression, patterns/, memory, graphs, ...
│  │  ├─ cli/            # CLI subcommands (setup, init, read, ...)
│  │  └─ hooks/          # editor/agent installers (Cursor, Claude Code, ...)
│  └─ tests/             # integration/e2e/adversarial tests
├─ cookbook/             # real examples + @leanctx/sdk
├─ packages/             # editor integrations (VSCode, Chrome, JetBrains, ...)
├─ docs/                 # repo docs (developer-facing)
└─ website/generated/    # generated schemas (tool + TDD schema)
```

## Common contribution types

### Add a shell compression pattern

1. Add a new module in `rust/src/core/patterns/<tool>.rs`
2. Implement:

```rust
pub fn compress(command: &str, output: &str) -> Option<String>
```

3. Register the module + routing in `rust/src/core/patterns/mod.rs` (`try_specific_pattern`)
4. Add tests (unit tests in the module or integration tests in `rust/tests/`)
5. Run the quality checks above

Tip: open a ticket via the [New Compression Pattern](.github/ISSUE_TEMPLATE/compression_pattern.md) template and include raw output + expected compressed output.

### Add or update an MCP tool

- Tool handlers live in `rust/src/tools/ctx_*.rs`
- Tool schemas/registration live in `rust/src/tool_defs/` (keep names/counts in sync)
- If you change the public tool surface, update `LEANCTX_FEATURE_CATALOG.md` (SSOT snapshot) and any affected docs

### Docs & examples

- Prefer real, runnable examples (no mock data)
- If you add a new example app, add it under `cookbook/examples/` and ensure it talks to a real `lean-ctx serve` instance

## Issues

- If your issue was closed but the problem persists, comment `/reopen` on it — as the original author, this reopens the issue automatically (GitHub itself does not let authors reopen maintainer-closed issues). The command is matched anywhere in your comment, so "Please `/reopen`" works too; issues closed as *not planned* stay a maintainer call
- Issues closed as *not planned* are maintainer decisions and are not reopened automatically; a comment is still welcome

## Pull requests

- Keep PRs focused (one theme per PR)
- Include a short test plan (commands you ran)
- If relevant, include a small “before/after” token-savings snippet

## Contributor License Agreement (CLA)

Before your first pull request can be merged, you need to sign our
[Contributor License Agreement](CLA.md). It is a one-time, automated step: the
CLA Assistant bot comments on your PR, and you sign by replying:

> I have read the CLA Document and I hereby sign the CLA

The CLA keeps lean-ctx Apache-2.0 for everyone while allowing the maintainer to
relicense (e.g. for the hosted/commercial offering). The free, open-source
runtime for individual developers stays free — that commitment is written into
the CLA itself (§8).

## License

lean-ctx is distributed under the Apache License 2.0; by contributing, your
contributions are licensed to the public under the same terms (see the [CLA](CLA.md)
for the full grant).
