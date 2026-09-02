# LeanCTX and claude-context

> **Status: historical comparison note — not canonical product copy.** Current
> LeanCTX scope and status are governed by
> `docs/internal/README.md` (internal, not in this repository). This note makes no
> universal privacy, latency, retrieval-quality, or feature-coverage claim.

The tools can be assessed as different ways to help an agent find project
material. LeanCTX's current scope is the local context Runtime around existing
coding agents: selection, shaping, reuse, recovery, and local inspection before
inference.

## Evaluation boundary

Compare data handling, deployment requirements, source coverage, and retrieval
quality in the environment you intend to run. Earlier implementation comparisons
and numerical claims are retained only as historical context and must not be
read as product guarantees.

The public boundary is deliberately narrow: LeanCTX is **The Context SDK for AI
Agents**, not a hosted semantic-search product or general agent platform. See
the internal Product Architecture (`docs/internal/vision/PRODUCT-ARCHITECTURE.md`, internal — not in this repository).
