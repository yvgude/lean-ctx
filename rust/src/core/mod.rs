// ---------------------------------------------------------------------------
// Domain: Compression
// ---------------------------------------------------------------------------
pub mod activity;
pub mod admitted_sources;
pub mod aggressiveness;
pub mod auto_capture;
pub mod auto_findings;
pub mod behavior_nudge;
pub mod cockpit_launch_record;
pub mod codebook;
#[cfg(target_os = "macos")]
pub mod codesign;
pub mod command_cwd;
pub mod command_root;
pub mod command_vars;
pub mod compress_preview;
pub mod compression_safety;
pub mod compressor;
pub mod context_prefetch;
pub mod datadog_push;
pub(crate) mod engine_artifact;
pub mod engine_interface;
pub mod entropy;
pub mod etpao;
pub mod extractive;
pub mod finops_export;
pub(crate) mod foreign_mcp;
pub use lean_ctx_compress::html_crush;
#[allow(unused_imports)]
pub mod ib;
pub mod import;
/// Simulated end-to-end self-test of the decision loop; a test, not product evidence.
#[cfg(test)]
pub(crate) mod integration_proof;
pub mod intelligence_runtime;
pub mod json_crush;
pub(crate) mod migration_lock;
pub mod output_sanitizer;
pub mod policy;
pub mod pop_pruning;
pub mod preservation;
pub mod process_capture;
pub mod protect;
pub mod rabin_karp;
pub mod relevance_tracker;
pub mod rooted_read;
pub mod rule_artifacts;
pub mod rule_discovery;
pub mod rules_canonical;
pub mod rules_channel;
pub mod rules_dedup;
pub mod rules_overhead;
pub(crate) mod rules_sections;
pub(crate) mod rules_validation;
pub mod runtime_flags;
pub mod shared_context;
pub mod solution_auto_capture;
pub mod solution_tracker;
/// Backward-compatible re-export of the Solution Intelligence types that live in
/// `core::knowledge`. Kept on purpose for library users (#1923).
pub mod solution_types;
pub mod structured_read;
pub use lean_ctx_compress::tabular_crush;
pub mod task_receipt_migration;
#[allow(unreachable_pub)]
#[cfg(windows)]
pub(crate) mod windows_file;
#[cfg(windows)]
pub mod windows_private;
pub use lean_ctx_compress::yaml_crush;

// ---------------------------------------------------------------------------
// Domain: Memory
// ---------------------------------------------------------------------------
pub mod interrupt;
pub mod legacy_adoption;
pub mod memory_archive;
pub mod memory_boundary;
pub(crate) mod memory_capacity;
pub(crate) mod memory_consolidation;
pub mod memory_guard;
pub mod memory_lifecycle;
pub mod memory_policy;
pub(crate) mod memory_salience;
pub mod prospective_memory;

// ---------------------------------------------------------------------------
// Domain: Graph
// ---------------------------------------------------------------------------
pub mod call_graph;
pub mod community;
pub mod graph_analysis;
pub mod graph_context;
pub mod graph_coordinator;
pub mod graph_enricher;
pub mod graph_export;
pub mod graph_index;
pub mod graph_parity;
pub mod graph_provider;
pub mod pagerank;
pub mod property_graph;
pub mod repomap;

// ---------------------------------------------------------------------------
// Domain: Context
// ---------------------------------------------------------------------------
pub mod context_admission;
pub mod context_bundle;
pub mod context_compiler;
pub mod context_deficit;
pub mod context_field;
pub mod context_handles;
pub mod context_ir;
// Accidental historical surface retained for source compatibility. Public
// embedders should use `engine::ContextEngine` or `lean-ctx-sdk`.
pub mod context_checkpoint;
#[doc(hidden)]
pub mod context_kernel;
pub mod context_ledger;
pub mod context_lint;
pub mod context_os;
pub mod context_overhead;
pub mod context_overlay;
pub mod context_package;
pub mod context_policies;
pub mod context_proof;
pub mod context_proof_v2;
pub mod context_quality;
pub mod context_radar;
pub mod context_snapshot;
pub mod context_store;
pub mod cross_source_edges;
pub mod cross_source_hints;
pub mod customer_proof_v2;

// ---------------------------------------------------------------------------
// Domain: Knowledge
// ---------------------------------------------------------------------------
pub mod claim_extractor;
pub mod execution_ledger;
pub mod knowledge;
pub mod knowledge_bootstrap;
pub mod knowledge_bridge;
pub mod knowledge_consolidate;
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
pub mod bm25_store;
pub mod bounded_lock;
pub mod bypass_hint;
pub mod content_cache;
pub mod content_chunk;
pub mod context_packing;
pub mod dense_backend;
pub mod embedding_access;
pub mod embedding_index;
pub(crate) mod embedding_quant;
pub mod embeddings;
pub mod energy;
pub mod hybrid_search;
pub mod instruction_files;
pub mod mcp_roots;
#[cfg(feature = "pgvector")]
pub(crate) mod pgvector_store;
#[cfg(feature = "qdrant")]
pub(crate) mod qdrant_store;
pub mod read_mode;
pub mod reference_store;
pub mod search_hook;
pub mod search_kernel;
pub mod search_reranking;
pub mod search_walk;
pub mod semantic;
pub mod semantic_cache;
pub mod splade_retrieval;

