// SPDX-License-Identifier: Apache-2.0
//! Check durable private writes through an independent SQLite connection.

use lean_ctx::core::context_os::{ContextBus, ObservationPersistenceError};
use lean_ctx::core::events::{EventKind, LeanCtxEvent};
use lean_ctx::core::ocla_bus::{OclaBusRecord, OclaEvent};

#[test]
fn private_observation_json_preserves_finite_float_bits() {
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("context-os.db");
    let bus = ContextBus::try_open_at(path.clone()).unwrap();
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let producer = uuid::Uuid::new_v4().to_string();
    for (index, confidence) in [
        f64::from_bits(0x3f28_00d6_569e_01b4),
        0.0,
        -0.0,
        f64::from_bits(1),
        f64::MIN_POSITIVE,
        f64::MAX,
        f64::MIN,
    ]
    .into_iter()
    .enumerate()
    {
        let record = OclaBusRecord {
            id: index as u64 + 1,
            timestamp_ms: 1,
            event: OclaEvent::IntentClassified {
                tier: "fixture".into(),
                confidence,
                reasoning: "roundtrip".into(),
            },
        };
        let cursor = bus.append_ocla_observation(&producer, &record).unwrap();
        let encoded: String = conn
            .query_row(
                "SELECT payload_json FROM context_local_observations WHERE id = ?1",
                [cursor],
                |row| row.get(0),
            )
            .unwrap();
        let decoded: OclaEvent = serde_json::from_str(&encoded).unwrap();
        let OclaEvent::IntentClassified {
            confidence: actual, ..
        } = decoded
        else {
            panic!("persisted event changed variant");
        };
        assert_eq!(
            actual.to_bits(),
            confidence.to_bits(),
            "finite value changed in storage"
        );
    }
}

#[test]
fn private_observation_writes_preserve_metadata_and_reject_invalid_input() {
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("context-os.db");
    let bus = ContextBus::try_open_at(path.clone()).unwrap();
    let producer = uuid::Uuid::new_v4().to_string();
    let mut ocla = OclaBusRecord {
        id: u64::MAX,
        timestamp_ms: u64::MAX,
        event: OclaEvent::OutcomeRecorded {
            session_id: "local-session".into(),
            accepted: true,
            implicit: false,
        },
    };
    let legacy = LeanCtxEvent {
        id: u64::MAX,
        timestamp: "2026-09-15T10:11:12.345".into(),
        kind: EventKind::CacheHit {
            path: "fixture.rs".into(),
            saved_tokens: 7,
        },
    };
    let cursor = bus.append_ocla_observation(&producer, &ocla).unwrap();
    assert_eq!(
        bus.append_ocla_observation(&producer, &ocla).unwrap(),
        cursor
    );
    let mut conflicting = ocla.clone();
    conflicting.timestamp_ms = 0;
    assert!(matches!(
        bus.append_ocla_observation(&producer, &conflicting),
        Err(ObservationPersistenceError::IdentityConflict)
    ));
    conflicting.timestamp_ms = ocla.timestamp_ms;
    conflicting.event = OclaEvent::OutcomeRecorded {
        session_id: "local-session".into(),
        accepted: false,
        implicit: false,
    };
    assert!(matches!(
        bus.append_ocla_observation(&producer, &conflicting),
        Err(ObservationPersistenceError::IdentityConflict)
    ));
    // A new write must still succeed after both rejected transactions.
    let legacy_cursor = bus.append_legacy_observation(&producer, &legacy).unwrap();
    assert!(legacy_cursor > cursor);
    assert_eq!(
        bus.append_legacy_observation(&producer, &legacy).unwrap(),
        legacy_cursor
    );
    assert!(
        bus.append_ocla_observation("not-a-producer", &ocla)
            .is_err()
    );
    ocla.id = 0;
    assert!(bus.append_ocla_observation(&producer, &ocla).is_err());
    ocla.id = 1;
    ocla.event = OclaEvent::IntentClassified {
        tier: "invalid".into(),
        confidence: f64::NAN,
        reasoning: "fixture".into(),
    };
    assert!(bus.append_ocla_observation(&producer, &ocla).is_err());
    drop(bus);

    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = conn.prepare(
        "SELECT schema_version, source_kind, producer_instance, source_id, source_timestamp, payload_json
         FROM context_local_observations ORDER BY id",
    ).unwrap();
    let rows: Vec<(i64, String, String, String, String, String)> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.0, 1);
        assert_eq!(row.2, producer);
        assert_eq!(row.3, u64::MAX.to_string());
    }
    assert_eq!(rows[0].1, "ocla");
    assert_eq!(rows[0].4, u64::MAX.to_string());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[0].5).unwrap(),
        serde_json::json!({
            "type": "OutcomeRecorded", "session_id": "local-session", "accepted": true, "implicit": false,
        })
    );
    assert_eq!(rows[1].1, "legacy");
    assert_eq!(rows[1].4, legacy.timestamp);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[1].5).unwrap(),
        serde_json::to_value(&legacy).unwrap()
    );
    let shared: i64 = conn
        .query_row("SELECT COUNT(*) FROM context_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        shared, 0,
        "private writes must never enter shared event traffic"
    );
}
