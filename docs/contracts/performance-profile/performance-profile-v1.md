# Performance Profile v1

Contract version: **v1**

> **Status: Research contract.** This schema is target architecture, not an
> available profile-distribution, team deployment, Calibrator, or Enterprise
> product. Local runtime profiles may exist, but promotion and publication stay
> subject to the evidence and scope gates in `docs/internal/README.md`.

Performance Profiles are portable, versioned configurations for how an agent
reads, searches, compresses, routes, and recovers context. They make a tested
set of choices shareable and reproducible across a local machine, a team, and a
benchmark environment.

## Schema

The normative machine-readable contract is
[performance-profile-v1.schema.json](performance-profile-v1.schema.json). A
profile MUST validate against it; unknown fields are rejected. `id`, `name`,
and `version` are required, and `version` uses Semantic Versioning.

All optional configuration sections are deliberately partial so a child profile
can override only the settings it changes. A deployed profile should set every
parameter whose value can affect a benchmark result.

| Section | Purpose |
|---|---|
| Metadata | Identity, ownership, publication scope, tags, and optional inheritance. |
| `context` | Token budget, reading and compression policy, reuse, recovery, search, and memory. |
| `capabilities` | Provider bindings for code context, shell output, knowledge, routing, and compression. |
| `constraints` | Quality, cost, latency, and verification limits. |
| `routing` | Model-tier cap and context-pressure degradation policy. |
| `pipeline` | Enables or disables intent, relevance, compression, and translation stages. |

Each present capability binding MUST name its `provider`; `strategy` and
`version` are provider-specific. Consumers SHOULD pin `version` for benchmark
or production use.

## Resolution and inheritance

`inherits` names a parent profile. Resolution happens before benchmarking or
deployment: a child value wins, objects merge recursively, and arrays replace
the parent array. A capability binding is an atomic provider choice and SHOULD
be replaced as a whole. Implementations MUST reject an unresolved parent or an
inheritance cycle.

## Calibrator relationship

The Calibrator optimizes a resolved Performance Profile, not an unstructured
set of runtime flags. It measures the profile against a workload and proposes
parameter changes that improve the stated quality, cost, or latency objective.
Accepted calibration output is published as a new profile version; an existing
version remains immutable for reproducible comparisons. Token measurements can
be recorded with the companion
[Tokenizer calibration evidence v1](../tokenizer-calibration-v1.md) contract.

## ExecutionReceipt relationship

Every benchmarked or deployed run SHOULD reference the exact profile identity
in its [ExecutionReceiptV1](../execution-receipt-v1.schema.json) `decision_refs`
array, for example `performance-profile://codex-rust-monorepo-v3@3.0.0`.
The reference records which configuration shaped the run while the receipt
records its cost, latency, decisions, and evidence.

Context pressure overrides degrade the read view only; they never select a model.
Legacy model-tier profile fields remain readable but have no effect on context
or model choice. The caller's explicit provider selection can be described by
the [CapabilityManifestV1](../capability-manifest-v1.schema.json) contract.

## Examples

### Minimal

```json
{
  "id": "local-review-v1",
  "name": "Local Code Review",
  "version": "1.0.0",
  "context": {
    "budget_tokens": 8000
  },
  "capabilities": {
    "code_context": {
      "provider": "leanctx"
    }
  }
}
```

### Standard

```json
{
  "id": "codex-rust-monorepo-v3",
  "name": "Codex Rust Monorepo",
  "version": "3.0.0",
  "description": "Optimized for large Rust monorepos with heavy shell output",
  "author": "community/alice",
  "visibility": "public",
  "tags": ["rust", "monorepo", "codex"],
  "context": {
    "budget_tokens": 48000,
    "read_strategy": "structural",
    "compression": "balanced",
    "reuse_threshold": 0.87,
    "recovery": true,
    "search": { "max_results": 12 },
    "memory": { "enabled": true }
  },
  "capabilities": {
    "code_context": { "provider": "leanctx", "strategy": "structural", "version": "1.9.0" },
    "shell_output": { "provider": "rtk", "version": "0.8.0" },
    "knowledge": { "provider": "company-graph" }
  },
  "constraints": {
    "quality_floor": 0.96,
    "max_cost_usd": 0.50,
    "max_latency_ms": 5000
  }
}
```

### Enterprise

```json
{
  "id": "platform-rust-services-v1",
  "name": "Platform Rust Services",
  "version": "1.2.0",
  "description": "Governed profile for verified production changes in Rust services.",
  "author": "platform-engineering",
  "created_at": "2026-08-21T09:00:00Z",
  "visibility": "team",
  "inherits": "codex-rust-monorepo-v3",
  "tags": ["rust", "production", "governed"],
  "context": {
    "budget_tokens": 64000,
    "read_strategy": "adaptive",
    "compression": "lossless",
    "reuse_threshold": 0.92,
    "recovery": true,
    "search": { "max_results": 20 },
    "memory": { "enabled": true }
  },
  "capabilities": {
    "code_context": { "provider": "leanctx", "strategy": "adaptive", "version": "1.9.0" },
    "shell_output": { "provider": "leanctx", "strategy": "bounded", "version": "1.9.0" },
    "knowledge": { "provider": "company-graph", "strategy": "approved", "version": "2026.08" },
    "routing": { "provider": "leanctx", "strategy": "policy", "version": "1.9.0" },
    "compression": { "provider": "leanctx", "strategy": "lossless", "version": "1.9.0" }
  },
  "constraints": {
    "quality_floor": 0.99,
    "max_cost_usd": 2.00,
    "max_latency_ms": 12000,
    "require_verification": true
  },
  "routing": {
    "degrade_under_pressure": false
  },
  "pipeline": {
    "intent": true,
    "relevance": true,
    "compression": true,
    "translation": true
  }
}
```

## Lifecycle

1. **Create** — author a validating profile with a unique `id` and SemVer
   `version`.
2. **Benchmark** — run representative workloads and attach the profile
   reference to each ExecutionReceipt.
3. **Calibrate** — compare receipt evidence and adjust parameters to meet the
   declared constraints.
4. **Deploy** — resolve inheritance, pin providers, and promote the exact
   profile version.
5. **Version** — publish calibrated changes under a new SemVer version; retain
   the prior version for comparison and rollback.

## OSS and paid boundary

| Offering | Profile capability |
|---|---|
| OSS | Create profiles, run local benchmarks, and calibrate parameters manually. |
| Pro | Retain benchmark history and run automated parameter sweeps. |
| Enterprise | Add governance and continuous optimization for shared, production profiles. |

These additions are control-plane services; they do not restrict local profile
creation or execution. See [OSS Plane Separation v1](../oss-plane-separation-v1.md)
and [Local-Free Invariant v1](../local-free-invariant-v1.md).
