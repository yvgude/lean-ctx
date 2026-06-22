# compress() SDK Cookbook (Python + TypeScript)

> Drop-in context compression for any LLM app. `compress(messages, model)` sends
> a chat-style array to the local lean-ctx daemon's deterministic
> [`POST /v1/compress`](../contracts/http-mcp-contract-v1.md) endpoint and returns
> the rewritten messages — byte-stable, so provider prompt caching keeps working.

Only **text payloads** are rewritten through lean-ctx's deterministic funnel;
images, `tool_use`/`tool_call` blocks and ids pass through untouched. lean-ctx's
own `ctx_*` tool results are left verbatim (they are already compressed).

## Install

```bash
pip install lean-ctx-sdk    # Python ≥ 3.9
npm install lean-ctx-sdk    # Node ≥ 18
```

Both SDKs talk to a running daemon — start it once with `lean-ctx proxy enable`.

## 1. Drop-in compress

```python
# Python
from lean_ctx import compress

messages = [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": large_log_or_file_dump},
]
messages = compress(messages, model="claude-sonnet-4")
# → send `messages` to your provider as usual
```

```ts
// TypeScript
import { compress } from "lean-ctx-sdk";

let messages = [
  { role: "system", content: "You are a helpful assistant." },
  { role: "user", content: largeLogOrFileDump },
];
messages = await compress(messages, { model: "claude-sonnet-4" });
```

## 2. Token-savings stats

Use the client directly to read the savings reported by the daemon:

```python
from lean_ctx import ProxyClient

result = ProxyClient().compress(messages, model="gpt-4o")
print(result.saved_tokens, result.saved_pct)   # e.g. 11979 17.2
messages = result.messages
```

```ts
import { ProxyClient } from "lean-ctx-sdk";

const result = await new ProxyClient().compress(messages, "gpt-4o");
console.log(result.stats.saved_tokens, result.stats.saved_pct);
messages = result.messages;
```

## 3. Vercel AI SDK middleware (TypeScript)

Compress every prompt automatically — no per-call changes:

```ts
import { wrapLanguageModel } from "ai";
import { openai } from "@ai-sdk/openai";
import { leanCtxMiddleware } from "lean-ctx-sdk";

const model = wrapLanguageModel({
  model: openai("gpt-4o"),
  middleware: leanCtxMiddleware({ model: "gpt-4o" }),
});
// generateText / streamText now send compressed prompts
```

`withLeanCtx(openai("gpt-4o"))` is a one-line shortcut. A compaction failure
(proxy down, auth, malformed) never breaks the generation — the original,
uncompressed prompt is sent instead.

## 4. LiteLLM (Python)

```python
import litellm
from lean_ctx import LeanCtxLiteLLMHandler

litellm.callbacks = [LeanCtxLiteLLMHandler(model="gpt-4o")]
# every completion now sends compressed messages
```

For non-LiteLLM code, `compress_request_data(data)` rewrites the `messages` of
any OpenAI-style request dict in place.

## 5. LangChain (Python)

```python
from langchain_core.messages import HumanMessage, SystemMessage
from lean_ctx import compress_messages

messages = compress_messages(
    [
        SystemMessage(content="You are a helpful assistant."),
        HumanMessage(content=large_log_or_file_dump),
    ],
    model="gpt-4o",
)
```

Message types and metadata are preserved (only `content` is rewritten).

## 6. Reference retrieval (reversibility)

When lean-ctx omits an oversized payload it leaves a durable reference id. Fetch
the original back on demand:

```python
from lean_ctx import ProxyClient

original = ProxyClient().resolve_reference("ref_abc123")
```

```ts
import { ProxyClient } from "lean-ctx-sdk";

const original = await new ProxyClient().resolveReference("ref_abc123");
```

## Configuration

The endpoint and session token are auto-discovered from the running daemon.
Every step is overridable:

| Setting | Env var | Default |
| --- | --- | --- |
| Proxy URL | `LEAN_CTX_PROXY_URL` | `http://127.0.0.1:<port>` |
| Proxy port | `LEAN_CTX_PROXY_PORT` | `config.toml` `proxy_port`, else UID-derived |
| Session token | `LEAN_CTX_PROXY_TOKEN` | `<data_dir>/session_token` |

```python
compress(messages, base_url="http://127.0.0.1:4444", token="…")
```

```ts
await compress(messages, { baseUrl: "http://127.0.0.1:4444", token: "…" });
```

If the daemon is down, `compress()` raises/rejects with `LeanCtxConnectionError`;
an unauthenticated request raises `LeanCtxAuthError`. Both extend `LeanCtxError`.

## Determinism (#498)

`/v1/compress` output is a pure function of `(messages, model)` — the same input
yields byte-identical output. Savings are reported in `stats`, never injected
into message bodies, so compressed prompts stay friendly to provider prompt
caching (Anthropic 90% / OpenAI 50% cached-token discounts). This is guarded by a
regression test (`proxy::compress_api::tests::determinism_regression_full_conversation_498`).

## Benchmark

Reproduce a head-to-head ratio + latency report (lean-ctx vs Headroom) over a
real corpus — see [`bench/compress/README.md`](../../bench/compress/README.md):

```bash
python bench/compress/benchmark.py --json
```

See also: [lean-ctx vs Headroom](../comparisons/vs-headroom.md).
