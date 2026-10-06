// SPDX-License-Identifier: Apache-2.0

#[path = "../src/http_server/relay_replay.rs"]
mod relay_replay;

use relay_replay::{
    MAX_RETENTION_SECONDS, MIN_RETENTION_SECONDS, RelayReplayStore, Reservation, StoreError,
    StoreLimits,
};

const NOW: i64 = 1_700_000_000;

fn reserve(store: &RelayReplayStore, id: u64, now: i64) -> Result<Reservation, StoreError> {
    let digest = format!("{id:064x}");
    store.reserve(
        &digest,
        "origin",
        "tenant",
        "project",
        &digest,
        now + MIN_RETENTION_SECONDS + 100,
        now,
    )
}

#[test]
fn clock_rollback_fails_closed_but_catchup_restores_admission() {
    let root = tempfile::tempdir().expect("replay regression fixture or operation must succeed");
    let limits = StoreLimits::new(8, 8, 128 * 1024);
    let store = RelayReplayStore::new(root.path(), limits, MIN_RETENTION_SECONDS)
        .expect("replay regression fixture or operation must succeed");
    assert!(matches!(
        reserve(&store, 1, NOW),
        Ok(Reservation::Reserved(_))
    ));
    let future = NOW + MAX_RETENTION_SECONDS;
    assert!(matches!(
        reserve(&store, 2, future),
        Ok(Reservation::Reserved(_))
    ));
    assert_eq!(reserve(&store, 3, NOW), Err(StoreError::Expired));
    drop(store);
    let reopened = RelayReplayStore::new(root.path(), limits, MIN_RETENTION_SECONDS)
        .expect("replay regression fixture or operation must succeed");
    assert_eq!(reserve(&reopened, 3, NOW), Err(StoreError::Expired));
    assert!(matches!(
        reserve(&reopened, 3, future),
        Ok(Reservation::Reserved(_))
    ));
    let Reservation::Reserved(lease) =
        reserve(&reopened, 4, future).expect("replay regression fixture or operation must succeed")
    else {
        panic!("expected fresh reservation");
    };
    reopened
        .release(&format!("{:064x}", 4), &lease)
        .expect("replay regression fixture or operation must succeed");
}

#[test]
fn gross_clock_rollback_is_retryable_without_lowering_the_floor() {
    let root = tempfile::tempdir().expect("replay regression fixture or operation must succeed");
    let limits = StoreLimits::new(8, 8, 128 * 1024);
    let store = RelayReplayStore::new(root.path(), limits, MIN_RETENTION_SECONDS)
        .expect("replay regression fixture or operation must succeed");
    let future = NOW + MAX_RETENTION_SECONDS + 1;
    let Reservation::Reserved(lease) =
        reserve(&store, 1, future).expect("replay regression fixture or operation must succeed")
    else {
        panic!("expected fresh reservation");
    };
    store
        .complete(&format!("{:064x}", 1), &lease)
        .expect("replay regression fixture or operation must succeed");
    let path = store.database_path().to_path_buf();
    let before = std::fs::read(&path).expect("replay regression fixture or operation must succeed");
    assert_eq!(
        reserve(&store, 2, NOW),
        Err(StoreError::Unavailable(
            "persisted relay replay clock is ahead of the system clock".to_string()
        ))
    );
    assert_eq!(
        std::fs::read(&path).expect("replay regression fixture or operation must succeed"),
        before
    );
    // Existing proofs still answer; no retry can create a second delivery.
    assert_eq!(reserve(&store, 1, NOW), Ok(Reservation::Completed));
    drop(store);
    let reopened = RelayReplayStore::new(root.path(), limits, MIN_RETENTION_SECONDS)
        .expect("replay regression fixture or operation must succeed");
    assert!(matches!(
        reserve(&reopened, 2, NOW),
        Err(StoreError::Unavailable(_))
    ));
    assert!(matches!(
        reserve(&reopened, 2, future),
        Ok(Reservation::Reserved(_))
    ));
}

#[test]
fn corrupt_persisted_clock_is_storage_failure_without_mutation() {
    let root = tempfile::tempdir().expect("replay regression fixture or operation must succeed");
    let store = RelayReplayStore::new(
        root.path(),
        StoreLimits::new(8, 8, 128 * 1024),
        MIN_RETENTION_SECONDS,
    )
    .expect("replay regression fixture or operation must succeed");
    assert!(matches!(
        reserve(&store, 1, NOW),
        Ok(Reservation::Reserved(_))
    ));
    let path = store.database_path().to_path_buf();
    let connection = rusqlite::Connection::open(&path)
        .expect("replay regression fixture or operation must succeed");
    connection
        .execute(
            "UPDATE relay_replay_meta SET last_observed_at = ?1 WHERE id = 1",
            [i64::MAX],
        )
        .expect("replay regression fixture or operation must succeed");
    drop(connection);
    let before = std::fs::read(&path).expect("replay regression fixture or operation must succeed");
    assert!(matches!(
        reserve(&store, 2, NOW),
        Err(StoreError::Unavailable(_))
    ));
    assert_eq!(
        std::fs::read(&path).expect("replay regression fixture or operation must succeed"),
        before
    );
}

#[test]
fn restoring_byte_limit_recovers_without_losing_existing_proofs() {
    let root = tempfile::tempdir().expect("replay regression fixture or operation must succeed");
    let limits = StoreLimits::new(8, 8, 128 * 1024);
    let store = RelayReplayStore::new(root.path(), limits, MIN_RETENTION_SECONDS)
        .expect("replay regression fixture or operation must succeed");
    let Reservation::Reserved(lease) =
        reserve(&store, 1, NOW).expect("replay regression fixture or operation must succeed")
    else {
        panic!("expected fresh reservation");
    };
    store
        .complete_with_response(&format!("{:064x}", 1), &lease, "original response")
        .expect("replay regression fixture or operation must succeed");
    let path = store.database_path().to_path_buf();
    let before = std::fs::read(&path).expect("replay regression fixture or operation must succeed");
    drop(store);
    let smaller = StoreLimits::new(8, 8, 4096);
    assert!(matches!(
        RelayReplayStore::new(root.path(), smaller, MIN_RETENTION_SECONDS),
        Err(StoreError::Unavailable(_))
    ));
    assert_eq!(
        std::fs::read(&path).expect("replay regression fixture or operation must succeed"),
        before
    );
    let reopened = RelayReplayStore::new(root.path(), limits, MIN_RETENTION_SECONDS)
        .expect("replay regression fixture or operation must succeed");
    assert_eq!(reserve(&reopened, 1, NOW), Ok(Reservation::Completed));
    assert_eq!(
        reopened
            .completed_response(
                &format!("{:064x}", 1),
                "origin",
                "tenant",
                "project",
                &format!("{:064x}", 1),
            )
            .expect("replay regression fixture or operation must succeed")
            .as_deref(),
        Some("original response")
    );
}
