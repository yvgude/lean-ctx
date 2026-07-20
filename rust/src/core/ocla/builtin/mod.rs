//! Built-in OCLA trait implementations (P4 — Strangler Fig adoption).
//!
//! Each module wraps existing lean-ctx modules behind the canonical OCLA trait
//! interface defined in `core::ocla::traits`. The trait boundary enables future
//! swapping, testing, mocking, and adoption tracking via OclaBus events.

pub mod agent_gateway;
pub mod compression_provider;
pub mod config_tuner;
pub mod connector_scheduler;
pub mod efficiency_analyzer;
pub mod experiment_runner;
pub mod intent_classifier;
pub mod metrics_exporter;
pub mod model_router;
pub mod observation_hook;
pub mod outcome_tracker;
pub mod response_optimizer;
pub mod savings_ledger;
pub mod usage_sink;
