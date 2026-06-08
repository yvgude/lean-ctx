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
lean-ctx-client = { git = "https://github.com/yvgude/lean-ctx", package = "lean-ctx-client" }
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
