# OCLA Contract Portal

> **Status: local implementation-contract index — not a product catalogue.** LeanCTX is the **Context Gateway for AI Systems**. Engine and SDK have distinct implementation contracts. A contract marked “Current” describes a checked-in artifact, not general availability or a public service; availability depends on edition, supported interface and release. Cloud, organization, marketplace, hosted-index, public-ranking, agent-building and broader OCLA surfaces remain Research or unavailable unless the canonical status map promotes them.
>
> Single entry point for the checked-in OCLA wire contracts, schemas and specifications. See [current positioning](../POSITIONING_CANONICAL.md) and [`docs/internal/README.md`](../internal/README.md).

This portal is the navigable index for the contracts in this directory. The
Rust OCLA types, JSON Schema, and Protobuf definitions remain the authoritative
wire definitions; documents here describe their use and surrounding systems.

## Quick Links

| Contract | Format | Status | Since |
|---|---|---|---|
| [ocla-wire-v1.schema.json](ocla-wire-v1.schema.json) | JSON Schema | Current | v1 |
| [ocla-agent-envelope-v1.schema.json](ocla-agent-envelope-v1.schema.json) | JSON Schema | Current | v1 |
| [ocla-bus-event-v1.schema.json](ocla-bus-event-v1.schema.json) | JSON Schema | Current | v1 |
| [ocla-contract-pack-v1.json](ocla-contract-pack-v1.json) | JSON | Current | v1 |
| [conformance-v1.md](conformance-v1.md) | Markdown | Current | v1 |
| [DEPRECATION.md](DEPRECATION.md) | Markdown | Current | v1 |

## Categories

