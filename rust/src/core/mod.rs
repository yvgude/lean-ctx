// ---------------------------------------------------------------------------
// Domain: Compression
// ---------------------------------------------------------------------------
pub mod adaptive_chunking;
pub mod addons;
pub mod aggressiveness;
pub mod attention_context;
pub mod auto_capture;
pub mod auto_findings;
pub mod codebook;
#[cfg(target_os = "macos")]
pub mod codesign;
pub mod compress_preview;
pub mod compression_safety;
pub mod compressor;
pub mod datadog_push;
pub mod entropy;
pub mod eval_ab;
pub mod eval_harness;
pub mod extractive;
pub mod finops_export;
pub mod information_bottleneck;
pub mod json_crush;
pub mod output_sanitizer;
pub mod policy;
pub mod pop_pruning;
pub mod predictive_coding;
pub mod predictive_prefetch;
pub mod preservation;
pub mod process_guard;
pub mod progressive_compression;
pub mod protect;
pub mod rabin_karp;
pub mod rules_canonical;
pub mod rules_channel;
pub mod rules_overhead;
pub mod structural_tokenizer;
pub mod structured_read;
pub mod tabular_crush;
pub mod yaml_crush;

/// Convenience re-export: all compression-related modules.
pub mod compression {
    pub use super::adaptive_chunking;
    pub use super::codebook;
    pub use super::compression_safety;
    pub use super::compressor;
    pub use super::entropy;
    pub use super::information_bottleneck;
    pub use super::json_crush;
    pub use super::pop_pruning;
    pub use super::preservation;
    pub use super::progressive_compression;
    pub use super::rabin_karp;
    pub use super::structural_tokenizer;
}

// ---------------------------------------------------------------------------
// Domain: Memory
// ---------------------------------------------------------------------------
pub mod episodic_memory;
pub mod memory_archive;
pub mod memory_boundary;
pub mod memory_capacity;
pub mod memory_consolidation;
pub mod memory_guard;
pub mod memory_lifecycle;
pub mod memory_policy;
pub mod memory_salience;
pub mod multiscale_index;
pub mod procedural_memory;
pub mod prospective_memory;

/// Convenience re-export: all memory-related modules.
pub mod memory {
    pub use super::episodic_memory;
    pub use super::memory_boundary;
    pub use super::memory_consolidation;
    pub use super::memory_lifecycle;
    pub use super::memory_policy;
    pub use super::procedural_memory;
    pub use super::prospective_memory;
}

// ---------------------------------------------------------------------------
// Domain: Graph
// ---------------------------------------------------------------------------
pub mod call_graph;
pub mod community;
pub mod gamma_cover;
pub mod graph_analysis;
pub mod graph_context;
pub mod graph_coordinator;
pub mod graph_enricher;
pub mod graph_export;
pub mod graph_features;
pub mod graph_index;
pub mod graph_parity;
pub mod graph_provider;
pub mod pagerank;
pub mod property_graph;
pub mod repomap;

/// Convenience re-export: all graph-related modules.
pub mod graph {
    pub use super::call_graph;
    pub use super::community;
    pub use super::gamma_cover;
    pub use super::graph_context;
    pub use super::graph_enricher;
    pub use super::graph_export;
    pub use super::graph_features;
    pub use super::graph_index;
    pub use super::graph_provider;
    pub use super::pagerank;
    pub use super::property_graph;
}

// ---------------------------------------------------------------------------
// Domain: Context
// ---------------------------------------------------------------------------
pub mod context_artifacts;
pub mod context_column;
pub mod context_compiler;
pub mod context_deficit;
pub mod context_field;
pub mod context_handles;
pub mod context_ir;
pub mod context_ledger;
pub mod context_lint;
pub mod context_os;
pub mod context_overhead;
pub mod context_overlay;
pub mod context_package;
pub mod context_policies;
pub mod context_proof;
pub mod context_proof_v2;
pub mod context_radar;
pub mod context_snapshot;
pub mod cross_source_edges;
pub mod cross_source_hints;

