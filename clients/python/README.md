# leanctx (Python SDK)

Thin, **dependency-free** Python client for the lean-ctx HTTP `/v1` contract.
Standard library only (`urllib`) — installs and runs anywhere, no transitive
dependencies. It speaks the wire protocol only; it never links the engine or
re-implements compression. Mirrors the TypeScript (`@leanctx/sdk`) and Rust
(`lean-ctx-client`) SDKs.

## Install

```bash
pip install leanctx
# from this repo:
pip install ./clients/python
```

## Usage

```python
from leanctx import LeanCtxClient, run_conformance

client = LeanCtxClient("http://127.0.0.1:8080")

# Discovery
caps = client.capabilities()   # GET /v1/capabilities
api = client.openapi()         # GET /v1/openapi.json

# Tools
listing = client.list_tools(limit=10)
text = client.call_tool_text("ctx_read", {"path": "README.md"})

# Live events (SSE)
for event in client.subscribe_events():
    print(event["kind"], event["payload"])
```

## Methods

| Method | Endpoint |
|--------|----------|
| `health()` | `GET /health` |
| `manifest()` | `GET /v1/manifest` |
| `capabilities()` | `GET /v1/capabilities` |
| `openapi()` | `GET /v1/openapi.json` |
| `list_tools(offset=, limit=)` | `GET /v1/tools` |
| `call_tool(name, arguments, ...)` | `POST /v1/tools/call` |
| `call_tool_text(name, arguments, ...)` | `POST /v1/tools/call` + text extraction |
| `subscribe_events(...)` | `GET /v1/events` (SSE) |

## Shared conformance kit

`run_conformance(client)` runs the language-agnostic SDK conformance checks
against a live server and returns a scorecard. It mirrors the TypeScript SDK's
`runConformance` and the server-side `lean-ctx conformance`, keeping all clients
in lockstep on the same contract.

```python
card = run_conformance(client)
assert card.all_passed, [c for c in card.checks if not c.passed]
```

## Framework adapters

Expose the lean-ctx tool surface to popular agent frameworks. Each framework is
an **optional** dependency, imported lazily — installing `leanctx` pulls in none
of them. The OpenAI adapter is a pure transformation and needs no extra package.

```python
from leanctx import LeanCtxClient
from leanctx.adapters import (
    to_openai_tools, run_openai_tool_call,   # no extra dep
    to_langchain_tools,                       # pip install "leanctx[langchain]"
    to_llamaindex_tools,                      # pip install "leanctx[llamaindex]"
    to_crewai_tools,                          # pip install "leanctx[crewai]"
)

client = LeanCtxClient("http://127.0.0.1:8080")

# OpenAI function calling
tools = to_openai_tools(client)
# ... pass tools to client.chat.completions.create(...), then:
text = run_openai_tool_call(client, tool_call)

# LangChain / LlamaIndex / CrewAI
lc_tools = to_langchain_tools(client)
```

## Errors

- `LeanCtxConfigError` — invalid arguments / configuration (no I/O performed).
- `LeanCtxTransportError` — request never produced a response (network/DNS/TLS).
- `LeanCtxHTTPError` — non-2xx response, with `status`, `method`, `url`,
  `error_code`, and parsed `body`.

## Non-goals

- No engine linkage and no re-implemented compression/indexing logic.
- Stability over surface: only the documented `/v1` contract is exposed.

## Development

```bash
cd clients/python
python -m pytest
```
