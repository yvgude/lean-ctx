# The Context OS Guide

lean-ctx began as a context layer for coding agents. It is now a **Context OS**:
a local-first runtime that any developer can build their own tools and agents on
— coding *or not* — through stable contracts, an extension surface, SDKs in
three languages, and persona-driven verticalization.

This guide is the map. It explains the architecture, the contracts you build
against, the extension points, and how the free local plane relates to the
commercial plane.

---

## 1. Principles

1. **Local-Free Invariant** — every single-developer feature runs locally, fully
   featured, with no account, license, or feature gate. Commercialization is
   *additive* (Team/Cloud), never subtractive. Enforced in CI by
   [`local-free-invariant-v1`](../contracts/local-free-invariant-v1.md).
2. **Contracts over code** — integrations target versioned wire contracts, not
   internal types. Every contract is machine-verified and drift-tested.
3. **Honesty** — we never claim enforcement we do not perform (see the
   [trust model](../contracts/extension-trust-v1.md)) and never ship mocks or
   stubs.
4. **Reproducibility** — the discovery documents and extension behavior are
   deterministic and self-checked by [`conformance-v1`](../contracts/conformance-v1.md).

---

## 2. Architecture at a glance

```
            ┌──────────────────────── clients ────────────────────────┐
            │  TS SDK (lean-ctx-client)   Python SDK (leanctx)   Rust SDK │
            │  + framework adapters (OpenAI/LangChain/LlamaIndex/Crew) │
            └───────────────┬─────────────────────────────────────────┘
                            │  HTTP /v1  (REST + SSE)  |  stdio MCP
            ┌───────────────▼─────────────────────────────────────────┐
            │  Discovery:  /v1/capabilities   /v1/openapi.json          │
            │  Tools:      /v1/tools          /v1/tools/call            │
            │  Events:     /v1/events (SSE)   Manifest: /v1/manifest    │
            ├──────────────────────────────────────────────────────────┤
            │  Personas → tool surface, read-mode, compressor, chunker, │
            │             intent taxonomy, sensitivity floor            │
            ├──────────────────────────────────────────────────────────┤
            │  Extension registry: read-modes · compressors · chunkers  │
            │  Ingestion + extractors: code · json · csv · eml · html · pdf │
            │  Plugins: hooks + manifest tools (sandboxed, trust-gated) │
            ├──────────────────────────────────────────────────────────┤
            │  Engine: compression · cache · BM25 · graph · knowledge   │
            │  Verifiable savings ledger → ROI/metering substrate       │
            └──────────────────────────────────────────────────────────┘
```

---

## 3. Discovery: branch on real capabilities

Never trial-and-error. Ask the server what it supports:

* `GET /v1/capabilities` ([`capabilities-contract-v1`](../contracts/capabilities-contract-v1.md))
  returns the contract version, active persona, transports, presets, tool
  surface, feature flags, the live extension registry, and every sub-contract
  version.
* `GET /v1/openapi.json` is a standard OpenAPI 3.0 description of the `/v1`
  surface — generate a typed client in any language.

Each SDK wraps these as `capabilities()` / `openapi()`.

---

## 4. Building your own tool

Three escalating options, cheapest first:

| You want to… | Use | Fork the engine? |
|--------------|-----|------------------|
| Call lean-ctx from your app/agent | an **SDK** (TS/Python/Rust) | No |
| Add a tool the agent can call | a **plugin manifest** `[[tools]]` | No |
| React to lifecycle events | a **plugin hook** | No |
| Add a read-mode / compressor / chunker | the **extension registry** | No (in-process today; WASM next) |
| Index a new file format | a **format extractor** | No |

### 4a. Plugin tools (no fork)

Declare a tool in `plugin.toml`; lean-ctx registers it as a native MCP tool at
startup and advertises it in `/v1/capabilities`:

