# OCLA Contract Portal

> **Status: local implementation-contract index — not a product catalogue.** LeanCTX is the **Context Gateway for AI Systems**. Engine and SDK have distinct implementation contracts. A contract marked “Current” describes a checked-in artifact, not general availability or a public service; availability depends on edition, supported interface and release. Cloud, organization, marketplace, hosted-index, public-ranking, agent-building and broader OCLA surfaces remain Research or unavailable unless the canonical status map promotes them.
>
> Single entry point for the checked-in OCLA wire contracts, schemas and specifications. See [current positioning](../POSITIONING_CANONICAL.md) and `docs/internal/README.md` (internal, not in this repository).

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
- [receipt-document-v1.md](receipt-document-v1.md) · [receipt-document-v1.schema.json](receipt-document-v1.schema.json) · [receipt-document/](receipt-document/) — Canonical signed local evidence receipt and its vectors.
- [accepted-outcome-v1.schema.json](accepted-outcome-v1.schema.json) — Tri-state outcome; success signals alone never imply acceptance.
- [decision-record-v1.schema.json](decision-record-v1.schema.json) — Admission-through-outcome decision audit records.
- [capability-manifest-v1.schema.json](capability-manifest-v1.schema.json) — Canonical capability schema; the OCLA path is an exact tested mirror.
- [agent-gateway-v1.schema.json](agent-gateway-v1.schema.json) — Agent gateway wire schema.
- [canonical-artifact-v1.schema.json](canonical-artifact-v1.schema.json) — Canonical artifact envelope.
- [knowledge-object-v1.schema.json](knowledge-object-v1.schema.json) — Content-addressed knowledge provenance record.
- [ocla-agent-envelope-v1.schema.json](ocla-agent-envelope-v1.schema.json) — OCLA agent envelope.
- [ocla-bus-event-v1.schema.json](ocla-bus-event-v1.schema.json) — OclaBus events.
- [ocla-wire-v1.schema.json](ocla-wire-v1.schema.json) — Canonical Token Envelope.
- [response-optimization-v1.schema.json](response-optimization-v1.schema.json) — Response optimization wire schema.
- [routing-decision-v1.schema.json](routing-decision-v1.schema.json) — Routing decision wire schema.
- [test-deployment-evidence-v1.schema.json](test-deployment-evidence-v1.schema.json) — Test-deployment evidence schema.
- [tokenizer-calibration-v1.schema.json](tokenizer-calibration-v1.schema.json) — Tokenizer calibration wire schema.
- [ocla/](ocla/) — OCLA Protobuf definition and capability manifests.
- [ocp/](ocp/) — OCP schemas (capabilities, context IR, events).

### Control Plane

- [context-candidate-admission-v1.md](context-candidate-admission-v1.md) — Context candidate admission.
- [context-gateway-v1.md](context-gateway-v1.md) — Context Gateway decision vocabulary.
- [context-gateway-v2.md](context-gateway-v2.md) — Context Gateway v2: keyed receipt objects.
- [egress-lifecycle-v1.md](egress-lifecycle-v1.md) — Egress lifecycle receipts of the proxy.
- [gateway-proof-v1/](gateway-proof-v1/) — Gateway proof cases and the tests that cover them.
- [context-policy-packs-v1.md](context-policy-packs-v1.md) — Policy packs.
- [context-quality-v1.md](context-quality-v1.md) — Evidence tiers, eval verdicts, retention probes, quality receipt.
- [knowledge-policy-contract-v1.md](knowledge-policy-contract-v1.md) — Knowledge policy.
- [persona-spec-v1.md](persona-spec-v1.md) — Persona specification.
- [quality-loop-v1.md](quality-loop-v1.md) — Quality-loop policy.
- [support-lifecycle-v1.json](support-lifecycle-v1.json) — Support lifecycle data.

### Evidence & Billing

