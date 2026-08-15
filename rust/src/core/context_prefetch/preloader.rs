//! Context prefetch planning from file-access trajectory predictions.
//!
//! Filters low-confidence and already-loaded files to build a bounded
//! preload plan for proactive context warming.

use super::trajectory::FileTrajectory;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

const LIVE_TRAJECTORY_CAPACITY: usize = 64;
const MAX_PREFETCH_FILES: usize = 3;
const MIN_PREFETCH_CONFIDENCE: f64 = 0.2;

#[derive(Debug)]
struct LivePrefetchState {
    trajectory: FileTrajectory,
    predictions: HashSet<String>,
}

impl Default for LivePrefetchState {
    fn default() -> Self {
        Self {
            trajectory: FileTrajectory::new(LIVE_TRAJECTORY_CAPACITY),
            predictions: HashSet::new(),
        }
    }
}

static LIVE_PREFETCH_STATE: OnceLock<Mutex<LivePrefetchState>> = OnceLock::new();

fn live_prefetch_state() -> &'static Mutex<LivePrefetchState> {
    LIVE_PREFETCH_STATE.get_or_init(|| Mutex::new(LivePrefetchState::default()))
}

fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_owned()
}

/// Record a completed `ctx_read` in the live trajectory used by the proxy
/// triage path. State is process-local because proxy requests and MCP tool
/// calls share the server process but not necessarily an executor thread.
pub fn record_file_read(path: &str) {
    if let Ok(mut state) = live_prefetch_state().lock() {
        state.trajectory.record(&normalize_path(path));
    } else {
        tracing::warn!("context prefetch trajectory lock poisoned; skipping read record");
    }
}

/// Build, retain, and asynchronously warm predictions after task triage.
pub fn plan_after_triage(task_class: &str) -> PrefetchPlan {
    let (plan, predictions) = {
        let Ok(mut state) = live_prefetch_state().lock() else {
            tracing::warn!("context prefetch state lock poisoned; skipping plan");
            return PrefetchPlan {
                files: Vec::new(),
                total_predicted_tokens: 0,
            };
        };

        // The active file is already loaded; do not predict it again. Older
        // trajectory entries remain candidates because they may be revisited.
        let loaded_files: Vec<String> = state
            .trajectory
            .accesses
            .last()
            .cloned()
            .into_iter()
            .collect();
        let loaded_refs: Vec<&str> = loaded_files.iter().map(String::as_str).collect();
        let plan = build_prefetch_plan(
            &state.trajectory,
            &loaded_refs,
            MAX_PREFETCH_FILES,
            MIN_PREFETCH_CONFIDENCE,
        );
        let predictions: Vec<String> = plan.files.iter().map(|entry| entry.path.clone()).collect();
        state.predictions = predictions.iter().cloned().collect();
        (plan, predictions)
    };

    if !predictions.is_empty() {
        tracing::debug!(
            task_class,
            predictions = ?predictions,
            "context prefetch plan created"
        );
        super::warming::warm_predictions(&predictions, None);
    }

    plan
}

/// Whether the current live prefetch plan predicted this path.
pub fn is_prefetch_prediction(path: &str) -> bool {
    live_prefetch_state()
        .lock()
        .is_ok_and(|state| state.predictions.contains(&normalize_path(path)))
}

/// A prefetch plan: files to preload and their predicted relevance.
#[derive(Debug, Clone)]
pub struct PrefetchPlan {
    /// Files selected for prefetch, highest confidence first.
    pub files: Vec<PrefetchEntry>,
    /// Sum of estimated token sizes; 0 until size integration is wired.
    pub total_predicted_tokens: usize,
}

/// One file candidate in a prefetch plan.
#[derive(Debug, Clone)]
pub struct PrefetchEntry {
    /// File path to preload.
    pub path: String,
    /// Transition probability in `(0.0, 1.0]`.
    pub confidence: f64,
    /// Human-readable selection rationale.
    pub reason: &'static str,
}

/// Build a prefetch plan from trajectory predictions and co-access data.
///
/// Predictions at or below `min_confidence` and files already present in the
/// current context are excluded.
pub fn build_prefetch_plan(
    trajectory: &FileTrajectory,
    loaded_files: &[&str],
    max_files: usize,
    min_confidence: f64,
) -> PrefetchPlan {
    let files: Vec<PrefetchEntry> = trajectory
        .predict(max_files.saturating_add(loaded_files.len()))
        .into_iter()
        .filter(|(path, confidence)| {
            *confidence > min_confidence && !loaded_files.contains(&path.as_str())
        })
        .take(max_files)
        .map(|(path, confidence)| PrefetchEntry {
            path,
            confidence,
            reason: "trajectory transition",
        })
        .collect();
    // Estimate unavailable without reading files; set to 0 until integration wiring provides cached sizes.
    let total_predicted_tokens = 0;

    PrefetchPlan {
        files,
        total_predicted_tokens,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn empty_trajectory_gives_empty_plan() {
        let plan = build_prefetch_plan(&FileTrajectory::new(10), &[], 3, 0.2);
        assert!(plan.files.is_empty());
        assert_eq!(plan.total_predicted_tokens, 0);
    }

    #[test]
    fn loaded_files_are_excluded() {
        let mut trajectory = FileTrajectory::new(10);
        for path in ["src/a.rs", "src/b.rs", "src/a.rs"] {
            trajectory.record(path);
        }

        let plan = build_prefetch_plan(&trajectory, &["src/b.rs"], 3, 0.2);
        assert!(plan.files.is_empty());
    }

    #[test]
    fn low_confidence_filtered() {
        let mut trajectory = FileTrajectory::new(10);
        for path in ["src/a.rs", "src/b.rs", "src/a.rs", "src/c.rs", "src/a.rs"] {
            trajectory.record(path);
        }

        let plan = build_prefetch_plan(&trajectory, &[], 3, 0.5);
        assert!(plan.files.is_empty());
    }

    #[test]
    fn selected_files_have_zero_token_estimate_until_wired() {
        let mut trajectory = FileTrajectory::new(10);
        for path in ["src/a.rs", "src/b.rs", "src/a.rs"] {
            trajectory.record(path);
        }

        let plan = build_prefetch_plan(&trajectory, &[], 1, 0.2);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.total_predicted_tokens, 0);
    }
}
