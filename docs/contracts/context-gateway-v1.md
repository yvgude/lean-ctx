# Context Gateway v1

Status: **experimental** — additive, unreleased; promoted to Stable only by the
release that ships it. License: Apache-2.0 (Community). Rust definition:
`lean_ctx_protocol::context_gateway`.

The Context Gateway decides, for every piece of context an agent or model is
about to receive, whether it may be delivered, in which form, to which
destination, and why. This contract is the one vocabulary every surface
(engine, proxy, SDK, HUD, audit export) uses for that decision. It is a
**vocabulary, not a policy engine**: it defines what a decision looks like and
which states are impossible, not which rules an organisation runs.

## Invariants

1. **No content.** Passports and receipts reference content by SHA-256 digest.
   Detector signals carry counts, never matched values. Unknown fields are
   rejected (`deny_unknown_fields`), so content cannot be smuggled in.
2. **One classification lattice.** `public < internal < confidential <
   restricted`. Secrets and credentials are `restricted`. There is no implicit
   default: in `governed` and `sovereign` mode unclassified content is
   `internal`, in `developer` mode `public`.
3. **Placement is a separate axis.** `local_only` and `organization_private`
   are destination constraints, not sensitivity levels. A destination whose
   locality is `unknown` satisfies no constraint.
4. **Monotone derivation.** An object derived from inputs (selection, summary,
   deduplication …) is classified as the join of its inputs, accumulates every
   constraint, policy and risk signal, and inherits the least trust. Inputs of
   different principals are never combined.
5. **Explicit identity.** A principal is either identified (`person`, `team`,
   `organization`, `project`, `agent`, `session`, `workload`) with an id, or
   `unknown` without one. `unknown` is never an authorisation.
6. **Honest coverage.** A detector records bytes and chunks inspected.
   `complete` means every byte; `partial` means some bytes were not inspected.
   A failed or timed-out detector cannot claim complete coverage, and only a
   completed full scan satisfies a *required* detector.
7. **Every restriction has a reason.** Any disposition other than `allow`
   carries at least one reason code `[a-z][a-z0-9_.]{2,63}`.
8. **Consistent receipts.** Blocked and quarantined counts equal the recorded
   `deny` / `quarantine` decisions; `selected ≤ permitted` and
   `permitted + blocked ≤ inspected`. Only a `delivered` receipt names the
   delivered context digest, and it must.
9. **Deterministic identity.** A receipt's digest is SHA-256 over its canonical
   JSON (sorted keys, no whitespace); identical decisions produce identical
   bytes (#498).

## Types

| Type | Purpose |
|---|---|
| `ClassificationV1` | Sensitivity lattice with `join` |
| `GatewayModeV1` | `developer`, `governed`, `sovereign` |
| `PrincipalV1` | Who asked; explicit `unknown` |
| `DestinationV1` | Provider, model, locality, organisation management, account, region |
| `DestinationConstraintV1` | `local_only`, `organization_private` |
| `ReasonCodeV1` | Stable machine reason |
| `PolicyRefV1` | Policy id, version, content digest |
| `DetectorSignalV1` | Normalised detector result (category, severity, confidence, coverage, status, latency) |
| `DetectorCoverageV1` | `complete`, `partial`, `unsupported`, `failed`, `not_required` with byte/chunk counts |
| `ContextDispositionV1` | `allow` < `allow_minimized` < `allow_redacted` < `allow_summary_only` < `allow_local_model_only` < `allow_with_approval` < `quarantine` < `deny` |
| `ContextDecisionV1` | Per-object disposition, reasons, signals, required transformations |
| `TransformationRecordV1` | One lineage step: kind, input/output digest, tokens before/after |
| `ContextObjectPassportV1` | Metadata that travels with one object |
| `ContextDecisionReceiptV1` | One governed delivery |
| `ContextQualitySectionV1` | Optional per-round context-quality evidence inside a receipt: retention counts per criticality, lost critical fact *kinds* (never values), recovery counters, evidence tier; `task_quality` is always `unmeasured` per round and `overall` must follow from the measured dimensions |

Only dispositions up to `allow_local_model_only` deliver content immediately;
`allow_with_approval` holds it until released.

## Mapping of existing classifications

| Source | Mapping |
|---|---|
| Team, Knowledge, Experiment classification | identical four levels |
| Engine sensitivity `secret` | `restricted` |
| Kernel sensitivity | identical four levels |
| Capsule sensitivity | `public`, `internal`, `restricted` |
| VIA `normal` / `sensitive` / `secret` | `internal` / `confidential` / `restricted` |
| VIA `local_only` | `confidential` + `local_only` |
| VIA `enterprise_private` | `confidential` + `organization_private` |

## Compatibility

Schema version 1 (`schema_version: 1` on passports and receipts). Additive
fields require a new minor revision of this document; any change to an
invariant requires `context-gateway-v2.md`.
