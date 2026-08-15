// ---------------------------------------------------------------------------
// Domain: Compression
// ---------------------------------------------------------------------------
pub mod adaptive_chunking;
pub mod adaptive_compression;
pub mod addons;
pub(crate) mod aggressiveness;
pub mod attention_context;
pub(crate) mod auto_capture;
pub(crate) mod auto_findings;
pub mod codebook;
#[cfg(target_os = "macos")]
pub(crate) mod codesign;
pub mod cognitive;
pub(crate) mod compress_preview;
pub(crate) mod compression_safety;
pub mod compressor;
pub mod context_prefetch;
pub(crate) mod datadog_push;
#[allow(dead_code)]
pub(crate) mod edit_snapshot;
pub mod entropy;
pub mod etpao;
pub mod eval_ab;
pub mod eval_harness;
pub(crate) mod extractive;
pub mod finops_export;
pub mod fleet_analytics;
pub mod html_crush;
#[allow(unused_imports)]
pub mod ib;
pub mod information_bottleneck;
pub mod integration_proof;
pub mod json_crush;
pub mod json_sample;
pub(crate) mod markdown_compact;
pub mod output_sanitizer;
pub mod policy;
pub mod pop_pruning;
pub mod predictive_coding;
pub mod predictive_prefetch;
pub mod preservation;
pub mod pro_triggers;
pub mod process_guard;
pub mod progressive_compression;
pub(crate) mod protect;
pub mod rabin_karp;
#[allow(dead_code)]
pub(crate) mod read_provenance;
pub mod relevance_tracker;
pub mod rule_artifacts;
pub(crate) mod rule_discovery;
pub mod rule_scorer;
pub mod rules_canonical;
pub mod rules_channel;
pub(crate) mod rules_overhead;
pub(crate) mod rules_sections;
pub(crate) mod rules_validation;
pub(crate) mod runtime_flags;
pub mod shared_context;
pub mod sidecar_transport;
pub mod structural_tokenizer;
pub(crate) mod structured_read;
pub mod tabular_crush;
pub mod verbosity;
#[allow(unreachable_pub)]
pub mod wasserstein;
pub mod yaml_crush;

// ---------------------------------------------------------------------------
// Domain: Memory
// ---------------------------------------------------------------------------
pub mod anti_interrupt;
pub mod episodic_memory;
pub mod interrupt;
pub mod memory_archive;
pub(crate) mod memory_boundary;
pub(crate) mod memory_capacity;
pub(crate) mod memory_consolidation;
pub mod memory_guard;
pub mod memory_lifecycle;
pub mod memory_policy;
pub(crate) mod memory_salience;
pub mod memory_scheduler;
pub mod multiscale_index;
pub mod procedural_memory;
pub(crate) mod prospective_memory;

// ---------------------------------------------------------------------------
// Domain: Graph
// ---------------------------------------------------------------------------
pub mod call_graph;
pub mod community;
pub mod gamma_cover;
pub(crate) mod graph_analysis;
pub mod graph_context;
pub(crate) mod graph_coordinator;
pub(crate) mod graph_enricher;
pub mod graph_expand;
pub mod graph_export;
pub mod graph_features;
pub mod graph_index;
pub(crate) mod graph_parity;
pub mod graph_provider;
pub(crate) mod pagerank;
pub mod property_graph;
pub(crate) mod repomap;

// ---------------------------------------------------------------------------
// Domain: Context
// ---------------------------------------------------------------------------
pub mod context_artifacts;
pub mod context_column;
pub(crate) mod context_compiler;
pub mod context_deficit;
pub mod context_field;
pub mod context_handles;
pub mod context_ir;
pub mod context_kernel;
pub mod context_ledger;
pub(crate) mod context_lint;
pub mod context_os;
pub mod context_overhead;
pub mod context_overlay;
pub(crate) mod context_package;
pub mod context_policies;
pub(crate) mod context_proof;
pub mod context_proof_v2;
pub mod context_radar;
pub(crate) mod context_snapshot;
pub mod cross_customer_learning;
pub mod cross_source_edges;
pub mod cross_source_hints;

