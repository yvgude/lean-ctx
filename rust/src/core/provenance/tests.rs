use std::thread;

use chrono::Utc;
use tempfile::tempdir;

use super::{
    CheckpointLinkState, CheckpointRecord, ObservationConfidence, ProvenanceRecord,
    ProvenanceStore, ProvenanceTracker,
};

/// A store keyed by the SHA-256 of the raw path (releases up to 3.12.0) is
/// moved to the `hash_project_root` key and keeps its records.
#[test]
fn tracker_adopts_a_store_under_the_legacy_path_hash() {
    use sha2::{Digest, Sha256};
    let data = tempdir().expect("data dir");
    let project = tempdir().expect("project dir");
    let root = project.path().to_string_lossy().into_owned();
    let legacy_key = hex::encode(Sha256::digest(root.as_bytes()));
    let legacy = ProvenanceStore::with_data_dir(data.path(), &legacy_key).expect("legacy store");
    legacy
        .record_file_touch(record("src/old.rs", "session-1", "op-legacy"))
        .expect("legacy record");

    let tracker = ProvenanceTracker::open_in(data.path(), project.path()).expect("tracker");
    assert_eq!(
        tracker.store().project_hash(),
        crate::core::project_hash::hash_project_root(&root)
    );
    assert_eq!(tracker.store().query_by_path("src/old.rs").len(), 1);
    assert!(!data.path().join("provenance").join(&legacy_key).exists());
}

fn store() -> (tempfile::TempDir, ProvenanceStore) {
    let directory = tempdir().expect("create temporary provenance directory");
    let store = ProvenanceStore::with_data_dir(directory.path(), "test-project")
        .expect("create provenance store");
    (directory, store)
}

fn record(path: &str, session_id: &str, operation_id: &str) -> ProvenanceRecord {
    ProvenanceRecord {
        project_hash: "test-project".to_owned(),
        session_id: session_id.to_owned(),
        agent_id: "agent-1".to_owned(),
        operation_id: operation_id.to_owned(),
        path: path.to_owned(),
        tool: "ctx_patch".to_owned(),
        observed_at: Utc::now(),
        before_sha256: Some("before".to_owned()),
        after_sha256: Some("after".to_owned()),
        lines_added: 3,
        lines_removed: 1,
        confidence: ObservationConfidence::Verified,
        ..ProvenanceRecord::default()
    }
}

#[test]
fn test_record_and_query_file_touch() {
    let (_directory, store) = store();
    let id = store
        .record_file_touch(record("src/lib.rs", "session-1", "operation-1"))
        .expect("record file touch");

    let records = store.query_by_path("src/lib.rs");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].confidence, ObservationConfidence::Verified);
}

#[test]
fn test_record_checkpoint() {
    let (_directory, store) = store();
    let id = store
        .record_checkpoint(CheckpointRecord {
            session_id: "session-1".to_owned(),
            commit_sha: "abc123".to_owned(),
            link_state: CheckpointLinkState::Orphaned,
            observed_at: Utc::now(),
            files_touched: 2,
            insertions: 4,
            deletions: 1,
            ..CheckpointRecord::default()
        })
        .expect("record checkpoint");

    let records = store.query_checkpoints(Some("session-1"));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].commit_sha, "abc123");
}

#[test]
fn test_query_by_path_filters_correctly() {
    let (_directory, store) = store();
    store
        .record_file_touch(record("src/a.rs", "session-1", "operation-1"))
        .expect("record first touch");
    store
        .record_file_touch(record("src/b.rs", "session-1", "operation-2"))
        .expect("record second touch");

    let records = store.query_by_path("src/a.rs");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, "src/a.rs");
}

#[test]
fn test_query_by_session() {
    let (_directory, store) = store();
    store
        .record_file_touch(record("src/a.rs", "session-1", "operation-1"))
        .expect("record first touch");
    store
        .record_file_touch(record("src/a.rs", "session-2", "operation-2"))
        .expect("record second touch");

    let records = store.query_by_session("session-2");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].session_id, "session-2");
}

#[test]
fn test_observe_edit_creates_record() {
    let (directory, store) = store();
    let tracker = ProvenanceTracker::with_store(store.clone(), directory.path());

    let id = tracker
        .observe_edit(
            "src/lib.rs",
            "ctx_patch",
            "before",
            "after",
            4,
            2,
            "session-1",
            "agent-1",
        )
        .expect("observe edit");

    let records = store.query_by_path("src/lib.rs");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].lines_added, 4);
}

#[test]
fn test_link_decisions() {
    let (directory, store) = store();
    let tracker = ProvenanceTracker::with_store(store.clone(), directory.path());
    let id = tracker
        .observe_edit(
            "src/lib.rs",
            "ctx_patch",
            "before",
            "after",
            1,
            0,
            "session-1",
            "agent-1",
        )
        .expect("observe edit");

    tracker
        .link_decisions(&id, vec!["decision-1".to_owned(), "decision-2".to_owned()])
        .expect("link decisions");

    assert_eq!(
        store.query_by_path("src/lib.rs")[0].decision_ids,
        vec!["decision-1", "decision-2"]
    );
}

#[test]
fn test_provenance_id_format() {
    let (_directory, store) = store();
    let id = store
        .record_file_touch(record("src/lib.rs", "session-1", "operation-1"))
        .expect("record file touch");

    assert!(id.starts_with("prov-"));
    assert_eq!(id.len(), 37);
    assert!(id[5..].bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[test]
fn test_serde_roundtrip() {
    let mut original = record("src/lib.rs", "session-1", "operation-1");
    original.decision_ids = vec!["decision-1".to_owned()];
    original.checkpoint_id = Some("ckpt-1".to_owned());

    let serialized = serde_json::to_string(&original).expect("serialize provenance record");
    let restored: ProvenanceRecord =
        serde_json::from_str(&serialized).expect("deserialize provenance record");
    assert_eq!(restored, original);
}

#[test]
fn test_concurrent_writes() {
    let (_directory, store) = store();
    let handles = (0..8)
        .map(|number| {
            let store = store.clone();
            thread::spawn(move || {
                store
                    .record_file_touch(record(
                        &format!("src/{number}.rs"),
                        "session-1",
                        &format!("operation-{number}"),
                    ))
                    .expect("concurrent write")
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("writer thread completes");
    }
    assert_eq!(store.query_by_session("session-1").len(), 8);
}
