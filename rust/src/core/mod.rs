// ---------------------------------------------------------------------------
// Domain: Compression
// ---------------------------------------------------------------------------
pub(crate) mod aggressiveness;
pub(crate) mod auto_capture;
pub(crate) mod auto_findings;
pub(crate) mod behavior_nudge;
pub mod codebook;
#[cfg(target_os = "macos")]
pub(crate) mod codesign;
pub mod command_cwd;
pub(crate) mod command_vars;
pub(crate) mod compress_preview;
pub(crate) mod compression_safety;
pub mod compressor;
pub mod context_prefetch;
pub(crate) mod datadog_push;
pub(crate) mod engine_artifact;
pub(crate) mod engine_interface;
pub mod entropy;
pub mod etpao;
pub(crate) mod extractive;
pub mod finops_export;
pub(crate) mod foreign_mcp;
pub mod html_crush;
#[allow(unused_imports)]
pub mod ib;
pub mod import;
/// Simulated end-to-end self-test of the decision loop; a test, not product evidence.
#[cfg(test)]
pub(crate) mod integration_proof;
pub(crate) mod intelligence_runtime;
pub mod json_crush;
pub(crate) mod markdown_compact;
pub(crate) mod migration_lock;
pub mod output_sanitizer;
pub mod policy;
pub mod pop_pruning;
pub mod preservation;
pub(crate) mod process_capture;
pub(crate) mod protect;
pub mod rabin_karp;
pub mod relevance_tracker;
pub mod rule_artifacts;
pub(crate) mod rule_discovery;
pub mod rules_canonical;
pub mod rules_channel;
pub(crate) mod rules_overhead;
pub(crate) mod rules_sections;
pub(crate) mod rules_validation;
pub(crate) mod runtime_flags;
pub mod shared_context;
pub mod solution_auto_capture;
pub mod solution_tracker;
/// Backward-compatible re-export of the Solution Intelligence types that live in
/// `core::knowledge`. Kept on purpose for library users (#1923).
pub mod solution_types;
pub(crate) mod structured_read;
pub mod tabular_crush;
pub(crate) mod task_receipt_migration;
#[allow(unreachable_pub)]
#[cfg(windows)]
pub(crate) mod windows_file;
#[cfg(windows)]
pub(crate) mod windows_private;
pub mod yaml_crush;

// ---------------------------------------------------------------------------
// Domain: Memory
// ---------------------------------------------------------------------------
pub mod episodic_memory;
pub mod interrupt;
pub(crate) mod legacy_adoption;
pub mod memory_archive;
pub(crate) mod memory_boundary;
pub(crate) mod memory_capacity;
pub(crate) mod memory_consolidation;
pub mod memory_guard;
pub mod memory_lifecycle;
pub mod memory_policy;
pub(crate) mod memory_salience;
pub mod procedural_memory;
pub(crate) mod prospective_memory;

// ---------------------------------------------------------------------------
// Domain: Graph
// ---------------------------------------------------------------------------
pub mod call_graph;
pub mod community;
pub(crate) mod graph_analysis;
pub mod graph_context;
pub(crate) mod graph_coordinator;
pub(crate) mod graph_enricher;
pub mod graph_export;
pub mod graph_index;
pub(crate) mod graph_parity;
pub mod graph_provider;
pub(crate) mod pagerank;
pub mod property_graph;
pub(crate) mod repomap;

// ---------------------------------------------------------------------------
// Domain: Context
// ---------------------------------------------------------------------------
pub mod context_admission;
pub(crate) mod context_bundle;
pub(crate) mod context_compiler;
pub mod context_deficit;
pub mod context_field;
pub mod context_handles;
pub mod context_ir;
// Accidental historical surface retained for source compatibility. Public
// embedders should use `crate::engine::ContextEngine` or `lean-ctx-sdk`.
pub mod context_checkpoint;
#[doc(hidden)]
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
pub mod context_quality;
pub mod context_radar;
pub(crate) mod context_snapshot;
pub(crate) mod context_store;
pub mod cross_source_edges;
pub mod cross_source_hints;
pub(crate) mod customer_proof_v2;

// ---------------------------------------------------------------------------
// Domain: Knowledge
// ---------------------------------------------------------------------------
pub mod claim_extractor;
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
pub mod semantic;
pub mod semantic_cache;
pub(crate) mod splade_retrieval;

