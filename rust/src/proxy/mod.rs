//! LLM reverse proxy — the core of the **Gateway pillar**.
//!
//! Intercepts Anthropic, OpenAI, Gemini and ChatGPT traffic, compresses
//! prompts, meters usage, and optionally translates request shapes.
//!
//! # TLS Requirements
//!
//! The proxy does NOT terminate TLS. For production/multi-tenant deployments,
//! place behind a TLS-terminating reverse proxy (nginx, caddy, traefik).
//! For mTLS, configure the reverse proxy with client certificates.
//! See: <https://github.com/yvgude/lean-ctx/issues/1371>
//!
//! # Org gateway coupling (ADR-023)
//!
//! The self-hosted org gateway (lean-ctx-enterprise) extends this proxy with
//! admin/usage-store routes. In the OSS build the proxy runs standalone.

pub mod adaptive_policy;
#[cfg(feature = "http-server")]
pub(crate) mod admitted;
#[cfg(feature = "http-server")]
pub(crate) mod agent_compact;
#[cfg(feature = "http-server")]
pub mod anthropic;
#[cfg(all(test, feature = "http-server"))]
mod auth_tests;
pub mod auto_routing;
#[cfg(feature = "http-server")]
pub mod bedrock;
pub mod break_even;
pub mod cache_aligner;
pub mod cache_attribution;
#[cfg(feature = "http-server")]
pub mod cache_breakpoint;
pub mod cache_policy;
pub mod cache_safety;
pub mod ccr;
#[cfg(all(test, feature = "http-server"))]
mod ccr_robustness_tests;
#[cfg(feature = "http-server")]
pub mod chatgpt;
#[cfg(feature = "http-server")]
pub mod chatgpt_cookies;
#[cfg(feature = "http-server")]
pub mod chatgpt_ws;
pub mod circuit_breaker;
#[cfg(feature = "http-server")]
mod codec;
pub mod cold_prefix;
pub mod compress;
pub mod compress_api;
pub mod compress_shared;
pub mod compression_savings;
#[cfg(feature = "http-server")]
mod connector;
#[cfg(feature = "http-server")]
pub(crate) mod conversation;
pub mod cost;
pub mod counterfactual;
pub mod dedup;
#[allow(warnings)]
pub mod determinism_guard;
pub mod effort;
#[cfg(feature = "http-server")]
mod egress_observation;
#[cfg(feature = "http-server")]
mod egress_target;
pub mod eligibility;
#[cfg(feature = "http-server")]
pub mod forward;
#[cfg(feature = "enterprise")]
pub mod gateway_identity;
#[cfg(feature = "http-server")]
pub mod google;
pub mod history_prune;
pub mod holdout;
#[cfg(all(test, feature = "http-server"))]
mod host_journey_tests;
#[cfg(all(test, feature = "http-server"))]
mod integration_tests;
#[cfg(feature = "http-server")]
mod intent;
pub mod introspect;
#[cfg(feature = "http-server")]
mod lineage;
pub mod metrics;
#[cfg(feature = "http-server")]
pub mod models_api;
#[cfg(feature = "http-server")]
pub mod ocla_cache_bridge;
#[cfg(feature = "http-server")]
pub mod openai;
#[cfg(feature = "http-server")]
pub mod openai_responses;
#[cfg(feature = "http-server")]
pub mod openai_responses_ws;
#[cfg(feature = "http-server")]
pub mod outbound_model_policy;
pub mod output_savings;
pub mod pii;
pub mod pipeline_bench;
#[cfg(feature = "enterprise")]
pub mod policy_gate;
pub mod pre_optimize;
pub mod prefix_cache_stats;
pub mod prefix_replay;
#[cfg(feature = "http-server")]
pub(crate) mod project_attribution;
pub mod prose;
#[allow(warnings)]
pub mod prose_compress;
pub mod prose_patterns;
pub mod prose_ranker;
#[cfg(feature = "http-server")]
pub mod providers;
#[cfg(feature = "http-server")]
pub mod quality_lab_api;
#[cfg(all(test, feature = "http-server"))]
mod reference_tests;
pub(crate) mod request_project;
pub mod response_optimizer;
#[cfg(feature = "http-server")]
pub(crate) mod response_shaper;
pub mod rollout;
pub mod routing;
#[cfg(feature = "http-server")]
pub(crate) mod sealed_origin;
#[cfg(feature = "shape-xlat")]
pub mod shape_xlat;
#[cfg(feature = "http-server")]
pub(crate) mod shaping_hook;
#[cfg(feature = "http-server")]
pub mod sse_keepalive;
#[cfg(all(test, feature = "http-server"))]
mod stats_tests;
pub mod sticky_tools;
pub(crate) mod store_binding;
#[cfg(feature = "http-server")]
pub(crate) mod tool_descriptions;
pub mod tool_kind;
#[cfg(feature = "http-server")]
pub mod tool_output;
#[cfg(all(test, feature = "http-server"))]
mod upstream_tests;
pub mod usage;
#[cfg(feature = "http-server")]
pub mod usage_accounting;
pub mod usage_meter;
pub mod usage_parity;
pub mod usage_sink;
pub mod value_gate_proxy;
pub mod verbosity;
#[cfg(all(test, feature = "http-server"))]
mod via_p0_characterization_tests;
#[cfg(feature = "http-server")]
pub mod web_app;

pub mod connector_schedule;
#[allow(warnings)]
pub mod live_zone;

// The HTTP proxy server: state, router, middleware and handlers. Everything
// above builds without `http-server` (#586); this half needs axum.
#[cfg(feature = "http-server")]
mod server;
#[cfg(feature = "http-server")]
pub use server::*;
