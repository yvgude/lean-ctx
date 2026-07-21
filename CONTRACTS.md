# LeanCTX Protocol Family & Contracts (v1)

LeanCTX is infrastructure. Contracts are the stable promises that client integrations, CI gates, proof artifacts, and future plugins rely on.

## Architecture positioning

```
MCP = how agents call LeanCTX (external interoperability)
LCP = how LeanCTX understands, transforms, and governs context (internal semantics)
```

MCP is the transport. The contracts below define what flows through it.

## Versioning rules (SemVer policy)

- **Schema versions are integers** (`schema_version` / `contract_version`).
- **Breaking change** => bump the corresponding version and add migration notes.
  - Examples: removing fields, changing field types, changing required fields, changing error semantics/status codes.
- **Non-breaking change** => keep version, document additive changes.
  - Examples: adding optional fields, adding new tools, adding new docs pages.
- **Compatibility**:
  - Newer runtimes should be able to **read older artifacts** where possible (at least for proofs / observability).
  - If multiple versions are supported concurrently, support is **explicitly documented**.

### Release SemVer mapping

For the `lean-ctx` release version (`MAJOR.MINOR.PATCH`):

| Change | Release bump | Contract effect |
|---|---|---|
| Bugfix, perf, docs, new compression pattern | PATCH | none |
| New tool, new endpoint, new optional field | MINOR | additive — contract version unchanged |
| Breaking change to a `stable` contract | MAJOR | contract version bump (`v1` → `v2`) |
| Any change to a `frozen` contract doc | forbidden | publish a **new** `-v2.md` file; the `-v1.md` file stays immutable |

### Contract file rule (v1 → v2)

A versioned contract doc (`docs/contracts/<name>-vN.md`) is an **artifact, not a living document**:

- Frozen docs never change — CI (`rust/tests/suite/contracts_frozen.rs`) hashes them and fails on any edit.
- A semantic revision lands as a **new file** (`<name>-v2.md`); the old file remains for existing integrations and gains a deprecation pointer in CONTRACTS.md (not in the frozen file itself).
- Typo fixes in frozen docs are deliberately treated as changes: regenerate the hash snapshot via `LEANCTX_UPDATE_FROZEN_HASHES=1 cargo test --test main suite::contracts_frozen` and justify it in the PR.

### Deprecation policy

- A surface (CLI command, MCP tool, HTTP endpoint, config key, contract version) is deprecated **at least 2 minor releases** before removal.
- Every deprecation is recorded in [`DEPRECATIONS.toml`](rust/data/DEPRECATIONS.toml) (compiled into the binary; lives inside `rust/` so `cargo publish` can package it) with `announced_in`, `earliest_removal`, and a `replacement`.
- `lean-ctx doctor` warns about every active deprecation shipping in the installed build.
- Every release that announces or executes a removal lists it in a dedicated **Deprecations** section of the CHANGELOG.
- `experimental` contracts are exempt — they may change or disappear without notice.

## Stability matrix

Status of every contract document (SSOT: `rust/src/core/contracts.rs::contract_docs()`; enforced by `rust/tests/contracts_frozen.rs` — no doc may stay unclassified):

| Status | Meaning |
|---|---|
| `frozen` | Normative surface immutable; change = new `-v2.md` file. CI-enforced via content hash. |
| `stable` | Additive evolution allowed; breaking change requires version bump + migration notes. |
| `experimental` | May change or disappear without notice. |

