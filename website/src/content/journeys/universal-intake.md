Your corpus is reports (PDF), web captures (HTML), CRM exports (CSV/JSON) and a mailbox (EML) — not source code. LeanCTX historically indexed *code*, so those documents never reached the BM25, semantic and knowledge stores. Universal intake admits any corpus and picks the right reader per format, automatically.

---

## 1. Index a mixed-format directory

The ingestion front-door admits documents and data, and a format extractor picks
the right reader per file — no per-format flags:

```python
client.call_tool_text("ctx_index", {"action": "build", "project_root": "./reports"})
hits = client.call_tool_text("ctx_semantic_search", {"query": "Q3 churn drivers"})
```

## 2. One extractor per format

| Format | What the extractor does |
|---|---|
| **PDF** | local text extraction → paragraph chunks (no upload) |
| **HTML** | rendered to clean Markdown, paragraph-chunked |
| **CSV/TSV** | RFC-4180 parse (quoted fields / embedded delimiters) → labeled record chunks |
| **EML** | salient header summary (From/To/Subject/Date) + `text/plain` body, MIME stripped |
| **JSON/NDJSON** | chunked per array element / object entry |

## 3. Under the hood — `rust/src/core/ingestion.rs` + `core/extractors/`

- **Ingestion** decides *whether* a file is indexable — code, document, data or
  text — instead of only source code.
- **Extractors** decide *how* to read a format. Each text format also registers as
  a named chunker in the extension registry, so it appears in `/v1/capabilities`
  and is conformance-checked for determinism and coverage.

## Payoff

Any corpus reaches the same engine. The retrieval, knowledge and compression that
powered coding agents now power a research, support or data agent over the
documents that domain actually uses.
