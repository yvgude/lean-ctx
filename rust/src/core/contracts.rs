use std::collections::BTreeMap;

// Machine-verified contract versions.
pub const MCP_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const CONTEXT_PROOF_V1_SCHEMA_VERSION: u32 = 1;
pub const CONTEXT_IR_V1_SCHEMA_VERSION: u32 = 1;
pub const INTENT_ROUTE_V1_SCHEMA_VERSION: u32 = 1;
pub const DEGRADATION_POLICY_V1_SCHEMA_VERSION: u32 = 1;
pub const WORKFLOW_EVIDENCE_LEDGER_V1_SCHEMA_VERSION: u32 = 1;
pub const AUTONOMY_DRIVERS_V1_SCHEMA_VERSION: u32 = 1;
pub const TOKENIZER_TRANSLATION_DRIVER_V1_SCHEMA_VERSION: u32 = 1;
pub const ATTENTION_LAYOUT_DRIVER_V1_SCHEMA_VERSION: u32 = 1;
pub const VERIFICATION_OBSERVABILITY_V1_SCHEMA_VERSION: u32 = 1;
pub const HANDOFF_LEDGER_V1_SCHEMA_VERSION: u32 = 1;
pub const HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION: u32 = 1;
pub const CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION: u32 = 1;
pub const KNOWLEDGE_POLICY_V1_SCHEMA_VERSION: u32 = 1;
pub const GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION: u32 = 1;
pub const A2A_SNAPSHOT_V1_SCHEMA_VERSION: u32 = 1;
pub const MEMORY_BOUNDARY_V1_SCHEMA_VERSION: u32 = 1;
pub const GOTCHAS_REMINDERS_V1_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_FRAMEWORK_V1_SCHEMA_VERSION: u32 = 1;
pub const CONTEXT_PACKAGE_V1_SCHEMA_VERSION: u32 = 1;
pub const CONTEXT_PACKAGE_V2_SCHEMA_VERSION: u32 = 2;

pub const PACKAGE_EXTENSION: &str = "ctxpkg";
pub const LEGACY_PACKAGE_EXTENSION: &str = "lctxpkg";
pub const MAX_PACKAGE_FILE_BYTES: u64 = 10 * 1024 * 1024; // 10 MB

#[must_use]
pub fn is_package_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext == PACKAGE_EXTENSION || ext == LEGACY_PACKAGE_EXTENSION)
}

#[must_use]
pub fn default_package_filename(name: &str, version: &str) -> String {
    format!("{name}-{version}.{PACKAGE_EXTENSION}")
}

// Documentation-level contracts (do not have a schema field in payloads).
pub const HTTP_MCP_CONTRACT_VERSION: u32 = 1;
pub const TEAM_SERVER_CONTRACT_VERSION: u32 = 1;
pub const CAPABILITIES_CONTRACT_VERSION: u32 = 1;

/// Stability classification of a contract document (GL #394).
///
/// The classification is normative — `tests/contracts_frozen.rs` enforces it:
/// * `Frozen` — the normative surface is immutable. Any change to the doc file
///   fails CI; semantic evolution requires a new `-v2.md` file (the v1 file
///   stays in place for existing integrations).
/// * `Stable` — additive evolution allowed (new optional fields, new sections);
///   breaking changes still require a version bump per CONTRACTS.md rules.
/// * `Experimental` — may change or disappear without notice; not covered by
///   the deprecation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractStatus {
    Frozen,
    Stable,
    Experimental,
}

impl ContractStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ContractStatus::Frozen => "frozen",
            ContractStatus::Stable => "stable",
            ContractStatus::Experimental => "experimental",
        }
    }
}

/// One contract document under `docs/contracts/`, classified for the
/// stability matrix in CONTRACTS.md and the `/v1/capabilities` response.
pub struct ContractDoc {
    /// Short stable identifier (used in capabilities `contract_status`).
    pub id: &'static str,
    /// File name inside `docs/contracts/` (the normative artifact).
    pub doc_file: &'static str,
    pub version: u32,
    pub status: ContractStatus,
}

