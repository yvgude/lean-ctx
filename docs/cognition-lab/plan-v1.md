# Cognition Lab Plan v1 (privacy-first, SSOT, CI-gated)

> **Status: research plan — not a current LeanCTX product or capability
> commitment.** LeanCTX is **The Context SDK for AI Agents**; current scope and
> status are governed by `docs/internal/README.md` (internal, not in this repository).

Tracked in GitLab: `#2344`.

This plan defines how LeanCTX evolves learned cognition drivers (layout/attention/budgeting) safely, reproducibly, and without compromising local-first trust.

## 1) Goals

- Improve *effective reasoning* via deterministic steering and learned drivers.
- Keep production (API-LLM) behavior stable via caps, proofs, and CI gates.
- Provide a research harness for open-weights models (optional) using the same interfaces.

## 2) Non-goals

- No weight modification for proprietary models.
- No telemetry by default.
- No “silent” behavior changes without versioning and gates.

## 3) Existing building blocks (evidence)

- Attention/layout primitives:
  - `rust/src/core/litm.rs`
  - `rust/src/core/neural/context_reorder.rs`
  - `rust/src/core/neural/token_optimizer.rs`
  - `rust/src/core/attention_model.rs`
- Adaptive policies:
  - `rust/src/core/mode_predictor.rs`
  - `rust/src/core/adaptive_thresholds.rs`
  - `rust/src/core/budget_tracker.rs`
- Verification:
  - `rust/src/core/output_verification.rs`
  - `rust/tests/scientific_verification.rs`

## 4) Data sources & privacy (opt-in only)

### Allowed data (local by default)

- tool call metadata (tool name, mode, sizes, timings)
- verification warnings counters (types + counts)
- non-sensitive outcome signals (e.g. “tests passed”, “lint failed”) when explicitly invoked by the user/tool

### Disallowed data (never collected)

- file contents
- shell stdout/stderr content
- secrets/tokens/credentials

### Opt-in model

- default: **off**
- configuration: `~/.lean-ctx/config.toml` (new section to be proposed in a follow-up ticket)
- redaction: must run before any export (reuse existing redaction pipeline)

## 5) Evaluation methodology (CI-gated)

### Offline evals

- Replayability: stable inputs → stable outputs
- Compression quality: verification warnings must not regress
- Token/cost impact: compare before/after with `ctx_benchmark` and `ctx_gain` metrics

### CI gates (proposal)

- Golden fixtures for critical transformations (ordering/layout where deterministic)
- Regression thresholds: no increase in verifier loss score above bound for benchmark suite
- “Safety gates”: redaction + PathJail tests must remain green

## 6) ONNX training/calibration loop (versioned)

- Model artifacts versioned by:
  - semantic version (major/minor/patch)
  - training dataset hash (metadata only)
  - calibration config hash
- Storage:
  - local cache directory under `~/.lean-ctx/models/` (proposed)
- Loading:
  - feature-flagged and bounded (never block tool execution)

## 7) Rollout strategy

- feature flags per driver:
  - `LEAN_CTX_NEURAL_LAYOUT=1` (example; final naming via follow-up ticket)
- staged rollout:
  - off → opt-in local → opt-in team (team server) → default-on only after long CI evidence
- rollback:
  - immediate disable via env/config
  - keep last-known-good model artifact

## 8) Next subtickets (to create)

- Telemetry opt-in + redaction contract (no content)
- Eval suite expansion (goldens + thresholds)
- Model artifact versioning + loader contracts