This index links to public artifacts present in this repository. Organization,
identity, billing, and commercial-service contracts moved outside this Engine
distribution. Use the [Enterprise overview](https://leanctx.com/enterprise/)
and the documentation supplied for your licensed release.

Frozen v1 source-boundary records preserve older repository and packaging
descriptions. See [current scope and historical-contract notes](../../CONTRACTS.md#current-product-and-historical-contract-scope)
before interpreting them as a current product or licensing statement.

### Wire Contracts (Data Plane)

- [task-envelope-v1.schema.json](task-envelope-v1.schema.json) — Canonical task identity and trace lineage.
- [context-plan-v1.schema.json](context-plan-v1.schema.json) — Deterministic projection of the kernel-owned semantic context plan.
- [execution-plan-v1.schema.json](execution-plan-v1.schema.json) — Version-pinned execution, capability, and budget choices.
- [execution-receipt-v1.schema.json](execution-receipt-v1.schema.json) — Execution observations, lineage, cost, and evidence.
- [accepted-outcome-v1.schema.json](accepted-outcome-v1.schema.json) — Tri-state outcome; success signals alone never imply acceptance.
- [decision-record-v1.schema.json](decision-record-v1.schema.json) — Admission-through-outcome decision audit records.
- [capability-manifest-v1.schema.json](capability-manifest-v1.schema.json) — Canonical capability schema; the OCLA path is an exact tested mirror.
- [agent-gateway-v1.schema.json](agent-gateway-v1.schema.json) — Agent gateway wire schema.
- [ocla-agent-envelope-v1.schema.json](ocla-agent-envelope-v1.schema.json) — OCLA agent envelope.
- [ocla-bus-event-v1.schema.json](ocla-bus-event-v1.schema.json) — OclaBus events.
- [ocla-wire-v1.schema.json](ocla-wire-v1.schema.json) — Canonical Token Envelope.
- [response-optimization-v1.schema.json](response-optimization-v1.schema.json) — Response optimization wire schema.
- [routing-decision-v1.schema.json](routing-decision-v1.schema.json) — Routing decision wire schema.
- [test-deployment-evidence-v1.schema.json](test-deployment-evidence-v1.schema.json) — Test-deployment evidence schema.
- [tokenizer-calibration-v1.schema.json](tokenizer-calibration-v1.schema.json) — Tokenizer calibration wire schema.

### Control Plane

- [autonomy-drivers-v1.md](autonomy-drivers-v1.md) — Autonomy drivers.
- [context-candidate-admission-v1.md](context-candidate-admission-v1.md) — Context candidate admission.
- [context-policy-packs-v1.md](context-policy-packs-v1.md) — Policy packs.
- [context-quality-v1.md](context-quality-v1.md) — Evidence tiers, eval verdicts, retention probes, quality receipt.
- [degradation-policy-v1.md](degradation-policy-v1.md) — Degradation policy.
- [knowledge-policy-contract-v1.md](knowledge-policy-contract-v1.md) — Knowledge policy.
- [persona-spec-v1.md](persona-spec-v1.md) — Persona specification.
- [quality-loop-v1.md](quality-loop-v1.md) — Quality-loop policy.
- [support-lifecycle-v1.json](support-lifecycle-v1.json) — Support lifecycle data.

### Evidence & Billing

- [billing-plane-v1-catalog.json](billing-plane-v1-catalog.json) — Billing-plane v1 catalog.
- [billing-plane-v2-catalog.json](billing-plane-v2-catalog.json) — Canonical v4 Community/Pro/Team/Enterprise catalog; v1 remains frozen for compatibility.
- [billing-plane-v1.md](billing-plane-v1.md) — Billing-plane v1.
- [billing-plane-v2.md](billing-plane-v2.md) — Billing-plane v2.
- [billing-plane-v3.md](billing-plane-v3.md) — Billing-plane v3.
- [delivery-evidence-v1.json](delivery-evidence-v1.json) — Delivery evidence.
- [delivery-manifest-v1.md](delivery-manifest-v1.md) — Delivery manifest.
- [edit-metering-v1.md](edit-metering-v1.md) — Edit metering.
- [evidence-bundle-v1.md](evidence-bundle-v1.md) — Evidence bundle.
- [settlement-evidence-v2.md](settlement-evidence-v2.md) — Settlement evidence.
- [team-seat-value-v1/](team-seat-value-v1/) — Frozen Team seat-entitlement, allocation, usage, and value-evidence contract family.
- [test-deployment-evidence-v1.md](test-deployment-evidence-v1.md) — Test-deployment evidence process.
- [workflow-evidence-ledger-v1.md](workflow-evidence-ledger-v1.md) — Workflow evidence ledger.

### SDK & Integration

- [a2a-contract-v1.md](a2a-contract-v1.md) — Agent-to-agent integration.
- [addon-manifest-v1.md](addon-manifest-v1.md) — Add-on manifest.
- [capabilities-contract-v1.md](capabilities-contract-v1.md) — Capabilities contract.
- [conformance-v1.md](conformance-v1.md) — SDK and implementation conformance suite.
- [engine-interface-compatibility-v1.md](engine-interface-compatibility-v1.md) — Local Engine boundary and future SDK compatibility policy.
- [native-engine-context-proof-v1.md](native-engine-context-proof-v1.md) — Internal local Engine interface proof and receipt-lineage boundary.
- [ctxpkg-registry-v1.md](ctxpkg-registry-v1.md) — Context-package registry.
- [extractors-v1.md](extractors-v1.md) — Extractor integration.
- [http-mcp-contract-v1.md](http-mcp-contract-v1.md) — HTTP MCP integration.
- [provider-framework-contract-v1.md](provider-framework-contract-v1.md) — Provider framework.
- [wasm-abi-v1.md](wasm-abi-v1.md) — WASM ABI.

### Security & Trust

- [extension-trust-v1.md](extension-trust-v1.md) — Extension trust.
- [frozen-hashes.json](frozen-hashes.json) — Frozen content hashes.
- [ocla-contract-pack-v1.json](ocla-contract-pack-v1.json) — Signed contract-pack content digests.
- [oss-plane-separation-v1.md](oss-plane-separation-v1.md) — OSS-plane separation.
- [personal-cloud-encryption-v1.md](personal-cloud-encryption-v1.md) — Personal-cloud encryption.
- [release-key-rotation-v1.md](release-key-rotation-v1.md) — Release-key rotation.

### Context, Runtime & Operations

- [attention-layout-driver-v1.md](attention-layout-driver-v1.md) — Attention layout driver.
- [compliance-report-v1.md](compliance-report-v1.md) — Compliance reporting.
- [context-ir-v1.md](context-ir-v1.md) — Context intermediate representation.
- [context-snapshot-v1.md](context-snapshot-v1.md) — Context snapshot.
- [deployment-rehearsal-v1.md](deployment-rehearsal-v1.md) — Deployment rehearsal.
- [graph-reproducibility-contract-v1.md](graph-reproducibility-contract-v1.md) — Graph reproducibility.
- [local-free-invariant-v1.md](local-free-invariant-v1.md) — Local-free invariant.
- [logical-session-presence-v1.md](logical-session-presence-v1.md) — Logical session presence.
- [memory-boundary-contract-v1.md](memory-boundary-contract-v1.md) — Memory boundary.
- [multi-agent-efficiency-benchmark-v1.md](multi-agent-efficiency-benchmark-v1.md) — Multi-agent efficiency benchmark.
- [ocla-config-tuning-v2.md](ocla-config-tuning-v2.md) — OCLA configuration tuning.
- [ocla-verifier-conformance-v1.md](ocla-verifier-conformance-v1.md) — OCLA verifier conformance.
- [pillar-boundaries-v1.md](pillar-boundaries-v1.md) — Pillar boundaries.
- [runtime-reality-inventory-v1.json](runtime-reality-inventory-v1.json) — Runtime reality inventory.
- [tokenizer-calibration-v1.md](tokenizer-calibration-v1.md) — Tokenizer calibration.
- [tokenizer-translation-driver-v1.md](tokenizer-translation-driver-v1.md) — Tokenizer translation driver.
- [wrapped-permalink-v1.md](wrapped-permalink-v1.md) — Wrapped permalink.

### Agent Workflow & Collaboration

- [gotchas-reminders-contract-v1.md](gotchas-reminders-contract-v1.md) — Gotchas and reminders.
- [handoff-transfer-bundle-v1.md](handoff-transfer-bundle-v1.md) — Handoff transfer bundle.
- [intent-route-v1.md](intent-route-v1.md) — Intent routing.
- [team-invite-links-v1.md](team-invite-links-v1.md) — Team invite links.

### Portal & Lifecycle

- [README.md](README.md) — This contract portal.
- [DEPRECATION.md](DEPRECATION.md) — Deprecation and security-advisory process.

## Schema Validation

All JSON schemas can be validated with:

```sh
json-schema-validator docs/contracts/ocla-wire-v1.schema.json < envelope.json
```

## Versioning Policy

- Major version (v1 → v2): breaking changes, new schema file, minimum 12-month migration window.
- Minor additions: backward-compatible, same schema file, default values.
- See [DEPRECATION.md](DEPRECATION.md) for the full process.
