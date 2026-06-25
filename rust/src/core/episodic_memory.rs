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
    #[must_use]
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
    #[must_use]
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

    #[must_use]
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

    #[must_use]
    pub fn recent(&self, n: usize) -> Vec<&Episode> {
        self.episodes.iter().rev().take(n).collect()
    }

    #[must_use]
    pub fn by_outcome(&self, outcome_label: &str) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.outcome.label() == outcome_label)
            .collect()
    }

    #[must_use]
    pub fn by_file(&self, file_path: &str) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.affected_files.iter().any(|f| f.contains(file_path)))
            .collect()
    }

    #[must_use]
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

    #[must_use]
    pub fn load(project_hash: &str) -> Option<Self> {
        let path = Self::store_path(project_hash)?;
        let data = std::fs::read_to_string(path).ok()?;
        let mut store: Self = serde_json::from_str(&data).ok()?;
        if store.migrate_legacy_episodes() {
            let _ = store.save();
        }
        Some(store)
    }

    /// One-time repair for episodes recorded before per-task metrics existed:
    /// those episodes share one id per session (derived from the session
    /// *start* date) and carry cumulative session token counters instead of
    /// per-task deltas. Detected via duplicate ids within one session;
    /// idempotent because rewritten ids are unique afterwards.
    fn migrate_legacy_episodes(&mut self) -> bool {
        use std::collections::HashSet;

        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut needs_migration = false;
        for ep in &self.episodes {
            if !seen.insert((ep.session_id.clone(), ep.id.clone())) {
                needs_migration = true;
                break;
            }
        }
        if !needs_migration {
            return false;
        }

        let mut sessions: HashSet<String> = HashSet::new();
        for ep in &self.episodes {
            sessions.insert(ep.session_id.clone());
        }

        for session_id in sessions {
            let mut idx: Vec<usize> = (0..self.episodes.len())
                .filter(|&i| self.episodes[i].session_id == session_id)
                .collect();
            idx.sort_by_key(|&i| self.episodes[i].timestamp);

            // Cumulative counters are monotonically non-decreasing; only
            // then is converting to deltas safe.
            let monotonic = idx
                .windows(2)
                .all(|w| self.episodes[w[0]].tokens_used <= self.episodes[w[1]].tokens_used);

            let mut prev_tokens: u64 = 0;
            let mut prev_ts: Option<DateTime<Utc>> = None;
            let mut used_ids: HashSet<String> = HashSet::new();
            for &i in &idx {
                let ts = self.episodes[i].timestamp;
                let ep = &mut self.episodes[i];
                let mut id = format!("ep-{}", ts.format("%Y%m%d-%H%M%S"));
                let mut n = 1;
                while !used_ids.insert(id.clone()) {
                    n += 1;
                    id = format!("ep-{}-{n}", ts.format("%Y%m%d-%H%M%S"));
                }
                ep.id = id;
                if monotonic {
                    let cumulative = ep.tokens_used;
                    ep.tokens_used = cumulative.saturating_sub(prev_tokens);
                    prev_tokens = cumulative;
                }
                if ep.duration_secs == 0
                    && let Some(p) = prev_ts
                {
                    ep.duration_secs = (ts - p).num_seconds().max(0) as u64;
                }
                prev_ts = Some(ts);
            }
        }
        true
    }

    #[must_use]
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

#[must_use]
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
        // Record-time based id: session ids start with the session *start*
        // date, so deriving the episode id from them produced colliding ids
        // for every task completed in one long-running session.
        id: format!("ep-{}", Utc::now().format("%Y%m%d-%H%M%S")),
        session_id: session.id.clone(),
        timestamp: Utc::now(),
        task_description,
        actions,
        outcome,
        affected_files,
        summary: String::new(),
        duration_secs: 0,
        // Cumulative session counter at record time; the caller converts
        // this into a per-task delta (see `finalize_episode_metrics`).
        tokens_used: session.stats.total_tokens_saved,
    }
}

/// Converts the cumulative session counters captured by
/// [`create_episode_from_session`] into per-task values.
///
/// `tokens_used` becomes the delta since the previous episode of the same
/// session (so the per-session sum of episode tokens matches the session
/// total), and `duration_secs` becomes the wall-clock span this task was the
/// active one (since the previous episode, or since session start for the
/// first).
pub fn finalize_episode_metrics(
    episode: &mut Episode,
    store: &EpisodicStore,
    session_started_at: DateTime<Utc>,
) {
    let prior_tokens: u64 = store
        .episodes
        .iter()
        .filter(|e| e.session_id == episode.session_id)
        .map(|e| e.tokens_used)
        .sum();
    episode.tokens_used = episode.tokens_used.saturating_sub(prior_tokens);

    let since = store
        .episodes
        .iter()
        .filter(|e| e.session_id == episode.session_id)
        .map(|e| e.timestamp)
        .max()
        .unwrap_or(session_started_at);
    episode.duration_secs = (episode.timestamp - since).num_seconds().max(0) as u64;
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

#[must_use]
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

    #[test]
    fn finalize_metrics_converts_cumulative_to_delta() {
        let mut store = EpisodicStore::new("test");
        let started = Utc::now() - chrono::Duration::seconds(600);

        let mut first = make_episode("T1", Outcome::Unknown);
        first.session_id = "sess-x".to_string();
        first.tokens_used = 1000; // cumulative at record time
        first.timestamp = started + chrono::Duration::seconds(100);
        finalize_episode_metrics(&mut first, &store, started);
        assert_eq!(first.tokens_used, 1000);
        assert_eq!(first.duration_secs, 100);
        store.episodes.push(first);

        let mut second = make_episode("T2", Outcome::Unknown);
        second.session_id = "sess-x".to_string();
        second.tokens_used = 1800; // cumulative at record time
        second.timestamp = started + chrono::Duration::seconds(400);
        finalize_episode_metrics(&mut second, &store, started);
        assert_eq!(second.tokens_used, 800); // delta since first
        assert_eq!(second.duration_secs, 300);
    }

    #[test]
    fn migrate_legacy_dedupes_ids_and_converts_tokens() {
        let mut store = EpisodicStore::new("test");
        let base = Utc::now() - chrono::Duration::seconds(1000);
        for (i, cumulative) in [1000u64, 3000, 6000].iter().enumerate() {
            let mut ep = make_episode(&format!("T{i}"), Outcome::Unknown);
            ep.id = "ep-20260516".to_string(); // legacy colliding id
            ep.session_id = "sess-legacy".to_string();
            ep.tokens_used = *cumulative;
            ep.duration_secs = 0;
            ep.timestamp = base + chrono::Duration::seconds(i as i64 * 120);
            store.episodes.push(ep);
        }

        assert!(store.migrate_legacy_episodes());
        let ids: std::collections::HashSet<String> =
            store.episodes.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), 3, "ids must be unique after migration");
        let tokens: Vec<u64> = store.episodes.iter().map(|e| e.tokens_used).collect();
        assert_eq!(tokens, vec![1000, 2000, 3000]);
        // Idempotent: unique ids → no second migration.
        assert!(!store.migrate_legacy_episodes());
    }
}
