//! Episodic Memory — persistent cross-session experiences with outcomes.
//!
//! Automatically records what the agent did in each session, with what result.
//! Enables learning from past experiences: "What happened last time I refactored auth?"

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::core::memory_policy::EpisodicPolicy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodicStore {
    pub project_hash: String,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub task_description: String,
    pub actions: Vec<Action>,
    pub outcome: Outcome,
    pub affected_files: Vec<String>,
    pub summary: String,
    pub duration_secs: u64,
    pub tokens_used: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub tool: String,
    pub description: String,
    pub timestamp: DateTime<Utc>,
    pub duration_ms: u64,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Outcome {
    Success { tests_passed: bool },
    Failure { error: String },
    Partial { details: String },
    Unknown,
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Success { .. } => "success",
            Outcome::Failure { .. } => "failure",
            Outcome::Partial { .. } => "partial",
            Outcome::Unknown => "unknown",
        }
    }
}

impl EpisodicStore {
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            episodes: Vec::new(),
        }
    }

    pub fn record_episode(&mut self, mut episode: Episode, policy: &EpisodicPolicy) {
        episode.actions.truncate(policy.max_actions_per_episode);

        if episode.summary.is_empty() {
            episode.summary = auto_summarize(&episode, policy.summary_max_chars);
        }

        self.episodes.push(episode);

        if self.episodes.len() > policy.max_episodes {
            self.episodes
                .drain(0..self.episodes.len() - policy.max_episodes);
        }
    }

    pub fn search(&self, query: &str) -> Vec<&Episode> {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();

        let mut scored: Vec<(&Episode, f32)> = self
            .episodes
            .iter()
            .filter_map(|ep| {
                let searchable = format!(
                    "{} {} {}",
                    ep.task_description.to_lowercase(),
                    ep.summary.to_lowercase(),
                    ep.affected_files.join(" ").to_lowercase()
                );
                let hits = terms.iter().filter(|t| searchable.contains(**t)).count();
                if hits > 0 {
                    let relevance = hits as f32 / terms.len() as f32;
                    Some((ep, relevance))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(ep, _)| ep).collect()
    }

    pub fn recent(&self, n: usize) -> Vec<&Episode> {
        self.episodes.iter().rev().take(n).collect()
    }

    pub fn by_outcome(&self, outcome_label: &str) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.outcome.label() == outcome_label)
            .collect()
    }

    pub fn by_file(&self, file_path: &str) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.affected_files.iter().any(|f| f.contains(file_path)))
            .collect()
    }

    pub fn stats(&self) -> EpisodicStats {
        let total = self.episodes.len();
        let successes = self
            .episodes
            .iter()
            .filter(|ep| matches!(ep.outcome, Outcome::Success { .. }))
            .count();
        let failures = self
            .episodes
            .iter()
            .filter(|ep| matches!(ep.outcome, Outcome::Failure { .. }))
            .count();
        let total_tokens: u64 = self.episodes.iter().map(|ep| ep.tokens_used).sum();

        EpisodicStats {
            total_episodes: total,
            successes,
            failures,
            success_rate: if total > 0 {
                successes as f32 / total as f32
            } else {
                0.0
            },
            total_tokens,
        }
    }

    fn store_path(project_hash: &str) -> Option<PathBuf> {
        let dir = crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("memory")
            .join("episodes");
        Some(dir.join(format!("{project_hash}.json")))
    }

    pub fn load(project_hash: &str) -> Option<Self> {
        let path = Self::store_path(project_hash)?;
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn load_or_create(project_hash: &str) -> Self {
        Self::load(project_hash).unwrap_or_else(|| Self::new(project_hash))
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::store_path(&self.project_hash)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("{e}"))?;
        std::fs::write(path, json).map_err(|e| format!("{e}"))
    }
}

#[derive(Debug)]
pub struct EpisodicStats {
    pub total_episodes: usize,
    pub successes: usize,
    pub failures: usize,
    pub success_rate: f32,
    pub total_tokens: u64,
}

pub fn create_episode_from_session(
    session: &super::session::SessionState,
    tool_calls: &[(String, u64)],
) -> Episode {
    let actions: Vec<Action> = tool_calls
        .iter()
        .map(|(tool, duration_ms)| Action {
            tool: tool.clone(),
            description: String::new(),
            timestamp: Utc::now(),
            duration_ms: *duration_ms,
            success: true,
        })
        .collect();

    let affected_files: Vec<String> = session
        .files_touched
        .iter()
        .map(|f| f.path.clone())
        .collect();

    let task_description = session
        .task
        .as_ref()
        .map(|t| t.description.clone())
        .unwrap_or_default();

    let outcome = if session.findings.iter().any(|f| {
        f.summary.to_lowercase().contains("error") || f.summary.to_lowercase().contains("failed")
    }) {
        Outcome::Failure {
            error: session
                .findings
                .iter()
                .find(|f| {
                    f.summary.to_lowercase().contains("error")
                        || f.summary.to_lowercase().contains("failed")
                })
                .map(|f| f.summary.clone())
                .unwrap_or_default(),
        }
    } else if !session.findings.is_empty() || !session.decisions.is_empty() {
        Outcome::Success { tests_passed: true }
    } else {
        Outcome::Unknown
    };

    Episode {
        id: format!("ep-{}", &session.id[..8.min(session.id.len())]),
        session_id: session.id.clone(),
        timestamp: Utc::now(),
        task_description,
        actions,
        outcome,
        affected_files,
        summary: String::new(),
        duration_secs: 0,
        tokens_used: session.stats.total_tokens_saved,
    }
}