/// The complete classified inventory of `docs/contracts/*.md` — the single
/// source of truth for the stability matrix. `tests/contracts_frozen.rs`
/// asserts that every file in the directory is listed here (no contract can
/// stay unclassified) and that frozen docs never change.
#[must_use]
pub fn contract_docs() -> Vec<ContractDoc> {
    use ContractStatus::{Experimental, Frozen, Stable};
    let doc = |id, doc_file, version, status| ContractDoc {
        id,
        doc_file,
        version,
        status,
    };
    vec![
        // ── Frozen: externally consumed platform/transport promises ────────
        doc("http-mcp", "http-mcp-contract-v1.md", 1, Frozen),
        doc("team-server", "team-server-contract-v1.md", 1, Frozen),
        doc("context-ir", "context-ir-v1.md", 1, Frozen),
        doc(
            "local-free-invariant",
            "local-free-invariant-v1.md",
            1,
            Frozen,
        ),
        doc(
            "oss-plane-separation",
            "oss-plane-separation-v1.md",
            1,
            Frozen,
        ),
        doc("billing-plane", "billing-plane-v1.md", 1, Frozen),
        doc("wasm-abi", "wasm-abi-v1.md", 1, Frozen),
        // ── Stable: additive evolution allowed ──────────────────────────────
        // capabilities is additive BY DESIGN: its drift test binds the doc's
        // key list to TOP_LEVEL_KEYS, so the doc grows with every new key —
        // freezing the file would contradict its own contract.
        doc("capabilities", "capabilities-contract-v1.md", 1, Stable),
        doc("billing-plane-v2", "billing-plane-v2.md", 2, Stable),
        // v2 = v1 + storageQuotaBytes/roiWebhookUrl (GL #387/#388); v1 stays frozen.
        doc("billing-plane-v3", "billing-plane-v3.md", 3, Stable),
        // v3 = v1 + business plan + sso_oidc entitlement (GL #460/#533); additive.
        doc("evidence-bundle", "evidence-bundle-v1.md", 1, Stable),
        // Offline-verifiable audit evidence ZIP (GL #425, H3 Epic A).
        doc("team-server-v2", "team-server-contract-v2.md", 2, Stable),
        doc("a2a", "a2a-contract-v1.md", 1, Stable),
        doc(
            "attention-layout-driver",
            "attention-layout-driver-v1.md",
            1,
            Stable,
        ),
        doc("autonomy-drivers", "autonomy-drivers-v1.md", 1, Stable),
        doc("ccp-session-bundle", "ccp-session-bundle-v1.md", 1, Stable),
        doc("conformance", "conformance-v1.md", 1, Stable),
        doc("degradation-policy", "degradation-policy-v1.md", 1, Stable),
        doc("extension-trust", "extension-trust-v1.md", 1, Stable),
        doc("extractors", "extractors-v1.md", 1, Stable),
        doc(
            "gotchas-reminders",
            "gotchas-reminders-contract-v1.md",
            1,
            Stable,
        ),
        doc(
            "graph-reproducibility",
            "graph-reproducibility-contract-v1.md",
            1,
            Stable,
        ),
        doc(
            "handoff-transfer-bundle",
            "handoff-transfer-bundle-v1.md",
            1,
            Stable,
        ),
        doc("intent-route", "intent-route-v1.md", 1, Stable),
        doc(
            "knowledge-policy",
            "knowledge-policy-contract-v1.md",
            1,
            Stable,
        ),
        doc(
            "memory-boundary",
            "memory-boundary-contract-v1.md",
            1,
            Stable,
        ),
        doc("persona-spec", "persona-spec-v1.md", 1, Stable),
        doc(
            "provider-framework",
            "provider-framework-contract-v1.md",
            1,
            Stable,
        ),
        doc(
            "tokenizer-translation-driver",
            "tokenizer-translation-driver-v1.md",
            1,
            Stable,
        ),
        doc(
            "workflow-evidence-ledger",
            "workflow-evidence-ledger-v1.md",
            1,
            Stable,
        ),
        doc("wrapped-permalink", "wrapped-permalink-v1.md", 1, Stable),
        // Community addon manifest (#858): self-declared stable (v1); the format
        // evolves additively (new optional fields), so Stable, not Frozen.
        doc("addon-manifest", "addon-manifest-v1.md", 1, Stable),
        // ── Experimental: may change without notice ─────────────────────────
        doc(
            "hosted-personal-index",
            "hosted-personal-index-v1.md",
            1,
            Experimental,
        ),
        doc(
            "personal-cloud-encryption",
            "personal-cloud-encryption-v1.md",
            1,
            Experimental,
        ),
        // 2026-06 org/cloud-plane wave — fresh surfaces, not yet consumed by
        // external integrations; promote to Stable deliberately, not by default.
        doc(
            "context-policy-packs",
            "context-policy-packs-v1.md",
            1,
            Experimental,
        ),
        doc("device-overview", "device-overview-v1.md", 1, Experimental),
        doc("email-digest", "email-digest-v1.md", 1, Experimental),
        doc("org-audit-log", "org-audit-log-v1.md", 1, Experimental),
        doc("org-sso-oidc", "org-sso-oidc-v1.md", 1, Experimental),
        // Quality loop (GL #494): edit-failure feedback into mode selection.
        doc("quality-loop", "quality-loop-v1.md", 1, Experimental),
        // Hosted ctxpkg registry (GL #406): fresh server surface.
        doc("ctxpkg-registry", "ctxpkg-registry-v1.md", 1, Experimental),
        doc(
            "team-invite-links",
            "team-invite-links-v1.md",
            1,
            Experimental,
        ),
        // Org policy & compliance surfaces, still evolving with the Enterprise
        // plane — Experimental until they stabilise. Commercial Enterprise
        // licensing (#667) and success-fee billing (#669) live in the private
        // cloud plane, not in the open engine (oss-plane-separation-v1).
        doc("org-policy", "org-policy-v1.md", 1, Experimental),
        doc(
            "compliance-report",
            "compliance-report-v1.md",
            1,
            Experimental,
        ),
    ]
}

