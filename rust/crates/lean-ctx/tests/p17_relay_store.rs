// SPDX-License-Identifier: Apache-2.0

#[path = "../src/http_server/relay_replay.rs"]
mod relay_replay;

use relay_replay::{
    MAX_RETENTION_SECONDS, MIN_RETENTION_SECONDS, RelayReplayLimits, RelayReplayStore, Reservation,
    StoreError, StoreLimits,
};
use std::fs;
use std::path::Path;
use tempfile::{TempDir, tempdir};

const NOW: i64 = 1_700_000_000;

fn limits(max_rows: u64, per_origin_max_rows: u64) -> StoreLimits {
    RelayReplayLimits::new(max_rows, per_origin_max_rows, 128 * 1024)
}

fn new_store(root: &Path, max_rows: u64, per_origin_max_rows: u64) -> RelayReplayStore {
    RelayReplayStore::new(
        root,
        limits(max_rows, per_origin_max_rows),
        MIN_RETENTION_SECONDS,
    )
    .expect("test store should initialize")
}

fn digest(number: u64) -> String {
    format!("{number:064x}")
}

fn expiry(now: i64) -> i64 {
    now + MIN_RETENTION_SECONDS + 100
}

fn reserve(
    store: &RelayReplayStore,
    number: u64,
    origin: &str,
    fingerprint: u64,
    now: i64,
) -> String {
    match store
        .reserve(
            &digest(number),
            origin,
            "tenant",
            "project",
            &digest(fingerprint),
            expiry(now),
            now,
        )
        .expect("reservation should succeed")
    {
        Reservation::Reserved(lease) => lease,
        result => panic!("expected reservation, got {result:?}"),
    }
}

fn root_with_a2a() -> TempDir {
    let root = tempdir().expect("temporary root");
    fs::create_dir_all(root.path().join(".lean-ctx").join("a2a")).expect("a2a directory");
    root
}

#[test]
fn completed_response_is_exact_scope_bound_and_durable() {
    let root = tempdir().expect("response replay fixture or operation must succeed");
    let store = new_store(root.path(), 8, 8);
    let lease = reserve(&store, 1, "origin", 10, NOW);
    let body = "{\n  \"opaque_signed_response\": true\n}";
    assert_eq!(
        store
            .completed_response(&digest(1), "origin", "tenant", "project", &digest(10))
            .expect("response replay fixture or operation must succeed"),
        None
    );
    store
        .complete_with_response(&digest(1), &lease, body)
        .expect("response replay fixture or operation must succeed");
    store
        .complete_with_response(&digest(1), &lease, body)
        .expect("response replay fixture or operation must succeed");
    assert!(
        store
            .complete_with_response(&digest(1), &lease, "changed")
            .is_err()
    );
    assert!(store.complete(&digest(1), &lease).is_err());
    assert!(store.release(&digest(1), &lease).is_err());
    drop(store);
    let reopened = new_store(root.path(), 8, 8);
    assert_eq!(
        reopened
            .completed_response(&digest(1), "origin", "tenant", "project", &digest(10))
            .expect("response replay fixture or operation must succeed")
            .as_deref(),
        Some(body)
    );
    for (id, origin, tenant, project, fingerprint) in [
        (2, "origin", "tenant", "project", 10),
        (1, "other", "tenant", "project", 10),
        (1, "origin", "other", "project", 10),
        (1, "origin", "tenant", "other", 10),
        (1, "origin", "tenant", "project", 11),
    ] {
        assert_eq!(
            reopened
                .completed_response(&digest(id), origin, tenant, project, &digest(fingerprint))
                .expect("response replay fixture or operation must succeed"),
            None
        );
    }
}