/// Convenience re-export: all context-related modules.
pub mod context {
    pub use super::context_artifacts;
    pub use super::context_column;
    pub use super::context_compiler;
    pub use super::context_deficit;
    pub use super::context_field;
    pub use super::context_handles;
    pub use super::context_ir;
    pub use super::context_ledger;
    pub use super::context_os;
    pub use super::context_overlay;
    pub use super::context_package;
    pub use super::context_policies;
    pub use super::context_proof;
    pub use super::context_proof_v2;
}

// ---------------------------------------------------------------------------
// Domain: Knowledge
// ---------------------------------------------------------------------------
pub mod claim_extractor;
pub mod cognition_loop;
pub mod cognition_scheduler;
pub mod knowledge;
pub mod knowledge_bootstrap;
pub mod knowledge_bridge;
pub mod knowledge_embedding;
pub mod knowledge_provider_extract;
pub mod knowledge_relations;

/// Convenience re-export: all knowledge-related modules.
pub mod knowledge_domain {
    pub use super::claim_extractor;
    pub use super::cognition_loop;
    pub use super::knowledge;
    pub use super::knowledge_bootstrap;
    pub use super::knowledge_bridge;
    pub use super::knowledge_embedding;
    pub use super::knowledge_relations;
}

// ---------------------------------------------------------------------------
// Domain: Search & Retrieval
// ---------------------------------------------------------------------------
pub mod bm25_cache;
pub mod bm25_index;
pub mod content_cache;
pub mod content_chunk;
pub mod context_packing;
pub mod cooccurrence;
pub mod dense_backend;
pub mod embedding_index;
pub mod embedding_quant;
pub mod embeddings;
pub mod energy;
pub mod hybrid_search;
#[cfg(feature = "qdrant")]
pub mod qdrant_store;
pub mod search_reranking;
pub mod semantic_cache;
pub mod semantic_chunks;
pub mod splade_retrieval;
pub mod spreading_activation;

/// Convenience re-export: all search-related modules.
pub mod search {
    pub use super::bm25_index;
    pub use super::content_chunk;
    pub use super::dense_backend;
    pub use super::embedding_index;
    pub use super::embeddings;
    pub use super::hybrid_search;
    pub use super::search_reranking;
    pub use super::semantic_cache;
    pub use super::semantic_chunks;
    pub use super::splade_retrieval;
}

// ---------------------------------------------------------------------------
// Domain: Session & Handoff
// ---------------------------------------------------------------------------
pub mod ccp_session_bundle;
pub mod handoff_ledger;
pub mod handoff_transfer_bundle;
pub mod session;
pub mod session_diff;
pub mod session_summary;
pub mod skillify;

/// Convenience re-export: all session-related modules.
pub mod session_domain {
    pub use super::ccp_session_bundle;
    pub use super::handoff_ledger;
    pub use super::handoff_transfer_bundle;
    pub use super::session;
    pub use super::session_diff;
}

// ---------------------------------------------------------------------------
// Domain: Attention & Placement
// ---------------------------------------------------------------------------
pub mod attention_layout_driver;
pub mod attention_model;
pub mod attention_placement;
pub mod litm;

/// Convenience re-export: all attention-related modules.
pub mod attention {
    pub use super::attention_layout_driver;
    pub use super::attention_model;
    pub use super::attention_placement;
    pub use super::litm;
}

// ---------------------------------------------------------------------------
// Domain: Neural / ML
// ---------------------------------------------------------------------------
pub mod neural;
// ORT runtime glue links against the `ort` crate, which is only pulled in by the
// `embeddings` or `neural` features. On platforms ORT does not support (e.g.
// FreeBSD, see #586) these features are disabled, so the modules must be gated
// to keep the build clean without them.
#[cfg(any(feature = "embeddings", feature = "neural"))]
pub mod ort_environment;
#[cfg(any(feature = "embeddings", feature = "neural"))]
pub mod ort_execution_providers;