| Contract | Doc | Version | Status |
|---|---|---|---|
| HTTP MCP | `docs/contracts/http-mcp-contract-v1.md` | 1 | frozen |
| Team Server | `docs/contracts/team-server-contract-v1.md` | 1 | frozen |
| Context IR | `docs/contracts/context-ir-v1.md` | 1 | frozen |
| Local-Free Invariant | `docs/contracts/local-free-invariant-v1.md` | 1 | frozen |
| OSS Plane Separation | `docs/contracts/oss-plane-separation-v1.md` | 1 | frozen |
| Billing Plane | `docs/contracts/billing-plane-v1.md` | 1 | frozen |
| WASM ABI | `docs/contracts/wasm-abi-v1.md` | 1 | frozen |
| Delivery Manifest | `docs/contracts/delivery-manifest-v1.md` | 1 | frozen |
| Deployment Rehearsal | `docs/contracts/deployment-rehearsal-v1.md` | 1 | frozen |
| Release Key Rotation | `docs/contracts/release-key-rotation-v1.md` | 1 | frozen |
| Capabilities | `docs/contracts/capabilities-contract-v1.md` | 1 | stable¹ |
| Billing Plane v2 | `docs/contracts/billing-plane-v2.md` | 2 | stable |
| Settlement Evidence | `docs/contracts/settlement-evidence-v2.md` | 2 | stable |
| A2A | `docs/contracts/a2a-contract-v1.md` | 1 | stable |
| Attention Layout Driver | `docs/contracts/attention-layout-driver-v1.md` | 1 | stable |
| Autonomy Drivers | `docs/contracts/autonomy-drivers-v1.md` | 1 | stable |
| CCP Session Bundle | `docs/contracts/ccp-session-bundle-v1.md` | 1 | stable |
| Conformance | `docs/contracts/conformance-v1.md` | 1 | stable |
| OCLA Verifier Conformance | `docs/contracts/ocla-verifier-conformance-v1.md` | 1 | stable |
| Degradation Policy | `docs/contracts/degradation-policy-v1.md` | 1 | stable |
| Extension Trust | `docs/contracts/extension-trust-v1.md` | 1 | stable |
| Extractors | `docs/contracts/extractors-v1.md` | 1 | stable |
| Gotchas/Reminders | `docs/contracts/gotchas-reminders-contract-v1.md` | 1 | stable |
| Graph Reproducibility | `docs/contracts/graph-reproducibility-contract-v1.md` | 1 | stable |
| Handoff Transfer Bundle | `docs/contracts/handoff-transfer-bundle-v1.md` | 1 | stable |
| Intent Route | `docs/contracts/intent-route-v1.md` | 1 | stable |
| Knowledge Policy | `docs/contracts/knowledge-policy-contract-v1.md` | 1 | stable |
| Memory Boundary | `docs/contracts/memory-boundary-contract-v1.md` | 1 | stable |
| Persona Spec | `docs/contracts/persona-spec-v1.md` | 1 | stable |
| Provider Framework | `docs/contracts/provider-framework-contract-v1.md` | 1 | stable |
| Tokenizer Translation Driver | `docs/contracts/tokenizer-translation-driver-v1.md` | 1 | stable |
| Workflow Evidence Ledger | `docs/contracts/workflow-evidence-ledger-v1.md` | 1 | stable |
| Wrapped Permalink | `docs/contracts/wrapped-permalink-v1.md` | 1 | stable |
| Context Candidate Admission | `docs/contracts/context-candidate-admission-v1.md` | 1 | experimental |
| Multi-Agent Efficiency Benchmark | `docs/contracts/multi-agent-efficiency-benchmark-v1.md` | 1 | experimental |
| Test Deployment Evidence | `docs/contracts/test-deployment-evidence-v1.md` | 1 | experimental |
| Hosted Personal Index | `docs/contracts/hosted-personal-index-v1.md` | 1 | experimental |
| Personal Cloud Encryption | `docs/contracts/personal-cloud-encryption-v1.md` | 1 | experimental |
| Context Snapshot | `docs/contracts/context-snapshot-v1.md` | 1 | experimental |
| OCLA Config Tuning | `docs/contracts/ocla-config-tuning-v2.md` | 2 | experimental |

¹ The capabilities document is additive **by design**: its drift test binds the doc's key list to `server_capabilities::TOP_LEVEL_KEYS`, so the doc must grow whenever a key is added. Freezing the file would contradict its own contract; removal or mutation of existing keys remains a breaking change.

Clients can read this matrix at runtime: `GET /v1/capabilities` returns a `contract_status` map (contract-id → status) next to the existing `contracts` version map.

The OpenAPI document (`GET /v1/openapi.json`) is part of the frozen surface: `rust/tests/openapi_stability.rs` compares the endpoint inventory against `docs/reference/openapi-v1.snapshot.json` — additive diffs are allowed, removed or mutated routes fail CI.

## Current contract versions (SSOT, machine-checked)

<!-- leanctx-contracts-kv:begin -->
leanctx.contract.mcp_manifest.schema_version=1
leanctx.contract.context_proof_v1.schema_version=1
leanctx.contract.context_ir_v1.schema_version=1
leanctx.contract.intent_route_v1.schema_version=1
leanctx.contract.degradation_policy_v1.schema_version=1
leanctx.contract.workflow_evidence_ledger_v1.schema_version=1
leanctx.contract.autonomy_drivers_v1.schema_version=1
leanctx.contract.tokenizer_translation_driver_v1.schema_version=1
leanctx.contract.attention_layout_driver_v1.schema_version=1
leanctx.contract.verification_observability_v1.schema_version=1
leanctx.contract.handoff_ledger_v1.schema_version=1
leanctx.contract.handoff_transfer_bundle_v1.schema_version=1
leanctx.contract.ccp_session_bundle_v1.schema_version=1
leanctx.contract.knowledge_policy_v1.schema_version=1
leanctx.contract.graph_reproducibility_v1.schema_version=1
leanctx.contract.a2a_snapshot_v1.schema_version=1
leanctx.contract.memory_boundary_v1.schema_version=1
leanctx.contract.gotchas_reminders_v1.schema_version=1
leanctx.contract.provider_framework_v1.schema_version=1
leanctx.contract.http_mcp.contract_version=1
leanctx.contract.team_server.contract_version=1
leanctx.contract.context_snapshot_v1.schema_version=1
<!-- leanctx-contracts-kv:end -->

