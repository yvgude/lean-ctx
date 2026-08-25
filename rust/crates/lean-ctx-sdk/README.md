# lean-ctx-sdk — in-process embedding research (Rust)

> **Status: Preview substrate — not a supported stable public Rust SDK.** Embed
> work is Preview; external addon authoring is Research. The only declared
> public SDK reference path is the **Preview** Python v1/OpenAI Agents wrapper.
> The separately governed Production SDK remains private/non-published staging;
> it is not this Apache crate.
> Current product scope and status are governed by
> [`docs/internal/README.md`](../../../docs/internal/README.md).

This crate records an experimental way to embed the lean-ctx context engine
**in-process** behind a small Rust API. It is a substrate for local developer
experiments (e.g. Lean-md) that want to
*call* lean-ctx directly — a shared session cache, compressed reads, code search
and symbol lookup — instead of going through the MCP server or CLI.

> **Not the `compress()` client.** The pip/npm packages also named
> `lean-ctx-sdk` are thin HTTP clients for the daemon's `/v1/compress` endpoint.
> *This* crate is the Rust **in-process** engine façade — different artifact,
> different job. See [`docs/guides/compress-sdk.md`](../../../docs/guides/compress-sdk.md)
> for the client SDKs.

## Experimental `Engine`

```rust
use lean_ctx_sdk::{Engine, ReadMode};

let engine = Engine::builder(".").build()?;

let first = engine.read("src/main.rs", ReadMode::Full)?;
let again = engine.read("src/main.rs", ReadMode::Full)?; // re-read collapses to a delta
assert!(again.saved_tokens >= first.saved_tokens);
# Ok::<(), lean_ctx_sdk::Error>(())
```

Because the `Engine` owns a **shared** `SessionCache`, an unchanged re-read can
be represented as a smaller delta. This is an implementation observation, not a
guaranteed token or task outcome. The engine dispatches registered local tools
(`ctx_read`, `ctx_search`, `ctx_symbol`, …) during experimentation.

## Safe by default

`EngineBuilder::build()` is read-mostly and scoped:

- **PathJail on** — every path is resolved against the project root; escapes and
  secret paths are rejected.
- **Scoped state** — engine data goes to a throwaway temp dir unless you call
  `.data_dir(…)`; your real `~/.lean-ctx` is never touched silently.
- **Auto-update off** for the embedded process.
- **Write/exec gated** — `ctx_edit`/`ctx_fill` need `.allow_write(true)`;
  `ctx_shell`/`ctx_execute` need `.allow_exec(true)`.

It also drops the engine's `jemalloc` feature, so embedding the SDK never forces
a `#[global_allocator]` onto your binary.

## Experimental surface

| Group | API | Backed by |
|-------|-----|-----------|
| Read | `Engine::read(path, ReadMode)` | `ctx_read` |
| Search | `Engine::search(pattern, subdir)` | `ctx_search` |
| Symbol | `Engine::symbol(name)` | `ctx_symbol` |
| Outline | `Engine::outline(path)` | `ctx_outline` |
| Tree | `Engine::tree(subdir)` | `ctx_tree` |
| Any tool | `Engine::call(name, json_args)` | the registry (write/exec gated) |
| Hashing | `hash::blake3_hex/str` | engine hash |
| Tokens | `tokens::count` | engine tokenizer |
| Compression | `compress::shell_output(…)` | shell pattern engine |
| Addon authoring | `addon::scaffold/audit/slugify` | Research-only addon experiments |

The source and linked RFCs are implementation material, not a public support or
roadmap commitment.

## Runtime note

Engine methods are synchronous and drive their own multi-threaded Tokio runtime,
so they must **not** be called from inside another Tokio runtime worker. From
async code, wrap calls in `tokio::task::spawn_blocking`.

## Build & test

```bash
# from rust/
cargo test  -p lean-ctx-sdk
cargo run   -p lean-ctx-sdk --example embed
cargo clippy -p lean-ctx-sdk --all-targets -- -D warnings
```

The crate is a workspace member but excluded from `default-members`, so the
engine's own `cargo build`/`test`/`clippy` are unchanged — build it explicitly
with `-p lean-ctx-sdk`.

## License

Apache-2.0, same as lean-ctx.
