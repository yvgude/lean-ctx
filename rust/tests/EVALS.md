# Evaluation / Quality Gates (local-first)

This folder contains **small, deterministic** evaluation tests that act as CI quality gates.

## What’s covered

- **Graph-driven context**: `core::graph_context` must include the expected related files for a minimal fixture project.
- **Knowledge embeddings**: `core::knowledge_embedding` semantic search must return the expected top hits (feature-gated).

## Where the gates live

- `p2_graph_embeddings_eval.rs`

## How to extend fixtures (rules)

- **Deterministic**: avoid timestamps in assertions; don’t depend on OS-specific paths.
- **Small**: keep fixture projects to a handful of files and minimal content.
- **Local-first**: do not require network access or external services.
- **Must-include assertions**: prefer “top‑k contains X” over brittle full ordering checks.
- **Isolation**: set `LEAN_CTX_DATA_DIR` to a temp directory inside the test (and unset afterwards).

## Adding a new gate

Add a new `#[test]` in `p2_graph_embeddings_eval.rs`:

- Create a tiny fixture project in a temp dir.
- Build/run the component under test.
- Assert a bounded invariant (e.g. “must include file A” / “top hit must be key=K”).

