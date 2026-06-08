# LeanCTX Vision

## The Cognitive Context Layer

In 2026, we've moved past "just sending a prompt." High performance with LLMs isn't about bigger context windows — it's about **Information Density**.

LeanCTX is the **Cognitive Context Layer** between your AI and your code — a cognitive filter and intelligence buffer in one. Not a proxy. Not a wrapper. It ensures every token reaching the LLM carries maximum signal. Every byte of noise stripped away is a byte of reasoning gained.

> The winners won't be those who can afford 1M token contexts.
> They'll be those who achieve the same result with 10K.

## The Four Dimensions

Building a high-performance LLM layer requires mastering four dimensions:

### 1. Compression Layer (Input Efficiency)

Sending 100% of the data dilutes the model's attention mechanism. Boilerplate, redundant re-reads, verbose CLI output — it all competes with the actual signal.

LeanCTX solves this with:
- **AST-Based Signatures** — Send the skeleton (classes, methods, types), not the flesh.
- **Delta-Loading** — Only transmit what changed since the last turn.
- **Session Caching** — Re-reads cost 13 tokens instead of thousands.
- **Token Shorthand (TDD)** — Replace verbose grammar with logical symbols to free up thinking tokens.
- **Entropy Filtering** — Shannon entropy analysis removes lines that carry no unique information.
- **95+ CLI Patterns** — Pattern-matched compression for every common dev tool.

### 2. Semantic Router (Model Selection)

Not every query needs a $15/1M token model. The future is tiered:
- **Intent Detection** — Is this a "What" (retrieval) or a "How" (reasoning) question?
- **Tiered Routing** — Simple lookups go to fast models. Complex architectural shifts go to the "Big Brain."
- **Mode Selection** — `map` mode for understanding, `signatures` for API surface, `full` for editing, `entropy` for noisy files.

LeanCTX already implements this at the file level: 10 read modes (full, map, signatures, diff, aggressive, entropy, task, reference, lines, auto) + optional line ranges let the model — or the user — choose the right fidelity for each task. An adaptive `ModePredictor` learns optimal modes per file type from past sessions.

### 3. Context Manager (Memory Architecture)

Performance drops when the context window is cluttered with irrelevant history.

LeanCTX manages this through:
- **Session Cache with Auto-TTL** — Files tracked, diffs computed, stale entries purged.
- **Context Checkpoints** — `ctx_compress` creates ultra-compact state summaries when conversations grow long.
- **Subagent Isolation** — `fresh` parameter and `ctx_cache clear` prevent stale cache hits when new agents spawn.
- **Sliding Window** — The model sees the latest state, not the full history of every read.

### 4. Quality Guardrail (Output Verification)

Performance isn't just speed; it's accuracy. When the model receives focused, high-entropy input:
- **Reasoning improves** — Less noise means more attention on logic nodes.
- **Deterministic anchoring** — Compressed outputs preserve exact paths, variable names, and error locations.
- **Self-correction becomes cheaper** — With lower token budgets consumed by input, more budget remains for iterative refinement.

## The Hidden Metric: Entropy

The goal is to maximize **Information Entropy per token**.

If you send `function`, you've used 1 token to convey almost zero unique information — the AI already knows it's a function. If you send `λ`, you've used 1 token to convey a specific logical operation while saving space for the *actual* logic.

LeanCTX is a **Lossless Minifier for Human Thought**.

## Brute Force vs. Cognitive Filter

| Dimension | Brute Force (Standard) | Cognitive Filter (LeanCTX) |
|---|---|---|
| **Data Sent** | Full files, raw logs, all history | AST signatures, diffs, state summaries |
| **Latency** | High (large input ingestion) | Low (minimal token processing) |
| **Reasoning** | Distracted by boilerplate | Focused on logic nodes |
| **Cost** | Linear (expensive) | Logarithmic (high ROI) |
| **Context Lifespan** | Burns through window fast | Extends effective session length |

## Where We Are (v3.7.0)

LeanCTX delivers strong coverage of Dimensions 1 and 3, with foundations for 2 and 4:

### Dimension 1 — Compression Layer (Production-Ready)
- 95+ CLI compression patterns for git, npm, docker, kubectl, cargo, and more
- 18 tree-sitter languages for AST-based signatures and outlines
- 72 MCP tools with 10 read modes (full, map, signatures, diff, aggressive, entropy, task, reference, lines, auto)
- Session caching with mtime-validation — re-reads cost ~13 tokens
- Cross-file codebook compression and archive system (`ctx_expand`)

### Dimension 2 — Semantic Router (Foundations)
- `ModePredictor` learns optimal read modes per file type from session history
- `IntentEngine` classifies query complexity for mode selection
- LITM-aware positioning per model family (Claude α=0.9, GPT, Gemini profiles)
- Thompson Sampling bandits for exploration vs. exploitation