// ---------------------------------------------------------------------------
// Domain: Knowledge
// ---------------------------------------------------------------------------
pub mod claim_extractor;
pub(crate) mod cognition_loop;
pub(crate) mod cognition_scheduler;
pub mod execution_ledger;
pub mod knowledge;
pub(crate) mod knowledge_bootstrap;
pub mod knowledge_bridge;
pub mod knowledge_embedding;
pub mod knowledge_provider_extract;
pub mod knowledge_relations;
pub mod knowledge_router;
pub mod outcome;
pub mod trust;

// ---------------------------------------------------------------------------
// Domain: Search & Retrieval
// ---------------------------------------------------------------------------
pub mod bm25_cache;
pub mod bm25_index;
pub mod content_cache;
pub mod content_chunk;
pub(crate) mod context_packing;
pub mod cooccurrence;
pub mod dense_backend;
pub mod embedding_index;
pub(crate) mod embedding_quant;
pub mod embeddings;
pub mod energy;
pub mod hybrid_search;
#[cfg(feature = "pgvector")]
pub(crate) mod pgvector_store;
#[cfg(feature = "qdrant")]
pub(crate) mod qdrant_store;
pub mod search_reranking;
pub mod semantic_cache;
pub mod semantic_chunks;
pub(crate) mod splade_retrieval;
pub mod spreading_activation;

// ---------------------------------------------------------------------------
// Domain: Session & Handoff
// ---------------------------------------------------------------------------
pub(crate) mod ccp_session_bundle;
pub(crate) mod handoff_ledger;
pub(crate) mod handoff_transfer_bundle;
pub mod session;
pub(crate) mod session_diff;
pub(crate) mod session_summary;
pub(crate) mod skillify;

// ---------------------------------------------------------------------------
// Domain: Attention & Placement
// ---------------------------------------------------------------------------
pub mod attention_layout_driver;
pub mod attention_model;
pub mod attention_placement;
pub mod litm;

// ---------------------------------------------------------------------------
// Domain: Neural / ML
// ---------------------------------------------------------------------------
pub mod neural;
// ORT runtime glue links against the `ort` crate, which is only pulled in by the
// `embeddings` or `neural` features. On platforms ORT does not support (e.g.
// FreeBSD, see #586) these features are disabled, so the modules must be gated
// to keep the build clean without them.
#[cfg(any(feature = "embeddings", feature = "neural"))]
pub(crate) mod ort_environment;
#[cfg(any(feature = "embeddings", feature = "neural"))]
pub(crate) mod ort_execution_providers;

// ---------------------------------------------------------------------------
// Domain: Patterns & Shell
// ---------------------------------------------------------------------------
pub mod patterns;

// ---------------------------------------------------------------------------
// Domain: Agents & A2A
// ---------------------------------------------------------------------------
pub mod a2a;
pub(crate) mod a2a_transport;
pub(crate) mod agent_identity;
pub(crate) mod agent_runtime_env;
pub(crate) mod agents;
pub(crate) mod autonomy;
pub mod autonomy_drivers;
pub mod stigmergy;

// ---------------------------------------------------------------------------
// Domain: Adaptive & Scoring
// ---------------------------------------------------------------------------
pub mod adaptive;
pub(crate) mod adaptive_mode_policy;
pub mod adaptive_thresholds;
pub mod auto_mode_resolver;
pub mod bandit;
pub mod decision_loop;
#[cfg(test)]
mod decision_loop_integration_test;
pub mod decision_loop_runtime;
#[cfg(test)]
mod decision_loop_runtime_tests;
pub mod litm_calibration;
pub mod measurement;
pub mod mode_predictor;
pub(crate) mod model_registry;
pub mod model_router;
pub mod shadow;
pub mod task_relevance;
pub mod task_spine;
pub mod token_calibration;

// ---------------------------------------------------------------------------
// Domain: Diagnostics & Quality
// ---------------------------------------------------------------------------
pub mod anomaly;
pub(crate) mod benchmark;
pub mod benchmark_compare;
pub(crate) mod benchmark_study;
/// Commercial-plane billing substrate (`billing-plane-v1`): plans, entitlements,
/// and usage metering derived from the signed savings ledger. Never gates local.
pub mod billing;
pub mod code_health;
pub(crate) mod cognitive_gate;
pub mod cognitive_load;
pub mod conformance;
pub mod contracts;
pub mod cost_per_outcome;
pub(crate) mod cyclomatic;
pub mod degradation_policy;
pub mod loop_detection;
pub mod output_verification;
pub mod quality;
pub(crate) mod quality_lab;
pub(crate) mod safety_needles;
pub mod scorecard;
pub mod setup_report;
pub(crate) mod slo;
pub(crate) mod slow_log;
pub(crate) mod smells;
pub(crate) mod subagent_contract;
pub mod surprise;
pub(crate) mod verification_observability;