---

## Core Context Contracts

Foundational representations that all other contracts build upon.

### Context IR v1 (Intermediate Representation)

The canonical representation of all context flowing through LeanCTX. Every tool call is recorded with source, lineage, tokens, compression ratio, and safety metadata.

- **Doc**: `docs/contracts/context-ir-v1.md`
- **Runtime source**: `rust/src/core/context_ir.rs`
- **Surface**: Recorded in hot-path after every `call_tool`; exported via `ctx_proof`; persisted to `~/.lean-ctx/context_ir_v1.json`

### Context Proof v1 (Verification Artifacts)

Cryptographic proofs that document what context was produced, how it was compressed, and whether it's reproducible.

- **Runtime source**: `rust/src/core/context_proof.rs`
- **Surface**: `ctx_proof` tool; exports to `project/.lean-ctx/proofs/`

### Verification Observability v1

Runtime observability for output verification (compression safety checks).

- **Runtime source**: `rust/src/core/verification_observability.rs`
- **Surface**: Verify footer in tool outputs when profile-enabled

### Context Snapshot v1 (Context Time Machine)

A git-anchored, signed, point-in-time record of the context-layer state — lineage (from IR), ledger Φ-scores + item states, token ROI, and session slice. Snapshots chain into an append-only timeline you can rewind, reproduce, resume, or share. Distilled-by-default (never raw transcripts), content-addressed id (BLAKE3), ed25519-signed.