// ---------------------------------------------------------------------------
// Domain: Session & Handoff
// ---------------------------------------------------------------------------
pub mod ccp_session_bundle;
pub mod handoff_ledger;
pub mod handoff_transfer_bundle;
pub mod session;
pub mod session_diff;
pub mod session_readset;
pub mod session_rights;
pub mod session_summary;
pub mod signer_trust;
pub mod skillify;

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
pub mod ort_environment;
#[cfg(feature = "embeddings")]
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
pub mod agent_templates;
pub mod agents;
pub mod autonomy;
pub mod canonical;
pub mod hive_gate;
pub mod hive_models;

// ---------------------------------------------------------------------------
// Domain: Adaptive & Scoring
// ---------------------------------------------------------------------------
pub mod adaptive;
pub mod adaptive_policy;
pub mod adaptive_thresholds;
pub mod auto_mode_resolver;
pub mod decision_loop;
pub mod decision_loop_runtime;
#[cfg(test)]
mod decision_loop_runtime_tests;
pub mod execution_lifecycle;
pub mod execution_protocol;
pub mod model_catalog;
pub mod model_registry;
pub mod shadow;
pub mod task_relevance;
pub mod task_spine;

// ---------------------------------------------------------------------------
// Domain: Diagnostics & Quality
// ---------------------------------------------------------------------------
pub mod benchmark;
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
pub mod slow_log;
pub mod smells;
pub mod subagent_contract;
pub mod surprise;
pub mod system_info;
pub mod verification_observability;

// ---------------------------------------------------------------------------
// Domain: Config & Infrastructure
// ---------------------------------------------------------------------------
pub mod active_inference;
pub mod agent_budget;
pub mod agent_lease;
pub mod anchor;
pub(crate) mod ann_cache;
pub mod atomic_fs;
pub mod attribution;
pub mod audit_trail;
pub mod binary_detect;
pub mod binary_identity;
pub mod bounce_tracker;
pub mod budget;
pub mod budgets;
pub mod cache;
pub mod capabilities;
pub mod capsule_transport;
pub mod causal_attribution;
pub mod ccr;
pub mod claude_host;
pub mod claude_md;
pub mod cli_cache;
pub mod client_capabilities;
pub mod client_constraints;
pub mod cloud_files;
pub mod compact_directive;
pub mod config;
pub mod config_heal;
pub mod consolidation;
pub mod consolidation_engine;
pub mod context_capsule;
pub mod contextops;
pub mod conversation;
pub mod crash_log;
pub mod crypto_provider;
pub mod dashboard_pro;
pub mod data_consolidate;
pub mod data_dir;
pub mod debug_log;
pub mod diagnostics_store;
pub mod dynamic_tools;
pub mod editor_signal;
pub mod egress;
pub mod error;
pub mod events;
pub mod eviction_orchestrator;
pub(crate) mod evidence;
pub mod extension_registry;
pub mod extractors;
pub mod file_lock;
pub mod filters;
pub mod gain;
pub mod git;
pub mod git_cache;
pub mod git_signals;
pub mod git_util;
pub mod git_worktree;
pub(crate) mod godot;
pub mod gotcha_tracker;
pub mod governor_hosts;
pub mod handle;
pub mod hasher;
pub mod heatmap;
pub mod hive_signals;
pub mod hnsw;
pub mod home;
pub mod homeostasis;
pub mod hook_mode;
pub mod host_coverage;
pub mod host_state;
pub mod immune_detector;
pub mod mcp_catalog;
pub mod metering;
pub mod ocla;
pub mod ocla_bus;
pub mod unit_claims;
pub mod work_graph;
pub mod work_graph_delegation;
pub mod work_graph_store;