### Dimension 3 — Context Manager (Production-Ready)
- **Context Continuity Protocol (CCP)** — cross-session memory (~400 tokens vs ~50K cold start)
- **Session Survival Engine** — structured recovery queries survive context compaction (executable `ctx_read`/`ctx_search` commands, knowledge recall hints, graph dependency clusters)
- **Context Ledger** — 128K window tracking with pressure signals
- **Multi-Agent Coordination** — `ctx_agent`, `ctx_share`, handoff packages, agent diaries
- **Knowledge System** — temporal facts, contradiction detection, memory lifecycle, consolidation
- **Property Graph** — SQLite-backed code graph with multi-edge BFS (imports, calls, exports, type_ref, tested_by), weighted scoring, incremental git-diff updates
- **Graph-Aware Reads** — every file read includes scored related files from the Property Graph
- **Hybrid Search Fusion** — Reciprocal Rank Fusion combines BM25 + semantic embeddings + graph proximity
- **Configurable Embedding Model** — select the local ONNX model via `[embedding].model` in `config.toml` or the `LEAN_CTX_EMBEDDING_MODEL` env var: all-MiniLM-L6-v2 (384d, default), jina-embeddings-v2-base-code (768d, code-optimized), or nomic-embed-text-v1.5 (768d); switching re-indexes once automatically

### Dimension 4 — Quality Guardrail (Production-Ready)
- Compression safety levels per command (verbatim/minimal/standard/aggressive)
- Deterministic anchoring preserves paths, symbols, and error locations
- **Progressive Search Throttling** — escalating hints for repeated searches (knowledge consolidation)
- **Sandbox-First Routing** — large outputs (>5KB) trigger efficiency hints
- **Terse Mode** — configurable concise response mode surviving compaction
- Full output verification layer with 19 versioned contracts and CI drift gates

### Platform Coverage
- Works with 30+ AI tools: Cursor, Claude Code, Copilot, Windsurf, Pi, Codex, Gemini, Kiro, Cline, JetBrains, Amp, Crush, Antigravity, OpenCode, OpenClaw, Hermes, and more
- Single Rust binary, zero telemetry, local-first

## Where We're Going

LeanCTX is evolving from a single context layer into a full **cognitive context layer** for everything between code/infrastructure and LLMs.

### Strategic Leaps

1. **Context as Code** — Declarative pipelines, profiles, and policies in TOML. Teams version-control their context strategies like infrastructure.
2. **Unified Context Graph** — Code, tests, commits, CI runs, and knowledge entries in a single semantic graph. Multi-edge BFS, graph-aware reads, and RRF search fusion already bridge graph + knowledge + session into a fused context layer.
3. **Agent Harness** — Roles, budgets, and policies for multi-agent governance. Token limits, cost caps, and tool permissions per agent role.
4. **Context Observability** — SLOs on context consumption, anomaly detection, session diffing, OpenTelemetry/Prometheus export.

### Cognition Interface (Production-realistic)

You can’t “change weights” of proprietary API LLMs. The production-realistic leap is to control the model’s *effective* cognition at inference time:

- **Constraints-aware compilation**: the same policy/profile compiles into client-safe instruction blocks (caps, approval models, hooks).  
  Evidence: `docs/integrations/client-constraints-matrix-v1.md`, `rust/src/core/instruction_compiler.rs`, `lean-ctx instructions --client <id> --profile <name>`.
- **Attention-aware layout**: optimize positioning and ordering so the model spends tokens on signal, not boilerplate.  
  Evidence: `rust/src/core/litm.rs`, `rust/src/core/neural/context_reorder.rs`.
- **Budget & SLO enforcement**: deterministic warn/throttle/block policies across MCP + HTTP surfaces.  
  Evidence: `rust/src/core/budget_tracker.rs`, `rust/src/core/budgets.rs`, `rust/src/core/slo.rs`.
- **Proof-carrying context**: verification checks + CI gates prevent drift and regressions.  
  Evidence: `rust/src/core/output_verification.rs`, `CONTRACTS.md`, `rust/tests/*_up_to_date.rs`.

See also: `docs/cognition-interface.md`.

### Remaining Future Directions

- **Full Semantic Routing** — Intent-based model tier recommendations (What/How/Do classification)
- **Output Verification** — Post-processing layer for accuracy guarantees
- **Adaptive ML Compression** — ML-driven entropy thresholds per language and project
- **Custom Embedding Models** — built-in model selection ships today (`[embedding].model`); next is loading arbitrary HuggingFace transformer models (custom repo + dimensions) and evaluating static-embedding engines such as model2vec / `potion-code-16M`. Embeddings run on CPU via the pure-Rust `rten` runtime (no GPU execution providers), so GPU/device selection is intentionally out of scope. Tracked in [#328](https://github.com/yvgude/lean-ctx/issues/328).
- **IDE Extensions** — Context HUD, profile switcher, context issues panel
- **Context Provider Framework** — Structured providers for Jira, GitHub Issues, CI/CD, logs

The end state: an AI that sees only what matters, remembers what's relevant, and reasons at maximum capacity — governed by policies you define.

**Tokens are the new gold. Context is the new infrastructure. Spend both wisely.**
