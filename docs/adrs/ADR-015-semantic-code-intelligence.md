# ADR-015: Evidence-Aware Semantic Code Intelligence

**Status:** Accepted
**Date:** 2026-10-02
**Authors:** Architecture Team

## Context

lean-ctx builds its code graph from tree-sitter. Tree-sitter is fast, local,
dependency-free and works on broken code, but it only sees syntax: it extracts
a call to `save`, not *which* `save`. Calls were bound to definitions by name
within the caller's scope (same file → unique import → unique project-wide);
anything else was dropped. Two producers had additionally been guessing:
graph enrichment mapped names to files last-wins, and the `ctx_impact` index
picked the alphabetically first definition. Every guess became a `calls` edge
that ranking, impact analysis and context selection treated as fact.

Separately, `ctx_refactor` already talked to local language servers and a live
JetBrains IDE through `lsp::router`, but that semantic knowledge never reached
the graph.

## Decision

### 1. Tree-sitter stays the always-available baseline

Nothing requires a language server. Without one, the graph is the structural
graph — minus the former guesses.

### 2. Edges carry typed evidence

`Edge.metadata` holds `EdgeEvidence { v, grade, by: [Contribution] }`, one
contribution `{ origin, grade, backend, sites }` per producer currently
deriving the edge; `grade` is the strongest:

| Grade | Meaning |
|---|---|
| `verified_semantic` | a local language server / IDE resolved the target |
| `resolved_structural` | bound by the caller's own scope (same file, unique import) |
| `heuristic_structural` | name unique in the project, but not in the caller's scope |

Ranking and impact weight an edge by `kind weight × grade factor` (heuristic
0.5, otherwise 1.0); edges without evidence keep their full weight. No
timestamps, no source text: metadata stays deterministic.

Each producer (graph enrichment, `ctx_impact` index) replaces or withdraws
only its own contribution; an edge is deleted only when no producer derives
it any more, so one producer can never downgrade or erase another's evidence.

A producer withdraws an edge only when its pass *settled* the question: an
unanswered site (backend unavailable or busy, budget exhausted, timeout, cold
server) is not evidence against an earlier verified edge. A file that no
longer declares any trait/interface is settled, so edges into it are
withdrawn. Read-merge-write of evidence runs under `BEGIN IMMEDIATE`, so two
processes updating the same edge cannot lose a contribution.

### 3. Escalate only uncertainty

Only call sites structure cannot bind — an ambiguous name, a name merely unique
project-wide, or a path call (`db::save`) — are sent to the semantic backend,
at the callee identifier's exact position (tree-sitter byte column encoded per
the negotiated LSP position encoding, UTF-8 preferred). Scope-bound calls and
names with no project definition are never queried.

A verified target binds the edge; a definition outside the indexed project
vetoes a structural guess. "No definition found" is **not** evidence — a cold
server answers it while indexing — so it is neither cached nor used as a veto.

### 4. Cache definitive answers, validate on reuse

Answers are cached per call site in the property graph. A row is reused only
while (a) the caller file's content hash is unchanged, (b) its context
fingerprint matches — the files defining the callee's name, their content
hashes and the root dependency manifests/lockfiles for definitions (a glob
import can gain a dependency symbol), the whole project revision for implementations
(implementors can change anywhere), (c) no *different* backend or server
version is live (a busy backend defers reuse instead of vouching for it), and
(d) for a resolved target, the recorded definition line still lies inside a
symbol of that name. Transient failures and "no result" are never cached.

Opportunistic queries (background, `ctx_callgraph`) never wait for a backend
busy with an interactive call, cap each request (5 s background, 2 s
interactive; also for the JetBrains bridge), and work against one absolute
deadline: a server start-up gets what is left, the request gets what is left
*after* it, and a server that never finished `initialize` is killed at once.
So they can neither hold a backend long nor overrun. Checking the live
backend's identity is a pure registry peek.

`GRAPH_ENGINE_VERSION` 6 forces existing property graphs — which may hold
unannotated, guessed `calls` edges — to rebuild once; until then an outdated
graph is never served (the index extractor bridges the gap).

### 5. `semantic_mode` decides who may start a server

| Mode | Background enrichment | Interactive (`ctx_callgraph`) |
|---|---|---|
| `off` | never queries | never queries |
| `auto` (default) | only when a backend is already live for the project; never starts one | uses live backends only |
| `eager` | may start the project's language servers (trusted workspaces) | may start them (trusted workspaces) |

`ctx_refactor` keeps starting a server on demand, as before. Starting a
language server runs project code (build scripts, proc macros), so `eager`
applies only to a trusted workspace (`lean-ctx trust`) — whether it comes from
the global config, `LEAN_CTX_SEMANTIC_MODE`, or the project — and an untrusted
repository runs as `auto`; its own `.lean-ctx.toml` may lower the mode but not
raise it. The mode, its trust check, and backend selection are evaluated for
the project being processed, not for the process's working directory.

In `auto` the graph build usually runs in the daemon, while the live server
lives in the MCP process that `ctx_refactor` started it in. So the router
itself schedules a background pass whenever a backend is used in a process:
at most one per project every 5 minutes, only for a current graph (otherwise
retried on a use 30 s later), and only after the call using the server has
released it.