#[test]
fn response_failure_preserves_pending_and_exact_byte_bound() {
    let root = tempdir().expect("response replay fixture or operation must succeed");
    let store = new_store(root.path(), 8, 8);
    let lease = reserve(&store, 1, "origin", 10, NOW);
    assert!(
        store
            .complete_with_response(&digest(1), "wrong-lease", "body")
            .is_err()
    );
    for body in [String::new(), "x".repeat(65537), "é".repeat(32769)] {
        assert!(
            store
                .complete_with_response(&digest(1), &lease, &body)
                .is_err()
        );
        assert_eq!(
            store
                .reserve(
                    &digest(1),
                    "origin",
                    "tenant",
                    "project",
                    &digest(10),
                    expiry(NOW),
                    NOW
                )
                .expect("response replay fixture or operation must succeed"),
            Reservation::InFlight
        );
        assert_eq!(
            store
                .completed_response(&digest(1), "origin", "tenant", "project", &digest(10))
                .expect("response replay fixture or operation must succeed"),
            None
        );
    }
    let body = "é".repeat(32768);
    store
        .complete_with_response(&digest(1), &lease, &body)
        .expect("response replay fixture or operation must succeed");
    assert_eq!(
        store
            .completed_response(&digest(1), "origin", "tenant", "project", &digest(10))
            .expect("response replay fixture or operation must succeed"),
        Some(body)
    );
}

#[test]
fn completed_response_is_pruned_only_with_its_replay_proof() {
    let root = tempdir().expect("response replay fixture or operation must succeed");
    let store = new_store(root.path(), 8, 8);
    let lease = reserve(&store, 1, "origin", 10, NOW);
    store
        .complete_with_response(&digest(1), &lease, "response")
        .expect("response replay fixture or operation must succeed");
    // The storage layer does not turn a short response lifetime into permission
    // to forget a longer replay proof. Signature expiry is the origin's check.
    assert_eq!(
        store
            .reserve(
                &digest(1),
                "origin",
                "tenant",
                "project",
                &digest(10),
                expiry(NOW),
                NOW + 61
            )
            .expect("response replay fixture or operation must succeed"),
        Reservation::Completed
    );
    assert_eq!(
        store
            .completed_response(&digest(1), "origin", "tenant", "project", &digest(10))
            .expect("response replay fixture or operation must succeed")
            .as_deref(),
        Some("response")
    );
    let connection = rusqlite::Connection::open(store.database_path())
        .expect("response replay fixture or operation must succeed");
    let retain_until: i64 = connection
        .query_row(
            "SELECT retain_until FROM relay_replay WHERE storage_id = ?1",
            [digest(1)],
            |row| row.get(0),
        )
        .expect("response replay fixture or operation must succeed");
    let _next = reserve(&store, 2, "origin", 20, retain_until + 1);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM relay_task_responses", [], |row| row
                .get::<_, i64>(
                0
            ))
            .expect("response replay fixture or operation must succeed"),
        0
    );
    assert_eq!(
        store
            .completed_response(&digest(1), "origin", "tenant", "project", &digest(10))
            .expect("response replay fixture or operation must succeed"),
        None
    );
}

