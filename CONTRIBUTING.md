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

## Pull requests

- Keep PRs focused (one theme per PR)
- Include a short test plan (commands you ran)
- If relevant, include a small “before/after” token-savings snippet

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.
