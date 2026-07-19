# lean-ctx-client

A thin, **stable** Rust client for the [lean-ctx](https://leanctx.com) Context OS
`/v1` HTTP contract. Talk to a running lean-ctx server from your own
program — an agent harness, a lead-gen worker, a research bot — **without
linking the engine**.

This is the Rust counterpart of the TypeScript SDK in `cookbook/sdk`. Both
target the same versioned contract:

- `docs/contracts/http-mcp-contract-v1.md`
- `docs/contracts/capabilities-contract-v1.md`

## Install

```toml
[dependencies]
lean-ctx-client = { version = "0.2", git = "https://github.com/yvgude/lean-ctx", package = "lean-ctx-client" }
serde_json = "1"
```

## Usage

```rust
use lean_ctx_client::{LeanCtxClient, CallContext};
use serde_json::json;

let client = LeanCtxClient::builder("http://127.0.0.1:7777")
    .bearer_token(std::env::var("LEANCTX_TOKEN").unwrap_or_default())
    .workspace_id("acme")
    .build()?;

// Discover capabilities before branching on features.
let caps = client.capabilities()?;
println!("plane = {}, tools = {}", caps["plane"], caps["tools"]["total"]);

// Call any tool over the boundary and read its text.
let text = client.call_tool_text(
    "ctx_search",
    Some(json!({ "pattern": "fn main", "path": "src/" })),
    None::<&CallContext>,
)?;

// Stream context events (blocking iterator).
for event in client.subscribe_events(&Default::default())? {
    let event = event?;
    println!("{} {}", event.id, event.kind);
}
# Ok::<(), lean_ctx_client::LeanCtxError>(())
```

## What it covers

| Method | Endpoint |
|--------|----------|
| `health()` | `GET /health` |
| `manifest()` | `GET /v1/manifest` |
| `capabilities()` | `GET /v1/capabilities` |
| `openapi()` | `GET /v1/openapi.json` |
| `list_tools(offset, limit)` | `GET /v1/tools` |
| `call_tool(...)` / `call_tool_text(...)` | `POST /v1/tools/call` |
| `subscribe_events(...)` | `GET /v1/events` (SSE) |

Open-ended documents (`manifest`, `capabilities`, `openapi.json`) are returned
as `serde_json::Value`, so new server keys never break a client build. Branch on
stable fields (`capabilities["plane"]`, `LeanCtxError::error_code()`), not on
human-readable messages.

## Offline OCLA v1 verification

The standalone client also decodes the public
`CanonicalTokenEnvelopeV1` and `AgentEnvelopeV1` JSON contracts without a
running server or an engine-crate dependency:

```rust
use lean_ctx_client::{
    decode_agent_envelope, decode_canonical_token_envelope,
    verify_agent_gateway_admissibility,
};

fn verify(token_wire: &[u8], agent_wire: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let token = decode_canonical_token_envelope(token_wire)?;
    let agent = decode_agent_envelope(agent_wire)?;
    verify_agent_gateway_admissibility(&agent)?;
    println!("{} {}", token.provider, agent.relay_id);
    Ok(())
}
```

Both decoders cap documents at 64 KiB and fail closed on malformed, unknown,
duplicate, non-canonical, unsupported-version, lineage, accounting, and
content-derived relay-ID drift. They verify local wire integrity only. The
separate gateway helper adds one explicit client policy: self-relays are
rejected. Neither layer claims remote admission or delivery, live adapter
interoperability, billing, savings, gRPC/OpenAPI delivery, or N-1
compatibility. Authoritative schemas are packaged with the crate and mirrored
from:

- `docs/contracts/ocla-wire-v1.schema.json`
- `docs/contracts/ocla-agent-envelope-v1.schema.json`

For process-level offline checks, the same bounded verifier is available from
the source checkout:

```bash
cargo run --locked --bin lean-ctx-ocla-verify -- token envelope.json
cargo run --locked --bin lean-ctx-ocla-verify -- agent relay.json --gateway
```

The CLI accepts only direct regular files. On Unix it atomically refuses
symlinks and opens non-blocking before validating the opened handle, so FIFOs,
devices, and directories cannot bypass its 64 KiB bound or block the process.
Other platforms apply the same pre-/post-open regular-file checks using the
available standard-library primitives.

## Local verification gates

The declared MSRV is Rust 1.74. Generate and verify the lockfile with that exact
toolchain before testing newer stable Rust:

```bash
rustup toolchain install 1.74.0 --profile minimal
cargo +1.74.0 generate-lockfile
cargo +1.74.0 test --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
./scripts/verify-packaged-source.sh
```

The committed lock intentionally pins the release-critical dependency graph.
This is local compatibility evidence, not a crates.io release or external
certification.

## Non-goals (the embedding boundary)

This crate is intentionally small and decoupled:

- **No engine linkage.** It does not depend on the `lean-ctx` engine crate.
  Integration is over the **process boundary** (HTTP/MCP). Full-crate linking of
  the engine is unsupported.
- **No re-implemented engine logic.** Compression, indexing, ranking, and
  knowledge live in the server; the client only speaks the wire contract.
- **Stability over surface.** Exported types mirror the versioned `/v1` contract.
  Engine internals are never re-exported here.
- **Bring your own async.** The client is blocking by design (one small HTTP
  dependency, no runtime). Wrap calls in a thread or `spawn_blocking` from async
  code.

## License

Apache-2.0