### 6. One backend per (project root, language)

The router keys backends by normalized project root and language, holds its
registry lock only for lookup/insert/evict, gives each backend its own lock
(single-flight start), evicts dead backends, shuts idle ones down after the
memory profile's TTL, and scrubs credential variables (`*_TOKEN`,
`*_SECRET`, `*_API_KEY`, …) from the server environment. `SSH_AUTH_SOCK` is
kept on purpose: a server running project code executes as the user and can
reach the agent anyway, while removing it breaks `cargo metadata` / `go list`
for private git dependencies — the scrub prevents reusable secrets from
leaking into server logs, it is not process isolation. Capabilities and server identity come from the `initialize`
handshake instead of being assumed; documents are opened once and updated
with `didChange`.

Which server serves a language is resolved per project. For TypeScript the
project's own `typescript` package decides: ≤ 6 ships `tsserver.js`, driven by
`typescript-language-server`; ≥ 7 (the native port) has no `tsserver.js`, and
its `tsc --lsp --stdio` is the server. Without a project TypeScript the
machine-wide install is used — `typescript-language-server` with the
`tsserver.js` installed beside it (passed as `tsserver.path`, since the server
does not look there itself), else a TypeScript ≥ 7 `tsc` on `PATH`. Versions
are read from `package.json`; nothing is executed to decide. npm's Windows
`.cmd` shims are not executables, so such a server starts as `node <the
package's bin entry>`, as the shim would. Doctor and the coverage surfaces
report the same resolution, including a binary configured as
`[lsp] <language> = "<path>"`.

### 7. `implements` edges

For each trait/interface the backend's `textDocument/implementation` yields
`implementor file → declaring file` edges (verified, budget-bounded, cached).
`references` and type-hierarchy edges are deliberately not stored: they would
duplicate import/type-ref connectivity at a large storage cost without a
consumer that needs them.

## Consequences

- Fewer false edges with or without a language server; more precise edges
  with one. Ranking counts a relationship once per kind at its strongest
  evidence instead of summing file- and symbol-level duplicates.
- Background cost is zero unless a backend is live (`auto`) and otherwise
  bounded per run (200 definition lookups / 20 s, 100 implementation lookups /
  10 s).
- `ctx_graph status` and `ctx_graph enrich` report verified / resolved /
  heuristic counts; `lean-ctx doctor` reports the mode and which servers can
  actually run (a rustup proxy without the component no longer counts).
- Coverage is reported per language (verified share of the caller files'
  `calls` edges plus whether that language's server can run) in
  `ctx_graph status`, `lean-ctx doctor` and the dashboard's capability legend.
- `ctx_impact` propagates twice — over all edges and over edges that are not
  heuristic (each file pair weighted by its strongest edge of that class) —
  and lists files reachable only through name matches as `weak_files` ("name
  match only"), so a guess is never presented as a fact. Propagation is exact
  (a heavier path found later still propagates) and deterministic.

## Measured

`core::semantic::e2e_tests` (ignored; needs the servers installed) builds a
fixture per language with an ambiguous call (two same-named methods), a
decoy (a library call whose name also exists in the project) and, where the
language has them, an interface with two implementors. It asserts the
ambiguous call is verified to the right file, the decoy is vetoed, no false
edge is written, and a second pass is answered entirely from the cache.

Measured on 2026-10-02 (Apple Silicon, macOS; small fixtures, so the cold
time is server start plus first indexing, not a large-repository figure):

| Language | Server | Ambiguous call | Decoy vetoed | `implements` | False edges | Cold | Warm / query | Cached pass | Server RSS |
|---|---|---|---|---|---|---|---|---|---|
| Rust | rust-analyzer 1.97.1 | verified | yes | yes | 0 | 4.9 s | 1.5 ms | 0.3 ms | 420 MiB |
| TypeScript 7 | `tsc --lsp` (typescript-go 7.0.2) | verified | yes | yes | 0 | 0.27 s | 1.5 ms | 0.1 ms | 92 MiB |
| TypeScript 5 | typescript-language-server + project TS 5.9.3 | verified | yes | yes | 0 | 2.0 s | 6.4 ms | 0.3 ms | 448 MiB |
| Python | pylsp 1.15.0 | verified | yes | n/a | 0 | 2.4 s | 1.4 ms | 0.1 ms | 212 MiB |
| Go | gopls v0.23.0 | verified | yes | yes | 0 | 0.4 s | 1.1 ms | 0.1 ms | 186 MiB |

"Cached pass" answers every question from `semantic_resolutions` without a
server request. RSS is the server process tree. The JetBrains path is
covered by a non-ignored test against a local fake bridge (port file,
`/health`, `/definition`), since no IDE runs in CI.

## Not built

No bundled or auto-installed language servers, no external or LLM-based
resolution, no separate semantic graph, no synchronous whole-repo semantic
indexing at startup, no editor bridge (VS Code/Cursor) — the last is a
separate decision once the core has proven itself.
