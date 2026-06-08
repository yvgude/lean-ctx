# Non-Coding Cookbook

Concrete recipes for building **non-coding** agents on lean-ctx. Each uses real,
shipped features — personas, extractors, SDKs, and adapters — no mocks.

Prerequisites: a running server (`lean-ctx serve` or the HTTP server) and one
SDK installed. Examples use the Python SDK; the TypeScript SDK is equivalent.

```python
from leanctx import LeanCtxClient
client = LeanCtxClient("http://127.0.0.1:8080")
```

---

## Recipe 1 — Lead-generation agent

**Goal:** prospect and enrich sales leads from web pages and notes.

1. **Select the persona.** `lead-gen` exposes web/search/knowledge tools, uses
   the `prose` compressor, paragraph chunking, and a `confidential` sensitivity
   floor — set it for the process:

   ```bash
   export LEAN_CTX_PERSONA=lead-gen
   ```

2. **Ingest a prospect page.** HTML is extracted to clean Markdown and chunked
   by paragraph automatically when indexed; or read a URL via the tool:

   ```python
   text = client.call_tool_text("ctx_url_read", {"url": "https://acme.example/about"})
   ```

3. **Persist enrichment facts** so later turns recall them cheaply:

   ```python
   client.call_tool("ctx_knowledge", {
       "action": "remember", "category": "decision",
       "content": "ACME: 200 employees, Series B, CTO is the buyer."})
   ```

4. **Wire into your harness.** Expose lean-ctx tools to your LLM loop:

   ```python
   from leanctx.adapters import to_openai_tools, run_openai_tool_call
   tools = to_openai_tools(client)        # pass to your OpenAI call
   # when the model returns a tool_call:
   result_text = run_openai_tool_call(client, tool_call)
   ```

Why it works: the `prose` compressor strips scraped-page boilerplate; the
`confidential` floor keeps lead data from leaking into shareable artifacts.

---

## Recipe 2 — Research assistant with cited synthesis

**Goal:** read documents/web and synthesize findings with citations.

1. **Persona:** `research` — `map` read-mode, the **`markdown` compressor**
   (strips HTML comments, badges, and link-URL noise while keeping text), and a
   `public` sensitivity floor.

   ```bash
   export LEAN_CTX_PERSONA=research
   ```

2. **Index a corpus** of mixed formats. The ingestion front-door admits
   `.md`, `.html`, `.pdf`, `.json`, `.csv` (not just code), and the extractor
   picks the right reader per format:

   ```python
   client.call_tool_text("ctx_index", {"action": "build", "project_root": "./reports"})
   ```

3. **Search semantically** across the indexed corpus:

   ```python
   hits = client.call_tool_text("ctx_semantic_search", {"query": "Q3 churn drivers"})
   ```

4. **Synthesize** in your agent, citing the chunk sources the tools return.

Why it works: format extractors normalize PDFs/HTML into paragraphs; the
`markdown` compressor removes link/badge noise so more of the budget is signal.

---

## Recipe 3 — Customer-support triage

**Goal:** triage inbound emails and resolve from a knowledge base.

1. **Persona:** `support` — `auto` read-mode, `prose` compressor, `internal`
   floor, intents `triage/diagnose/resolve/escalate/document`.

2. **Extract the email.** `.eml` files become a salient-header summary (From/To/
   Subject/Date) plus the `text/plain` body — MIME boilerplate stripped:

   ```python
   # When indexing a mailbox dir, .eml is handled by the eml extractor.
   client.call_tool_text("ctx_index", {"action": "build", "project_root": "./tickets"})
   ```

3. **Find the resolution** in your KB and draft a reply with your LLM, using
   `ctx_semantic_search` + `ctx_knowledge` recall.

4. **Stream live updates** to a dashboard via SSE:

   ```python
   for event in client.subscribe_events():
       dashboard.push(event["kind"], event["payload"])
   ```

---

## Recipe 4 — Data-analysis pipeline

**Goal:** ingest structured data and report.

1. **Persona:** `data-analysis` — `map` read-mode, `identity` compressor
   (preserves tabular structure), `lines` chunker.

2. **Ingest CSV/JSON.** The CSV extractor parses RFC-4180 (quoted fields,
   embedded delimiters) into labeled records; JSON is chunked per element/entry:

   ```python
   client.call_tool_text("ctx_index", {"action": "build", "project_root": "./data"})
   ```

3. **Query** with `ctx_search` / `ctx_semantic_search`, then compute and report
   in your harness.

Why it works: `identity` + `lines` keep rows intact so the model reasons over
real records, not reflowed prose.

---

## Building a custom vertical

Not one of the four? Ship a persona file at `<personas_dir>/<name>.toml`:

```toml
name = "compliance"
tool_profile = "custom"
tools = ["ctx_read", "ctx_search", "ctx_semantic_search", "ctx_knowledge"]
default_read_mode = "map"
compressor = "prose"
chunker = "paragraph"
intent_taxonomy = ["scan", "flag", "cite", "report"]
sensitivity_floor = "confidential"
```

Then `export LEAN_CTX_PERSONA=compliance`. See
[`persona-spec-v1`](../contracts/persona-spec-v1.md). Add a domain tool with a
plugin manifest, or a domain compressor/chunker via the extension registry —
both surface in `/v1/capabilities` and are conformance-checked.

---

## Verifying your integration

```python
from leanctx import run_conformance
card = run_conformance(client)
assert card.all_passed, [c for c in card.checks if not c.passed]
```

And prove the savings to stakeholders:

```bash
lean-ctx savings roi --json
```
