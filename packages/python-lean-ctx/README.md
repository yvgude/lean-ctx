# lean-ctx (Python SDK)

Context compression for AI agents — a thin, dependency-free client for the local
[lean-ctx](https://leanctx.com) daemon.

```bash
pip install lean-ctx
```

## Drop-in `compress(messages, model)`

Compress a chat-style `messages` array before sending it to any model. Only text
payloads are rewritten through lean-ctx's deterministic funnel; images,
tool-call blocks and ids pass through untouched, and the output is byte-stable so
it stays friendly to provider prompt caching.

```python
from lean_ctx import compress

messages = [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": large_log_or_file_dump},
]

messages = compress(messages, model="claude-sonnet-4")
# → send `messages` to your provider as usual
```

Works with both OpenAI-style (`content: "string"`) and Anthropic-style
(`content: [{type: "text", …}, {type: "tool_result", …}]`) messages.

### Token-savings stats

```python
from lean_ctx import ProxyClient

result = ProxyClient().compress(messages, model="gpt-4o")
print(result.saved_tokens, result.saved_pct)   # e.g. 1840 63.1
messages = result.messages
```

## Configuration

The endpoint and session token are auto-discovered from the running daemon. Every
step is overridable:

| Setting | Env var | Default |
| --- | --- | --- |
| Proxy URL | `LEAN_CTX_PROXY_URL` | `http://127.0.0.1:<port>` |
| Proxy port | `LEAN_CTX_PROXY_PORT` | `config.toml` `proxy_port`, else UID-derived |
| Session token | `LEAN_CTX_PROXY_TOKEN` | `<data_dir>/session_token` |

Or pass them explicitly (useful in CI / against a remote proxy):

```python
compress(messages, base_url="http://127.0.0.1:4444", token="…")
```

If the daemon is not running, `compress()` raises `LeanCtxConnectionError`; an
unauthenticated request raises `LeanCtxAuthError`. Both subclass `LeanCtxError`.

## CLI helpers

`LeanCtxClient` wraps the `lean-ctx` binary for `read` / `search` / `shell` /
`gain` / `benchmark`. Framework adapters `LeanCtxRetriever` (LangChain) and
`LeanCtxNodeParser` (LlamaIndex) are available via the `langchain` / `llamaindex`
extras.

## License

MIT