#[test]
fn response_schema_migration_preserves_existing_proofs_and_metadata() {
    let root = tempdir().expect("response replay fixture or operation must succeed");
    let store = new_store(root.path(), 8, 8);
    let completed = reserve(&store, 1, "origin", 10, NOW);
    store
        .complete(&digest(1), &completed)
        .expect("response replay fixture or operation must succeed");
    let pending = reserve(&store, 2, "origin", 20, NOW);
    let path = store.database_path().to_path_buf();
    drop(store);
    let connection = rusqlite::Connection::open(&path)
        .expect("response replay fixture or operation must succeed");
    // The pre-v2 schema is the exact unchanged four original objects.
    connection.execute_batch("DROP TABLE relay_task_responses; UPDATE relay_replay_meta SET schema_version=1; PRAGMA user_version=1;").expect("response replay fixture or operation must succeed");
    let snapshot = |connection: &rusqlite::Connection| {
        let mut query = connection
            .prepare("SELECT * FROM relay_replay ORDER BY storage_id")
            .expect("response replay fixture or operation must succeed");
        query
            .query_map([], |row| {
                (0..10)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<Result<Vec<_>, _>>()
            })
            .expect("response replay fixture or operation must succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("response replay fixture or operation must succeed")
    };
    let before = snapshot(&connection);
    let metadata: (String, i64, i64) = connection
        .query_row(
            "SELECT creation_marker, retention_seconds, last_observed_at FROM relay_replay_meta",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("response replay fixture or operation must succeed");
    drop(connection);
    let migrated = new_store(root.path(), 8, 8);
    let connection = rusqlite::Connection::open(&path)
        .expect("response replay fixture or operation must succeed");
    assert_eq!(snapshot(&connection), before);
    assert_eq!(connection.query_row(
        "SELECT creation_marker, retention_seconds, last_observed_at FROM relay_replay_meta", [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
    ).expect("response replay fixture or operation must succeed"), metadata);
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("response replay fixture or operation must succeed"),
        2
    );
    drop(connection);
    assert_eq!(
        migrated
            .reserve(
                &digest(1),
                "origin",
                "tenant",
                "project",
                &digest(10),
                expiry(NOW),
                NOW
            )
            .expect("response replay fixture or operation must succeed"),
        Reservation::Completed
    );
    assert_eq!(
        migrated
            .reserve(
                &digest(2),
                "origin",
                "tenant",
                "project",
                &digest(20),
                expiry(NOW),
                NOW
            )
            .expect("response replay fixture or operation must succeed"),
        Reservation::InFlight
    );
    migrated
        .release(&digest(2), &pending)
        .expect("response replay fixture or operation must succeed");
    drop(migrated);
    let _reopened = new_store(root.path(), 8, 8);
}

#[test]
fn response_migration_rejects_unexpected_v1_schema_without_advancing_version() {
    let root = tempdir().expect("response replay fixture or operation must succeed");
    let store = new_store(root.path(), 8, 8);
    let path = store.database_path().to_path_buf();
    drop(store);
    let connection = rusqlite::Connection::open(&path)
        .expect("response replay fixture or operation must succeed");
    connection.execute_batch("DROP TABLE relay_task_responses; UPDATE relay_replay_meta SET schema_version=1; PRAGMA user_version=1; CREATE TABLE unexpected (value TEXT);").expect("response replay fixture or operation must succeed");
    assert!(RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS).is_err());
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("response replay fixture or operation must succeed"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='relay_task_responses'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("response replay fixture or operation must succeed"),
        0
    );
}

#[test]
fn duplicate_conflict_complete_and_reopen_are_durable() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 8, 8);
    let lease = reserve(&store, 1, "origin-a", 10, NOW);

    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(10),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::InFlight)
    );
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(11),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::Conflict)
    );
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-b",
            "tenant",
            "project",
            &digest(10),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::Conflict)
    );
    store
        .complete(&digest(1), &lease)
        .expect("completion should succeed");
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(10),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::Completed)
    );
    drop(store);

    let reopened = new_store(root.path(), 8, 8);
    assert_eq!(
        reopened.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(10),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::Completed)
    );
}

#[test]
fn scope_values_are_compared_without_composite_key_collisions() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 8, 8);
    let lease = store
        .reserve(
            &digest(1),
            "origin:a",
            "tenant",
            "project:b",
            &digest(10),
            expiry(NOW),
            NOW,
        )
        .expect("reserve")
        .into_reserved();
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin",
            "a:tenant",
            "project:b",
            &digest(10),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::Conflict)
    );
    store
        .complete(&digest(1), &lease)
        .expect("completion should succeed");
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin:a",
            "tenant",
            "project:b",
            &digest(10),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::Completed)
    );
}

#[test]
fn per_origin_cap_is_fair_and_global_capacity_is_typed() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 3, 1);
    let _a = reserve(&store, 1, "origin-a", 1, NOW);
    assert!(matches!(
        store.reserve(
            &digest(2),
            "origin-a",
            "tenant",
            "project",
            &digest(2),
            expiry(NOW),
            NOW,
        ),
        Err(StoreError::CapacityExhausted {
            retry_after_seconds,
        }) if retry_after_seconds >= 1
    ));
    let _b = reserve(&store, 3, "origin-b", 3, NOW);
    let _c = reserve(&store, 4, "origin-c", 4, NOW);

    let root = tempdir().expect("second temporary root");
    let store = new_store(root.path(), 2, 2);
    let _a = reserve(&store, 1, "origin-a", 1, NOW);
    let _b = reserve(&store, 2, "origin-b", 2, NOW);
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::InFlight)
    );
    assert!(matches!(
        store.reserve(
            &digest(5),
            "origin-c",
            "tenant",
            "project",
            &digest(5),
            expiry(NOW),
            NOW,
        ),
        Err(StoreError::CapacityExhausted {
            retry_after_seconds,
        }) if retry_after_seconds >= 1
    ));
}

