# Journey 17 — Beyond Coding: Web & Research

> Not every agent task is code. You want the agent to read a changelog, pull an
> API spec, summarise an RFC, or extract the claims from a blog post or a video —
> without pasting raw HTML into the context window. This journey covers
> `ctx_url_read`: one tool that turns a URL, PDF, or YouTube video into
> **compressed, citation-backed context**.

Source files referenced here:
- `rust/src/tools/registered/ctx_url_read.rs` — the MCP tool (`CtxUrlReadTool`), arg parsing + clamps
- `rust/src/core/web/mod.rs` — `read_url`, `ReadMode`, `ReadOptions`, `DEFAULT_MAX_TOKENS`/`DEFAULT_MAX_ITEMS`
- `rust/src/core/web/url_guard.rs` — SSRF guard (scheme + private/loopback/link-local block)
- `rust/src/core/web/fetch.rs` — bounded, redirect-revalidated HTTP fetch (`DEFAULT_TIMEOUT_SECS`)
- `rust/src/core/web/html_to_text.rs` — HTML → clean Markdown
- `rust/src/core/web/pdf.rs` — remote PDF → text
- `rust/src/core/web/youtube.rs` — video URL → transcript
- `rust/src/core/web/distill.rs` — research-compression modes
- `rust/src/core/web/citation.rs` — source attribution (`Citation`)
- `rust/src/core/evidence.rs` — `Claim` (confidence + source) for `facts`/`quotes`

---

## 0. The principle

> `ctx_url_read` is the web counterpart of `ctx_read`: one tool call, one token
> budget, boilerplate stripped, the source preserved for citation. Nothing is
> fetched unless you pass a URL, and only `http`/`https` URLs that survive the
> SSRF guard are ever requested.

---

## 1. The pipeline

`read_url` (`core/web/mod.rs`) is the single entry point; the MCP tool is a thin
wrapper over it. The flow:

1. **`url_guard`** validates the URL and blocks SSRF targets.
2. **`fetch`** downloads it (bounded, manual-redirect, SSRF-revalidated) — or
   **`youtube`** pulls a transcript for video URLs.
3. **`html_to_text`** renders HTML to clean Markdown (and **`pdf`** converts a
   remote PDF to text).
4. **`distill`** applies the requested research-compression mode.
5. **`citation`** attaches source attribution.

---

## 2. The tool surface

`CtxUrlReadTool::handle` (`ctx_url_read.rs`) parses the arguments, clamps them,
and calls `web::read_url`.

| Argument | Type | Default | Clamp | Code |
|----------|------|---------|-------|------|
| `url` | string | — (required) | — | `get_str(args, "url")` |
| `mode` | string | `auto` | enum | `ReadMode::parse` |
| `query` | string | — | — | `get_str(args, "query")` |
| `max_tokens` | integer | `6000` | `200..=50_000` | `DEFAULT_MAX_TOKENS` |
| `max_items` | integer | `12` | `1..=100` | `DEFAULT_MAX_ITEMS` |
| `timeout_secs` | integer | `20` | `1..=60` | `fetch::DEFAULT_TIMEOUT_SECS` |

---

## 3. Distillation modes

`ReadMode` (`core/web/mod.rs`) selects how fetched content is distilled before it
is returned. `distill.rs` implements the extractive, relevance-ranked logic.

| Mode | What you get | Code path |
|------|--------------|-----------|
| `auto` | Markdown for pages, transcript for videos (default) | `ReadMode::Auto` |
| `markdown` | Clean Markdown of the main content | `html_to_text` |
| `text` | Plain text (Markdown decorations stripped) | `distill` |
| `links` | Extracted hyperlinks (max 100) | `MAX_LINKS` |
| `facts` | Sentences carrying factual signals, as `Claim`s | `distill` + `evidence::Claim` |
| `quotes` | Central / query-relevant sentences as evidence | `distill` + `evidence::Claim` |
| `transcript` | De-duplicated, filler-stripped transcript | `youtube` + `transcript_compact` |

`mode` parsing accepts a few aliases: `md`→markdown, `plain`→text, `summary`→transcript.

```bash
# Auto mode — Markdown for a page
ctx_url_read url="https://example.com/post"

# A remote PDF as text within a 3000-token budget
ctx_url_read url="https://example.com/paper.pdf" mode="text" max_tokens=3000

# A YouTube transcript
ctx_url_read url="https://youtu.be/VIDEO" mode="transcript"
```

---

## 4. Citations & evidence

The `facts` and `quotes` modes do not just summarise: each returned item is a
`Claim` (`core/evidence.rs`) carrying a **confidence score** and the **source
URL** it came from (`citation.rs`). That makes web research auditable — the agent
can attribute every statement, and you can verify it later. A `query` boosts
relevance so extraction focuses on the part of the page you care about.

```bash
ctx_url_read url="https://example.com/spec" mode="facts" query="rate limits and quotas"
```

---

## 5. Research compression

A single documentation page can blow a context window. `read_url` distils the
fetched content down to `max_tokens` (default `DEFAULT_MAX_TOKENS` = 6000) using
extractive, relevance-ranked compression, and caps `facts`/`quotes` at
`max_items` (default `DEFAULT_MAX_ITEMS` = 12). The tool then appends the usual
savings line (`append_savings`) so the token budget is visible.

---

## 6. Safety — the SSRF guard

`url_guard.rs` enforces, before any request and again after each redirect in
`fetch.rs`:

- only `http` / `https` schemes are allowed;
- requests to **private, loopback and link-local** addresses are blocked.

So an agent cannot be steered into probing your internal network. Fetches are
bounded in size and honour `timeout_secs` (default 20, max 60).

---

## 7. Where it fits

`ctx_url_read` ships with the binary and is registered in
`rust/crates/lean-ctx-server/src/server/registry.rs`, so it is exposed automatically wherever lean-ctx
runs as an MCP server — no extra configuration. Pair it with
`ctx_knowledge` to remember what you learned, and it becomes a durable research
loop that survives the session.
