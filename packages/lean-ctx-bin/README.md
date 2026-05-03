# lean-ctx-bin

Pre-built binary distribution of [lean-ctx](https://github.com/yvgude/lean-ctx) — the Cognitive Filter for AI Engineering.

No Rust toolchain required. The correct binary for your platform is downloaded automatically during `npm install`.

## Install

```bash
npm install -g lean-ctx-bin
```

After installing, run the one-command setup:

```bash
lean-ctx setup
```

This auto-detects your shell and editors, installs shell aliases, creates MCP configs, and verifies everything.

## Supported Platforms

| Platform | Architecture |
|----------|-------------|
| Linux | x86_64, aarch64 |
| macOS | x86_64, Apple Silicon |
| Windows | x86_64 |

## Alternative Install Methods

```bash
# Universal installer (no Rust needed)
curl -fsSL https://leanctx.com/install.sh | sh
# or: curl -fsSL https://leanctx.com/install.sh | bash

# Homebrew (macOS/Linux)
brew tap yvgude/lean-ctx && brew install lean-ctx

# Cargo (requires Rust)
cargo install lean-ctx
```

## Links

- [Documentation](https://leanctx.com/docs/getting-started)
- [GitHub](https://github.com/yvgude/lean-ctx)
- [crates.io](https://crates.io/crates/lean-ctx)
- [Discord](https://discord.gg/pTHkG9Hew9)
