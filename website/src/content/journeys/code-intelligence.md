You're exploring or refactoring an unfamiliar codebase. This journey covers the tools that build a graph of your code and answer structural questions: what calls this? what breaks if I change it? what are the most important files?

## The graph underneath everything

All code-intelligence features read from one **property graph** of your repo:
files, symbols (functions/types), and the edges between them (imports, calls,
references). It's built with tree-sitter (18 languages) and stored at
`graphs/<project-hash>/index.json.zst`.

You usually don't build it by hand — it builds lazily on first use and updates in
the background. To build explicitly:

```bash
lean-ctx graph build              # build/refresh the graph
lean-ctx index build-graph        # same, via the index command
lean-ctx graph status             # is it built? how big?
```

## "What's connected to this?" — `lean-ctx graph` / `ctx_graph`

```bash
lean-ctx graph related src/auth.rs        # neighbors in the graph
lean-ctx graph symbol "validate_token"    # find + describe a symbol
lean-ctx graph context "login flow"       # graph-driven context for a query
lean-ctx graph export-html --out graph.html
```

`ctx_graph` (MCP) actions: `build`, `related`, `symbol`, `impact`, `context`,
`diagram`, `status`, `enrich`. It's the unified entry point; the more focused
tools below are specializations.

## "What breaks if I change this?" — `ctx_impact` / `lean-ctx graph impact`

**Blast-radius analysis.** Given a file or symbol, it returns everything
transitively affected.

```bash
lean-ctx graph impact src/auth/verify.rs
```

`ctx_impact` (MCP, in the **standard** profile) actions: `analyze`, `diff`
(impact of a working-tree diff), `chain` (the dependency chain), plus
`build`/`update`/`status`. This is the tool to call before a risky refactor.

## "Who calls this?" — `ctx_callgraph`

```text
ctx_callgraph action=callers symbol=validate_token
ctx_callgraph action=callees symbol=handle_login
ctx_callgraph action=trace from=main to=db_connect
ctx_callgraph action=risk symbol=validate_token
```

Breadth-first search over call edges. `risk` scores how dangerous a symbol is to
change based on fan-in/fan-out. In the **standard** profile.

## "What matters most here?" — `ctx_repomap`

**PageRank over the symbol graph.** Returns the most important symbols/files
within a token budget — the fastest way for an AI to understand a new repo.

```text
ctx_repomap max_tokens=2000
ctx_repomap focus_files=["src/auth.rs"]
```

In the **standard** profile. There is no `repomap` CLI command — it's an MCP tool
only. (CLI users get a similar view via `lean-ctx overview`.)

## Architecture & health — `ctx_architecture`

```text
ctx_architecture action=overview        # layers, clusters at a glance
ctx_architecture action=cycles          # dependency cycles
ctx_architecture action=hotspots        # high-churn / high-coupling spots
ctx_architecture action=health          # an overall score
ctx_architecture action=entrypoints     # where execution starts
```

Community detection and layering over the property graph. In the **standard**
profile.

## Code smells — `lean-ctx smells` / `ctx_smells`

Eight rules over the graph (god objects, long functions, deep nesting, etc.).

```bash
lean-ctx smells summary          # counts by rule
lean-ctx smells scan             # all findings
lean-ctx smells rules            # what the 8 rules are
lean-ctx smells file src/big.rs  # findings for one file
```

`ctx_review` (MCP) goes further: an automated review combining impact, callers,
test coverage, and smells (`review`, `diff-review`, `checklist`).

## Refactoring — `ctx_refactor`

LSP-backed, so it's rename-safe across the project.

```text
ctx_refactor action=rename path=src/auth.rs line=42 new_name=verify_jwt
ctx_refactor action=references path=src/auth.rs line=42
ctx_refactor action=definition path=src/main.rs line=10
```

In the **standard** profile. Requires the relevant language server; configure
binaries under the `[lsp]` config map if auto-detection misses one.

## Seeing it — `lean-ctx visualize` / `heatmap`

```bash
lean-ctx visualize --open            # interactive D3 HTML report
lean-ctx heatmap --top 20            # hottest files by access
lean-ctx heatmap --by connections    # rank by graph connectivity
```

`visualize` renders the graph; `heatmap` shows which files get touched most,
useful for spotting where attention — and risk — concentrates.

## Index utilities — `lean-ctx index`

```bash
lean-ctx index status            # what's indexed
lean-ctx index build             # build the search index
lean-ctx index build-full        # full reindex
lean-ctx index build-graph       # (re)build the property graph
lean-ctx index watch             # keep it fresh on file changes
```

**Golden output — `lean-ctx index status`** shows each index, its readiness,
size, and build timestamp, so you can tell at a glance whether search and the
graph are current:

```text
  Project:     /Users/you/dev/lean-ctx
  Graph Index: (ready, 896 files, 407.3 KB, built 2026-05-30 12:10:48 UTC)
  BM25 Index:  (ready, 2.7 MB, built 2026-05-30 12:10:50 UTC)
  Code Graph:  (ready, 0 nodes, 1.3 MB, built 2026-05-31 14:49:12 UTC)
  Semantic:    idle
```

`Semantic: idle` means the optional embedding backend is not running — BM25 +
graph still work. Index scanning can be disabled with `LEAN_CTX_NO_INDEX=1` /
`LEAN_CTX_DISABLE_SEARCH_INDEX=1`, and bounded with `graph_index_max_files`.