fn auto_summarize(episode: &Episode, max_chars: usize) -> String {
    let tool_counts = count_tools(&episode.actions);
    let top_tools: Vec<String> = tool_counts
        .into_iter()
        .take(3)
        .map(|(tool, count)| format!("{tool}x{count}"))
        .collect();

    let files_hint = if episode.affected_files.len() <= 3 {
        episode.affected_files.join(", ")
    } else {
        format!(
            "{}, ... +{} more",
            episode.affected_files[..3].join(", "),
            episode.affected_files.len() - 3
        )
    };

    let task = if episode.task_description.chars().count() > max_chars {
        episode.task_description.chars().take(max_chars).collect()
    } else {
        episode.task_description.clone()
    };
    let mut summary = format!(
        "{task} [{}] tools:[{}]",
        episode.outcome.label(),
        top_tools.join(",")
    );

    if !files_hint.is_empty() {
        summary.push_str(&format!(" files:[{files_hint}]"));
    }

    summary
}

fn count_tools(actions: &[Action]) -> Vec<(String, usize)> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for action in actions {
        *counts.entry(&action.tool).or_insert(0) += 1;
    }
    let mut sorted: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    sorted.sort_by_key(|item| std::cmp::Reverse(item.1));
    sorted
}

pub fn format_episode_compact(episode: &Episode) -> String {
    format!(
        "[{}] {} — {} ({} actions, {} files)",
        episode.outcome.label(),
        episode.task_description,
        episode.summary,
        episode.actions.len(),
        episode.affected_files.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_episode(task: &str, outcome: Outcome) -> Episode {
        Episode {
            id: "ep-test".to_string(),
            session_id: "sess-1".to_string(),
            timestamp: Utc::now(),
            task_description: task.to_string(),
            actions: vec![
                Action {
                    tool: "ctx_read".to_string(),
                    description: String::new(),
                    timestamp: Utc::now(),
                    duration_ms: 50,
                    success: true,
                },
                Action {
                    tool: "ctx_shell".to_string(),
                    description: String::new(),
                    timestamp: Utc::now(),
                    duration_ms: 200,
                    success: true,
                },
            ],
            outcome,
            affected_files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
            summary: String::new(),
            duration_secs: 60,
            tokens_used: 5000,
        }
    }

    #[test]
    fn record_and_search() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        store.record_episode(
            make_episode(
                "Refactor auth module",
                Outcome::Success { tests_passed: true },
            ),
            &policy,
        );
        store.record_episode(
            make_episode(
                "Fix database connection",
                Outcome::Failure {
                    error: "timeout".to_string(),
                },
            ),
            &policy,
        );

        let results = store.search("auth refactor");
        assert_eq!(results.len(), 1);
        assert!(results[0].task_description.contains("auth"));
    }

    #[test]
    fn filter_by_outcome() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        store.record_episode(
            make_episode("Task 1", Outcome::Success { tests_passed: true }),
            &policy,
        );
        store.record_episode(
            make_episode(
                "Task 2",
                Outcome::Failure {
                    error: "err".to_string(),
                },
            ),
            &policy,
        );
        store.record_episode(
            make_episode(
                "Task 3",
                Outcome::Success {
                    tests_passed: false,
                },
            ),
            &policy,
        );

        assert_eq!(store.by_outcome("success").len(), 2);
        assert_eq!(store.by_outcome("failure").len(), 1);
    }

    #[test]
    fn filter_by_file() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        store.record_episode(make_episode("Task", Outcome::Unknown), &policy);

        let results = store.by_file("main.rs");
        assert_eq!(results.len(), 1);

        let results = store.by_file("nonexistent.rs");
        assert!(results.is_empty());
    }

    #[test]
    fn recent_episodes() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        for i in 0..5 {
            store.record_episode(
                make_episode(&format!("Task {i}"), Outcome::Unknown),
                &policy,
            );
        }

        let recent = store.recent(3);
        assert_eq!(recent.len(), 3);
        assert!(recent[0].task_description.contains('4'));
    }

    #[test]
    fn stats_calculation() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        store.record_episode(
            make_episode("T1", Outcome::Success { tests_passed: true }),
            &policy,
        );
        store.record_episode(
            make_episode(
                "T2",
                Outcome::Failure {
                    error: "e".to_string(),
                },
            ),
            &policy,
        );
        store.record_episode(
            make_episode(
                "T3",
                Outcome::Success {
                    tests_passed: false,
                },
            ),
            &policy,
        );

        let stats = store.stats();
        assert_eq!(stats.total_episodes, 3);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.failures, 1);
        assert!((stats.success_rate - 0.6667).abs() < 0.01);
    }

    #[test]
    fn auto_summary_generation() {
        let mut ep = make_episode("Fix the login bug", Outcome::Success { tests_passed: true });
        ep.summary = String::new();
        let summary = auto_summarize(&ep, EpisodicPolicy::default().summary_max_chars);
        assert!(summary.contains("Fix the login bug"));
        assert!(summary.contains("[success]"));
        assert!(summary.contains("ctx_read"));
    }

    #[test]
    fn max_episodes_enforced() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        for i in 0..510 {
            store.record_episode(
                make_episode(&format!("Task {i}"), Outcome::Unknown),
                &policy,
            );
        }
        assert!(store.episodes.len() <= policy.max_episodes);
    }

    #[test]
    fn format_compact() {
        let ep = make_episode("Deploy v2", Outcome::Success { tests_passed: true });
        let output = format_episode_compact(&ep);
        assert!(output.contains("[success]"));
        assert!(output.contains("Deploy v2"));
    }
}