pub mod agent_registry;
pub mod compliance_evidence;
pub mod edit_metering;
pub mod edit_quality;
pub mod efficacy;
pub mod evidence_bundle;
pub mod failure_template;
pub mod grammar_usage;
pub mod graph_cache;
pub mod host_env;
pub mod http_client;
pub mod ide_permissions;
pub use lean_ctx_syntax::import_resolver;
pub mod index_admission;
pub mod index_filter;
pub mod index_namespace;
pub mod index_orchestrator;
pub mod index_paths;
pub mod index_progress;
pub mod ingestion;
pub mod input_filters;
pub mod installation_id;
pub mod instruction_compiler;
// Compiled into build.rs (`#[path]`); here only for its tests.
#[cfg(test)]
mod build_origin;
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
pub mod language_capabilities;
#[cfg(target_os = "macos")]
pub(crate) mod launchd;
pub mod layout_pin;
pub mod levenshtein;
pub mod limits;
pub mod llm_feedback;
pub mod logging;
pub mod mcp_manifest;
pub mod multi_repo;
pub(crate) mod nc_compress;
pub mod ocp;
pub mod openapi;
pub mod opencode_config;
pub mod output_echo;
pub mod output_shapers;
pub mod owasp_alignment;
pub mod path_locks;
pub mod path_mode_memory;
pub mod path_resolve;
pub mod path_suggest;
pub mod paths;
pub mod pathutil;
pub mod persona;
pub mod phase_timing;
pub mod pipeline;
pub mod portable_binary;
pub mod product_capabilities;
pub mod profile_suggest;
pub mod profiles;
pub mod project_hash;
pub mod project_link;
/// Supervised execution channel for protected sessions (macOS refuses nested
/// Seatbelt profiles, so command children are started by the launcher's
/// wrapper, outside the MCP sandbox).
#[cfg(unix)]
pub mod protected_execution;
/// Kernel store boundary of protected sessions: the pinned child rules, the
/// in-process store-path check and the launcher's control-file profiles.
pub mod protected_store;
pub mod protocol;
pub mod provenance;
pub mod provider_bandit;
pub mod provider_cache;
pub mod providers;
pub mod proxy_cache_attribution;
pub mod proxy_endpoint;
pub mod proxy_introspect;
pub mod proxy_metrics;
pub mod proxy_usage;
pub mod proxy_value_metrics;
pub mod read_stub_index;
pub mod recovery;
pub mod redaction;
pub mod reference_docs;
/// Request ledger of a serving process: counters, oldest pending request and
/// last completions for doctor and health (#2197).
pub mod request_ledger;
pub mod response_optimizer;
pub mod roles;
pub mod root_selection;
pub mod route_extractor;
pub mod sandbox;
#[cfg(target_os = "linux")]
pub(crate) mod sandbox_landlock;
pub(crate) mod sandbox_seatbelt;
pub mod sanitize;
pub mod savings_footer;
pub mod savings_ledger;
pub mod savings_tracker;
pub mod scent_field;
pub mod search_delta;
pub mod search_index;
pub mod secret_detection;
pub mod secure_fs;
pub mod security_events;
pub mod security_posture;
pub mod sensitivity;
pub mod server_capabilities;
pub mod session_token;
pub mod share;
pub mod shell_allowlist;
pub mod shell_file_effects;
pub mod shell_history;
pub mod startup_guard;
pub mod stats;
pub mod symbol_map;
pub mod vault_map;
pub use lean_ctx_syntax::syntax_validate;
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
pub mod telemetry_ledger;
pub mod telemetry_notices;
pub mod telemetry_send;
pub mod telemetry_v2;
pub mod terse;
pub mod text_decode;
pub mod theme;
pub mod tokens;
pub mod tool_catalog;
pub mod tool_health;
pub mod tool_lifecycle;
pub mod tool_profiles;
pub mod tool_visibility;
pub mod transcript_compact;
pub mod triage;
pub mod untrusted;
pub mod update_scheduler;
pub mod updater;
pub mod value;
pub mod value_gate;
pub mod version_check;
pub mod walk_filter;
/// WASM extension runtime (`wasm-abi-v1`): sandboxed, language-independent
/// compressors and providers. Feature-gated behind `wasm`.
#[cfg(feature = "wasm")]
pub(crate) mod wasm_ext;
pub mod web;
pub mod workflow;
pub mod workspace_config;
pub mod wrapped;
pub(crate) mod wrapped_share;
pub(crate) mod wrapped_svg;
pub mod xdg_migrate;

// ---------------------------------------------------------------------------
// Feature-gated modules
// ---------------------------------------------------------------------------
pub mod archive;
pub mod archive_fts;
pub mod artifact_index;
pub mod artifacts;
#[cfg(feature = "tree-sitter")]
pub(crate) use lean_ctx_syntax::ast_walk;
#[cfg(feature = "tree-sitter")]
pub mod chunks_ts;
pub mod context_gc;
pub use lean_ctx_syntax::deep_queries;
pub mod deps;
#[cfg(test)]
mod doc_claims;
pub mod editor_registry;
pub mod feature_list;
pub mod firewall;
pub mod pathjail;
pub mod pathjail_scope;
pub mod pathjail_write;
pub mod relevance_gate;
pub mod signatures;
#[cfg(feature = "tree-sitter")]
pub use lean_ctx_syntax::signatures_ts;
pub mod storage_maintenance;
pub mod type_ref_edges;
pub mod workspace_args;
pub mod workspace_authority;
pub mod workspace_trust;
