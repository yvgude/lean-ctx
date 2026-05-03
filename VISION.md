# LeanCTX Vision

## The Cognitive Filter

In 2026, we've moved past "just sending a prompt." High performance with LLMs isn't about bigger context windows — it's about **Information Density**.

LeanCTX is the **Intelligence Buffer** between human and machine. Not a proxy. Not a wrapper. A **Cognitive Filter** that ensures every token reaching the LLM carries maximum signal. Every byte of noise stripped away is a byte of reasoning gained.

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
- **90+ CLI Patterns** — Pattern-matched compression for every common dev tool.

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

## Where We Are (v3.4.5)

LeanCTX delivers strong coverage of Dimensions 1 and 3, with foundations for 2 and 4:

### Dimension 1 — Compression Layer (Production-Ready)
- 90+ CLI compression patterns for git, npm, docker, kubectl, cargo, and more
- 18 tree-sitter languages for AST-based signatures and outlines
- 49 MCP tools with 10 read modes (full, map, signatures, diff, aggressive, entropy, task, reference, lines, auto)
- Session caching with mtime-validation — re-reads cost ~13 tokens
- Cross-file codebook compression and archive system (`ctx_expand`)

### Dimension 2 — Semantic Router (Foundations)
- `ModePredictor` learns optimal read modes per file type from session history
- `IntentEngine` classifies query complexity for mode selection
- LITM-aware positioning per model family (Claude α=0.9, GPT, Gemini profiles)
- Thompson Sampling bandits for exploration vs. exploitation

### Dimension 3 — Context Manager (Production-Ready)
- **Context Continuity Protocol (CCP)** — cross-session memory (~400 tokens vs ~50K cold start)
- **Context Ledger** — 128K window tracking with pressure signals
- **Multi-Agent Coordination** — `ctx_agent`, `ctx_share`, handoff packages, agent diaries
- **Knowledge System** — temporal facts, contradiction detection, memory lifecycle, consolidation
- **Property Graph** — SQLite-backed code graph with call/impact/architecture analysis

### Dimension 4 — Quality Guardrail (Planned)
- Compression safety levels per command (verbatim/minimal/standard/aggressive)
- Deterministic anchoring preserves paths, symbols, and error locations
- Full output verification layer planned for Phase 5

### Platform Coverage
- Works with 24+ AI tools: Cursor, Claude Code, Copilot, Windsurf, Codex, Gemini, Kiro, Cline, JetBrains, Amp, Crush, Antigravity, OpenCode, OpenClaw, Hermes, and more
- Single Rust binary, zero telemetry, local-first

## Where We're Going — Context OS

LeanCTX is evolving from a Context Layer into a **Context OS for AI Development** — a lightweight operating system for everything between code/infrastructure and LLMs.

### Strategic Leaps

1. **Context as Code** — Declarative pipelines, profiles, and policies in TOML. Teams version-control their context strategies like infrastructure.
2. **Unified Context Graph** — Code, tests, commits, CI runs, and knowledge entries in a single semantic graph. Agents query the graph, not individual files.
3. **Agent Harness** — Roles, budgets, and policies for multi-agent governance. Token limits, cost caps, and tool permissions per agent role.
4. **Context Observability** — SLOs on context consumption, anomaly detection, session diffing, OpenTelemetry/Prometheus export.

### Remaining Future Directions

- **Full Semantic Routing** — Intent-based model tier recommendations (What/How/Do classification)
- **Output Verification** — Post-processing layer for accuracy guarantees
- **Adaptive ML Compression** — ML-driven entropy thresholds per language and project
- **IDE Extensions** — Context HUD, profile switcher, context issues panel
- **Context Provider Framework** — Structured providers for Jira, GitHub Issues, CI/CD, logs

The end state: an AI that sees only what matters, remembers what's relevant, and reasons at maximum capacity — governed by policies you define.

**Tokens are the new gold. Context is the new infrastructure. Spend both wisely.**
