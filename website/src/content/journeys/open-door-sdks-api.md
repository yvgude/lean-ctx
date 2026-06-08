You want LeanCTX's compression, retrieval and memory — but driven from *your* loop, in *your* language, against a contract that won't break under you. LeanCTX looked coding-agent shaped (stdio MCP, IDE configs); this journey is the language-neutral door into the same engine, plus a way to *discover* what a server supports instead of trial-and-error.

---

## 1. Run the local server — REST + SSE + MCP on one port

```bash
lean-ctx serve                      # → http://127.0.0.1:8080  (prints a loopback bearer token)
```

## 2. Discover capabilities — branch on facts, not guesses

`GET /v1/capabilities` returns the contract version, active persona, transports,
presets, the live tool surface and every registered extension; `GET /v1/openapi.json`
is a standard OpenAPI 3.0 description you can generate a typed client from.

```bash
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/v1/capabilities
```

## 3. Call it from an SDK — same wire contract in every language

```python
# pip install leanctx
from leanctx import LeanCtxClient
client = LeanCtxClient("http://127.0.0.1:8080", bearer_token=TOKEN)

caps = client.capabilities()                       # branch on real features
text = client.call_tool_text("ctx_read", {"path": "notes/acme.md"})
```

The TypeScript (`@leanctx/sdk`) and Rust (`lean-ctx-client`) SDKs mirror the same
surface — all are thin wire clients that never link the engine, so they stay
stable as LeanCTX evolves.

## 4. Wire it into your LLM loop with a framework adapter

```python
from leanctx.adapters import to_openai_tools, run_openai_tool_call
tools = to_openai_tools(client)                    # pass to your OpenAI call
result = run_openai_tool_call(client, tool_call)   # when the model picks one
```

Adapters ship for OpenAI, LangChain, LlamaIndex and CrewAI — present when the
framework is installed, with a clear `ImportError` when it isn't.

## 5. Under the hood — `rust/src/http_server/`

- **Stable `/v1` contract** — `GET /v1/capabilities` and `GET /v1/openapi.json`
  are the single source of truth, generated from code and drift-tested in CI.
- **Three first-party SDKs** — `clients/python`, `cookbook/sdk` (TypeScript),
  `clients/rust/lean-ctx-client`; each ships a client-side conformance kit
  (`run_conformance`) so your integration can prove it speaks the contract.

## Payoff

LeanCTX becomes a service any developer embeds — in any language, behind a
versioned, discoverable contract — instead of a coding-agent-only tool.