// ---------------------------------------------------------------------------
// Domain: Session & Handoff
// ---------------------------------------------------------------------------
pub(crate) mod ccp_session_bundle;
pub(crate) mod handoff_ledger;
pub(crate) mod handoff_transfer_bundle;
pub mod session;
pub(crate) mod session_diff;
pub(crate) mod session_summary;
pub(crate) mod signer_trust;
pub(crate) mod skillify;

// ---------------------------------------------------------------------------
// Domain: Attention & Placement
// ---------------------------------------------------------------------------
pub mod litm;

// ---------------------------------------------------------------------------
// Domain: Embeddings / ML
// ---------------------------------------------------------------------------
// ORT runtime glue links against the `ort` crate, which is only pulled in by the
// `embeddings` feature. On platforms ORT does not support (e.g.
// FreeBSD, see #586) these features are disabled, so the modules must be gated
// to keep the build clean without them.
#[cfg(all(feature = "embeddings", any(feature = "ort-cuda", test)))]
pub(crate) mod ort_cuda_imports;
#[cfg(feature = "embeddings")]
pub(crate) mod ort_environment;
#[cfg(feature = "embeddings")]
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
pub mod canonical;
pub mod hive;

// ---------------------------------------------------------------------------
// Domain: Adaptive & Scoring
// ---------------------------------------------------------------------------
pub mod adaptive;
pub mod adaptive_thresholds;
pub mod auto_mode_resolver;
pub mod decision_loop;
#[cfg(test)]
mod decision_loop_integration_test;
pub mod decision_loop_runtime;
#[cfg(test)]
mod decision_loop_runtime_tests;
pub mod execution_lifecycle;
pub mod execution_protocol;
pub(crate) mod model_registry;
pub mod shadow;
pub mod task_relevance;
pub mod task_spine;

// ---------------------------------------------------------------------------
// Domain: Diagnostics & Quality
// ---------------------------------------------------------------------------
pub(crate) mod benchmark;
pub mod code_health;
pub mod conformance;
pub mod contracts;
#[cfg(feature = "tree-sitter")]
pub(crate) mod cyclomatic;
pub mod loop_detection;
pub mod output_verification;
pub mod quality;
pub(crate) mod safety_needles;
pub mod scorecard;
pub mod setup_report;
pub(crate) mod slow_log;
pub(crate) mod smells;
pub(crate) mod subagent_contract;
pub mod surprise;
pub(crate) mod system_info;
pub(crate) mod verification_observability;

// ---------------------------------------------------------------------------
// Domain: Config & Infrastructure
// ---------------------------------------------------------------------------
pub mod active_inference;
pub mod agent_budget;
pub mod agent_lease;
pub mod anchor;
pub(crate) mod ann_cache;
pub(crate) mod atomic_fs;
pub mod attribution;
pub mod audit_trail;
pub(crate) mod binary_detect;
pub mod binary_identity;
pub mod bounce_tracker;
pub(crate) mod budget;
pub mod budgets;
pub mod cache;
pub mod capabilities;
pub mod capsule_transport;
pub mod causal_attribution;
pub(crate) mod cli_cache;
pub(crate) mod client_capabilities;
pub(crate) mod client_constraints;
pub(crate) mod cloud_files;
pub mod compact_directive;
pub mod config;
pub(crate) mod config_heal;
pub mod consolidation;
pub mod consolidation_engine;
pub mod context_capsule;
pub(crate) mod contextops;
pub(crate) mod conversation;
pub mod crash_log;
pub mod crypto_provider;
pub(crate) mod dashboard_pro;
pub(crate) mod data_consolidate;
pub mod data_dir;
pub(crate) mod debug_log;
pub mod diagnostics_store;
pub mod editor_signal;
pub(crate) mod egress;
pub mod error;
pub mod events;
pub(crate) mod eviction_orchestrator;
pub(crate) mod evidence;
pub mod extension_registry;
pub mod extractors;
pub(crate) mod file_lock;
pub(crate) mod filters;
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
pub mod hnsw;
pub mod home;
pub mod homeostasis;
pub mod host_coverage;
pub(crate) mod immune_detector;
pub mod mcp_catalog;
pub mod metering;
pub mod ocla;
pub mod ocla_bus;
pub mod work_graph;
pub mod work_graph_delegation;
pub mod work_graph_store;

