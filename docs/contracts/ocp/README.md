# OCP schema mirror

Vendored copy of the Open Context Protocol v0.1-draft JSON Schemas.
**Source of truth:** the `open-context-protocol` repository
(`spec/schemas/`). Do not edit here — update upstream via the OCP RFC
process, then re-vendor.

Purpose: `tests/ocp_self_validation.rs` proves on every CI run that
LeanCTX's real wire output (Context-IR, capability grants/checks, policy
packs, evidence entries, governance events via `core/ocp.rs`) validates
against the published spec — LeanCTX is the OCP reference implementation
(GL #430, H3 Epic C).