/// Contract-id → stability status, exported through `/v1/capabilities` so
/// clients can verify compatibility before relying on a surface (GL #394).
#[must_use]
pub fn status_kv() -> BTreeMap<&'static str, &'static str> {
    contract_docs()
        .into_iter()
        .map(|d| (d.id, d.status.as_str()))
        .collect()
}

#[must_use]
pub fn versions_kv() -> BTreeMap<&'static str, u32> {
    BTreeMap::from([
        (
            "leanctx.contract.mcp_manifest.schema_version",
            MCP_MANIFEST_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.context_proof_v1.schema_version",
            CONTEXT_PROOF_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.context_ir_v1.schema_version",
            CONTEXT_IR_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.intent_route_v1.schema_version",
            INTENT_ROUTE_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.degradation_policy_v1.schema_version",
            DEGRADATION_POLICY_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.workflow_evidence_ledger_v1.schema_version",
            WORKFLOW_EVIDENCE_LEDGER_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.autonomy_drivers_v1.schema_version",
            AUTONOMY_DRIVERS_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.tokenizer_translation_driver_v1.schema_version",
            TOKENIZER_TRANSLATION_DRIVER_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.attention_layout_driver_v1.schema_version",
            ATTENTION_LAYOUT_DRIVER_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.verification_observability_v1.schema_version",
            VERIFICATION_OBSERVABILITY_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.handoff_ledger_v1.schema_version",
            HANDOFF_LEDGER_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.handoff_transfer_bundle_v1.schema_version",
            HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.ccp_session_bundle_v1.schema_version",
            CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.knowledge_policy_v1.schema_version",
            KNOWLEDGE_POLICY_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.graph_reproducibility_v1.schema_version",
            GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.a2a_snapshot_v1.schema_version",
            A2A_SNAPSHOT_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.memory_boundary_v1.schema_version",
            MEMORY_BOUNDARY_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.gotchas_reminders_v1.schema_version",
            GOTCHAS_REMINDERS_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.provider_framework_v1.schema_version",
            PROVIDER_FRAMEWORK_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.context_package_v1.schema_version",
            CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.context_package_v2.schema_version",
            CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
        ),
        (
            "leanctx.contract.http_mcp.contract_version",
            HTTP_MCP_CONTRACT_VERSION,
        ),
        (
            "leanctx.contract.team_server.contract_version",
            TEAM_SERVER_CONTRACT_VERSION,
        ),
        (
            "leanctx.contract.capabilities.contract_version",
            CAPABILITIES_CONTRACT_VERSION,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_docs_have_unique_ids_and_files() {
        let docs = contract_docs();
        let mut ids: Vec<_> = docs.iter().map(|d| d.id).collect();
        let mut files: Vec<_> = docs.iter().map(|d| d.doc_file).collect();
        ids.sort_unstable();
        files.sort_unstable();
        let unique_ids: std::collections::BTreeSet<_> = ids.iter().collect();
        let unique_files: std::collections::BTreeSet<_> = files.iter().collect();
        assert_eq!(unique_ids.len(), docs.len(), "duplicate contract id");
        assert_eq!(unique_files.len(), docs.len(), "duplicate doc file");
    }

    #[test]
    fn frozen_set_covers_the_platform_promises() {
        // The freeze (GL #394) is only meaningful if the externally consumed
        // surfaces are actually in it. Removing one of these from `Frozen`
        // is itself a breaking policy change.
        let docs = contract_docs();
        for id in [
            "http-mcp",
            "team-server",
            "context-ir",
            "local-free-invariant",
            "oss-plane-separation",
            "billing-plane",
            "wasm-abi",
        ] {
            let entry = docs.iter().find(|d| d.id == id).expect("listed");
            assert_eq!(
                entry.status,
                ContractStatus::Frozen,
                "{id} must stay frozen"
            );
        }
    }

    #[test]
    fn status_kv_matches_docs() {
        let kv = status_kv();
        assert_eq!(kv.len(), contract_docs().len());
        assert_eq!(kv["http-mcp"], "frozen");
        assert_eq!(kv["hosted-personal-index"], "experimental");
        assert_eq!(kv["personal-cloud-encryption"], "experimental");
    }

    #[test]
    fn doc_files_follow_versioned_naming() {
        // v1→v2 rule: every doc file carries its version suffix so a breaking
        // change lands as a NEW file instead of mutating the old one.
        for d in contract_docs() {
            assert!(
                d.doc_file.ends_with(&format!("-v{}.md", d.version)),
                "{} must end in -v{}.md",
                d.doc_file,
                d.version
            );
        }
    }
}
