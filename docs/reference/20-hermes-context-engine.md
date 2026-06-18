# Journey 20 — Hermes Context Engine

> You're embedding lean-ctx *inside* an agent framework rather than calling it as
> an MCP server. This journey covers making lean-ctx the **active context engine**
> for [Hermes Agent](https://github.com/hermes-agent): it replaces Hermes' built-in
> `ContextCompressor`, owns the context window, and gives the agent first-class
> recall tools to page durable memory back in losslessly.

Source files referenced here:
- `rust/src/tools/ctx_transcript_compact.rs` — `compact_messages`, `render_result`,
  `serialize_transcript` (the deterministic compaction core)
- `rust/src/tools/registered/ctx_transcript_compact.rs` — the MCP/`/v1` tool wrapper
- `integrations/hermes-lean-ctx/engine.py` — `LeanCtxEngine` (the `ContextEngine` adapter)
- `integrations/hermes-lean-ctx/compaction.py` — the local Python fallback
- `integrations/hermes-lean-ctx/tools.py`, `schemas.py` — native recall tools
- `integrations/hermes-lean-ctx/transport.py`, `config.py`, `presets.py` — `/v1`
  client, `LEANCTX_*` config, model-window presets
- `integrations/hermes-lean-ctx/plugin.yaml` — the Hermes plugin manifest

---

## 0. The mental model — engine vs. MCP server

Every other journey treats lean-ctx as a tool an agent *calls*. Here it is the
other way around: lean-ctx becomes the component the agent loop *delegates its
context window to*.

- As an **MCP server**, lean-ctx answers `ctx_*` tool calls the model decides to
  make. It never sees the full conversation.
- As a **context engine**, the host (Hermes) hands lean-ctx the entire message
  array on every turn and asks it to compact it. lean-ctx decides what stays
  verbatim, what becomes a recoverable summary, and what gets offloaded into
  durable session memory.

Only one context engine can be active at a time, so lean-ctx and Hermes'
built-in `ContextCompressor` (or `hermes-lcm`) are mutually exclusive.

---

## 1. The compaction core — `ctx_transcript_compact`

**What it does:** Compacts an OpenAI-format message array deterministically. It
keeps the system preamble and a *fresh tail* verbatim, replaces older turns with
a recoverable summary, and offloads the raw turns into lean-ctx session memory so
the recall tools (and the autonomy consolidation pipeline) can page them back in.

It is the 77th MCP tool and is exposed on both the MCP surface and the HTTP `/v1`
tools API, so every client — this plugin, the CLI, other editors — gets the same
tested behaviour.

```text
ctx_transcript_compact messages=<OpenAI message array>
                        fresh_tail_tokens=4000      # recent tokens kept verbatim
                        protect_min_messages=6      # min recent messages kept verbatim
                        focus_topic="auth refactor" # optional: bias the summary
```

| Parameter | Default | Meaning |
|---|---|---|
| `messages` (required) | – | OpenAI-format message array to compact |
| `fresh_tail_tokens` | `4000` | Recent tokens kept verbatim (the fresh tail) |
| `protect_min_messages` | `6` | Minimum recent messages kept verbatim |
| `focus_topic` | – | Optional topic to prioritise in the summary |

**Returns** JSON `{messages, stats}`: the compacted array plus deterministic
stats (`original_tokens`, `compacted_tokens`, `did_compact`, …).

**Two invariants, enforced and tested:**
1. **A `tool_call` and its `tool_result` are never split** across the compaction
   boundary. Truncating between them would leave the model with a dangling call.
2. **Output is byte-stable** for the same input — no timestamps, counters or
   randomness — so it preserves the provider's prompt-cache prefix (#498).

**Under the hood:** `compact_messages()` (`rust/src/tools/ctx_transcript_compact.rs`)
splits the array into the protected head + tail and the summarizable middle,
renders the summary, and returns a `CompactResult`. The registered wrapper
(`registered/ctx_transcript_compact.rs`) then best-effort offloads the summarized
turns into the bound session via `ctx_session` (as a `finding`), capped at
`OFFLOAD_MAX_CHARS` (8 000). Offload is skipped when no session is bound (e.g. a
one-shot CLI call), so the tool is safe to call anywhere.

---

## 2. The plugin — `integrations/hermes-lean-ctx`

**What it does:** A thin Python `ContextEngine` (`LeanCtxEngine`, `engine.py`) that
Hermes loads via `register_context_engine`. It is an adapter, not a
re-implementation — the heavy lifting stays in the daemon.

```
Hermes agent loop
   └─ ContextEngine ABC ── LeanCtxEngine (this plugin, thin adapter)
                               └─ leanctx SDK ── HTTP /v1 ── lean-ctx daemon
                                                              └─ ctx_transcript_compact,
                                                                 ctx_search, ctx_knowledge, …
```

- **`compress(messages)`** keeps the system preamble + fresh tail verbatim and
  replaces older turns with a recoverable summary. It calls the daemon's
  `ctx_transcript_compact`; if the daemon is unreachable it falls back to a pure
  Python compaction (`compaction.py`) so the agent loop never breaks.
- **Native recall tools** (`tools.py` / `schemas.py`) inject `ctx_search`,
  `ctx_semantic_search`, `ctx_read`, `ctx_expand`, `ctx_knowledge` and
  `ctx_summary` into the agent's tool list, so the model can page detail back in
  on demand after a compaction.
- **Cross-session persistence** via session lifecycle hooks: `resume` on start,
  `ctx_summary` + a deterministic `ctx_handoff` ledger on end.
- **Model-window presets** (`presets.py`) infer the context window from the model
  name until the host calls `update_model(context_length=…)`, which always wins.

---

## 3. Setup

```bash
# 1. Install the plugin (symlinks this checkout into ~/.hermes/plugins).
cd integrations/hermes-lean-ctx && ./scripts/install.sh

# 2. Start the lean-ctx HTTP tools API (serves /v1; default port 8080).
#    NOTE: the always-on proxy (4444+) does NOT serve /v1/tools — use `serve`.
lean-ctx serve --host 127.0.0.1 --port 8080

# 3. Install the SDK in Hermes' Python.
pip install leanctx

# 4. Activate the engine in ~/.hermes/config.yaml:
#    context:
#      engine: "lean-ctx"
```

`lean-ctx init --agent hermes` prints this same engine-plugin hint, so the
onboarding path points here automatically.

If the server is not on the default, point the plugin at it:

```bash
export LEANCTX_BASE_URL=http://127.0.0.1:8080
export LEANCTX_TOKEN=<token>     # only if you ran serve with --auth-token
```

---

## 4. Configuration — `LEANCTX_*` env vars

Read by `config.py`; Hermes' explicit `update_model(context_length=…)` always
overrides the inferred window.

| Variable | Default | Meaning |
|---|---|---|
| `LEANCTX_BASE_URL` | `http://127.0.0.1:8080` | lean-ctx `/v1` base URL |
| `LEANCTX_HTTP_PORT` | `8080` | Port used when `LEANCTX_BASE_URL` is unset |
| `LEANCTX_TOKEN` | – | Bearer token (if `serve --auth-token`) |
| `LEANCTX_TIMEOUT` | `30.0` | HTTP timeout (seconds) |
| `LEANCTX_CONTEXT_LENGTH` | `200000` | Window used until the host calls `update_model` |
| `LEANCTX_THRESHOLD_FRACTION` | `0.75` | Fraction of the window at which compaction fires |
| `LEANCTX_PROTECT_FRACTION` | `0.25` | Recent fraction kept verbatim (the fresh tail) |
| `LEANCTX_PROTECT_MIN_MESSAGES` | `6` | Minimum recent messages kept verbatim |
| `LEANCTX_PROTECT_MIN_TOKENS` | `2000` | Minimum tail token budget |
| `LEANCTX_ENABLE_TOOLS` | `1` | Inject native recall tools into the agent |
| `LEANCTX_CORE_COMPACTION` | `1` | Prefer the daemon tool (fallback: local Python) |
| `LEANCTX_WORKSPACE_ID` / `LEANCTX_CHANNEL_ID` | – | Optional routing for multi-workspace daemons |

---

## 5. Why lean-ctx over the alternatives

| | built-in `ContextCompressor` | `hermes-lcm` | **hermes-lean-ctx** |
|---|---|---|---|
| Strategy | summarize + drop | DAG + SQLite + FTS | BM25 + graph + knowledge + semantic + LITM placement |
| Recall after compaction | lossy | lossless (grep/expand) | lossless (`ctx_search`/`ctx_semantic_search`/`ctx_expand`/`ctx_read`/`ctx_knowledge`) |
| Cross-session memory | no | per-project | yes (sessions, knowledge, handoff ledgers) |
| Determinism / prompt-cache | n/a | partial | deterministic, byte-stable output |
| Engine location | in-agent | in-plugin | in the lean-ctx daemon (single source of truth) |

---

## 6. Testing & benchmarks

```bash
# Hermetic unit tests (no daemon required):
cd integrations/hermes-lean-ctx && python -m pytest

# Live integration against a real daemon:
lean-ctx serve --host 127.0.0.1 --port 8080 --auth-token test-token &
LEANCTX_LIVE_URL=http://127.0.0.1:8080 LEANCTX_LIVE_TOKEN=test-token \
  python -m pytest tests/test_live_daemon.py -v
```

`benchmarks/` (`run.py`) is a real, runnable head-to-head harness — token
savings, `compress()` latency and recoverable-recall against the import-guarded
`ContextCompressor` / `hermes-lcm`. No mock data: unit tests exercise the real
compaction logic and a recording gateway; live tests hit a real daemon.

---

## Where the neighbouring topics live

| Topic | Reference |
|---|---|
| The `/v1` HTTP+SSE contract and SDKs | [Team, Cloud & CI](09-team-cloud-ci.md) |
| `lean-ctx serve` over HTTP, multi-repo | [Advanced & Integrations](05-advanced.md) |
| Sessions, knowledge, handoff ledgers | [Memory & Knowledge](03-memory-and-knowledge.md) |
| Writing your own plugins / WASM | [Advanced & Integrations](05-advanced.md) |
