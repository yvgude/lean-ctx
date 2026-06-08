# Format Extractors & Chunkers — `extractors-v1`

Status: stable · EPIC 12.13 · Code: [`rust/src/core/extractors/`](../../rust/src/core/extractors/)

The front-door that turns a non-code **document/data** file into clean LLM text
plus **structure-aware** chunks. It complements `ingestion-v1` (which decides
*whether* to index a path) by deciding *how* to read a given format. This is
what lets the Context OS index a corpus of emails, spreadsheets, web pages, and
reports — not just source code.

## Dispatch

`extractors::extract(path, bytes) -> Extracted { kind, text, chunks }` selects an
extractor by file extension:

| Extension(s) | `kind` | Extractor | Chunking strategy |
|--------------|--------|-----------|-------------------|
| `.json` `.jsonl` `.ndjson` | `json` | [`json`](../../rust/src/core/extractors/json.rs) | one chunk per top-level array element / object entry |
| `.csv` `.tsv` | `csv` | [`csv`](../../rust/src/core/extractors/csv.rs) | header-prefixed row groups (RFC-4180 quoting) |
| `.eml` | `eml` | [`eml`](../../rust/src/core/extractors/eml.rs) | salient-header summary + body paragraphs; `text/plain` parts of multipart |
| `.html` `.htm` `.xhtml` | `html` | `web::html_to_text` | paragraphs of rendered Markdown |
| `.pdf` | `pdf` | `web::pdf` | paragraphs of extracted text |
| anything else | `text` | verbatim | paragraphs (blank-line split) |

## Invariants

Every extractor is **total** and **graceful** — required because input is
arbitrary (agent-supplied, possibly malformed):

1. **Never panics** on any byte input (PDF parsing is `catch_unwind`-guarded).
2. **Empty input ⇒ no chunks.**
3. **Non-empty input ⇒ ≥1 non-empty chunk** (invalid JSON/CSV/EML degrades to a
   single text chunk rather than dropping content).
4. **Deterministic** — identical input yields identical output.

These are the same invariants the conformance suite (`conformance-v1`) enforces.

## Registry integration

The text-based chunkers (`csv`, `json`, `eml`, `html`) register into the
[`extension-registry-v1`](./capabilities-contract-v1.md) through the same public
API extensions use, so they:

* appear under `extensions.chunkers` in `GET /v1/capabilities`, and
* are exercised by `lean-ctx conformance` on every build.

PDF is byte-only (binary input) and is reached via `extract()`, not the
text→chunks registry.

## Versioning

`extractors-v1` is additive — new formats / extensions may be added in a minor
revision. Changing an existing format's `kind` tag or chunking contract is a
breaking change requiring `-v2`.