// ---------------------------------------------------------------------------
// Domain: Patterns & Shell
// ---------------------------------------------------------------------------
pub mod patterns;

// ---------------------------------------------------------------------------
// Domain: Agents & A2A
// ---------------------------------------------------------------------------
pub mod a2a;
pub mod a2a_transport;
pub mod agent_identity;
pub mod agent_runtime_env;
pub mod agents;
pub mod autonomy_drivers;

// ---------------------------------------------------------------------------
// Domain: Adaptive & Scoring
// ---------------------------------------------------------------------------
pub mod adaptive;
pub mod adaptive_mode_policy;
pub mod adaptive_thresholds;
pub mod auto_mode_resolver;
pub mod bandit;
pub mod litm_calibration;
pub mod mode_predictor;
pub mod model_registry;
pub mod task_relevance;

// ---------------------------------------------------------------------------
// Domain: Diagnostics & Quality
// ---------------------------------------------------------------------------
pub mod anomaly;
pub mod benchmark;
pub mod benchmark_compare;
/// Commercial-plane billing substrate (`billing-plane-v1`): plans, entitlements,
/// and usage metering derived from the signed savings ledger. Never gates local.
pub mod billing;
pub mod code_health;
pub mod cognitive_load;
pub mod conformance;
pub mod contracts;
pub mod cyclomatic;
pub mod degradation_policy;
pub mod loop_detection;
pub mod output_verification;
pub mod quality;
pub mod safety_needles;
pub mod scorecard;
pub mod setup_report;
pub mod slo;
pub mod slow_log;
pub mod smells;
pub mod subagent_contract;
pub mod surprise;
pub mod verification_observability;

// ---------------------------------------------------------------------------
// Domain: Config & Infrastructure
// ---------------------------------------------------------------------------
pub mod active_inference;
pub mod agent_budget;
pub mod anchor;
pub mod ann_cache;
pub mod atomic_fs;
pub mod audit_trail;
pub mod binary_detect;
pub mod bounce_tracker;
pub mod budget_tracker;
pub mod budgets;
pub mod cache;
pub mod cache_telemetry;
pub mod capabilities;
pub mod cli_cache;
pub mod client_capabilities;
pub mod client_constraints;
pub mod cloud_files;
pub mod config;
pub mod config_heal;
pub mod consolidation;
pub mod consolidation_engine;
pub mod contextops;
pub mod conversation;
pub mod crash_log;
pub mod data_consolidate;
pub mod data_dir;
pub mod debug_log;
pub mod diagnostics_store;
pub mod editor_signal;
pub mod egress;
pub mod error;
pub mod events;
pub mod eviction_orchestrator;
pub mod evidence;
pub mod evidence_ledger;
pub mod extension_registry;
pub mod extractors;
pub mod feedback;
pub mod fep_prefetch;
pub mod filters;
pub mod free_energy_budget;
pub mod gain;
pub mod gateway;
pub mod git;
pub mod git_cache;
pub mod git_signals;
pub mod git_util;
pub mod godot;
pub mod gotcha_tracker;
pub mod hasher;
pub mod heatmap;
pub mod hebbian_cache;
pub mod hnsw;
pub mod home;
pub mod homeostasis;
pub mod immune_detector;
pub mod qubo_select;

