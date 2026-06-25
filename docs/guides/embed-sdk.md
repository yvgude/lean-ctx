# Embed lean-ctx (Rust SDK)

Build *on* lean-ctx in-process. The `lean-ctx-sdk` crate gives you an `Engine`
handle that wraps a **shared session cache** and dispatches the real engine tools
(`ctx_read`, `ctx_search`, `ctx_symbol`, …) directly — no MCP server, no CLI.

This is the path for power-developer tools (the Lean-md use case). If instead you
want to *ship a tool the agent calls*, build an [Addon](addons.md). If you just
want drop-in text compression over HTTP, use the
[`compress()` client SDKs](compress-sdk.md).

> **Name disambiguation.** The pip/npm `lean-ctx-sdk` packages are the HTTP
> `compress()` clients. The Rust crate described here is the **in-process engine
> façade** — same name, different ecosystem and job.

## Why embed (the re-read delta)

The `Engine` owns one `SessionCache` across calls, so a re-read of an unchanged
file collapses to a token-cheap delta — the core reason to embed rather than
shell out per call:

```rust
use lean_ctx_sdk::{Engine, ReadMode};

let engine = Engine::builder(".").build()?;

let first = engine.read("src/main.rs", ReadMode::Full)?;
println!("read #1: {} tokens, saved {}", first.original_tokens, first.saved_tokens);

let again = engine.read("src/main.rs", ReadMode::Full)?;
println!("read #2: saved {} ({:.0}%)", again.saved_tokens, again.saved_pct());
// read #2 typically saves ~99% — the shared-cache delta.
# Ok::<(), lean_ctx_sdk::Error>(())
```

## Add the dependency

The crate lives in the lean-ctx workspace (`rust/crates/lean-ctx-sdk`). While it
is unpublished, depend on it by path:

```toml
[dependencies]
lean-ctx-sdk = { path = "path/to/lean-ctx/rust/crates/lean-ctx-sdk" }
```

## The surface (v1)

| You want | Call |
|----------|------|
| Read a file (compressed, cached) | `engine.read(path, ReadMode::Auto)` |
| Search code | `engine.search("pattern", None)` |
| Find a symbol definition | `engine.symbol("MyType")` |
| Outline one file | `engine.outline("src/lib.rs")` |
| Directory tree / repo map | `engine.tree(None)` |
| Any other tool | `engine.call("ctx_impact", args)` |
| Count tokens (no engine) | `lean_ctx_sdk::tokens::count(text)` |
| Hash content (no engine) | `lean_ctx_sdk::hash::blake3_str(text)` |
| Author + audit an addon | `lean_ctx_sdk::addon::scaffold/audit` |

`ReadMode` mirrors the engine modes: `Auto`, `Full`, `Raw`, `Signatures`, `Map`,
`Diff`, `Reference`, `Task`, `Lines { start, end }`.

`Engine::call(name, args)` reaches **every** registered tool with a raw JSON arg
map — the escape hatch for capabilities without a typed method yet. A string
`path` argument is PathJail-resolved automatically. See the full
[surface map](../rfcs/sdk-embedding-v1.md).

## Safe by default

`Engine::builder(root).build()` is read-mostly and scoped:

- **PathJail on** — paths resolve against `root`; escapes/secret paths error with
  `Error::Path`.
- **Scoped state** — engine data goes to a throwaway temp dir. Point it at your
  real lean-ctx data to share session memory with the CLI/MCP:

  ```rust
  let engine = Engine::builder(".")
      .data_dir("/home/me/.local/share/lean-ctx")
      .build()?;
  # Ok::<(), lean_ctx_sdk::Error>(())
  ```

- **Write/exec gated** — mutating tools are denied unless you opt in:

  ```rust
  let engine = Engine::builder(".")
      .allow_write(true)   // ctx_edit, ctx_fill
      .allow_exec(true)    // ctx_shell, ctx_execute
      .build()?;
  # Ok::<(), lean_ctx_sdk::Error>(())
  ```

The SDK also drops the engine's `jemalloc` feature, so embedding never forces a
`#[global_allocator]` onto your binary.

## Runtime constraint

Engine methods are synchronous and drive their own multi-threaded Tokio runtime.
**Do not call them from inside another Tokio runtime worker.** From async code:

```rust,ignore
let out = tokio::task::spawn_blocking(move || engine.read("src/main.rs", ReadMode::Full))
    .await
    .unwrap()?;
```

## Distribution stays Addons

The SDK is for *building*; distribution is still the [Addon](addons.md) system. A
binary you build with the SDK and ship as an addon runs under the gateway OS
sandbox + output redaction + trust/signing — embedding does not weaken
end-user security.

## Run the example

```bash
# from the lean-ctx repo
cargo run -p lean-ctx-sdk --example embed
```

## See also

- [Extending lean-ctx — which mechanism to use](extensions.md)
- [SDK surface map + RFC](../rfcs/sdk-embedding-v1.md)
- [Addons — community extensions](addons.md)
- [`compress()` client SDKs (pip/npm)](compress-sdk.md)
