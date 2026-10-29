//! AI session summaries (#292): periodically distil the working session into a
//! compact, semantically-recallable digest.
//!
//! Pipeline: [`generate::build_candidate`] (under the session lock, cheap, owned)
//! → [`maybe_record_periodic`] (off the hot path: cadence check + persist) →
//! [`recall::recall`] (semantic when embeddings are warm, else lexical).
//!
//! Deterministic and local-first: no LLM is required to produce or recall a
//! summary.

pub(crate) mod generate;
pub(crate) mod recall;
pub(crate) mod record;
pub(crate) mod store;

pub(crate) use recall::recall;
pub(crate) use record::{SummaryCandidate, SummaryRecord};

use crate::core::session::SessionState;
use store::SummaryStore;

fn config() -> crate::core::config::SummariesConfig {
    crate::core::config::Config::load().summaries
}

/// Build a lock-free candidate from the live session. Call while holding the
/// session lock; persist the result off the hot path with [`maybe_record_periodic`].
pub(crate) fn build_candidate(session: &SessionState) -> SummaryCandidate {
    generate::build_candidate(session)
}

/// Record `candidate` iff enabled and the turn cadence is due. Returns the title
/// of the recorded summary, or `None` if skipped.
pub(crate) fn maybe_record_periodic(
    project_root: &str,
    candidate: SummaryCandidate,
) -> Option<String> {
    let cfg = config();
    if !cfg.enabled || !candidate.has_content {
        return None;
    }
    // The cadence watermark is read and advanced under the same store lock.
    SummaryStore::mutate_locked(project_root, |store| {
        if candidate.tool_calls < store.last_recorded_calls + u64::from(cfg.every_n_turns) {
            return None;
        }
        record_into(store, candidate, cfg.max_kept as usize)
    })
    .unwrap_or_else(|error| {
        tracing::warn!(event = "session_summary_not_recorded", %error);
        None
    })
}

/// Force-record a summary now (explicit action), ignoring the turn cadence.
pub(crate) fn record_now(
    project_root: &str,
    candidate: SummaryCandidate,
) -> Result<String, String> {
    if !candidate.has_content {
        return Err("session has nothing to summarize yet".to_string());
    }
    let cfg = config();
    match SummaryStore::mutate_locked(project_root, |store| {
        record_into(store, candidate, cfg.max_kept as usize)
    }) {
        Ok(Some(title)) => Ok(title),
        Ok(None) => Err("failed to persist summary".to_string()),
        Err(error) => Err(format!("failed to persist summary: {error}")),
    }
}

/// Adds the admitted record to `store`; the caller persists it.
fn record_into(
    store: &mut SummaryStore,
    candidate: SummaryCandidate,
    max_kept: usize,
) -> Option<String> {
    let calls = candidate.tool_calls;
    let seq = store.next_seq();
    // Stored bytes: the ADMITTED record only. A candidate with a field the
    // gateway withholds or classifies as restricted is not recorded at all.
    let rec = candidate.into_record(seq).admitted_for_store()?;
    let title = rec.title.clone();
    store.last_recorded_calls = calls;
    store.push(rec, max_kept);
    Some(title)
}

/// The stored summaries of a project as they may be served NOW (oldest
/// first): each record through the originless rule, a record that is not
/// released left out. The store on disk is not touched by this view.
pub(crate) fn list(project_root: &str) -> Vec<SummaryRecord> {
    served(SummaryStore::load_or_create(project_root)).summaries
}

/// `store` reduced to the records that may be served now. A read-only view:
/// callers never save it.
pub(crate) fn served(mut store: SummaryStore) -> SummaryStore {
    store.summaries = store
        .summaries
        .iter()
        .filter_map(SummaryRecord::served)
        .collect();
    store
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

    /// E3 / G04: a summary was persisted exactly as it was distilled and
    /// recalled exactly as it was stored. It is now stored admitted (a masked
    /// value stays masked on disk) and served under the originless rule (not
    /// at all once a project policy is active).
    #[test]
    fn summaries_are_stored_admitted_and_served_under_the_originless_rule() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let mut session = session_with_work(3);
            session.add_decision(&format!("Rotate the deploy key {AWS_KEY} weekly"), None);
            record_now(&root, build_candidate(&session)).unwrap();

            let on_disk = SummaryStore::load_or_create(&root);
            assert_eq!(on_disk.summaries.len(), 1);
            let stored = serde_json::to_string(&on_disk.summaries).unwrap();
            assert!(!stored.contains(AWS_KEY), "the store holds a raw key");

            // Control: a clean summary is recalled as before.
            let hits = recall(&root, "traversal edges", 5);
            assert!(!hits.is_empty());
            assert!(hits[0].record.title.contains("traversal"));
            assert_eq!(list(&root).len(), 1);
        }
        let policy = crate::core::policy::load(
            "name = \"session-summary-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        assert!(list(&root).is_empty(), "no source authority, not served");
        assert!(recall(&root, "traversal edges", 5).is_empty());
        assert_eq!(
            SummaryStore::load_or_create(&root).summaries.len(),
            1,
            "the served view never changes the store"
        );
    }

    /// AP07-G7: the store was loaded and written without a lock (and not
    /// atomically), so parallel recorders dropped each other's summaries.
    #[test]
    fn parallel_recorders_keep_every_summary() {
        use std::sync::{Arc, Barrier};
        const RECORDERS: usize = 8;
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();
        let barrier = Arc::new(Barrier::new(RECORDERS));
        let recorders: Vec<_> = (0..RECORDERS)
            .map(|_| {
                let candidate = build_candidate(&session_with_work(3));
                let (barrier, root) = (Arc::clone(&barrier), root.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    record_now(&root, candidate)
                })
            })
            .collect();
        for recorder in recorders {
            recorder.join().unwrap().unwrap();
        }
        assert_eq!(
            SummaryStore::load_or_create(&root).summaries.len(),
            RECORDERS
        );
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