#[test]
fn expired_completed_proofs_are_pruned_but_pending_and_valid_proofs_survive() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 4, 3);
    let short_expiry = NOW + 1;
    let short_lease = store
        .reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            short_expiry,
            NOW,
        )
        .expect("short reservation")
        .into_reserved();
    store
        .complete(&digest(1), &short_lease)
        .expect("short completion");

    let valid_lease = reserve(&store, 2, "origin-a", 2, NOW);
    store
        .complete(&digest(2), &valid_lease)
        .expect("valid completion");
    let pending_lease = reserve(&store, 3, "origin-b", 3, NOW);

    let later = NOW + MIN_RETENTION_SECONDS + 2;
    let replacement = store
        .reserve(
            &digest(1),
            "origin-c",
            "tenant",
            "project",
            &digest(1),
            expiry(later),
            later,
        )
        .expect("expired completed proof should be replaceable")
        .into_reserved();
    assert_ne!(replacement, short_lease);
    assert_eq!(
        store.reserve(
            &digest(2),
            "origin-a",
            "tenant",
            "project",
            &digest(2),
            expiry(later),
            later,
        ),
        Ok(Reservation::Completed)
    );
    assert_eq!(
        store.reserve(
            &digest(3),
            "origin-b",
            "tenant",
            "project",
            &digest(3),
            expiry(later),
            later,
        ),
        Ok(Reservation::InFlight)
    );
    store
        .release(&digest(1), &replacement)
        .expect("replacement release");
    store
        .release(&digest(3), &pending_lease)
        .expect("pending release");
}

#[test]
fn persisted_monotonic_time_rejects_backward_clock_reacceptance() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 8, 8);
    let short_expiry = NOW + 1;
    let lease = store
        .reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            short_expiry,
            NOW,
        )
        .expect("short reservation")
        .into_reserved();
    store.complete(&digest(1), &lease).expect("completion");
    let later = NOW + MIN_RETENTION_SECONDS + 2;
    let other = reserve(&store, 2, "origin-b", 2, later);
    assert!(matches!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            short_expiry,
            NOW,
        ),
        Err(StoreError::Expired)
    ));
    assert_eq!(
        store.reserve(
            &digest(2),
            "origin-b",
            "tenant",
            "project",
            &digest(2),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::InFlight)
    );
    store.release(&digest(2), &other).expect("release");
}

#[test]
fn failed_transactions_keep_previous_proofs_and_leases_are_one_time() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 1, 1);
    let lease = reserve(&store, 1, "origin-a", 1, NOW);
    assert!(matches!(
        store.reserve(
            &digest(2),
            "origin-b",
            "tenant",
            "project",
            &digest(2),
            expiry(NOW),
            NOW,
        ),
        Err(StoreError::CapacityExhausted { .. })
    ));
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::InFlight)
    );
    assert_eq!(
        store.complete(&digest(1), &"0".repeat(32)),
        Err(StoreError::LeaseMismatch)
    );
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::InFlight)
    );
    store.complete(&digest(1), &lease).expect("completion");
    store
        .complete(&digest(1), &lease)
        .expect("idempotent completion");
    assert_eq!(
        store.release(&digest(1), &lease),
        Err(StoreError::LeaseMismatch)
    );
}

#[test]
fn recreation_rejects_stale_completion() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 8, 8);
    let storage_id = digest(1);
    let old_lease = reserve(&store, 1, "origin-a", 1, NOW);
    let database_path = store.database_path().to_path_buf();
    drop(store);
    fs::remove_file(&database_path).expect("delete test database");

    assert!(matches!(
        RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS),
        Err(StoreError::Unavailable(_))
    ));
    assert!(
        !database_path.exists(),
        "missing established database must not be recreated"
    );
    // Explicit test-only administrative reset of BOTH files, not automatic recovery.
    fs::remove_file(database_path.with_extension("established")).expect("reset test marker");

    let recreated = new_store(root.path(), 8, 8);
    let new_lease = reserve(&recreated, 1, "origin-a", 1, NOW);
    assert_ne!(old_lease, new_lease);
    assert_eq!(
        recreated.complete(&storage_id, &old_lease),
        Err(StoreError::LeaseMismatch)
    );
    assert_eq!(
        recreated.reserve(
            &storage_id,
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::InFlight)
    );
    recreated
        .complete(&storage_id, &new_lease)
        .expect("new lease completion");
}