// ---------------------------------------------------------------------------
// Domain: Config & Infrastructure
// ---------------------------------------------------------------------------
pub mod active_inference;
pub mod agent_attribution;
pub mod agent_budget;
pub mod agent_lease;
pub mod anchor;
pub(crate) mod ann_cache;
pub(crate) mod atomic_fs;
pub mod attribution;
pub mod audit_trail;
pub(crate) mod binary_detect;
pub mod bounce_tracker;
pub(crate) mod budget;
pub mod budget_tracker;
pub mod budgets;
pub mod cache;
pub mod cache_diagnostics;
pub(crate) mod cache_telemetry;
pub mod capabilities;
pub mod capsule_transport;
pub mod causal_attribution;
pub mod chain_compression;
pub(crate) mod cli_cache;
pub(crate) mod client_capabilities;
pub(crate) mod client_constraints;
pub(crate) mod cloud_files;
pub mod config;
pub(crate) mod config_heal;
pub mod consolidation;
pub mod consolidation_engine;
pub mod content_handle;
pub mod context_capsule;
pub(crate) mod contextops;
pub(crate) mod conversation;
pub mod crash_log;
pub(crate) mod data_consolidate;
pub mod data_dir;
pub(crate) mod debug_log;
#[allow(unused)]
pub(crate) mod delivered_ranges;
pub mod delta_response;
pub mod diagnostics_store;
pub mod echo_ratio;
pub mod editor_signal;
pub(crate) mod egress;
pub mod error;
pub mod events;
pub(crate) mod eviction_orchestrator;
pub(crate) mod evidence;
pub mod evidence_classification;
pub mod evidence_ledger;
pub mod extension_registry;
pub mod extractors;
pub mod feedback;
pub(crate) mod fep_prefetch;
pub(crate) mod filters;
pub mod free_energy_budget;
pub mod gain;
pub(crate) mod git;
pub mod git_cache;
pub(crate) mod git_signals;
pub(crate) mod git_util;
pub(crate) mod godot;
pub mod gotcha_tracker;
pub mod handle;
pub mod hasher;
pub mod heatmap;
pub mod hebbian_cache;
pub mod hnsw;
pub mod home;
pub mod homeostasis;
pub(crate) mod immune_detector;
pub mod live_evidence_ledger;
pub mod marginal_gate;
pub mod mcp_catalog;
pub mod metering;
pub mod negative_knowledge;
pub mod ocla;
pub mod ocla_bus;
pub(crate) mod quality_benchmark;
pub(crate) mod qubo_select;
pub mod query_aware;
pub mod session_budget;
pub mod work_graph;

