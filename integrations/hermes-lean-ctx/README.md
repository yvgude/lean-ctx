# hermes-lean-ctx

**lean-ctx as Hermes' active context engine.** This plugin replaces Hermes
Agent's built-in `ContextCompressor` with deterministic, prompt-cache-friendly
compaction and injects lean-ctx's code-intelligence + cross-session memory tools
natively into the agent's tool list.

Instead of being "just another MCP server the agent might call", lean-ctx
*owns* the context window: it decides what to keep verbatim, offloads older
turns into durable memory, and gives the agent first-class recall tools to page
that memory back in losslessly.

## Why lean-ctx over the alternatives

| | built-in `ContextCompressor` | hermes-lcm | **hermes-lean-ctx** |
|---|---|---|---|
| Strategy | summarize + drop | DAG + SQLite + FTS | BM25 + graph + knowledge + semantic + LITM placement |
| Recall after compaction | lossy | lossless (grep/expand) | lossless (`ctx_search`/`ctx_semantic_search`/`ctx_expand`/`ctx_read`/`ctx_knowledge`) |
| Cross-session memory | no | per-project | yes (sessions, knowledge, handoff ledgers) |
| Determinism / prompt-cache | n/a | partial | deterministic, byte-stable output (prompt-cache friendly) |
| Engine location | in-agent | in-plugin | in the lean-ctx daemon (Single Source of Truth) |

Compaction logic lives in the daemon's `ctx_transcript_compact` tool, so every
client (this plugin, the CLI, other editors) gets identical, tested behaviour.

## How it works

```
Hermes agent loop
   └─ ContextEngine ABC ── LeanCtxEngine (this plugin, thin adapter)
                               └─ leanctx SDK ── HTTP /v1 ── lean-ctx daemon
                                                              └─ ctx_transcript_compact,
                                                                 ctx_search, ctx_knowledge, …
```

- **`compress(messages)`** keeps the system preamble and a *fresh tail* verbatim,
  and replaces older turns with a compact, recoverable summary. It calls the
  daemon's `ctx_transcript_compact` (which also offloads the raw turns into
  session memory for recall). If the daemon is unreachable it falls back to a
  pure-Python compaction so the agent loop never breaks.
- **`tool_call`/`tool_result` pairs are never split** across the compaction
  boundary — this invariant is enforced and tested on both the daemon and plugin
  side.
- **Native tools** (`get_tool_schemas`/`handle_tool_call`) expose `ctx_search`,
  `ctx_semantic_search`, `ctx_read`, `ctx_expand`, `ctx_knowledge`, `ctx_summary`
  so the agent can page detail back in on demand.
- **Cross-session persistence** via session lifecycle hooks: `resume` on start,
  `ctx_summary` + a deterministic `ctx_handoff` ledger on end.

## Requirements

- A running **lean-ctx daemon** exposing the HTTP `/v1` tools API
  (`lean-ctx serve`). See [Setup](#setup).
- The **`leanctx` Python SDK** in Hermes' environment: `pip install leanctx`.
- Optional: `tiktoken` for exact token counts (a char-based estimate is used
  otherwise).
- A Hermes Agent build that supports context-engine plugins
  (`context.engine` + `register_context_engine`).

## Setup

```bash
# 1. Install the plugin (symlinks this checkout into ~/.hermes/plugins).
./scripts/install.sh

# 2. Start the lean-ctx HTTP tools API (serves /v1; default port 8080).
#    NOTE: the always-on proxy (port 4444+) does NOT serve /v1/tools — use serve.
lean-ctx serve --host 127.0.0.1 --port 8080

# 3. Install the SDK in Hermes' Python.
pip install leanctx

# 4. Activate the engine in ~/.hermes/config.yaml:
#    context:
#      engine: "lean-ctx"
```

If your server is not on the default, point the plugin at it:

```bash
export LEANCTX_BASE_URL=http://127.0.0.1:8080
export LEANCTX_TOKEN=<token>     # only if you ran serve with --auth-token
```

Only one context engine can be active, so lean-ctx and hermes-lcm cannot be
enabled at the same time.

## Configuration (`LEANCTX_*` env vars)

| Variable | Default | Meaning |
|---|---|---|
| `LEANCTX_BASE_URL` | `http://127.0.0.1:8080` | lean-ctx `/v1` base URL |
| `LEANCTX_HTTP_PORT` | `8080` | Port used when `LEANCTX_BASE_URL` is unset |
| `LEANCTX_TOKEN` | – | Bearer token (if `serve --auth-token`) |
| `LEANCTX_TIMEOUT` | `30.0` | HTTP timeout (seconds) |
| `LEANCTX_CONTEXT_LENGTH` | `200000` | Context window used until the host calls `update_model` |
| `LEANCTX_THRESHOLD_FRACTION` | `0.75` | Fraction of the window at which compaction fires |
| `LEANCTX_PROTECT_FRACTION` | `0.25` | Recent fraction kept verbatim (the fresh tail) |
| `LEANCTX_PROTECT_MIN_MESSAGES` | `6` | Minimum recent messages kept verbatim |
| `LEANCTX_PROTECT_MIN_TOKENS` | `2000` | Minimum tail token budget |
| `LEANCTX_ENABLE_TOOLS` | `1` | Inject native recall tools into the agent |
| `LEANCTX_CORE_COMPACTION` | `1` | Prefer the daemon's `ctx_transcript_compact` (fallback: local) |
| `LEANCTX_WORKSPACE_ID` / `LEANCTX_CHANNEL_ID` | – | Optional routing for multi-workspace daemons |

Model context windows are inferred from the model name when the host does not
provide one (see `presets.py`); Hermes' explicit `update_model(context_length=…)`
always wins.

## Testing

```bash
# Hermetic unit tests (no daemon required):
python -m pytest

# Live integration against a real daemon:
lean-ctx serve --host 127.0.0.1 --port 8080 --auth-token test-token &
LEANCTX_LIVE_URL=http://127.0.0.1:8080 LEANCTX_LIVE_TOKEN=test-token \
  python -m pytest tests/test_live_daemon.py -v
```

No mock data is used: unit tests exercise the real compaction logic and a
recording gateway; live tests hit a real daemon.

## Benchmarks

`benchmarks/` contains a real, runnable head-to-head harness (token savings,
`compress()` latency, recoverable-recall). See [benchmarks/README.md](benchmarks/README.md).

## License

Apache-2.0 — see the lean-ctx repository.