#[test]
fn missing_or_changed_marker_preserves_existing_proofs() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 8, 8);
    let lease = reserve(&store, 1, "origin-a", 1, NOW);
    store.complete(&digest(1), &lease).expect("complete proof");
    let database = store.database_path();
    let marker = database.with_extension("established");
    let original_marker = fs::read(&marker).expect("marker bytes");
    let original_database = fs::read(database).expect("database bytes");
    fs::remove_file(&marker).expect("delete owned test marker");
    assert!(RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS).is_err());
    assert_eq!(
        fs::read(database).expect("existing database"),
        original_database
    );

    let mut changed = original_marker.clone();
    changed[0] = if changed[0] == b'0' { b'1' } else { b'0' };
    fs::write(&marker, changed).expect("changed test marker");
    assert!(RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS).is_err());
    assert!(matches!(
        store.complete(&digest(1), &lease),
        Err(StoreError::Unavailable(_))
    ));
    assert_eq!(
        fs::read(database).expect("existing database"),
        original_database
    );

    fs::write(&marker, &original_marker).expect("restore test marker");
    let reopened = new_store(root.path(), 8, 8);
    assert_eq!(
        reopened.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            expiry(NOW),
            NOW
        ),
        Ok(Reservation::Completed)
    );
}

#[test]
fn interrupted_initialization_and_oversized_markers_fail_closed() {
    let root = root_with_a2a();
    let database = root.path().join(".lean-ctx/a2a/relay-replay-v1.sqlite");
    let marker = database.with_extension("established");
    for bytes in [String::new(), "a".repeat(32), "a".repeat(1024)] {
        fs::write(&marker, bytes).expect("interrupted initialization fixture");
        assert!(RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS).is_err());
        assert!(
            !database.exists(),
            "uncertain initialization cannot create a fresh database"
        );
    }
}

#[cfg(unix)]
#[test]
fn established_marker_symlinks_are_rejected() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 8, 8);
    let marker = store.database_path().with_extension("established");
    let bytes = fs::read(&marker).expect("marker bytes");
    let outside = tempdir().expect("outside fixture");
    let target = outside.path().join("marker");
    fs::write(&target, &bytes).expect("outside marker");
    fs::remove_file(&marker).expect("remove owned test marker");
    std::os::unix::fs::symlink(&target, &marker).expect("marker symlink fixture");
    assert!(RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS).is_err());
    assert_eq!(fs::read(&target).expect("unchanged outside marker"), bytes);
}

