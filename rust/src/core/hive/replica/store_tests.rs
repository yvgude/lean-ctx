// SPDX-License-Identifier: Apache-2.0

use super::*;

const CANARY: &str = "canary-statement-7f3a9c";

fn files_contain(dir: &Path, needle: &[u8]) -> bool {
    std::fs::read_dir(dir).unwrap().any(|e| {
        let bytes = std::fs::read(e.unwrap().path()).unwrap_or_default();
        bytes.windows(needle.len()).any(|w| w == needle)
    })
}

#[test]
fn no_plaintext_reaches_the_database_or_its_wal() {
    let data = tempfile::tempdir().unwrap();
    let mk = [5u8; 32];
    let (replica, opened) = Replica::open(data.path(), "pl_one", &mk).unwrap();
    assert_eq!(opened, Opened::Created);
    replica
        .set_meta(META_PLACE_NAME, CANARY.as_bytes())
        .unwrap();
    replica
        .add_pending("ev_1", 10, 0, Some("ob_1"), CANARY.as_bytes())
        .unwrap();
    replica
        .put_own_source(
            "n_one",
            "fact",
            &serde_json::json!({"category": "arch", "key": CANARY}),
        )
        .unwrap();
    let dk = *blake3::hash(CANARY.as_bytes()).as_bytes();
    replica.add_dedupe_key(&dk, "n_one").unwrap();
    replica.deny_purged(&dk, "leaked_secret").unwrap();

    let dir = replica.dir().to_path_buf();
    assert!(dir.join("replica.db-wal").exists(), "WAL mode");
    assert!(
        !files_contain(&dir, CANARY.as_bytes()),
        "plaintext in db or wal"
    );
    assert!(!files_contain(&dir, &dk), "an unkeyed dedupe key is stored");
    drop(replica);
    assert!(
        !files_contain(&dir, CANARY.as_bytes()),
        "plaintext after checkpoint"
    );

    let (replica, opened) = Replica::open(data.path(), "pl_one", &mk).unwrap();
    assert_eq!(opened, Opened::Existing);
    assert_eq!(
        replica.meta(META_PLACE_NAME).unwrap().unwrap().as_slice(),
        CANARY.as_bytes()
    );
    assert_eq!(replica.dedupe_node(&dk).unwrap().as_deref(), Some("n_one"));
    assert!(replica.is_purged(&dk).unwrap());
    assert_eq!(
        replica.own_source("n_one").unwrap().unwrap().1["key"],
        CANARY
    );
}

#[test]
fn a_missing_master_key_rebuilds_the_place() {
    let data = tempfile::tempdir().unwrap();
    let (replica, _) = Replica::open(data.path(), "pl_two", &[1u8; 32]).unwrap();
    replica.set_cursor(42).unwrap();
    drop(replica);
    let (replica, opened) = Replica::open(data.path(), "pl_two", &[2u8; 32]).unwrap();
    assert_eq!(opened, Opened::RebuiltKeyMissing);
    assert_eq!(replica.cursor().unwrap(), None, "cloned again from scratch");
}

#[test]
fn the_cursor_only_moves_forward_and_survives_reopen() {
    let data = tempfile::tempdir().unwrap();
    let mk = [7u8; 32];
    let (replica, _) = Replica::open(data.path(), "pl_three", &mk).unwrap();
    assert_eq!(replica.cursor().unwrap(), None);
    replica.set_cursor(10).unwrap();
    replica.set_cursor(10).unwrap();
    assert!(replica.set_cursor(9).is_err());
    replica.set_epoch(3).unwrap();
    replica.set_id_salt(&[8u8; 32]).unwrap();
    drop(replica);
    let (replica, _) = Replica::open(data.path(), "pl_three", &mk).unwrap();
    assert_eq!(replica.cursor().unwrap(), Some(10));
    assert_eq!(replica.epoch().unwrap(), Some(3));
    assert_eq!(replica.id_salt().unwrap(), Some([8u8; 32]));
}

#[test]
fn pending_events_queue_offline_in_hlc_order() {
    let data = tempfile::tempdir().unwrap();
    let (replica, _) = Replica::open(data.path(), "pl_four", &[3u8; 32]).unwrap();
    replica.add_pending("ev_b", 20, 0, None, b"second").unwrap();
    replica.add_pending("ev_a", 10, 5, None, b"first").unwrap();
    replica.add_pending("ev_c", 20, 1, None, b"third").unwrap();
    let order: Vec<String> = replica
        .pending()
        .unwrap()
        .into_iter()
        .map(|p| p.0)
        .collect();
    assert_eq!(order, ["ev_a", "ev_b", "ev_c"]);
    assert!(replica.settle_pending("ev_a").unwrap());
    assert!(!replica.settle_pending("ev_a").unwrap());
    assert_eq!(replica.pending().unwrap()[0].1.as_slice(), b"second");
}

#[test]
fn signatures_round_trip_encrypted() {
    let data = tempfile::tempdir().unwrap();
    let (replica, _) = Replica::open(data.path(), "pl_five", &[3u8; 32]).unwrap();
    let sig = crate::core::hive::capture::dedupe::signature(&[1u8; 32], "The relay drops frames.");
    replica.put_signature("n_b", &sig).unwrap();
    replica.put_signature("n_a", &sig).unwrap();
    let all = replica.signatures().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].0, "n_a");
    assert_eq!(all[0].1, sig);
}

#[test]
fn bad_inputs_are_refused() {
    let data = tempfile::tempdir().unwrap();
    assert!(Replica::open(data.path(), "../escape", &[3u8; 32]).is_err());
    let (replica, _) = Replica::open(data.path(), "pl_six", &[3u8; 32]).unwrap();
    assert!(replica.deny_purged(&[0; 32], "because").is_err());
    assert!(
        replica
            .put_own_source("n", "note", &serde_json::json!({}))
            .is_err()
    );
    assert!(replica.set_meta("rk_wrapped", b"x").is_err());
    assert!(replica.meta("rk_wrapped").unwrap().is_none());
}

#[test]
fn a_newer_schema_is_not_opened() {
    let data = tempfile::tempdir().unwrap();
    let (replica, _) = Replica::open(data.path(), "pl_seven", &[3u8; 32]).unwrap();
    let path = replica.dir().join("replica.db");
    drop(replica);
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(conn);
    let err = Replica::open(data.path(), "pl_seven", &[3u8; 32]).unwrap_err();
    assert!(err.to_string().contains("engine_too_old"), "{err}");
}
