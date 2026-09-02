# Cognition Interface (v1)

> **Status: research architecture note — not a public product name or capability
> claim.** LeanCTX is **The Context SDK for AI Agents**, a local
> context layer for existing agents: it selects, shapes, reuses, and recovers
> task-fit context. Current scope and status are
> governed by `docs/internal/README.md` (internal, not in this repository). Profiles,
> autonomous orchestration, Team Server, and broader SDK surfaces referenced
> below are Research or unavailable.

LeanCTX cannot modify proprietary model weights. This note explores a
**Cognition Interface**: a deterministic control surface for shaping what an
agent receives, how it is budgeted, remembered, and verified.

This is production-realistic for API LLMs and becomes even stronger when paired with open-weights models in an optional Cognition Lab track.

## What “Cognition Interface” means in practice

### 1) Context I/O (signal in)

- Deterministic reads/search/shell output with explicit tool calls.
- Bounded outputs (size caps, truncation markers).
- Sandboxed file access (PathJail / allowed roots).

Evidence:
- `rust/src/core/pathjail.rs`
- `rust/src/core/output_verification.rs`
- `rust/src/core/cache.rs`

### 2) Orchestration (routing + budgets)

- Profile-driven pipelines (“Context as Code”): read modes, budgets, verification, autonomy.
- Intent/mode prediction and adaptive thresholds (bandits) to keep cost/quality stable.
- Client constraints compilation: the same policy must compile into client-safe instruction blocks.

Evidence:
- `rust/src/core/profiles.rs`
- `rust/src/core/intent_engine.rs`
- `rust/src/core/mode_predictor.rs`
- `rust/src/core/adaptive_thresholds.rs`
- `rust/src/core/instruction_compiler.rs`
- `docs/integrations/client-constraints-matrix-v1.md`

### 3) Memory (what persists)

- Session continuity (CCP), structured knowledge, contradictions/relations.
- Exportable handoffs and auditability across agents.

Evidence:
- `rust/src/core/session.rs`
- `rust/src/core/knowledge.rs`
- `rust/src/core/gotcha_tracker/*`
- `rust/src/core/a2a/*`

### 4) Verification (what must hold)

- Deterministic checks on compressed outputs (paths, identifiers, structure, line numbers).
- Proof artifacts and CI gates to prevent “it worked yesterday” drift.

Evidence:
- `rust/src/core/output_verification.rs`
- `CONTRACTS.md`
- `rust/tests/*_up_to_date.rs`

### 5) Delivery (everywhere)

- MCP + HTTP MCP + Team Server let the same primitives run locally, in CI, and enterprise setups.
- SDK + cookbook runs against a real server instance (no mock mode).

Evidence:
- `rust/src/http_server/mod.rs`
- `lean-ctx-enterprise: http_server/team.rs` (ADR-023)
- `cookbook/sdk/src/client.e2e.test.ts`

## Contract: deterministic steering, not “prompt magic”

The Cognition Interface is only useful if it is:

- **Deterministic**: same inputs/policies → same compiled output.
- **Bounded**: caps enforced per client/model constraints.
- **Auditable**: evidence artifacts and CI gates catch drift.
- **Local-first**: no telemetry unless explicitly enabled.

## Optional: Cognition Lab track

For open-weights experimentation (or internal models), the same interface becomes a research harness:

- learned attention/layout drivers
- calibration/evaluation suites
- ONNX models with versioning + rollout policies

See: `docs/cognition-lab/plan-v1.md` (tracked in GitLab `#2344`).
