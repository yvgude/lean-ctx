# Implementation Status

> **Status: orientation index, not a product-status or release record.** The
> authoritative product narrative, availability labels, and execution priority
> are `docs/internal/README.md` (internal, not in this repository) and its
> Product Architecture (`docs/internal/vision/PRODUCT-ARCHITECTURE.md`, internal — not in this repository). Historical
> implementation snapshots live in [`docs/archive/`](archive/).

## Current product boundary

- **Available:** the local Runtime, CLI, MCP, proxy, Attach paths, context
  selection/compression/reuse/recovery, and local Receipt/offline-verification
  primitives.
- **Preview:** Python SDK v1 and its declared OpenAI Agents reference wrapper,
  plus the converging session/Receipt contract.
- **Research:** Performance Profiles, first-class Context Kits, Performance
  Benchmark, AutoTune, organization control planes, marketplace, managed
  execution, public rankings, and agent-building.

Implementation substrate is not a public product claim. For the current source
map, see [`ARCHITECTURE.md`](../ARCHITECTURE.md); for generated Runtime surface,
see the [MCP tool inventory](reference/generated/mcp-tools.md) and
[configuration inventory](reference/generated/config-keys.md).

## Development workflow

Build and test without stopping the installed Runtime:

```bash
cd rust && cargo build --release
cargo test --lib
cargo clippy --all-features -- -D warnings
cargo fmt --check
lean-ctx dev-install
```
