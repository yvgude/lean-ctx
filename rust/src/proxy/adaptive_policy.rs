//! Task-aware compression settings and outcome learning.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

/// The controls used by the compression pipeline for one task class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressionPolicy {
    pub code_preserve: bool,
    pub log_compression_level: u8,
    pub history_keep_turns: u8,
    pub max_search_results: u16,
    pub compress_tool_output: bool,
}

/// Namespace for adaptive task-class policy selection.
#[derive(Debug, Default, Clone, Copy)]
pub struct AdaptiveCompressionPolicy;

impl AdaptiveCompressionPolicy {
    pub fn select_policy(task_class: &str) -> CompressionPolicy {
        select_policy(task_class)
    }

    pub fn best_policy_for(task_class: &str) -> CompressionPolicy {
        best_policy_for(task_class)
    }
}

impl CompressionPolicy {
    const CODING: Self = Self {
        code_preserve: true,
        log_compression_level: 3,
        history_keep_turns: 8,
        max_search_results: 20,
        compress_tool_output: true,
    };
    const DEBUGGING: Self = Self {
        code_preserve: true,
        log_compression_level: 0,
        history_keep_turns: 12,
        max_search_results: 50,
        compress_tool_output: false,
    };
    const EXPLORATION: Self = Self {
        code_preserve: false,
        log_compression_level: 3,
        history_keep_turns: 4,
        max_search_results: 8,
        compress_tool_output: true,
    };
    const RESEARCH: Self = Self {
        code_preserve: true,
        log_compression_level: 2,
        history_keep_turns: 6,
        max_search_results: 20,
        compress_tool_output: true,
    };
    const CHAT: Self = Self {
        code_preserve: false,
        log_compression_level: 3,
        history_keep_turns: 3,
        max_search_results: 10,
        compress_tool_output: true,
    };
}

/// Maps equivalent triage labels onto the policy classes persisted in the outcome log.
fn canonical_task_class(task_class: &str) -> &str {
    match task_class {
        "debug" => "debugging",
        "explore" => "exploration",
        "question" => "chat",
        _ => task_class,
    }
}

/// Selects the task-aware default, normalizing `pre_optimize` coding variants.
pub fn select_policy(task_class: &str) -> CompressionPolicy {
    match canonical_task_class(task_class) {
        "coding" | "coding_fix" | "coding_new" | "refactor" => CompressionPolicy::CODING,
        "debugging" => CompressionPolicy::DEBUGGING,
        "exploration" => CompressionPolicy::EXPLORATION,
        "research" => CompressionPolicy::RESEARCH,
        _ => CompressionPolicy::CHAT,
    }
}

/// A persisted Value Gate feedback observation for a policy decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyOutcome {
    pub task_class: String,
    pub policy_used: CompressionPolicy,
    pub tokens_saved: usize,
    pub session_success: Option<bool>,
    /// Percentage of the task input removed by compression, when measured.
    #[serde(default)]
    pub savings_pct: f64,
    pub timestamp: u64,
}

/// Records an outcome without making compression availability depend on local I/O.
pub fn record_outcome(
    task_class: impl AsRef<str>,
    policy: CompressionPolicy,
    tokens_saved: usize,
    success: Option<bool>,
) {
    record_outcome_with_savings(task_class, policy, tokens_saved, success, 0.0);
}

/// Records an outcome including the measured compression savings percentage.
pub fn record_outcome_with_savings(
    task_class: impl AsRef<str>,
    policy: CompressionPolicy,
    tokens_saved: usize,
    success: Option<bool>,
    savings_pct: f64,
) {
    let outcome = PolicyOutcome {
        task_class: canonical_task_class(task_class.as_ref()).to_owned(),
        policy_used: policy,
        tokens_saved,
        session_success: success,
        savings_pct,
        timestamp: now_unix(),
    };
    let _ = append_outcome(&outcomes_path(), &outcome);
}

/// Stores the Value Gate verdict in the adaptive-policy JSONL feedback log.
///
/// `savings_pct` is zero when the caller has no before-compression baseline;
/// this preserves honest telemetry while still teaching policy success rates.
pub fn record_value_gate_outcome(
    task_class: impl AsRef<str>,
    policy: CompressionPolicy,
    accepted: bool,
    savings_pct: f64,
) {
    record_outcome_with_savings(task_class, policy, 0, Some(accepted), savings_pct);
}

/// Returns the historically highest-success policy, or the task-class default.
pub fn best_policy_for(task_class: &str) -> CompressionPolicy {
    best_policy_from(task_class, &outcomes_path())
}

