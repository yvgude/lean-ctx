Most agent workflows are not pure coding. You ask the agent to read a changelog, check an API spec, summarise an RFC, or pull the key points out of a long video. LeanCTX makes those "beyond coding" tasks first-class with one tool — `ctx_url_read` — that fetches a public web page, PDF, or YouTube video and returns it as **compressed, citation-backed context** instead of raw HTML pasted into the window.

## Read a page, get clean text

The default `auto` mode extracts the main article, drops navigation and ads, and returns clean Markdown within a token budget:

```bash
ctx_url_read url="https://example.com/blog/whats-new"
```

You get the content the page is actually about — not its chrome — already distilled to fit the context window.

## Extract claims you can cite

For research, raw prose is rarely what you want. The `facts` and `quotes` modes return discrete claims, each with a **confidence score** and the **source URL** it came from, so the agent can attribute every statement and you can verify it later. Add a `query` to focus extraction:

```bash
ctx_url_read url="https://example.com/api/docs" mode="facts" query="rate limits and quotas"
```

Each returned claim carries its source, which is exactly what you need when the agent writes a summary, a comparison, or a decision memo that has to be defensible.

## PDFs and YouTube

The same tool handles more than HTML:

```bash
# A remote PDF (spec, paper, datasheet) → text
ctx_url_read url="https://example.com/paper.pdf" mode="text"

# A YouTube video → flattened transcript
ctx_url_read url="https://youtu.be/VIDEO" mode="transcript"
```

A talk, a tutorial, or a recorded standup becomes quotable text the agent can reason over.

## Keep it in budget

A single documentation page can blow the context window. `ctx_url_read` distils the fetched content down to `max_tokens` (default 6000) with relevance-ranked, extractive compression, and `max_items` caps how many claims `facts`/`quotes` return (default 12):

```bash
ctx_url_read url="https://example.com/very-long-guide" max_tokens=3000 query="authentication"
```

You spend tokens on the part that answers the question, not the whole page.

## Make it stick

Pair web research with memory so findings survive the session. Pull the facts, then remember the ones that matter:

```bash
ctx_url_read url="https://example.com/spec" mode="facts" query="breaking changes"
ctx_knowledge action=remember category=research content="v5 removes the legacy auth header — source: example.com/spec"
```

Now the next session recalls what you learned without re-fetching.

## Safety

Fetching is SSRF-guarded: only `http`/`https` URLs are allowed, and requests to private, loopback and link-local addresses are blocked, so an agent cannot be steered into probing your internal network. Requests honour a timeout (`timeout_secs`, default 20, max 60).

## Setup

`ctx_url_read` ships with the binary and is exposed automatically wherever LeanCTX runs as an MCP server — no extra configuration. If your agent is connected via the [standard setup](/docs/getting-started/), the tool is already there; just call it. The full reference, every mode and more examples live on the [Web & Research](/docs/concepts/web-research/) page.
