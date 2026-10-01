//! Four-arm experiment definition and configuration.

use serde::{Deserialize, Serialize};

/// Which experimental arm a task is running under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Arm {
    /// No compression, no routing — reference model only.
    Control,
    /// lean-ctx compression, reference model (no routing).
    CompressOnly,
    /// No compression, intent-based routing (Haiku/Sonnet/Opus tiers).
    RouteOnly,
    /// lean-ctx compression + intent-based routing.
    Combined,
}

impl Arm {
    pub(crate) fn all() -> &'static [Arm] {
        &[
            Arm::Control,
            Arm::CompressOnly,
            Arm::RouteOnly,
            Arm::Combined,
        ]
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Arm::Control => "control",
            Arm::CompressOnly => "compress_only",
            Arm::RouteOnly => "route_only",
            Arm::Combined => "combined",
        }
    }

    pub(crate) fn uses_compression(&self) -> bool {
        matches!(self, Arm::CompressOnly | Arm::Combined)
    }

    pub(crate) fn uses_routing(&self) -> bool {
        matches!(self, Arm::RouteOnly | Arm::Combined)
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
    /// Which arms to run (default: all four).
    pub arms: Vec<Arm>,
    /// Reference model for Control + CompressOnly arms.
    pub reference_model: String,
    /// Tier mapping for RouteOnly + Combined arms.
    pub tiers: TierConfig,
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
            tiers: TierConfig::default(),
            repeats: 1,
            concurrency: 4,
            python_bin: super::sandbox::DEFAULT_PYTHON_BIN.into(),
            task_timeout_secs: 120,
        }
    }
}

/// Model tier configuration for routing arms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TierConfig {
    pub fast: String,
    pub standard: String,
    pub premium: String,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            fast: "claude-haiku-4-5".into(),
            standard: "claude-sonnet-4".into(),
            premium: "claude-opus-4".into(),
        }
    }
}

/// A single experiment combining a dataset with all arms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FourArmExperiment {
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
    pub routing_tier: Option<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_all_returns_four() {
        assert_eq!(Arm::all().len(), 4);
    }

    #[test]
    fn arm_compression_routing_flags() {
        assert!(!Arm::Control.uses_compression());
        assert!(!Arm::Control.uses_routing());
        assert!(Arm::CompressOnly.uses_compression());
        assert!(!Arm::CompressOnly.uses_routing());
        assert!(!Arm::RouteOnly.uses_compression());
        assert!(Arm::RouteOnly.uses_routing());
        assert!(Arm::Combined.uses_compression());
        assert!(Arm::Combined.uses_routing());
    }

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

    #[test]
    fn default_config_has_all_arms() {
        let cfg = StudyConfig::default();
        assert_eq!(cfg.arms.len(), 4);
        assert_eq!(cfg.repeats, 1);
    }
}