#[test]
fn malformed_existing_state_and_legacy_json_fail_closed() {
    let root = root_with_a2a();
    let legacy_path = root
        .path()
        .join(".lean-ctx")
        .join("a2a")
        .join("relay-replay-v1.json");
    fs::write(&legacy_path, b"{\"schema_version\": 1}").expect("legacy fixture");
    assert!(matches!(
        RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS),
        Err(StoreError::MigrationRequired(_))
    ));

    let root = root_with_a2a();
    let database_path = root
        .path()
        .join(".lean-ctx")
        .join("a2a")
        .join("relay-replay-v1.sqlite");
    fs::write(&database_path, b"not a sqlite database").expect("malformed fixture");
    assert!(matches!(
        RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS),
        Err(StoreError::Unavailable(_))
    ));

    let root = root_with_a2a();
    let database_path = root
        .path()
        .join(".lean-ctx")
        .join("a2a")
        .join("relay-replay-v1.sqlite");
    fs::File::create(&database_path).expect("empty fixture");
    assert!(matches!(
        RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS),
        Err(StoreError::Unavailable(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlink_database_path_is_rejected() {
    use std::os::unix::fs::symlink;

    let root = root_with_a2a();
    let a2a = root.path().join(".lean-ctx").join("a2a");
    let target = root.path().join("outside.sqlite");
    fs::write(&target, b"not a sqlite database").expect("target fixture");
    symlink(&target, a2a.join("relay-replay-v1.sqlite")).expect("symlink fixture");
    assert!(matches!(
        RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS),
        Err(StoreError::Unavailable(_))
    ));
}

#[test]
fn database_page_bound_and_input_bounds_are_enforced() {
    let root = tempdir().expect("temporary root");
    let store = RelayReplayStore::new(
        root.path(),
        RelayReplayLimits::new(64, 64, 64 * 1024),
        MIN_RETENTION_SECONDS,
    )
    .expect("bounded store");
    let _lease = reserve(&store, 1, "origin-a", 1, NOW);
    let file_size = fs::metadata(store.database_path())
        .expect("database metadata")
        .len();
    assert!(file_size <= 64 * 1024);

    assert!(matches!(
        RelayReplayStore::new(
            root.path().join("other"),
            limits(8, 8),
            MIN_RETENTION_SECONDS - 1
        ),
        Err(StoreError::InvalidInput(_))
    ));
    assert!(matches!(
        store.reserve(
            &"A".repeat(64),
            "origin-a",
            "tenant",
            "project",
            &digest(2),
            expiry(NOW),
            NOW,
        ),
        Err(StoreError::InvalidInput(_))
    ));
}

const PAGE_SIZE: u64 = 4 * 1024;

fn byte_capped_store(root: &Path, max_database_bytes: u64) -> RelayReplayStore {
    RelayReplayStore::new(
        root,
        RelayReplayLimits::new(1_000_000, 1_000_000, max_database_bytes),
        MIN_RETENTION_SECONDS,
    )
    .expect("byte-bounded store should initialize")
}

fn database_bytes(root: &Path) -> u64 {
    fs::metadata(root.join(".lean-ctx/a2a/relay-replay-v1.sqlite"))
        .expect("database metadata")
        .len()
}

/// Fill until the byte cap - not the row cap - rejects, completing every proof.
/// Returns the accepted ids and the first id that was refused.
fn fill_to_byte_cap(
    store: &RelayReplayStore,
    first: u64,
    origin: &str,
    now: i64,
) -> (Vec<u64>, u64) {
    let mut accepted = Vec::new();
    let mut next = first;
    loop {
        assert!(
            next - first < 5_000,
            "the byte cap, not the row cap, must be the limiting bound"
        );
        match store.reserve(
            &digest(next),
            origin,
            "tenant",
            "project",
            &digest(next),
            expiry(now),
            now,
        ) {
            Ok(Reservation::Reserved(lease)) => {
                store
                    .complete(&digest(next), &lease)
                    .expect("fill completion should succeed");
                accepted.push(next);
                next += 1;
            }
            Err(StoreError::Unavailable(message))
                if message.contains("SQLite or filesystem full") =>
            {
                return (accepted, next);
            }
            other => panic!("unexpected fill result at {next}: {other:?}"),
        }
    }
}

#[test]
fn byte_cap_is_reclaimed_by_retention_and_survives_reopen() {
    let root = tempdir().expect("temporary root");
    let store = byte_capped_store(root.path(), 128 * 1024);

    let (filled, blocked) = fill_to_byte_cap(&store, 1, "origin-a", NOW);
    assert!(
        filled.len() >= 4,
        "byte cap must admit a useful number of proofs, got {}",
        filled.len()
    );
    drop(store);

    // Reopen with the cap set to exactly the page count the fill committed. The
    // store is now at its bound with no unused page left, which is precisely the
    // state a page_count-only capacity test turns into a one-way fuse: SQLite
    // keeps the file at its high-water mark after DELETE, so retention could
    // never restore byte capacity again.
    let at_bound = database_bytes(root.path());
    assert_eq!(at_bound % PAGE_SIZE, 0, "page-aligned database expected");
    let store = byte_capped_store(root.path(), at_bound);

    // Every completed proof retains until expiry(NOW); at that instant the next
    // reservation prunes them and must reuse the freed pages in place.
    let later = NOW + MIN_RETENTION_SECONDS + 100;
    let survivor = store
        .reserve(
            &digest(blocked),
            "origin-b",
            "tenant",
            "project",
            &digest(blocked),
            expiry(later),
            later,
        )
        .expect("retention must restore byte capacity at the page bound")
        .into_reserved();
    store
        .complete(&digest(blocked), &survivor)
        .expect("survivor completion");

    // Reuse must be sustained, not a single-row window.
    let (refilled, _) = fill_to_byte_cap(&store, blocked + 1, "origin-b", later);
    assert!(
        refilled.len() * 2 >= filled.len(),
        "reclaimed capacity {} must be comparable to the original fill {}",
        refilled.len(),
        filled.len()
    );
    // The bound itself stays strict: reuse never grows the file past the cap.
    let final_size = database_bytes(root.path());
    assert!(
        final_size <= at_bound,
        "page bound must stay strict after reuse: {final_size} > {at_bound}"
    );
    drop(store);

    let reopened = byte_capped_store(root.path(), at_bound);
    // A duplicate lookup answers before any capacity check, so this proves the
    // refilled proof survived the reopen even with the store at its bound.
    assert_eq!(
        reopened.reserve(
            &digest(blocked),
            "origin-b",
            "tenant",
            "project",
            &digest(blocked),
            expiry(later),
            later,
        ),
        Ok(Reservation::Completed)
    );
    // Once the refilled generation also expires, capacity is reusable again.
    let even_later = later + MIN_RETENTION_SECONDS + 200;
    assert!(matches!(
        reopened.reserve(
            &digest(filled[0]),
            "origin-c",
            "tenant",
            "project",
            &digest(filled[0]),
            expiry(even_later),
            even_later,
        ),
        Ok(Reservation::Reserved(_))
    ));
}

#[test]
fn byte_cap_reuses_leaf_space_without_free_pages() {
    let root = tempdir().expect("temporary root");
    let store = byte_capped_store(root.path(), 128 * 1024);
    let first = reserve(&store, 1, "origin-a", 1, NOW);
    store
        .complete(&digest(1), &first)
        .expect("first completion");
    let at_bound = database_bytes(root.path());
    let connection = rusqlite::Connection::open(store.database_path())
        .expect("response replay fixture or operation must succeed");
    let free_pages: i64 = connection
        .pragma_query_value(None, "freelist_count", |row| row.get(0))
        .expect("response replay fixture or operation must succeed");
    assert_eq!(free_pages, 0, "fixture must exercise retained leaf pages");
    drop(connection);
    drop(store);

    let store = byte_capped_store(root.path(), at_bound);
    let second = reserve(&store, 2, "origin-a", 2, NOW);
    store
        .complete(&digest(2), &second)
        .expect("second completion");
    assert_eq!(database_bytes(root.path()), at_bound);
    drop(store);
    let reopened = byte_capped_store(root.path(), at_bound);
    assert_eq!(
        reopened.reserve(
            &digest(2),
            "origin-a",
            "tenant",
            "project",
            &digest(2),
            expiry(NOW),
            NOW
        ),
        Ok(Reservation::Completed)
    );
}

#[test]
fn retention_and_signed_expiry_reject_unbounded_horizons() {
    let root = tempdir().expect("temporary root");
    for retention in [i64::MAX, MAX_RETENTION_SECONDS + 1] {
        assert!(matches!(
            RelayReplayStore::new(root.path(), limits(8, 8), retention),
            Err(StoreError::InvalidInput(_))
        ));
    }
    assert!(
        !root.path().join(".lean-ctx").exists(),
        "a rejected constructor must not leave any on-disk state"
    );

    let store = new_store(root.path(), 8, 8);
    for signed_expires_at in [i64::MAX, NOW + MAX_RETENTION_SECONDS + 1] {
        assert!(matches!(
            store.reserve(
                &digest(1),
                "origin-a",
                "tenant",
                "project",
                &digest(1),
                signed_expires_at,
                NOW,
            ),
            Err(StoreError::InvalidInput(_))
        ));
    }

    // The horizon itself stays usable, and a signed expiry longer than the
    // retention floor still pins the completed proof past that floor.
    let lease = store
        .reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            NOW + MAX_RETENTION_SECONDS,
            NOW,
        )
        .expect("bounded horizon reservation")
        .into_reserved();
    store.complete(&digest(1), &lease).expect("completion");
    let after_retention = NOW + MIN_RETENTION_SECONDS + 1;
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            expiry(after_retention),
            after_retention,
        ),
        Ok(Reservation::Completed)
    );
}

