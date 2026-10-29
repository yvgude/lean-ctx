# lean-ctx-bin

Pre-built binary distribution of **LeanCTX Engine**, the open-source runtime for [LeanCTX](https://github.com/yvgude/lean-ctx), the **Context Gateway for AI Systems**.

**Control what your AI can see.** Select relevant context, apply supported access and content controls, retain local task and knowledge records, and inspect evidence of observed context operations: **Select · Control · Learn · Prove**. The [LeanCTX SDK](https://github.com/Thinkery-AG/leanctx-sdk) provides the separate application integration surface.

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

## LeanCTX Cockpit

The postinstall step also adds the free LeanCTX Cockpit (`lean-ctx cockpit`, a
local web UI) from the same GitHub release as the engine. Its bundle must match
the release's `COCKPIT_SHA256SUMS` and is then verified offline by
`lean-ctx cockpit install --from`; any problem only prints a warning and the
engine install still succeeds. Skip it with `LEAN_CTX_NO_COCKPIT=1`.

## License

This npm package and the LeanCTX Engine (`lean-ctx`) are licensed under
Apache-2.0. The LeanCTX Cockpit binary is not open source: it is free to use,
including commercially, under the LeanCTX Cockpit EULA
(`LicenseRef-LeanCTX-Cockpit-EULA`, shipped inside the Cockpit package as
`LICENSE.md`).

## Links

- [Documentation](https://leanctx.com/docs/getting-started)
- [GitHub](https://github.com/yvgude/lean-ctx)
- [crates.io](https://crates.io/crates/lean-ctx)
- [Discord](https://discord.gg/6qUJ3QmDyP)
