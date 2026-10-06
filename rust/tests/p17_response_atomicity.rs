// SPDX-License-Identifier: Apache-2.0

#[path = "../src/http_server/relay_replay.rs"]
mod relay_replay;

use relay_replay::{MIN_RETENTION_SECONDS, RelayReplayStore, Reservation, StoreError, StoreLimits};
use std::fs;
use std::path::Path;

const NOW: i64 = 1_700_000_000;

fn open(root: &Path, bytes: u64) -> Result<RelayReplayStore, StoreError> {
    RelayReplayStore::new(root, StoreLimits::new(8, 8, bytes), MIN_RETENTION_SECONDS)
}

fn id(number: u64) -> String {
    format!("{number:064x}")
}

fn reserve(store: &RelayReplayStore, number: u64) -> Result<Reservation, StoreError> {
    store.reserve(
        &id(number),
        "origin",
        "tenant",
        "project",
        &id(number),
        NOW + MIN_RETENTION_SECONDS,
        NOW,
    )
}

fn lease(store: &RelayReplayStore, number: u64) -> String {
    match reserve(store, number).expect("reserve fixture") {
        Reservation::Reserved(lease) => lease,
        other => panic!("expected new reservation, got {other:?}"),
    }
}

#[test]
fn full_database_response_failure_rolls_back_completion_and_recovers() {
    let root = tempfile::tempdir().expect("temporary root");
    let store = open(root.path(), 128 * 1024).expect("initialize");
    let completed = lease(&store, 1);
    store
        .complete_with_response(&id(1), &completed, "existing response")
        .expect("complete fixture");
    let pending = lease(&store, 2);
    let path = store.database_path().to_path_buf();
    drop(store);
    let bound = fs::metadata(&path).expect("database metadata").len();
    assert!(
        bound < 64 * 1024,
        "fixture must force overflow-page allocation"
    );
    let bounded = open(root.path(), bound).expect("open at current page limit");
    let before = fs::read(&path).expect("snapshot database");
    let body = "x".repeat(64 * 1024);
    let error = bounded
        .complete_with_response(&id(2), &pending, &body)
        .expect_err("SQLITE_FULL required");
    assert!(matches!(&error, StoreError::Unavailable(_)), "{error:?}");
    assert!(
        error.to_string().contains("full"),
        "must fail on page capacity: {error}"
    );
    assert_eq!(fs::read(&path).expect("database after failure"), before);
    assert_eq!(reserve(&bounded, 2), Ok(Reservation::InFlight));
    assert_eq!(
        bounded
            .completed_response(&id(2), "origin", "tenant", "project", &id(2))
            .expect("pending lookup"),
        None
    );
    assert_eq!(
        bounded
            .completed_response(&id(1), "origin", "tenant", "project", &id(1))
            .expect("completed lookup")
            .as_deref(),
        Some("existing response")
    );
    drop(bounded);
    let recovered = open(root.path(), 128 * 1024).expect("restore capacity");
    recovered
        .complete_with_response(&id(2), &pending, &body)
        .expect("same lease completes after recovery");
    assert_eq!(reserve(&recovered, 2), Ok(Reservation::Completed));
    assert_eq!(
        recovered
            .completed_response(&id(2), "origin", "tenant", "project", &id(2))
            .expect("recovered response"),
        Some(body)
    );
    assert_eq!(
        recovered.release(&id(2), &pending),
        Err(StoreError::LeaseMismatch)
    );
}

#[test]
fn full_database_migration_rolls_back_schema_and_preserves_leases() {
    let root = tempfile::tempdir().expect("temporary root");
    let store = open(root.path(), 128 * 1024).expect("initialize");
    let completed = lease(&store, 1);
    store
        .complete(&id(1), &completed)
        .expect("complete old proof");
    let pending = lease(&store, 2);
    let path = store.database_path().to_path_buf();
    drop(store);
    let connection = rusqlite::Connection::open(&path).expect("fixture connection");
    connection.execute_batch("DROP TABLE relay_task_responses; UPDATE relay_replay_meta SET schema_version=1; PRAGMA user_version=1; VACUUM;").expect("exact v1 fixture without spare pages");
    let free: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .expect("free pages");
    assert_eq!(free, 0);
    drop(connection);
    let before = fs::read(&path).expect("snapshot v1 database");
    let marker_path = path.with_extension("established");
    let marker = fs::read(&marker_path).expect("snapshot marker");
    let error =
        open(root.path(), before.len() as u64).expect_err("migration needs additional pages");
    assert!(matches!(&error, StoreError::Unavailable(_)), "{error:?}");
    assert!(
        error.to_string().contains("full"),
        "must fail on page capacity: {error}"
    );
    assert_eq!(
        fs::read(&path).expect("v1 after rejected migration"),
        before
    );
    assert_eq!(
        fs::read(&marker_path).expect("marker after failure"),
        marker
    );
    let connection = rusqlite::Connection::open(&path).expect("inspect rollback");
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    let meta: i64 = connection
        .query_row("SELECT schema_version FROM relay_replay_meta", [], |row| {
            row.get(0)
        })
        .expect("metadata version");
    let responses: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='relay_task_responses'",
            [],
            |row| row.get(0),
        )
        .expect("response table presence");
    assert_eq!((version, meta, responses), (1, 1, 0));
    drop(connection);
    let recovered = open(root.path(), 128 * 1024).expect("migration after restoring capacity");
    assert_eq!(reserve(&recovered, 1), Ok(Reservation::Completed));
    assert_eq!(reserve(&recovered, 2), Ok(Reservation::InFlight));
    recovered
        .release(&id(2), &pending)
        .expect("original lease remains valid");
    assert!(matches!(
        reserve(&recovered, 2),
        Ok(Reservation::Reserved(_))
    ));
}