pub(crate) mod agent_registry;
pub mod compliance;
pub mod compliance_report;
pub(crate) mod edit_metering;
pub(crate) mod edit_quality;
pub(crate) mod efficacy;
pub mod evidence_bundle;
pub mod grammar_usage;
pub(crate) mod graph_cache;
pub(crate) mod http_client;
pub mod ide_permissions;
pub mod import_resolver;
pub mod index_admission;
pub mod index_bundle;
pub(crate) mod index_filter;
pub(crate) mod index_namespace;
pub mod index_orchestrator;
pub(crate) mod index_paths;
pub mod index_progress;
pub mod ingestion;
pub mod input_filters;
pub(crate) mod installation_id;
pub(crate) mod instruction_compiler;
pub(crate) mod integrity;
pub mod intent_engine;
pub(crate) mod intent_lang;
pub mod intent_protocol;
pub(crate) mod intent_router;
pub mod introspect;
pub mod io_boundary;
pub mod io_health;
pub mod journal;
pub mod jsonc;
pub(crate) mod knowledge_vault;
pub mod language_capabilities;
#[cfg(target_os = "macos")]
pub(crate) mod launchd;
pub(crate) mod layout_pin;
pub(crate) mod learning_sync;
pub(crate) mod levenshtein;
pub(crate) mod limits;
pub mod llm_enhance;
pub(crate) mod llm_feedback;
pub mod locomo;
pub(crate) mod logging;
pub mod mcp_manifest;
pub mod mdl_mode;
pub mod mdl_selector;
pub mod multi_repo;
pub(crate) mod nc_compress;
pub mod ocp;
pub mod openapi;
pub(crate) mod output_echo;
pub(crate) mod owasp_alignment;
pub(crate) mod path_locks;
pub(crate) mod path_mode_memory;
pub mod path_resolve;
pub mod paths;
pub mod pathutil;
pub mod persona;
pub mod pipeline;
pub mod plugins;
pub(crate) mod portable_binary;
pub(crate) mod profile_suggest;
pub mod profiles;
pub(crate) mod project_hash;
pub mod protocol;
pub mod provider_bandit;
pub mod provider_cache;
pub mod providers;
pub(crate) mod read_stub_index;
pub(crate) mod recovery;
pub(crate) mod redaction;
pub mod reference_docs;
pub mod roles;
pub(crate) mod route_extractor;
pub mod saliency;
pub mod sandbox;
#[cfg(target_os = "linux")]
pub(crate) mod sandbox_landlock;
pub(crate) mod sandbox_seatbelt;
pub(crate) mod sanitize;
pub(crate) mod savings_autopush;
pub mod savings_footer;
pub mod savings_ledger;
pub mod savings_tracker;
pub mod scent_field;
pub(crate) mod search_delta;
pub mod search_index;
pub mod secret_detection;
pub mod security_posture;
pub mod sensitivity;
pub mod server_capabilities;
pub mod session_token;
pub(crate) mod share;
pub mod shell_allowlist;
pub mod startup_guard;
pub mod stats;
pub mod structural_diff;
pub mod symbol_map;
pub(crate) mod syntax_validate;
pub(crate) mod task_benchmark;
pub mod task_briefing;
/// macOS Seatbelt self-sandbox (#356): wraps launchd-owned daemon/proxy/updater
/// in a `sandbox-exec` profile that denies `~/Documents`/`~/Desktop`/
/// `~/Downloads`, so the TCC privacy prompt can never appear.
#[cfg(target_os = "macos")]
pub mod tcc_guard_sandbox;
pub mod tdd_schema;
pub mod telemetry;
pub(crate) mod telemetry_ledger;
pub mod terse;
pub mod theme;
pub mod threshold_learning;
pub mod tokenizer_translation_driver;
pub mod tokens;
pub(crate) mod tool_health;
pub(crate) mod tool_lifecycle;
pub mod tool_profiles;
pub(crate) mod transcript_compact;
pub mod triage;
pub(crate) mod update_scheduler;
pub(crate) mod updater;
pub mod value_gate;
pub(crate) mod version_check;
pub(crate) mod visualizer;
pub(crate) mod walk_filter;
/// WASM extension runtime (`wasm-abi-v1`): sandboxed, language-independent
/// compressors and providers. Feature-gated behind `wasm`.
#[cfg(feature = "wasm")]
pub(crate) mod wasm_ext;
pub mod web;
pub mod workflow;
pub(crate) mod workspace_config;
pub mod wrapped;
pub(crate) mod wrapped_share;
pub(crate) mod wrapped_svg;
pub(crate) mod xdg_migrate;

// ---------------------------------------------------------------------------
// Feature-gated modules
// ---------------------------------------------------------------------------
pub mod archive;
pub mod archive_fts;
pub(crate) mod artifact_index;
pub(crate) mod artifacts;
pub(crate) mod ast_walk;
pub(crate) mod buddy;
#[cfg(feature = "tree-sitter")]
pub(crate) mod chunks_ts;
pub(crate) mod context_gc;
pub mod deep_queries;
pub(crate) mod deps;
pub mod editor_registry;
pub(crate) mod firewall;
pub(crate) mod memory_branch;
pub mod pathjail;
pub(crate) mod relevance_gate;
pub mod signatures;
#[cfg(feature = "tree-sitter")]
pub(crate) mod signatures_ts;
pub mod storage_maintenance;
pub(crate) mod structured_compact;
pub(crate) mod type_ref_edges;
pub(crate) mod workspace_trust;

#[cfg(test)]
mod science_integration;
#[cfg(test)]
mod science_props;

#[cfg(test)]
mod science_benchmark;
