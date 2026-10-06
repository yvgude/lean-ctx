# Graph Reproducibility Contract v1

GitLab: `#2318`

## Goal

Make graph-driven tooling **reproducible** (CI/proofs), **deterministic** (stable ordering), **bounded** (output caps), and **safe-by-default**.

This contract covers:

- Property graph storage under `$LEAN_CTX_DATA_DIR/graphs/<project_hash>/graph.db`
- Graph build + freshness semantics (`ctx_impact action=build|status`)
- Deterministic exports from `ctx_impact` and `ctx_architecture` (incl. `format=json`)
- Architecture proof artifacts exported via `ctx_proof`

## Version (SSOT)

- `leanctx.contract.graph_reproducibility_v1.schema_version=1`
  - SSOT: `CONTRACTS.md`
  - Runtime: `rust/src/core/contracts.rs`

## Build triggers & freshness

### Auto-build (zero-config)

- Tools **may** auto-build the property graph when it is missing/empty:
  - `ctx_impact action=analyze` (auto-build if empty)
  - `ctx_architecture action=overview|...` (auto-build if empty)

### Explicit rebuild

- `ctx_impact action=build` is the authoritative rebuild:
  - Clears and rebuilds `graph.db` (in `$LEAN_CTX_DATA_DIR/graphs/<project_hash>/`)
  - Writes `graph.meta.json` alongside the database

### Freshness check

- `ctx_impact action=status` reports whether the graph looks **fresh** or **stale**.
- Staleness is determined by comparing build metadata (git head/dirty) to current repo state when available.

### Property-graph mirror replacement

The index-to-property-graph mirror clears and replaces code nodes, edges and
the file catalog in one SQLite transaction. A failed clear, insert or commit
retains the previously committed graph; readers do not observe the temporary
empty state between removal and insertion. Provider cross-source edges are
preserved. Failure to begin the transaction has no graph side effects.

## Determinism (MUST)

Same repo snapshot + same policies ⇒ same **logical outputs** (stable ordering + stable truncation).

Rules:

- File enumeration during `build` is **sorted** lexicographically (relative paths).
- Resolved import targets are **sorted + deduplicated** before inserting edges.
- Traversal adjacency lists are **sorted + deduplicated** (BFS/DFS becomes deterministic).
- Output lists are **stable-sorted**, then **truncated** with explicit `truncated` markers.

## Output format (tools)

Both tools accept:

- `format=text|json` (default: `text`)

When `format=json`:

- Output is machine-readable JSON (no token-suffix lines).
- Payload includes:
  - `schema_version` (Graph Reproducibility Contract v1)
  - `tool`, `action`
  - `project.project_root_hash` + `project.project_identity_hash` (hash-only; never leak identity strings)
  - `graph` summary (exists, nodes, edges, db_path)
  - action-specific fields + `truncated` flags

## Proof artifacts (architecture overview)

When exporting proofs with `ctx_proof write=true`, the runtime also exports:

- `architecture-overview-v1_<ts>.json`
- `architecture-overview-v1_<ts>.html`

to `.lean-ctx/proofs/` (redacted-by-default for CI attachment safety).

## Boundedness (MUST)

Graph tool outputs are capped by hard budgets (see `rust/src/core/budgets.rs`) to prevent DoS/token-burn.

## Security & privacy

- Graph DB + meta are written **only** under the project’s `.lean-ctx/` directory.
- Proof artifacts are redacted before writing (same safety policy as other proof exports).
- No secrets must appear in graph artifacts, logs, or exports.
