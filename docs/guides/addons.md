# Addons — WASM extensions

> **Status: Preview — local addon channel.** Installing, building and running a
> WASM addon on your own machine is a supported path. LeanCTX does **not** offer
> a marketplace, a curated registry, ranking or recommendation, managed binary
> distribution, or a commercial catalog, and this document is not a promise of
> one.

An addon is a sandboxed WebAssembly module that runs *inside* the context
pipeline. Today that means one thing: a **compressor**. Context providers use
the same ABI and are wired the same way; every other extension point
(chunkers, read modes, render transforms) exists as a Rust trait and is not
reachable over the WASM ABI yet.

If your extension only calls an API or runs on its own, **write an MCP server
instead**. It needs nothing from this repository, you publish it yourself, and
lean-ctx already audits foreign MCP servers in `tools health`. The addon
channel is reserved for code that has to run inside the pipeline.

## Using an addon

```bash
lean-ctx addon add ./my-addon-1.0.0.ctxpkg   # verifies, shows what it is, asks
lean-ctx addon list                          # what is installed and what it loads
lean-ctx addon info @ns/my-addon             # the author's manifest, module digests
lean-ctx addon remove @ns/my-addon
```

`add` is the only verb that stores executable code, so it is the only one that
asks — and it refuses to proceed non-interactively unless you pass `--yes`.

## Writing one

A directory with a manifest and at least one module:

```
my-addon/
  lean-ctx-addon.toml
  my_addon.wasm
```

```toml
[addon]
name = "@ns/my-addon"
version = "1.0.0"
description = "What it does, in one line"
```

The module exports the ABI v1 entrypoints — see
[`wasm-abi-v1`](../contracts/wasm-abi-v1.md) for the exact signatures. Any
language that compiles to `wasm32-unknown-unknown` works.

```bash
lean-ctx addon release ./my-addon
```

That produces a signed `.ctxpkg` with the modules **embedded**. No artifact
host, no checksum files, no CI: the package signature covers the executable
bytes, so there is nothing external to hash or serve. Publish it wherever you
like, or with `lean-ctx pack publish` to a registry you name.

The first `release` creates an ed25519 signing key under your data directory.
It identifies you as the publisher across releases — back it up.

## What the sandbox does and does not do

**Enforced**, on every call:

- No ambient environment. A module sees nothing of your shell.
- A fresh WASM store per call, so nothing leaks between invocations.
- The host truncates the output to the byte budget *after* decoding it, so a
  module cannot exceed the budget by ignoring it.
- Modules are stored read-only and never marked executable — they are fed to an
  interpreter, never to the OS loader.

**Verified at install:**

- The package signature, re-checked locally rather than trusted from wherever
  it came ("registry compromise ≠ client compromise").
- Each module against its pinned SHA-256, before anything is written.
- WebAssembly magic bytes, so a mislabelled payload is refused early.

**Not claimed:** a module can compute anything it likes within those bounds.
The sandbox limits reach, not intent. Install addons from publishers you have
reason to trust, and read `addon info` before you do.

## Development override

`LEAN_CTX_WASM_DIR=<dir>` loads unsigned `.wasm` files straight from a
directory, for authoring a module before packaging it. It is a developer
convenience with none of the verification above — not an install path.

See [Product Architecture](../internal/vision/PRODUCT-ARCHITECTURE.md) for the
local-first guardrails this channel sits inside.