```toml
[plugin]
name = "weather"
version = "0.1.0"

[[tools]]
name = "weather_lookup"
description = "Look up the weather for a city"
command = "weather-bin"
timeout_ms = 8000
input_schema = { type = "object", properties = { city = { type = "string" } }, required = ["city"] }

[trust]
permissions = ["network"]   # declared; surfaced for consent
```

The command receives the tool's JSON arguments on stdin and returns text on
stdout, sandboxed per the [trust model](../contracts/extension-trust-v1.md)
(scrubbed env + cwd jail + timeout by default).

### 4b. Hooks

`on_session_start`, `on_session_end`, `pre_read`, `post_compress`,
`on_knowledge_update` — declare a command per hook; lean-ctx fires it with the
event JSON on stdin. Hooks are zero-cost when no plugin listens.

### 4c. Extensions

Register a named `ReadMode`, `Compressor`, or `Chunker` through the
[extension registry](../contracts/capabilities-contract-v1.md). Built-ins use the
exact same API, and every registered transform is conformance-checked for
determinism, byte-budget, and coverage.

---

## 5. Beyond code: ingestion, extractors, personas

* **Ingestion** ([`ingestion-spec-v1`]) decides *whether* a file is indexable —
  code, documents, data, or text — not just source code.
* **Extractors** ([`extractors-v1`](../contracts/extractors-v1.md)) decide *how*
  to read a format: JSON, CSV/TSV, EML, HTML, and PDF become clean text plus
  structure-aware chunks.
* **Personas** ([`persona-spec-v1`](../contracts/persona-spec-v1.md)) bundle a
  tool surface, read-mode, compressor, chunker, intent taxonomy, and sensitivity
  floor. Built-ins: `coding`, `research`, `lead-gen`, `support`,
  `data-analysis`. Select with `LEAN_CTX_PERSONA` or `config.persona`.

Together these turn lean-ctx into the context layer for a lead-gen agent, a
research assistant, a support triager, or a data pipeline — see the
[non-coding cookbook](./cookbook-non-coding.md).

---

## 6. Proving value: savings ledger → ROI

Every compression is recorded in a tamper-evident, SHA-256-chained savings
ledger. `lean-ctx savings sign` produces an Ed25519-signed batch; `lean-ctx
savings roi --json` derives a privacy-preserving [`RoiReport`] (net tokens, USD,
per-event averages, top tools) **strictly from the signed batch** — numbers and
hashes only, no paths/prompts/code. This is the metering substrate the Cloud
plane builds on (EPIC 13), and proof of value you can run locally today.

---

## 7. Planes: free local vs. commercial

| Plane | What | Cost |
|-------|------|------|
| **Personal** (default) | All local features: compression, cache, knowledge, sessions, gateway, extractors, personas, plugins, savings ledger, ROI | Free, ungated |
| **Team / Cloud** | Additive: sync, RBAC, marketplace, hosted connectors, domain packs, metered billing (EPIC 13) | Commercial, opt-in |

The default plane is `personal` and `/v1/capabilities` reports it. Local
features never react to license/plan/account environment variables — guaranteed
by the [Local-Free Invariant CI gate](../contracts/local-free-invariant-v1.md).

---

## 8. Self-check

```bash
lean-ctx conformance          # contracts honored + extensions well-behaved
lean-ctx savings roi          # local ROI from the signed ledger
```

Each SDK ships the same client-side conformance kit (`runConformance` /
`run_conformance`) so your integration can prove it speaks the contract before
you ship.

---

## 9. Reference

* RFC: [`rfc-v1.md`](./rfc-v1.md)
* Contracts: [`docs/contracts/`](../contracts/)
* SDKs: [`cookbook/sdk`](../../cookbook/sdk) · [`clients/python`](../../clients/python) · [`clients/rust/lean-ctx-client`](../../clients/rust/lean-ctx-client)
* Non-coding recipes: [`cookbook-non-coding.md`](./cookbook-non-coding.md)
