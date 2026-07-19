# Settlement Evidence v2 (`settlement-evidence-v2`)

Status: **stable** · Plane: OSS contract / private policy input · Schema version: 2

Settlement Evidence v2 defines a bounded, payload-free handoff from the open
LeanCTX data plane to a private approval, dispute, settlement, or invoicing
plane. The OSS implementation can canonicalize, reconcile, verify, and export
evidence. It cannot approve a customer, decide a dispute, validate a contract,
calculate a commercial formula, issue an invoice, or mutate private state.

Requirement coverage: CO-09, EV-03, EV-05, EV-06, EV-07, EV-09, EV-10, BC-06.
HC-03/12/13/17, RG-05, and BC-03 remain dependencies or explicit non-claims.

## Compatibility boundary

`billing-plane-v1` is frozen. Its `Usage` JSON shape and
`Usage::is_billable() == signed && chain_valid` behavior remain unchanged.
Settlement Evidence v2 names that predicate
`Usage::source_integrity_verified()`.

A true v1 predicate proves only that the source aggregate reports a valid
signature and chain. It is never sufficient for v2 eligibility. In particular,
it does not prove quality, causality, exclusive attribution, a complete period,
contract validity, customer approval, a settlement amount, or invoice
authority.

The committed `legacy-usage-v1.json` fixture pins the original ten-field wire
shape and both methods to the exact same boolean.

## OSS/private boundary

Open and Apache-2.0:

- versioned Rust types and deterministic reconciliation;
- canonical BLAKE3 content addresses;
- bounded offline load, verify, and export;
- payload-free positive and adversarial fixtures;
- typed ineligibility reasons.

Private and absent from this repository:

- customer approval or dispute state mutation;
- approval authority and trust-anchor administration;
- schedules, caps, price formulas, deal economics, and contract interpretation;
- invoice creation, payment processing, or settlement execution.

Private services consume this contract over the published service/SDK boundary;
they do not import internal runtime modules.

## Inputs

Verification takes two independent files:

1. a `SettlementEvidenceManifestV2`; and
2. an out-of-band `SettlementEvidenceTrustStoreV2` pinned by the verifier.

A manifest cannot make itself trusted. Each active item must name a
`trust_decision_id` and `trust_anchor_id`, and the exact
`(evidence_id, trust_decision_id, trust_anchor_id)` tuple must appear in the
separate trust store. Trust-store provenance remains caller-owned; the OSS
verifier checks content-addressed consistency, not legal authority.

Both top-level types and every nested type deny unknown fields.

## Manifest

Normative top-level fields:

| Field | Rule |
|---|---|
| `schema_version` | exactly `2` |
| `kind` | exactly `lean-ctx.settlement-evidence` |
| `manifest_id` | `manifest:blake3:<64 lowercase hex>` over canonical manifest with a pending ID |
| `subject_id` | opaque `subject:blake3:<64 lowercase hex>`; never a person, account name, or path |
| `period` | non-negative epoch-second start and strictly later end |
| `currency` | exactly three uppercase ASCII letters representing the caller's ISO 4217 alpha code |
| `claimed_amount_minor_units` | unsigned integer minor units; no float |
| `evidence[]` | at most 1,000 payload-free items |

Each item is independently content-addressed as
`artifact:blake3:<64 lowercase hex>`. Its ID commits to subject, state, trust
references, typed measurement method/class, typed claim, correction references,
and correction-reason ID.

## Required evidence roles

Eligibility requires active, externally trusted evidence for every role:

| Role | Required claim |
|---|---|
| `baseline` | content-addressed baseline version and integer baseline tokens |
| `price` | content-addressed price version, matching currency, integer micro-unit price |
| `contract` | content-addressed contract version |
| `quality` | content-addressed quality gate with `passed=true` |
| `attribution` | one or more exclusive mechanism claims with integer tokens/minor units and source evidence IDs |
| `period_completion` | exact manifest bounds with `complete=true` |
| `customer_approval` | content-addressed approval artifact with `approved=true` |

All non-attribution roles require exactly one active item. Missing roles fail
closed; multiple active items are ambiguous. Multiple attribution items are
allowed only for distinct mechanism IDs.

Every item also carries a content-addressed `measurement.method_artifact_id`
and an explicit closed evidence class. Baseline and period are `measured`;
quality and attribution are `reconciled`; price, contract, and customer
approval are `declared`. A mismatched, `derived`, or `unknown` class fails
closed. A signature never upgrades the class.

