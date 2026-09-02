# Workflow Evidence Ledger v1 (EvidenceLedgerV1)

> **Status: historical research contract — unavailable as a public workflow or
> evidence product.** The local Runtime has receipt and offline-verification
> primitives; a workflow system and canonical evidence bundle remain Research.
> Current product scope and status are governed by
> `docs/internal/README.md` (internal, not in this repository).

GitLab: `#2315`

This archived design lets experimental LeanCTX workflows couple transitions to
**Evidence Keys** (evidence-gated transitions). Evidence Ledger v1 describes
how those items could be stored, hashed, and evaluated for gates.

## Ziele

- Deterministisch: gleiche Inputs → gleiche Evidence IDs/Keys.
- Auditierbar: Evidence ist **content-addressed** (MD5) und bounded.
- Privacy-first: keine Secrets; Values werden redacted und nur als Hash + kurzer Excerpt gespeichert.
- Kompatibel: bestehendes Workflow-Verhalten bleibt nutzbar; Ledger ergänzt/vereinheitlicht.

## Speicherort

- Global (local-first): `~/.lean-ctx/workflows/evidence-ledger-v1.json`

## Schema (v1)

- `schema_version` (SSOT: `leanctx.contract.workflow_evidence_ledger_v1.schema_version=1`)
- `items[]` mit:
  - `kind`: `tool_receipt|manual|proof_artifact|ci_receipt`
  - `key`: Evidence Key (z.B. `tool:ctx_shell`)
  - `id`: deterministic content hash
  - optional: `input_md5`, `output_md5`, `agent_id`, `client_name`
  - optional: `value_md5`, `value_excerpt` (redacted + bounded)
  - optional: `artifact_name` (basename only)

## Gating semantics

- Ein Transition Requirement (`requires_evidence`) ist erfüllt, wenn mindestens ein Ledger Item mit identischem `key` existiert.
- Default Workflows verwenden Tool-Receipts (z.B. `tool:ctx_read`, `tool:ctx_shell`) als Evidence.

## Automatic evidence

- Tool calls erzeugen automatisch `tool:{tool}` und `tool:{tool}:{action}` Evidence Keys (z.B. `tool:ctx_read:full`).
- `ctx_workflow evidence_add` schreibt manual Evidence in den Ledger.
- `ctx_proof` schreibt Proof-Artefakte als `proof:*` Evidence (basename + md5).

## Relevanter Code

- Ledger: `rust/src/core/evidence_ledger.rs`
- Workflow Tool: `rust/src/tools/ctx_workflow.rs`
- Tool receipts boundary: `rust/src/server/mod.rs`
- Proof export wiring: `rust/src/tools/ctx_proof.rs`