#[test]
fn pending_reservations_never_auto_expire_and_retry_is_not_a_promise() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 1, 1);
    let lease = reserve(&store, 1, "origin-a", 1, NOW);

    // Simulated crash: the effect is unknown, so the row is never auto-expired.
    let far_future = NOW + MAX_RETENTION_SECONDS;
    let retry = match store.reserve(
        &digest(2),
        "origin-b",
        "tenant",
        "project",
        &digest(2),
        expiry(far_future),
        far_future,
    ) {
        Err(StoreError::CapacityExhausted {
            retry_after_seconds,
        }) => retry_after_seconds,
        other => panic!("a pending row must keep holding capacity: {other:?}"),
    };
    assert!(
        retry >= 60,
        "capacity that only reconciliation can free must not advertise a 1s recovery, got {retry}"
    );
    assert_eq!(
        store.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            expiry(NOW),
            NOW,
        ),
        Ok(Reservation::InFlight)
    );

    // Manual reconciliation - not the clock - releases the capacity.
    store
        .release(&digest(1), &lease)
        .expect("operator reconciliation");
    let _recovered = reserve(&store, 2, "origin-b", 2, far_future);
}

#[test]
fn same_name_weakened_schema_is_rejected() {
    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 8, 8);
    let database_path = store.database_path().to_path_buf();
    let marker = fs::read_to_string(database_path.with_extension("established"))
        .expect("established marker");
    drop(store);

    // Same object names, same column names and order, but no PRIMARY KEY, no
    // UNIQUE(lease), no CHECK(state) and untyped columns: enough to permit
    // duplicate storage_id rows that find_entry would answer from arbitrarily.
    let connection = rusqlite::Connection::open(&database_path).expect("reconstruction fixture");
    connection
        .execute_batch(
            "DROP TABLE relay_replay;
             DROP TABLE relay_replay_meta;
             CREATE TABLE relay_replay_meta (
                 id, schema_version, creation_marker, retention_seconds, last_observed_at);
             CREATE TABLE relay_replay (
                 storage_id, origin, tenant, project, fingerprint,
                 signed_expires_at, retain_until, lease, state, accepted_at);
             CREATE INDEX relay_replay_retain_idx ON relay_replay (state, retain_until);
             CREATE INDEX relay_replay_origin_state_idx ON relay_replay (origin, state);",
        )
        .expect("weakened schema fixture");
    connection
        .execute(
            "INSERT INTO relay_replay_meta VALUES (1, 1, ?1, ?2, 0)",
            rusqlite::params![marker, MIN_RETENTION_SECONDS],
        )
        .expect("weakened metadata fixture");
    drop(connection);

    assert!(matches!(
        RelayReplayStore::new(root.path(), limits(8, 8), MIN_RETENTION_SECONDS),
        Err(StoreError::Unavailable(_))
    ));
}