The verifier sums attributed tokens and minor units using checked `u64`
arithmetic. The minor-unit sum must equal `claimed_amount_minor_units`, and
attributed tokens cannot exceed baseline tokens. It does not derive the amount
from price evidence; formulas and contract interpretation remain private.

## Exclusive attribution

Every attribution item must set `exclusive=true`. Each source evidence ID can
appear only once across all active mechanisms. Reuse by another mechanism is
`duplicate_attribution` and makes the manifest ineligible. Total source
references are bounded to 1,000.

This prevents compression, cache, routing, or any later mechanism from claiming
the same source observation twice.

## Correction and supersession lineage

An active correction carries:

- one to 32 sorted `supersedes[]` content addresses; and
- one content-addressed `correction_reason_id`.

Both fields must be present together. Self-reference, duplicate targets,
unbounded targets, invalid IDs, correction fields on non-active evidence, or the
same superseded target claimed by two corrections are invalid lineage.

A manifest item whose own state is `superseded` or `disputed` cannot satisfy
a role and produces a typed reason. A corrected active item can qualify only
when its external trust tuple is independently pinned.

## Determinism and limits

Canonicalization:

1. sort attribution source IDs;
2. sort supersession IDs;
3. sort evidence by its stable evidence ID only;
4. sort trust decisions;
5. serialize compact JSON in struct field order;
6. hash the representation with the corresponding ID set to its pending value.

Input permutations therefore produce identical IDs and canonical exports.
Limits are fail-closed:

- manifest file: 4 MiB;
- evidence items: 1,000;
- trust decisions: 1,000;
- UTF-8 bytes per string: 256;
- attribution source IDs per item: 1,000;
- total attribution source IDs: 1,000;
- supersession references per item: 32.

The same structural and serialized-size preflight applies to constructors,
direct reconciliation, hashing, canonicalization, file loading, and export.
Oversized evidence or trust input therefore returns before cloning, sorting,
hashing, serialization, or unbounded reconciliation traversal.

Offline reads reject symbolic links and non-regular files, open with no-follow
semantics where supported, verify the opened-file metadata, and read at most
the limit plus one byte. Export rejects symbolic-link targets or parents,
writes a same-directory create-new/no-follow temporary file, syncs it, renames
atomically, syncs the parent directory, and removes temporary files on error.

## Eligibility output and non-claims

`SettlementEligibilityV2` binds both `manifest_id` and `trust_store_id`, and
contains deterministic totals plus sorted, deduplicated typed reasons. It also
always emits:

```json
{
  "invoice_authority": false,
  "contract_validity_verified": false,
  "customer_approval_authority_verified": false
}
```

`eligible=true` means only structural completeness and internal consistency
under this contract and the supplied trust store. It is not an approval,
settlement, invoice, contract-validity, or payment claim.

Typed reasons cover unsupported schema/kind, invalid content addresses, invalid
subject/period/currency, limits, trust-store failures, missing or ambiguous
roles, duplicate IDs, untrusted/disputed/superseded evidence, incomplete
period, failed quality, absent approval, currency mismatch, non-exclusive or
duplicate attribution, invalid correction lineage, amount mismatch, baseline
overrun, and arithmetic overflow.

Signature-valid or chain-valid input alone remains ineligible.

## CLI

Read-only offline verification:

```text
lean-ctx billing settlement verify \
  <manifest.json> <trust-store.json> [--json]
```

Canonical export after content-address verification:

```text
lean-ctx billing settlement export \
  <manifest.json> <trust-store.json> <canonical.json> [--json]
```

Export does not change approval, dispute, settlement, ledger, or local runtime
state.

## Fixtures and gates

- `rust/tests/fixtures/settlement-evidence-v2/eligible.json`
- `rust/tests/fixtures/settlement-evidence-v2/trusted-decisions.json`
- `rust/tests/fixtures/settlement-evidence-v2/legacy-usage-v1.json`
- `rust/tests/settlement_evidence_v2_gate.rs`

The gate covers canonical/permutation stability, v1 compatibility, out-of-band
trust, all fail-closed evidence states, signed-chain insufficiency, exclusive
attribution, checked overflow, limits, unknown fields, correction lineage,
offline export, and payload/PII/path/secret-free fixtures.
