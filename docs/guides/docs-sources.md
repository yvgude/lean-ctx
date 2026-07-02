# Doc Corpora — notes, wikis and PDFs as retrieval sources

Your agent's context isn't only code. Runbooks, ADRs, meeting notes, an
Obsidian vault, a folder of PDF specs — lean-ctx indexes them as a **document
corpus** next to the code index, searchable through the same tools.

## Declare a corpus

Create `.lean-ctx-artifacts.json` in the project root and register the folders
(or single files) that matter:

```json
{
  "artifacts": [
    { "name": "docs",    "path": "docs",             "description": "Architecture docs + ADRs" },
    { "name": "runbooks","path": "ops/runbooks",     "description": "Incident runbooks" },
    { "name": "vault",   "path": "~/notes/projects", "description": "Personal Obsidian notes" }
  ]
}
```

- **Relative paths** are project-scoped — no further setup.
- **Absolute / `~` paths** may live *outside* the repo (a vault, a shared
  drive). They additionally need one allow-list entry, because PathJail rejects
  everything outside the project by default:

```toml
# ~/.config/lean-ctx/config.toml — read-only is the right grant for corpora
read_only_roots = ["~/notes/projects"]
```

(`extra_roots` grants read-write; `LEAN_CTX_ALLOW_PATH` works per-shell. A
rejected path shows up as a warning in the search output rather than failing
silently.)

## Search it

```bash
# BM25 over the doc corpus (agents: ctx_search action="semantic", artifacts=true)
lean-ctx semantic-search "key rotation policy" --artifacts
```

Doc hits are tagged `[artifact]` and fuse across linked projects the same way
code results do. Since GL#1132 the corpus walker also accepts **PDF** — text is
extracted locally (panic-safe; scanned/image-only PDFs produce a warning, not a
failure) and chunked like any Markdown file.

## What gets indexed

| Rule | Value |
|---|---|
| File types | `md`, `mdx`, `txt`, `pdf`, `json`, `yaml`, `toml`, `sql`, `proto`, `tf`, `hcl`, `rego`, `graphql`, `sh`, … |
| Size cap | 2 MB per file (larger files are skipped) |
| Chunking | content-defined (Rabin-Karp), ≤ 50 chunks per file, deterministic (#498) |
| Refresh | incremental — unchanged files are never re-read, re-index of an unchanged corpus is byte-identical |
| Secrets | `.env`-like and secret-like paths are refused by default |
| Ignore rules | honors `.gitignore` + `extra_ignore_patterns` |

## When to reach for an addon instead

The built-in corpus indexing is lexical (BM25) and tuned for repo-adjacent
docs. If your notes are the *primary* corpus and you want embeddings + LLM
reranking over them, wire [`qmd`](addons.md) (`lean-ctx addon add qmd`) — an
on-device Markdown search engine — into the gateway, and keep lean-ctx as the
layer that fuses everything.

*See also: [Context Infrastructure](context-infrastructure.md),
[Addons](addons.md), [Monorepo & linked projects](monorepo.md).*
