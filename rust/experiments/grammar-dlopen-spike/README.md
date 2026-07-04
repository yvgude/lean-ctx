# Phase 0 spike: dlopen a tree-sitter grammar at runtime

Prototype for #690 (grammar tiering — long-tail tree-sitter grammars as signed
runtime addons). Answers the single highest-risk question before committing to
a manifest schema or CI matrix: **can a tree-sitter grammar be loaded from a
platform-specific dylib fetched at runtime, instead of statically linked?**

**Phase 1c update:** the grammar dylib this spike proved out graduated into a
real, CI-built crate at `crates/grammar-addons/lua` (package `grammar-lua`,
built by `.github/workflows/grammar-addons.yml`) and a real loader at
`core::signatures_ts::grammar_loader`. `host/` below stays put as a
standalone manual-verification harness for locally-built dylibs — it is not
part of the release pipeline.

## Result: yes, proven on Windows

```
$ cargo build --release -p grammar-lua
$ cargo build --release -p grammar-dlopen-host
$ ./target/release/grammar-dlopen-host <path-to>/grammar_lua.dll
loaded language, abi_version = 15
parsed root kind: chunk
SPIKE OK: dlopen-loaded Lua grammar parsed "return 42" cleanly
```

Windows carries the most uncertainty here (DLL export tables are stricter
than ELF/Mach-O default symbol visibility), so it's the platform to prove
first. Not yet re-verified on Linux/macOS — expected to be easier there, but
worth a quick CI check before Phase 1 lands.

## Design

`dylib/` is a `cdylib` crate depending on `tree-sitter-lua`. It does **not**
try to re-export the grammar crate's raw `tree_sitter_lua` C symbol — that
symbol is compiled from vendored C via `tree-sitter-lua`'s own `build.rs`,
and whether a symbol from a transitively-linked static object gets exported
in a Windows DLL by default was the actual open question. Instead, it
defines its own `#[no_mangle]` wrapper:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn lc_grammar_language() -> *const () {
    unsafe { (tree_sitter_lua::LANGUAGE.into_raw())() }
}
```

`LanguageFn::into_raw()`/`::from_raw()` (from the `tree-sitter-language`
crate — already a transitive dependency of every grammar crate lean-ctx
uses) round-trip a plain `unsafe extern "C" fn() -> *const ()`, which is
exactly what `libloading::Symbol` gives back after `dlopen` + symbol lookup.
Because `lc_grammar_language` is a first-class item defined directly in this
cdylib crate, rustc's cdylib export-table generation covers it unconditionally
— no platform-specific linker flags, `.def` file, or `dllexport` attribute
needed. Swapping the grammar dependency (and this one match arm) is the
entire diff needed to spike a different long-tail language.

`host/` is a standalone binary that `dlopen`s a built dylib, resolves
`lc_grammar_language`, reconstructs a `tree_sitter::Language` via
`LanguageFn::from_raw`, and parses a real source snippet with it — proving
the full round trip, not just that the symbol resolves.

## What this proves vs. what's still open

Proven:
- The dlopen/FFI mechanism itself works, on the platform with the most risk.
- `Language::abi_version()` is readable post-load, which is what a real
  loader needs to reject a grammar dylib built against an incompatible
  tree-sitter core version before calling `set_language` on it.

Not yet built (tracked as later phases under #690, not in this spike):
- Manifest schema for a grammar addon (ext, platform/arch, dylib hash,
  signature) — extending the pattern in `core/addons/manifest.rs` /
  `packages/leanctx-verify/src/verify.rs::verify_manifest_signature`.
- Wiring behind `signatures_ts::queries::get_language()` with a real
  fetch-and-cache path (this spike takes the dylib path as a CLI arg).
- CI build matrix producing signed dylibs per grammar × platform × arch.
- Zero-config UX (first-use fetch, offline fallback to the regex extractor,
  `doctor` healing) and the binary-size CI gate.

### The fetch-on-demand piece specifically

`core/updater.rs` (lean-ctx's own self-updater) already has almost the whole
shape needed, directly reusable rather than net-new:

- `https_agent()` / `download_bytes(url)` — bounded-timeout HTTPS GET.
- `platform_asset_name()` — OS/arch → asset filename, same shape needed for
  e.g. `grammar-lua-x86_64-pc-windows-msvc.dll`.
- `verify_download_integrity()` — SHA256SUMS-based checksum verification
  (a real grammar loader would layer Ed25519 manifest signing on top, per
  the addon-manifest pattern in `packages/leanctx-verify`, since a bare
  checksum isn't provenance).

Notably, `replace_binary`'s Windows locked-file handling (spawn a deferred
`.bat` that retries the rename for up to 60s) does **not** apply to grammar
dylibs — that complexity exists because the self-updater must replace the
*currently-running* executable. A grammar dylib is never the running image,
so a freshly downloaded file can just be `dlopen`'d with no such conflict —
meaningfully simpler than the binary-update case.

## Running it

`host/` is an **opt-in** workspace member (`experiments/...`), not in
`default-members` — `cargo build`/`test`/`clippy` on the main crate are
unaffected. Point it at any locally-built grammar-addon dylib (e.g. the real
`grammar-lua` crate) with `-p`:

```
cargo build --release -p grammar-lua
cargo build --release -p grammar-dlopen-host
./target/release/grammar-dlopen-host <path-to-built>/grammar_lua.dll
```

Automated coverage now lives with the real pieces: `grammar-addons.yml` CI
builds `grammar-lua` per platform, and `core::signatures_ts::grammar_loader`
has its own unit tests. This harness stays a manual debugging tool for a
locally-built dylib, not a CI-run test.