pub mod agent_registry;
pub mod compliance;
pub mod compliance_report;
pub mod edit_quality;
pub mod efficacy;
pub mod evidence_bundle;
pub mod graph_cache;
pub mod ide_permissions;
pub mod import_resolver;
pub mod index_bundle;
pub mod index_namespace;
pub mod index_orchestrator;
pub mod index_paths;
pub mod ingestion;
pub mod input_filters;
pub mod instruction_compiler;
pub mod integrity;
pub mod intent_engine;
pub(crate) mod intent_lang;
pub mod intent_protocol;
pub mod intent_router;
pub mod introspect;
pub mod io_boundary;
pub mod io_health;
pub mod journal;
pub mod jsonc;
pub mod knowledge_vault;
pub mod language_capabilities;
#[cfg(target_os = "macos")]
pub mod launchd;
pub mod layout_pin;
pub mod learning_sync;
pub mod limits;
pub mod llm_enhance;
pub mod llm_feedback;
pub mod locomo;
pub mod logging;
pub mod mcp_manifest;
pub mod mdl_selector;
pub mod multi_repo;
pub mod nc_compress;
pub mod ocp;
pub mod openapi;
pub mod output_echo;
pub mod owasp_alignment;
pub mod path_locks;
pub mod path_mode_memory;
pub mod path_resolve;
pub mod paths;
pub mod pathutil;
pub mod persona;
pub mod pipeline;
pub mod plugins;
pub mod portable_binary;
pub mod profile_suggest;
pub mod profiles;
pub mod project_hash;
pub mod protocol;
pub mod provider_bandit;
pub mod provider_cache;
pub mod providers;
pub mod read_stub_index;
pub mod redaction;
pub mod reference_docs;
pub mod roles;
pub mod route_extractor;
pub mod saliency;
pub mod sandbox;
#[cfg(target_os = "linux")]
pub mod sandbox_landlock;
pub mod sandbox_seatbelt;
pub mod sanitize;
pub mod savings_autopush;
pub mod savings_footer;
pub mod savings_ledger;
pub mod scent_field;
pub mod search_delta;
pub mod search_index;
pub mod secret_detection;
pub mod security_posture;
pub mod sensitivity;
pub mod server_capabilities;
pub mod session_token;
pub mod share;
pub mod shell_allowlist;
pub mod startup_guard;
pub mod stats;
pub mod structural_diff;
pub mod symbol_map;
pub mod syntax_validate;
pub mod task_briefing;
/// macOS Seatbelt self-sandbox (#356): wraps launchd-owned daemon/proxy/updater
/// in a `sandbox-exec` profile that denies `~/Documents`/`~/Desktop`/
/// `~/Downloads`, so the TCC privacy prompt can never appear.
#[cfg(target_os = "macos")]
pub mod tcc_guard_sandbox;
pub mod tdd_schema;
pub mod team_slo;
pub mod telemetry;
pub mod terse;
pub mod theme;
pub mod threshold_learning;
pub mod tokenizer_translation_driver;
pub mod tokens;
pub mod tool_health;
pub mod tool_lifecycle;
pub mod tool_profiles;
pub mod transcript_compact;
pub mod update_scheduler;
pub mod updater;
pub mod version_check;
pub mod visualizer;
pub mod walk_filter;
/// WASM extension runtime (`wasm-abi-v1`): sandboxed, language-independent
/// compressors and providers. Feature-gated behind `wasm`.
#[cfg(feature = "wasm")]
pub mod wasm_ext;
pub mod web;
pub mod workflow;
pub mod workspace_config;
pub mod wrapped;
pub mod wrapped_share;
pub mod wrapped_svg;
pub mod xdg_migrate;

// ---------------------------------------------------------------------------
// Feature-gated modules
// ---------------------------------------------------------------------------
pub mod archive;
pub mod archive_fts;
pub mod artifact_index;
pub mod artifacts;
pub mod ast_walk;
pub mod buddy;
#[cfg(feature = "tree-sitter")]
pub mod chunks_ts;
pub mod deep_queries;
pub mod deps;
pub mod editor_registry;
pub mod firewall;
pub mod pathjail;
pub mod signatures;
#[cfg(feature = "tree-sitter")]
pub mod signatures_ts;
pub mod storage_maintenance;
pub mod structured_compact;
pub mod type_ref_edges;
pub mod workspace_trust;
