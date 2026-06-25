//! AI session summaries (#292): periodically distil the working session into a
//! compact, semantically-recallable digest.
//!
//! Pipeline: [`generate::build_candidate`] (under the session lock, cheap, owned)
//! → [`maybe_record_periodic`] (off the hot path: cadence check + persist) →
//! [`recall::recall`] (semantic when embeddings are warm, else lexical).
//!
//! Deterministic and local-first: no LLM is required to produce or recall a
//! summary.

pub mod generate;
pub mod recall;
pub mod record;
pub mod store;

pub use recall::{RecallHit, recall};
pub use record::{SummaryCandidate, SummaryRecord};

use crate::core::session::SessionState;
use store::SummaryStore;

fn config() -> crate::core::config::SummariesConfig {
    crate::core::config::Config::load().summaries
}

/// Build a lock-free candidate from the live session. Call while holding the
/// session lock; persist the result off the hot path with [`maybe_record_periodic`].
#[must_use]
pub fn build_candidate(session: &SessionState) -> SummaryCandidate {
    generate::build_candidate(session)
}

/// Record `candidate` iff enabled and the turn cadence is due. Returns the title
/// of the recorded summary, or `None` if skipped.
#[must_use]
pub fn maybe_record_periodic(project_root: &str, candidate: SummaryCandidate) -> Option<String> {
    let cfg = config();
    if !cfg.enabled || !candidate.has_content {
        return None;
    }
    let store = SummaryStore::load_or_create(project_root);
    if candidate.tool_calls < store.last_recorded_calls + u64::from(cfg.every_n_turns) {
        return None;
    }
    let mut store = store;
    record_into(&mut store, candidate, cfg.max_kept as usize)
}

/// Force-record a summary now (explicit action), ignoring the turn cadence.
pub fn record_now(project_root: &str, candidate: SummaryCandidate) -> Result<String, String> {
    if !candidate.has_content {
        return Err("session has nothing to summarize yet".to_string());
    }
    let cfg = config();
    let mut store = SummaryStore::load_or_create(project_root);
    record_into(&mut store, candidate, cfg.max_kept as usize)
        .ok_or_else(|| "failed to persist summary".to_string())
}

fn record_into(
    store: &mut SummaryStore,
    candidate: SummaryCandidate,
    max_kept: usize,
) -> Option<String> {
    let calls = candidate.tool_calls;
    let seq = store.next_seq();
    let rec = candidate.into_record(seq);
    let title = rec.title.clone();
    store.last_recorded_calls = calls;
    store.push(rec, max_kept);
    store.save().ok()?;
    Some(title)
}

/// All stored summaries for a project (oldest first).
#[must_use]
pub fn list(project_root: &str) -> Vec<SummaryRecord> {
    SummaryStore::load_or_create(project_root).summaries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::SessionState;

    fn isolated() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path().join("data"));
        let root = tmp.path().join("proj").to_string_lossy().to_string();
        (tmp, root)
    }

    fn session_with_work(calls: u32) -> SessionState {
        let mut s = SessionState::new();
        s.set_task("Implement traversal edges", None);
        s.add_decision("Use Hebbian decay for co-access weights", None);
        s.touch_file("src/core/cooccurrence.rs", None, "full", 1200);
        s.stats.total_tool_calls = calls;
        s
    }

    #[test]
    fn cadence_gates_then_records() {
        let _g = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = isolated();

        // Below cadence (default every_n_turns=25): skipped.
        let c = build_candidate(&session_with_work(5));
        assert!(maybe_record_periodic(&root, c).is_none());

        // At/over cadence: recorded.
        let c = build_candidate(&session_with_work(30));
        assert!(maybe_record_periodic(&root, c).is_some());
        assert_eq!(list(&root).len(), 1);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn record_now_and_lexical_recall() {
        let _g = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = isolated();

        let c = build_candidate(&session_with_work(3));
        record_now(&root, c).unwrap();

        let hits = recall(&root, "traversal edges cooccurrence", 5);
        assert!(!hits.is_empty(), "should recall the summary lexically");
        assert!(hits[0].record.title.contains("traversal"));

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
