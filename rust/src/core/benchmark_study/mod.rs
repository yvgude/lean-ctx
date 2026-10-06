//! Compression savings benchmark study (E-Bench).
//!
//! Two-arm experiment harness: Control / CompressOnly on one reference model
//! against standard coding benchmarks (HumanEval, MBPP, SWE-bench).
//! Measures lean-ctx cost savings and quality retention on the same model.

pub(crate) mod analysis;
pub(crate) mod datasets;
#[allow(dead_code)]
pub(crate) mod experiment;
pub(crate) mod llm_client;
pub(crate) mod metrics;
pub(crate) mod report;
pub(crate) mod runner;
#[allow(dead_code)]
pub(crate) mod sandbox;
pub(crate) mod stats;

pub(crate) use analysis::PublicationAnalysis;
pub(crate) use experiment::StudyConfig;
pub(crate) use runner::run_study;