- **Doc**: `docs/contracts/context-snapshot-v1.md`
- **Runtime source**: `rust/src/core/context_snapshot/`
- **Surface**: Phase 0 contract (#1023); builder + `snapshot`/`timeline` CLI land in Phase 1 (#1024)

---

## Runtime Contracts

Govern how the runtime processes, budgets, and degrades context.

### Degradation Policy v1 (Budgets/SLOs)

- **Doc**: `docs/contracts/degradation-policy-v1.md`
- **Runtime source**: `rust/src/core/degradation_policy.rs`
- **Surface**: Enforced at tool-call boundary when enabled

### Workflow Evidence Ledger v1

- **Doc**: `docs/contracts/workflow-evidence-ledger-v1.md`
- **Runtime source**: `rust/src/core/evidence_ledger.rs`
- **Surface**: `ctx_workflow` evidence-gated transitions + automatic tool receipts

### Autonomy Drivers v1

- **Doc**: `docs/contracts/autonomy-drivers-v1.md`
- **Runtime source**: `rust/src/core/autonomy_drivers.rs` + `rust/src/tools/autonomy.rs`
- **Surface**: Deterministic driver planner + bounded driver reports; proof export via `ctx_proof`

### Intent Route v1 (Orchestration Routing)

- **Doc**: `docs/contracts/intent-route-v1.md`
- **Runtime source**: `rust/src/core/intent_router.rs`
- **Surface**: `ctx_intent` with `format=json` returns `IntentRouteV1`

### Tokenizer-aware Translation Driver v1

- **Doc**: `docs/contracts/tokenizer-translation-driver-v1.md`
- **Runtime source**: `rust/src/core/tokenizer_translation_driver.rs`
- **Surface**: Deterministic ruleset selection (model_key -> ruleset) + bounded translation

### Attention-aware Layout Driver v1

- **Doc**: `docs/contracts/attention-layout-driver-v1.md`
- **Runtime source**: `rust/src/core/attention_layout_driver.rs`
- **Surface**: Deterministic reorder for delivery surfaces when profile-enabled

---

## Memory & Collaboration Contracts

Define how context persists, transfers between agents, and crosses boundaries.

### CCP Session Bundle v1

- **Doc**: `docs/contracts/ccp-session-bundle-v1.md`
- **Runtime source**: `rust/src/core/ccp_session_bundle.rs` + `rust/src/core/session.rs`
- **Surface**: `ctx_session action=export|import` (redacted-by-default, bounded, replayable)

### Knowledge Policy v1

- **Doc**: `docs/contracts/knowledge-policy-contract-v1.md`
- **Runtime source**: `rust/src/core/memory_policy.rs` + `rust/src/core/knowledge.rs`
- **Surface**: `ctx_knowledge action=policy value=show|validate`

### Graph Reproducibility v1

- **Doc**: `docs/contracts/graph-reproducibility-contract-v1.md`
- **Runtime source**: `rust/src/core/property_graph/*`
- **Surface**: `ctx_impact` / `ctx_architecture` with `format=json`

### A2A Contract v1 (Multi-Agent)

- **Doc**: `docs/contracts/a2a-contract-v1.md`
- **Runtime source**: `rust/src/core/agents.rs` + `rust/src/core/a2a/*`
- **Surface**: `ctx_agent`, `ctx_task`, rate limiting, cost attribution

### Handoff Transfer Bundle v1

- **Doc**: `docs/contracts/handoff-transfer-bundle-v1.md`
- **Runtime source**: `rust/src/core/handoff_transfer_bundle.rs`
- **Surface**: `ctx_handoff action=export|import` (redacted-by-default, bounded, identity-aware)

### Memory Boundary v1

- **Doc**: `docs/contracts/memory-boundary-contract-v1.md`
- **Runtime source**: `rust/src/core/memory_boundary.rs`
- **Surface**: `FactPrivacy` scoping, cross-project gates, audit events

### Gotchas/Reminders v1

- **Doc**: `docs/contracts/gotchas-reminders-contract-v1.md`
- **Runtime source**: `rust/src/core/gotcha_tracker/model.rs`
- **Surface**: Time-bounded reminders with provenance and decay

---

## Extension Contracts

Interfaces for external integrations and future plugin system.

### Provider Framework v1 (Context I/O)

- **Doc**: `docs/contracts/provider-framework-contract-v1.md`
- **Runtime source**: `rust/src/core/providers/` + `rust/src/tools/ctx_provider.rs`
- **Surface**: `ctx_provider` tool (GitLab issues, MRs, pipelines); TTL-based cache; redaction on all outputs
- **Future**: This contract defines the shape for third-party Context Provider plugins

### CompressionPattern (planned v1)

- **Status**: Interface extracted from `rust/src/core/patterns/mod.rs`
- **Future**: Plugin-loadable compression patterns for proprietary CLI tools

### LcpTool (planned v1)

- **Status**: Mirrors existing `McpTool` trait in `rust/src/server/tool_trait.rs`
- **Future**: Plugin-registered tools that inherit the full runtime pipeline

---

## Transport Contracts

Define how LeanCTX communicates with the outside world.

### MCP Manifest v1 (Tool Inventory)

- **Artifact**: `website/generated/mcp-tools.json`
- **Schema**: `schema_version` + normalized tool entries (`name`, `description`, `input_schema`, `schema_md5`)
- **Runtime source**: `rust/src/core/mcp_manifest.rs`

### HTTP MCP v1

- **Doc**: `docs/contracts/http-mcp-contract-v1.md`
- **Stable endpoints**: `/health`, `/v1/manifest`, `/v1/tools`, `/v1/tools/call`, `/v1/events`, `/v1/context/summary`
- **Event schema**: `ContextEventV1` with `version`, `parentId`, `consistencyLevel`
- **Typed errors**: JSON `error_code` + `error`

### Team Server v1

- **Doc**: `docs/contracts/team-server-contract-v1.md`
- **Workspaces**: `x-leanctx-workspace` header + `workspaceId` body + deterministic fallback
- **Audit log**: JSONL with `argumentsMd5` only (no raw args)

---

## Compatibility matrix

| Integration | Transport | Contracts relied on | Setup |
|---|---|---|---|
| Cursor | MCP (stdio) + Shell Hook | MCP manifest v1 + tool schemas + shell patterns | `lean-ctx setup` |
| Claude Code | MCP (stdio) + Shell Hook | MCP manifest v1 + tool schemas + shell patterns | `lean-ctx init --agent claude` |
| CodeBuddy | MCP (stdio) + Shell Hook | MCP manifest v1 + tool schemas + shell patterns | `lean-ctx init --agent codebuddy` |
| GitHub Copilot | MCP (stdio) + Shell Hook | MCP manifest v1 + tool schemas | `lean-ctx init --agent copilot` |
| Remote agents | HTTP | HTTP MCP v1 + typed errors | `lean-ctx serve` |
| Teams | HTTP | Team Server v1 + audit log | `lean-ctx team serve` |
| Future plugins | In-process / Subprocess | Provider v1 + CompressionPattern v1 + LcpTool v1 | `~/.config/lean-ctx/plugins/` |
