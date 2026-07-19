//! OCLA (Open Context Layer Architecture) — trait definitions and built-in impls.
//!
//! This module defines the 5 OCLA traits that P3 implements:
//! - `EfficiencyAnalyzer` (LEARN) — read-only efficiency recommendations
//! - `ConfigTuner` (ACT) — proposal → approval → apply → changelog
//! - `ExperimentRunner` (ACT) — deterministic experiments with auto-rollback
//! - `IntentClassifier` (OBSERVE) — classifies user intent into personas/tiers
//! - `OutcomeTracker` (OBSERVE) — captures explicit accept/reject signals

pub mod builtin;

use serde::{Deserialize, Serialize};

// ─── EfficiencyAnalyzer (LEARN) ──────────────────────────────────────────────

/// Read-only efficiency recommendations. Never mutates state.
pub trait EfficiencyAnalyzer: Send + Sync {
    /// Analyze a file path and return the recommended compression mode.
    fn recommend_mode(&self, path: &str) -> Option<String>;

    /// Current efficiency score for a path (0.0 = worst, 1.0 = best).
    fn efficiency_score(&self, path: &str) -> f64;

    /// Summary of all tracked paths with their efficiency.
    fn summary(&self) -> Vec<EfficiencyEntry>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EfficiencyEntry {
    pub path: String,
    pub mode: String,
    pub score: f64,
}

// ─── ConfigTuner (ACT) ───────────────────────────────────────────────────────

/// Proposals configuration changes with an approval/rollback lifecycle.
pub trait ConfigTuner: Send + Sync {
    /// Propose a configuration change. Returns a proposal ID.
    fn propose(&self, change: ConfigChange) -> Result<String, TunerError>;

    /// Apply an approved proposal.
    fn apply(&self, proposal_id: &str) -> Result<ApplyResult, TunerError>;

    /// Rollback the last applied change.
    fn rollback(&self) -> Result<RollbackResult, TunerError>;

    /// Read the changelog.
    fn changelog(&self) -> Vec<ChangeLogEntry>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChange {
    pub key: String,
    pub old_value: Option<String>,
    pub new_value: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyResult {
    pub proposal_id: String,
    pub applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackResult {
    pub rolled_back_proposal: String,
    pub restored_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeLogEntry {
    pub proposal_id: String,
    pub change: ConfigChange,
    pub applied_at: u64,
    pub rolled_back: bool,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum TunerError {
    #[error("proposal not found: {0}")]
    NotFound(String),
    #[error("proposal already applied: {0}")]
    AlreadyApplied(String),
    #[error("no changes to rollback")]
    NothingToRollback,
    #[error("config error: {0}")]
    ConfigError(String),
}

// ─── ExperimentRunner (ACT) ──────────────────────────────────────────────────

/// Runs deterministic A/B experiments with auto-rollback on failure.
pub trait ExperimentRunner: Send + Sync {
    /// Start an experiment with the given holdout fraction.
    fn start(&self, config: ExperimentConfig) -> Result<String, ExperimentError>;

    /// Get current experiment status.
    fn status(&self, experiment_id: &str) -> Result<ExperimentStatus, ExperimentError>;

    /// Conclude an experiment (stop collecting, compute result).
    fn conclude(&self, experiment_id: &str) -> Result<ExperimentResult, ExperimentError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentConfig {
    pub name: String,
    pub holdout_fraction: f64,
    pub min_samples: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentStatus {
    pub experiment_id: String,
    pub name: String,
    pub control_n: u64,
    pub treatment_n: u64,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentResult {
    pub experiment_id: String,
    pub significant: bool,
    pub treatment_better: bool,
    pub effect_size_pct: f64,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ExperimentError {
    #[error("experiment not found: {0}")]
    NotFound(String),
    #[error("experiment already running: {0}")]
    AlreadyRunning(String),
    #[error("insufficient samples: need {needed}, have {have}")]
    InsufficientSamples { needed: u64, have: u64 },
}

// ─── IntentClassifier (OBSERVE) ──────────────────────────────────────────────

/// Classifies user intent for routing and persona selection.
pub trait IntentClassifier: Send + Sync {
    /// Classify a user query into task type, tier, and persona.
    fn classify(&self, query: &str) -> IntentClassification;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentClassification {
    pub task_type: String,
    pub model_tier: String,
    pub persona: String,
    pub confidence: f64,
    pub scope: String,
}

// ─── OutcomeTracker (OBSERVE) ────────────────────────────────────────────────

/// Tracks explicit user feedback (accept/reject) on generated outputs.
pub trait OutcomeTracker: Send + Sync {
    /// Record an explicit outcome signal.
    fn record(&self, outcome: Outcome) -> u64;

    /// Get recent outcomes for a session.
    fn recent(&self, session_id: &str, limit: usize) -> Vec<OutcomeRecord>;

    /// Acceptance rate for a session (0.0–1.0).
    fn acceptance_rate(&self, session_id: &str) -> Option<f64>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub session_id: String,
    pub accepted: bool,
    pub tool: Option<String>,
    pub implicit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRecord {
    pub id: u64,
    pub outcome: Outcome,
    pub timestamp_ms: u64,
}