#[cfg(unix)]
#[test]
fn created_database_marker_and_directory_are_owner_private() {
    use std::os::unix::fs::PermissionsExt;

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("permission metadata")
            .permissions()
            .mode()
            & 0o777
    }

    let root = tempdir().expect("temporary root");
    let store = new_store(root.path(), 8, 8);
    let database = store.database_path();
    assert_eq!(mode_of(database), 0o600, "database must be owner-only");
    assert_eq!(
        mode_of(&database.with_extension("established")),
        0o600,
        "established marker must be owner-only"
    );
    assert_eq!(
        mode_of(database.parent().expect("a2a directory")),
        0o700,
        "the a2a directory this store creates must be owner-only"
    );
    // Shared parents are left exactly as they were found.
    assert_ne!(
        mode_of(&root.path().join(".lean-ctx")),
        0o000,
        "the shared .lean-ctx directory must remain usable"
    );
}

#[cfg(unix)]
#[test]
fn existing_shared_directory_keeps_its_mode_while_database_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempdir().expect("temporary root");
    let directory = root.path().join(".lean-ctx/a2a");
    fs::create_dir_all(&directory).expect("existing shared directory");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
        .expect("shared directory mode");
    let store = new_store(root.path(), 8, 8);
    assert_eq!(
        fs::metadata(&directory)
            .expect("response replay fixture or operation must succeed")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(store.database_path())
            .expect("response replay fixture or operation must succeed")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let lease = reserve(&store, 1, "origin-a", 1, NOW);
    store.complete(&digest(1), &lease).expect("complete proof");
    drop(store);
    let reopened = new_store(root.path(), 8, 8);
    assert_eq!(
        reopened.reserve(
            &digest(1),
            "origin-a",
            "tenant",
            "project",
            &digest(1),
            expiry(NOW),
            NOW
        ),
        Ok(Reservation::Completed)
    );
}

trait ReservedLease {
    fn into_reserved(self) -> String;
}

impl ReservedLease for Reservation {
    fn into_reserved(self) -> String {
        match self {
            Reservation::Reserved(lease) => lease,
            result => panic!("expected reserved lease, got {result:?}"),
        }
    }
}
