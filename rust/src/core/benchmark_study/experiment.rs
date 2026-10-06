//! Two-arm experiment definition and configuration.

use serde::{Deserialize, Serialize};

/// Which experimental arm a task is running under. Both arms use the same
/// reference model; only lean-ctx compression differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Arm {
    /// No compression.
    Control,
    /// lean-ctx compression.
    CompressOnly,
}

impl Arm {
    pub(crate) fn all() -> &'static [Arm] {
        &[Arm::Control, Arm::CompressOnly]
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Arm::Control => "control",
            Arm::CompressOnly => "compress_only",
        }
    }

    pub(crate) fn uses_compression(&self) -> bool {
        matches!(self, Arm::CompressOnly)
    }
}

impl std::fmt::Display for Arm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Configuration for a benchmark study run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StudyConfig {
    /// Which arms to run (default: both).
    pub arms: Vec<Arm>,
    /// Reference model shared by every arm.
    pub reference_model: String,
    /// Number of repeats per task (for pass@k).
    pub repeats: usize,
    /// Maximum concurrent tasks per arm.
    pub concurrency: usize,
    /// Python binary path for sandbox execution.
    pub python_bin: String,
    /// Timeout per task in seconds.
    pub task_timeout_secs: u64,
}

impl Default for StudyConfig {
    fn default() -> Self {
        Self {
            arms: Arm::all().to_vec(),
            reference_model: "claude-sonnet-4".into(),
            repeats: 1,
            concurrency: 4,
            python_bin: super::sandbox::DEFAULT_PYTHON_BIN.into(),
            task_timeout_secs: 120,
        }
    }
}

/// A single experiment combining a dataset with all arms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StudyExperiment {
    pub config: StudyConfig,
    pub dataset_name: String,
    pub results: Vec<ArmResult>,
}

/// Results for a single arm across all tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArmResult {
    pub arm: Arm,
    pub tasks_total: usize,
    pub tasks_passed: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub task_results: Vec<TaskResult>,
}

impl ArmResult {
    pub(crate) fn pass_rate(&self) -> f64 {
        if self.tasks_total == 0 {
            return 0.0;
        }
        self.tasks_passed as f64 / self.tasks_total as f64
    }

    pub(crate) fn cost_per_1k(&self) -> f64 {
        if self.tasks_total == 0 {
            return 0.0;
        }
        self.total_cost_usd / self.tasks_total as f64 * 1000.0
    }
}

/// Result for a single task under a specific arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskResult {
    pub task_id: String,
    pub passed: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub model_used: String,
    pub compressed_tokens: Option<u64>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_rate_zero_tasks() {
        let r = ArmResult {
            arm: Arm::Control,
            tasks_total: 0,
            tasks_passed: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cost_usd: 0.0,
            task_results: vec![],
        };
        assert_eq!(r.pass_rate(), 0.0);
    }

    #[test]
    fn pass_rate_calculation() {
        let r = ArmResult {
            arm: Arm::Control,
            tasks_total: 10,
            tasks_passed: 8,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cost_usd: 1.0,
            task_results: vec![],
        };
        assert!((r.pass_rate() - 0.8).abs() < 1e-9);
        assert!((r.cost_per_1k() - 100.0).abs() < 1e-9);
    }
}
