# @leanctx/sdk

Thin, dependency-free TypeScript client for the lean-ctx **HTTP `/v1` contract**.
It speaks the wire protocol only — it never links the engine or re-implements
compression — so it stays stable as lean-ctx evolves and works in Node, Deno,
Bun, and the browser (anywhere `fetch` exists).

## Install

```bash
npm install @leanctx/sdk
```

## Usage

```ts
import { LeanCtxClient, toolResultToText, runConformance } from "@leanctx/sdk";

const client = new LeanCtxClient({ baseUrl: "http://127.0.0.1:8080" });

// Discovery
const caps = await client.capabilities(); // GET /v1/capabilities
const api = await client.openapi();        // GET /v1/openapi.json

// Tools
const { tools, total } = await client.listTools();
const text = await client.callToolText("ctx_read", { path: "README.md" });

// Live events (SSE)
for await (const ev of client.subscribeEvents()) {
  console.log(ev.kind, ev.payload);
}
```

## Methods

| Method | Endpoint |
|--------|----------|
| `health()` | `GET /health` |
| `manifest()` | `GET /v1/manifest` |
| `capabilities()` | `GET /v1/capabilities` |
| `openapi()` | `GET /v1/openapi.json` |
| `listTools({ offset, limit })` | `GET /v1/tools` |
| `callToolResult(name, args, ctx)` | `POST /v1/tools/call` |
| `callToolText(name, args, ctx)` | `POST /v1/tools/call` + text extraction |
| `subscribeEvents({ workspaceId, … })` | `GET /v1/events` (SSE) |

## Shared conformance kit

`runConformance(client)` runs the language-agnostic SDK conformance checks
(health, capabilities shape, OpenAPI shape, tools listing) against a live server
and returns a scorecard. It mirrors the server-side `lean-ctx conformance`
command and is kept in lockstep with the Python SDK so every client proves the
same contract.

```ts
const card = await runConformance(client);
if (!card.allPassed) console.error(card.checks.filter((c) => !c.passed));
```

## Non-goals

- No engine linkage and no re-implemented compression/indexing logic.
- Stability over surface: only the documented `/v1` contract is exposed.
- Bring-your-own runtime: any standard `fetch` works; pass `fetchImpl` to inject.
