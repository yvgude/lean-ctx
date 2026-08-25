//! Context Control Kernel — unified orchestration over all context stores.

use crate::core::knowledge::KnowledgeQuery;
use crate::core::knowledge::snapshot::{KnowledgeRef, KnowledgeSnapshot};
use crate::core::knowledge::store::KnowledgeStore;

const KNOWLEDGE_POLICY_VERSION: &str = "knowledge-task-flow-v1";

/// Mutable task context assembled from technical and organisational sources.
#[derive(Debug, Clone)]
pub struct ContextState {
    pub technical_refs: Vec<String>,
    pub knowledge_query: KnowledgeQuery,
    pub knowledge_snapshot: Option<KnowledgeSnapshot>,
}

impl ContextState {
    #[must_use]
    pub fn new(technical_refs: Vec<String>, knowledge_query: KnowledgeQuery) -> Self {
        Self {
            technical_refs,
            knowledge_query,
            knowledge_snapshot: None,
        }
    }
}

/// Query governed knowledge and attach a deterministic task-local snapshot.
pub fn enrich_with_knowledge(
    task_id: &str,
    context: &mut ContextState,
    store: &dyn KnowledgeStore,
) -> Vec<KnowledgeRef> {
    let items = store.query(&context.knowledge_query);
    let snapshot = KnowledgeSnapshot::from_items(task_id, KNOWLEDGE_POLICY_VERSION, &items);
    let references = snapshot.knowledge_refs.clone();
    context.knowledge_snapshot = Some(snapshot);
    references
}

/// Preserve normal task execution when no Knowledge Hub is configured.
pub fn enrich_with_optional_knowledge(
    task_id: &str,
    context: &mut ContextState,
    store: Option<&dyn KnowledgeStore>,
) -> Vec<KnowledgeRef> {
    store.map_or_else(Vec::new, |store| {
        enrich_with_knowledge(task_id, context, store)
    })
}

pub mod a2a_fixes;
pub mod accounting_fix;
pub mod activation;
pub(crate) mod activation_e2e;
pub mod adaptive_bridge;
pub mod adaptive_hook;
pub(crate) mod airgap_e2e;
pub mod attribution;
pub(crate) mod bench;
pub mod bounded;
pub mod bridge;
pub(crate) mod bridge_e2e;
pub mod capsule_wire;
pub(crate) mod client_e2e;
pub mod client_profile;
pub mod client_wiring;
pub mod config_bridge;
pub(crate) mod conformance;
pub mod context_broker;
pub mod context_dedup;
pub mod coverage_class;
pub mod ctx_read_dedup;
pub mod dashboard_report;
pub mod dedup_wiring;
pub mod degradation;
pub mod enforce;
pub mod envelope_bridge;
pub(crate) mod envelope_e2e;
pub(crate) mod envelope_wiring;
pub mod etpao;
pub mod etpao_live;
pub mod evidence_hook;
pub mod evidence_wiring;
pub mod feedback;
pub(crate) mod feedback_e2e;
pub mod health;
pub mod health_api;
pub mod hotpath_wiring;
pub mod identity;
pub(crate) mod identity_resolver;
pub(crate) mod integration_e2e;
pub mod invalidation;
pub mod kernel_config;
pub mod knowledge_health;
pub mod learning;
pub mod list_tools_opt;
pub mod live_dashboard;
pub mod mcp_bridge;
pub mod mcp_coverage;
pub(crate) mod mcp_e2e;
pub mod mcp_schema_opt;
pub(crate) mod multi_agent_e2e;
pub mod orchestrator;
pub mod outcome_signal;
pub(crate) mod perf_benchmark;
pub mod policy;
pub mod policy_engine;
pub(crate) mod production_e2e;
pub mod provider_display;
pub(crate) mod provider_metrics_e2e;
pub mod provider_normalization;
pub mod provider_parity;
pub(crate) mod provider_traces;
pub(crate) mod providers;
pub mod proxy_bridge;
pub(crate) mod quality_e2e;
pub mod receipt_builder;
pub mod receipt_chain;
pub mod recovery;
pub mod response_evidence;
pub mod result_fusion;
pub mod schema_wiring;
pub(crate) mod smoke_test;
pub mod startup;
pub mod stream_controller;
pub(crate) mod token_envelope;
pub mod tool_surface;
pub mod types;
pub mod usage_normalizer;
pub(crate) mod wiring_e2e;
