# Contract: WASM Extension ABI v1 (`wasm-abi-v1`)

Status: stable · Feature: `wasm` (off by default) · Source: `rust/src/core/wasm_ext.rs`

A sandboxed, language-independent way to contribute extensions —
**compressors** (EPIC 12.8) and **context providers** (EPIC 12.10) — without
recompiling lean-ctx. A guest is a plain `.wasm` module compiled from any
language (Rust, AssemblyScript, TinyGo, Zig, …) that satisfies a tiny, uniform
ABI.

This contract upholds the **Local-Free Invariant**: the runtime is a local,
compile-optional capability (`features.wasm_runtime` in `/v1/capabilities`),
free and ungated. It never depends on an account, license, or plan.

## Required exports

Every guest module MUST export:

| Export | Signature | Purpose |
|--------|-----------|---------|
| `memory` | linear memory | host reads/writes input & output |
| `alloc` | `(n: i32) -> i32` | reserve `n` bytes, return a pointer the host writes input into |
| *entrypoint* | `(in_ptr: i32, in_len: i32, arg: i32) -> i64` | transform input → output |

The entrypoint return value **packs the output region** as
`(out_ptr << 32) | out_len`, where both halves are unsigned 32-bit values. The
host reads `out_len` bytes at `out_ptr` from `memory`.

`arg` carries an entrypoint-specific integer (see below). Inputs/outputs are
UTF-8 bytes (compressor) or JSON bytes (provider).

## Well-known entrypoints

### `lctx_compress` — text → compressed text

- `arg` = soft byte budget (`-1` = none).
- The host **additionally enforces the hard budget** after decoding
  (`truncate_to_budget`, UTF-8 safe), so a naive guest can never exceed it.
- On any guest trap or decode failure the host falls back to the (budgeted)
  input — a faulty extension never breaks a read.
- Registered as a first-class [`Compressor`](extension-registry-v1.md):
  discoverable via `/v1/capabilities → extensions.compressors` and checked by the
  [conformance scorecard](conformance-v1.md) like any built-in.

### `lctx_provider_fetch` — request JSON → result JSON

- `arg` = `0`.
- Input: `{"action": "<action>", "params": {project, state, limit, query, id}}`.
- Output: a lenient `ProviderResult` JSON:
  `{"resource_type": "...", "items": [{"id","title",...}], "total_count?, truncated?}`.
  Missing optional fields default sensibly; unknown fields are ignored.
- Registered as a first-class [`ContextProvider`](provider-framework-contract-v1.md):
  discoverable via the provider registry and flows through the standard
  consolidation pipeline.

## Discovery & loading

Both kinds are **opt-in**, discovered from the `LEAN_CTX_WASM_DIR` directory:

- `*.wasm` whose stem names a **compressor** → registered automatically.
- `*.wasm` plus an optional sidecar `<stem>.provider.json`
  (`{"id","display","actions":[…]}`) → registered as a **provider**. Missing
  fields default to the file stem and a single `fetch` action.

Programmatic loading is also available:
`WasmCompressor::load(path, name)` and
`WasmProvider::load(path, id, display, actions)`.

## Execution model & sandbox

- **Thread-safe by construction.** The `Send + Sync` `Engine` + `Module` are
  retained; a fresh `Store` + instance is created **per call**, so calls cannot
  leak state into one another and the transform is deterministic.
- **No host imports.** Guests run against an empty linker — no syscalls, no
  network, no filesystem, no clock. Pure `bytes → bytes`. This is the strongest
  alignment with `extension-trust-v1`: a WASM guest is sandboxed by the runtime
  itself, not merely by policy.
- **Bounds-checked.** The host validates the returned `[out_ptr, out_ptr+out_len)`
  region against memory size and reports an error instead of reading OOB.

## Invariants (host-guaranteed)

1. Never panics on malformed modules or traps — errors are returned as
   `WasmError`.
2. A compressor always honors the byte budget (host-enforced post-step).
3. Identical input + arg + module ⇒ identical output (fresh store per call).
4. Output bytes are always read from within the guest's own linear memory.

## Versioning

Additive entrypoints (new `lctx_*` names) are backward compatible. Changing the
packed return convention or the required exports is a breaking change and bumps
the ABI to `wasm-abi-v2`.