pub(crate) mod agent_registry;
pub mod compliance_evidence;
pub mod edit_metering;
pub(crate) mod edit_quality;
pub(crate) mod efficacy;
pub mod evidence_bundle;
pub mod failure_template;
pub mod grammar_usage;
pub(crate) mod graph_cache;
pub(crate) mod host_env;
pub(crate) mod http_client;
pub mod ide_permissions;
pub mod import_resolver;
pub mod index_admission;
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
pub mod language_capabilities;
#[cfg(target_os = "macos")]
pub(crate) mod launchd;
pub(crate) mod layout_pin;
pub(crate) mod levenshtein;
pub(crate) mod limits;
pub mod llm_enhance;
pub(crate) mod llm_feedback;
pub(crate) mod logging;
pub mod mcp_manifest;
pub mod multi_repo;
pub(crate) mod nc_compress;
pub mod ocp;
pub mod openapi;
pub mod opencode_config;
pub(crate) mod output_echo;
pub(crate) mod owasp_alignment;
pub(crate) mod path_locks;
pub(crate) mod path_mode_memory;
pub mod path_resolve;
pub(crate) mod path_suggest;
pub mod paths;
pub mod pathutil;
pub mod persona;
pub(crate) mod phase_timing;
pub mod pipeline;
pub(crate) mod portable_binary;
pub mod product_capabilities;
pub(crate) mod profile_suggest;
pub mod profiles;
pub(crate) mod project_hash;
pub(crate) mod project_link;
/// Supervised execution channel for protected sessions (macOS refuses nested
/// Seatbelt profiles, so command children are started by the launcher's
/// wrapper, outside the MCP sandbox).
#[cfg(unix)]
pub(crate) mod protected_execution;
pub mod protocol;
pub mod provenance;
pub mod provider_bandit;
pub mod provider_cache;
pub mod providers;
pub(crate) mod read_stub_index;
pub(crate) mod recovery;
pub(crate) mod redaction;
pub mod reference_docs;
pub mod roles;
pub(crate) mod root_selection;
pub(crate) mod route_extractor;
pub mod sandbox;
#[cfg(target_os = "linux")]
pub(crate) mod sandbox_landlock;
pub(crate) mod sandbox_seatbelt;
pub(crate) mod sanitize;
pub mod savings_footer;
pub mod savings_ledger;
pub mod savings_tracker;
pub mod scent_field;
pub(crate) mod search_delta;
pub mod search_index;
pub mod secret_detection;
pub(crate) mod secure_fs;
pub mod security_events;
pub mod security_posture;
pub mod sensitivity;
pub mod server_capabilities;
pub mod session_token;
pub(crate) mod share;
pub mod shell_allowlist;
pub(crate) mod shell_file_effects;
pub mod startup_guard;
pub mod stats;
pub mod symbol_map;
pub(crate) mod syntax_validate;
pub mod task_briefing;
/// macOS Seatbelt self-sandbox (#356): wraps launchd-owned daemon/proxy/updater
/// in a `sandbox-exec` profile that denies `~/Documents`/`~/Desktop`/
/// `~/Downloads`, so the TCC privacy prompt can never appear.
#[cfg(target_os = "macos")]
pub mod tcc_guard_sandbox;
pub mod tdd_schema;
pub mod telemetry;
pub mod telemetry_aggregate;
pub mod telemetry_consent;
pub mod telemetry_failure;
pub mod telemetry_features;
pub(crate) mod telemetry_ledger;
pub mod telemetry_notices;
pub mod telemetry_send;
pub mod telemetry_v2;
pub mod terse;
pub(crate) mod text_decode;
pub mod theme;
pub mod tokens;
pub(crate) mod tool_health;
pub(crate) mod tool_lifecycle;
pub mod tool_profiles;
pub(crate) mod transcript_compact;
pub mod triage;
pub(crate) mod update_scheduler;
pub(crate) mod updater;
pub mod value;
pub mod value_gate;
pub(crate) mod version_check;
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
#[cfg(feature = "tree-sitter")]
pub(crate) mod chunks_ts;
pub(crate) mod context_gc;
pub mod deep_queries;
pub(crate) mod deps;
#[cfg(test)]
mod doc_claims;
pub mod editor_registry;
pub(crate) mod firewall;
pub mod pathjail;
pub mod pathjail_scope;
pub(crate) mod relevance_gate;
pub mod signatures;
#[cfg(feature = "tree-sitter")]
pub(crate) mod signatures_ts;
pub mod storage_maintenance;
pub(crate) mod structured_compact;
pub(crate) mod type_ref_edges;
pub mod workspace_authority;
pub(crate) mod workspace_trust;