- [billing-plane-v1-catalog.json](billing-plane-v1-catalog.json) — Billing-plane v1 catalog.
- [billing-plane-v2-catalog.json](billing-plane-v2-catalog.json) — v4 plan catalog with the legacy wire names Community/Pro/Team/Enterprise (today: LeanCTX OSS and LeanCTX Hive Personal · Team · Enterprise); v1 remains frozen for compatibility.
- [delivery-evidence-v1.json](delivery-evidence-v1.json) — Delivery evidence.
- [delivery-evidence-v1.schema.json](delivery-evidence-v1.schema.json) — Delivery evidence schema.
- [delivery-manifest-v1.md](delivery-manifest-v1.md) — Delivery manifest.
- [delivery-manifest-v1.schema.json](delivery-manifest-v1.schema.json) — Delivery manifest schema.
- [edit-metering-v1.md](edit-metering-v1.md) — Edit metering.
- [evidence-bundle-v1.md](evidence-bundle-v1.md) — Evidence bundle.
- [evidence-bundle-v2.md](evidence-bundle-v2.md) · [evidence-bundle-v2.schema.json](evidence-bundle-v2.schema.json) — Customer-proof evidence bundle v2.
- [evidence-bundle-v2-verification-v1.md](evidence-bundle-v2-verification-v1.md) — Evidence bundle v2 verification.
- [team-seat-value-v1/](team-seat-value-v1/) — Frozen Team seat-entitlement, allocation, usage, and value-evidence contract family.
- [team-context-outcome-v1.md](team-context-outcome-v1.md) — Team context outcome report.
- [team-context-v1/](team-context-v1/) · [team-context-v1.SOURCE-v1.md](team-context-v1.SOURCE-v1.md) — Team Context v1 schemas and their source pin.
- [test-deployment-evidence-v1.md](test-deployment-evidence-v1.md) — Test-deployment evidence process.

### SDK & Integration

- [a2a-contract-v1.md](a2a-contract-v1.md) — Agent-to-agent integration.
- [addon-manifest-v1.md](addon-manifest-v1.md) — Add-on manifest.
- [capabilities-contract-v1.md](capabilities-contract-v1.md) — Capabilities contract.
- [conformance-v1.md](conformance-v1.md) — SDK and implementation conformance suite.
- [engine-interface-compatibility-v1.md](engine-interface-compatibility-v1.md) — Local Engine boundary and future SDK compatibility policy.
- [engine-interface/](engine-interface/) — Engine interface vectors.
- [engine-context-plan-v1.md](engine-context-plan-v1.md) — Local Engine context planning.
- [engine-context-sources-v1.md](engine-context-sources-v1.md) · [engine-context-sources-v2.md](engine-context-sources-v2.md) — Explicit Engine source planning; v2 adds the presenter capability.
- [engine-context-store-v1.md](engine-context-store-v1.md) — Engine Context Store reads.
- [engine-outcome-v1.md](engine-outcome-v1.md) — Operator-attested Engine outcome.
- [engine-provider-execution-v1.md](engine-provider-execution-v1.md) — Engine provider execution.
- [engine-source-execution-v1.md](engine-source-execution-v1.md) — Operator-owned source execution.
- [intelligence-runtime-install-v1.md](intelligence-runtime-install-v1.md) — Optional Intelligence Runtime installation.
- [native-engine-context-proof-v1.md](native-engine-context-proof-v1.md) — Internal local Engine interface proof and receipt-lineage boundary.
- [Hosted Index V2 candidate guide](../guides/hosted-index-v2.md) — Explicit destinations, owned-data recovery, pairing and current qualification limits.
- [extractors-v1.md](extractors-v1.md) — Extractor integration.
- [http-mcp-contract-v1.md](http-mcp-contract-v1.md) — HTTP MCP integration (frozen v1).
- [http-mcp-contract-v2.md](http-mcp-contract-v2.md) — HTTP MCP v2 (directed events).
- [provider-framework-contract-v1.md](provider-framework-contract-v1.md) — Provider framework.
- [wasm-abi-v1.md](wasm-abi-v1.md) — WASM ABI.

### Security & Trust

