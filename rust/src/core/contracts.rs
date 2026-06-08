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

pub fn is_package_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext == PACKAGE_EXTENSION || ext == LEGACY_PACKAGE_EXTENSION)
}

pub fn default_package_filename(name: &str, version: &str) -> String {
    format!("{name}-{version}.{PACKAGE_EXTENSION}")
}

// Documentation-level contracts (do not have a schema field in payloads).
pub const HTTP_MCP_CONTRACT_VERSION: u32 = 1;
pub const TEAM_SERVER_CONTRACT_VERSION: u32 = 1;
pub const CAPABILITIES_CONTRACT_VERSION: u32 = 1;

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
