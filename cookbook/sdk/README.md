# lean-ctx-client — research implementation

> **Status: Research — not a supported public LeanCTX | SDK.** This TypeScript
> client is local HTTP-contract test scaffolding. The only declared public SDK
> path is the **Preview** Python v1/OpenAI Agents reference wrapper. Current
> product scope and status are governed by
> [`docs/internal/README.md`](../../docs/internal/README.md).

This dependency-free TypeScript client exercises the local lean-ctx **HTTP
`/v1` contract**. It speaks the wire protocol only; it is not a stability,
browser-support, or production-integration commitment.

## Local development use

```bash
npm install lean-ctx-client
```

## Usage

```ts
import { LeanCtxClient, toolResultToText, runConformance } from "lean-ctx-client";

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
| `contextSummary({ workspaceId, … })` | `GET /v1/context/summary` |
| `searchEvents(query, { … })` | `GET /v1/events/search` |
| `eventLineage(eventId, { depth })` | `GET /v1/events/lineage` |
| `metrics()` | `GET /v1/metrics` |

## Historical conformance scaffolding

`runConformance(client)` runs the language-agnostic SDK conformance checks
against a live server and returns a scorecard. It mirrors historical server-side
conformance work; it does not establish a supported multi-language SDK matrix.

```ts
const card = await runConformance(client);
if (!card.allPassed) console.error(card.checks.filter((c) => !c.passed));
```

The kit and its route list are implementation details that may change or be
removed. Do not use them as a public compatibility guarantee.

### Historical SemVer note

Earlier implementation work coupled this package's major version to the engine
`http_mcp` contract major. That convention is not a published SDK commitment.

## Non-goals

- No engine linkage and no re-implemented compression/indexing logic.
- No public stability or compatibility commitment.
- Bring-your-own runtime: any standard `fetch` works; pass `fetchImpl` to inject.