- [extension-trust-v1.md](extension-trust-v1.md) — Extension trust.
- [audit-schedule-v1.md](audit-schedule-v1.md) — Audit schedule.
- [branch-protection-v1.md](branch-protection-v1.md) — Branch protection.
- [certification-levels-v1.md](certification-levels-v1.md) — OCLA certification levels.
- [decision-record-signature-v1.md](decision-record-signature-v1.md) — DecisionRecordV1 signature profile.
- [frozen-hashes.json](frozen-hashes.json) — Frozen content hashes.
- [ocla-contract-pack-v1.json](ocla-contract-pack-v1.json) — Signed contract-pack content digests.
- [oss-plane-separation-v1.md](oss-plane-separation-v1.md) — OSS-plane separation.
- [oss-plane-separation-v2.md](oss-plane-separation-v2.md) — OSS-plane separation v2.
- [personal-cloud-encryption-v1.md](personal-cloud-encryption-v1.md) — Personal-cloud encryption.
- [release-key-rotation-v1.md](release-key-rotation-v1.md) — Release-key rotation.
- [release-integrity-v1.md](release-integrity-v1.md) — Release integrity.

### Context, Runtime & Operations

- [attention-layout-driver-v1.md](attention-layout-driver-v1.md) — Attention layout driver.
- [context-ir-v1.md](context-ir-v1.md) — Context intermediate representation.
- [context-plan-projection-digest-v1.md](context-plan-projection-digest-v1.md) — Context-plan projection digest.
- [context-snapshot-v1.md](context-snapshot-v1.md) — Context snapshot.
- [deployment-rehearsal-v1.md](deployment-rehearsal-v1.md) — Deployment rehearsal.
- [graph-reproducibility-contract-v1.md](graph-reproducibility-contract-v1.md) — Graph reproducibility.
- [local-free-invariant-v1.md](local-free-invariant-v1.md) — Local-free invariant.
- [local-free-invariant-v2.md](local-free-invariant-v2.md) — Local-free invariant v2 (3.12, LeanCTX Hive).
- [logical-session-presence-v1.md](logical-session-presence-v1.md) — Logical session presence.
- [memory-boundary-contract-v1.md](memory-boundary-contract-v1.md) — Memory boundary.
- [multi-agent-efficiency-benchmark-v1.md](multi-agent-efficiency-benchmark-v1.md) — Multi-agent efficiency benchmark.
- [ocla-config-tuning-v2.md](ocla-config-tuning-v2.md) — OCLA configuration tuning.
- [ocla-cache-validator-v1.md](ocla-cache-validator-v1.md) — OCLA cache lookup validators.
- [ocla-verifier-conformance-v1.md](ocla-verifier-conformance-v1.md) — OCLA verifier conformance.
- [performance-profile/](performance-profile/) — Performance profile v1 and its schema.
- [pillar-boundaries-v1.md](pillar-boundaries-v1.md) — Pillar boundaries.
- [reference-scheduler-estimates-v1.md](reference-scheduler-estimates-v1.md) — Reference scheduler estimate semantics.
- [runtime-reality-inventory-v1.json](runtime-reality-inventory-v1.json) — Runtime reality inventory.
- [tokenizer-calibration-v1.md](tokenizer-calibration-v1.md) — Tokenizer calibration.
- [wrapped-permalink-v1.md](wrapped-permalink-v1.md) — Wrapped permalink.

### Agent Workflow & Collaboration

- [gotchas-reminders-contract-v1.md](gotchas-reminders-contract-v1.md) — Gotchas and reminders.
- [handoff-transfer-bundle-v1.md](handoff-transfer-bundle-v1.md) — Handoff transfer bundle.
- [team-invite-links-v1.md](team-invite-links-v1.md) — Team invite links.

### Portal & Lifecycle

- [README.md](README.md) — This contract portal.
- [DEPRECATION.md](DEPRECATION.md) — Deprecation and security-advisory process.
- [public-product-claims-v1.md](public-product-claims-v1.md) — Public product claims and narrative governance.

## Schema Validation

All JSON schemas can be validated with:

```sh
json-schema-validator docs/contracts/ocla-wire-v1.schema.json < envelope.json
```

## Versioning Policy

- Major version (v1 → v2): breaking changes, new schema file, minimum 12-month migration window.
- Minor additions: backward-compatible, same schema file, default values.
- See [DEPRECATION.md](DEPRECATION.md) for the full process.
