# lean-ctx-sdk — in-process embedding façade (Rust)

Embed the lean-ctx context engine **in-process** behind a small, stable Rust
API. This is the substrate for power-developer tools (e.g. Lean-md) that want to
*call* lean-ctx directly — a shared session cache, compressed reads, code search
and symbol lookup — instead of going through the MCP server or CLI.

> **Not the `compress()` client.** The pip/npm packages also named
> `lean-ctx-sdk` are thin HTTP clients for the daemon's `/v1/compress` endpoint.
> *This* crate is the Rust **in-process** engine façade — different artifact,
> different job. See [`docs/guides/compress-sdk.md`](../../../docs/guides/compress-sdk.md)
> for the client SDKs.

## The headline: a shared-cache `Engine`

```rust
use lean_ctx_sdk::{Engine, ReadMode};

let engine = Engine::builder(".").build()?;

let first = engine.read("src/main.rs", ReadMode::Full)?;
let again = engine.read("src/main.rs", ReadMode::Full)?; // re-read collapses to a delta
assert!(again.saved_tokens >= first.saved_tokens);
# Ok::<(), lean_ctx_sdk::Error>(())
```

Because the `Engine` owns a **shared** `SessionCache`, a read followed by a
re-read of an unchanged file collapses to a token-cheap delta — the property
that makes lean-ctx worth embedding. The engine dispatches the *real* registered
tools (`ctx_read`, `ctx_search`, `ctx_symbol`, …), exactly as the MCP server
does.

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

## Surface (v1)

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
| Addon authoring | `addon::scaffold/audit/slugify` | addon scaffold + audit gate |

See [`docs/rfcs/sdk-embedding-v1.md`](../../../docs/rfcs/sdk-embedding-v1.md) for
the full surface map and roadmap, and
[`docs/guides/embed-sdk.md`](../../../docs/guides/embed-sdk.md) for the guide.

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