fn best_policy_from(task_class: &str, path: &Path) -> CompressionPolicy {
    let task_class = canonical_task_class(task_class);
    let mut policies = BTreeMap::<PolicyKey, PolicyStats>::new();
    for outcome in load_outcomes(path) {
        if canonical_task_class(&outcome.task_class) != task_class {
            continue;
        }
        let Some(success) = outcome.session_success else {
            continue;
        };
        let stats = policies
            .entry(PolicyKey::from(outcome.policy_used))
            .or_default();
        stats.total += 1;
        stats.successes += usize::from(success);
    }

    policies
        .into_iter()
        .filter(|(_, stats)| stats.total > 0)
        .max_by(|(left_key, left), (right_key, right)| {
            (
                left.successes * right.total,
                left.successes,
                left.total,
                left_key,
            )
                .cmp(&(
                    right.successes * left.total,
                    right.successes,
                    right.total,
                    right_key,
                ))
        })
        .map(|(key, _)| key.into())
        .unwrap_or_else(|| select_policy(task_class))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PolicyKey {
    code_preserve: bool,
    log_compression_level: u8,
    history_keep_turns: u8,
    max_search_results: u16,
    compress_tool_output: bool,
}

impl From<CompressionPolicy> for PolicyKey {
    fn from(policy: CompressionPolicy) -> Self {
        Self {
            code_preserve: policy.code_preserve,
            log_compression_level: policy.log_compression_level,
            history_keep_turns: policy.history_keep_turns,
            max_search_results: policy.max_search_results,
            compress_tool_output: policy.compress_tool_output,
        }
    }
}

impl From<PolicyKey> for CompressionPolicy {
    fn from(key: PolicyKey) -> Self {
        Self {
            code_preserve: key.code_preserve,
            log_compression_level: key.log_compression_level,
            history_keep_turns: key.history_keep_turns,
            max_search_results: key.max_search_results,
            compress_tool_output: key.compress_tool_output,
        }
    }
}

#[derive(Default)]
struct PolicyStats {
    total: usize,
    successes: usize,
}

fn outcomes_path() -> PathBuf {
    crate::core::paths::data_dir()
        .unwrap_or_else(|_| PathBuf::from(".local/share/lean-ctx"))
        .join("policy_outcomes.jsonl")
}

fn append_outcome(path: &Path, outcome: &PolicyOutcome) -> std::io::Result<()> {
    let line = serde_json::to_string(outcome)
        .map_err(|error| std::io::Error::other(format!("serialize policy outcome: {error}")))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

fn load_outcomes(path: &Path) -> Vec<PolicyOutcome> {
    fs::read_to_string(path)
        .ok()
        .into_iter()
        .flat_map(|content| content.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lean-ctx-adaptive-policy-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn coding_preserves_source_code() {
        let policy = select_policy("coding_new");
        assert!(policy.code_preserve);
        assert_eq!(policy.log_compression_level, 3);
    }

    #[test]
    fn debugging_preserves_all_error_context() {
        let policy = select_policy("debugging");
        assert_eq!(policy.log_compression_level, 0);
        assert!(!policy.compress_tool_output);
    }

    #[test]
    fn debugging_aliases_share_the_same_policy_class() {
        assert_eq!(select_policy("debug"), select_policy("debugging"));
    }

    #[test]
    fn exploration_compresses_aggressively() {
        let policy = select_policy("exploration");
        assert_eq!(policy.log_compression_level, 3);
        assert_eq!(policy.max_search_results, 8);
    }

    #[test]
    fn selection_normalizes_pre_optimize_classes() {
        assert_eq!(select_policy("coding_fix"), select_policy("coding"));
        assert_eq!(select_policy("coding_new"), select_policy("coding"));
        assert_eq!(select_policy("refactor"), select_policy("coding"));
        assert_eq!(select_policy("unknown"), select_policy("chat"));
    }

    #[test]
    fn recording_and_retrieval_choose_highest_success_rate() {
        let path = test_path("outcomes.jsonl");
        let _ = fs::remove_file(&path);
        let default = select_policy("research");
        let preferred = CompressionPolicy {
            log_compression_level: 1,
            ..default
        };
        append_outcome(
            &path,
            &PolicyOutcome {
                task_class: "research".into(),
                policy_used: default,
                tokens_saved: 10,
                session_success: Some(false),
                savings_pct: 10.0,
                timestamp: 1,
            },
        )
        .unwrap();
        append_outcome(
            &path,
            &PolicyOutcome {
                task_class: "research".into(),
                policy_used: preferred,
                tokens_saved: 20,
                session_success: Some(true),
                savings_pct: 20.0,
                timestamp: 2,
            },
        )
        .unwrap();

        assert_eq!(best_policy_from("research", &path), preferred);
        let _ = fs::remove_file(path);
    }
}
