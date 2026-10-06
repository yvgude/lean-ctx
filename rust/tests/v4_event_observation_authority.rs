// SPDX-License-Identifier: Apache-2.0

use lean_ctx::core::context_os::{
    ContextBus, LocalObservationPayload, ObservationPersistenceError,
};
use lean_ctx::core::events::{EventKind, LeanCtxEvent};
use lean_ctx::core::ocla_bus::{self, OclaEvent, SavingsSource, ThresholdMetric};

fn variants() -> Vec<OclaEvent> {
    vec![
        OclaEvent::RequestCompleted {
            model: "m".into(),
            input_tokens: 1,
            output_tokens: 2,
            duration_ms: 3,
            session_id: Some("s".into()),
        },
        OclaEvent::FeedbackRecorded {
            session_id: "s".into(),
            outcome: lean_ctx::core::ocla_bus::FeedbackOutcome::Accept,
            tool: Some("t".into()),
        },
        OclaEvent::ThresholdShift {
            language: "rust".into(),
            old_value: 1.0,
            new_value: 2.0,
            metric: ThresholdMetric::Entropy,
        },
        OclaEvent::CompressionApplied {
            path: Some("p".into()),
            before_tokens: 10,
            after_tokens: 5,
            strategy: "s".into(),
        },
        OclaEvent::SavingsRecorded {
            input_saved: 4,
            output_saved: 5,
            source: SavingsSource::Compression,
            attribution_id: Some("a".into()),
            evidence_class: Some("e".into()),
            measurement_method: Some("m".into()),
        },
        OclaEvent::IntentClassified {
            tier: "t".into(),
            confidence: 0.5,
            reasoning: "r".into(),
        },
        OclaEvent::OutcomeRecorded {
            session_id: "s".into(),
            accepted: true,
            implicit: false,
        },
        OclaEvent::ResponseOptimized {
            cache_hit: true,
            is_duplicate: false,
            tokens_saved: 6,
        },
        OclaEvent::ModelRouted {
            requested_model: "a".into(),
            routed_model: "b".into(),
            tier: "t".into(),
            model_changed: true,
        },
        OclaEvent::AgentChainEvent {
            agent_id: "a".into(),
            action: "x".into(),
            parent_agent: None,
        },
        OclaEvent::CrossAgentStubServed {
            path: "p".into(),
            tokens_saved: 7,
            serving_agent: "a".into(),
            original_agent: "b".into(),
        },
    ]
}

fn run_isolated(name: &str) -> bool {
    if std::env::var("V4_OBSERVATION_CHILD").as_deref() == Ok(name) {
        return false;
    }
    let data = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("V4_OBSERVATION_CHILD", name)
        .env("LEAN_CTX_DATA_DIR", data.path())
        .env("HOME", data.path())
        .env("USERPROFILE", data.path());
    if let Some(root) = std::env::var_os("SystemRoot") {
        child.env("SystemRoot", root);
    }
    let output = child.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
    true
}

#[test]
fn legacy_producer_commits_before_projection_and_preserves_history() {
    if run_isolated("legacy_producer_commits_before_projection_and_preserves_history") {
        return;
    }

    use lean_ctx::core::events;
    let historical = LeanCtxEvent {
        id: 900_001,
        timestamp: "2026-09-16T08:00:00.000".into(),
        kind: EventKind::CacheHit {
            path: "historical.rs".into(),
            saved_tokens: 7,
        },
    };
    let path = lean_ctx::core::paths::state_dir()
        .unwrap()
        .join("events.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = format!("{}\n", serde_json::to_string(&historical).unwrap());
    std::fs::write(&path, &original).unwrap();
    let id = events::emit(EventKind::CacheHit {
        path: "canonical.rs".into(),
        saved_tokens: 8,
    });
    assert_ne!(id, 0);
    let merged = events::try_load_events_from_file(10).unwrap();
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0], historical);
    assert_eq!(merged[1].id, id);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let runtime = lean_ctx::core::context_os::try_runtime().unwrap();
    assert_eq!(
        runtime.bus.read_legacy_observations(10).unwrap(),
        merged[1..]
    );
    let projection = events::latest_events(10);
    assert_eq!(
        events::emit(EventKind::Anomaly {
            metric: "invalid".into(),
            expected: 1.0,
            actual: f64::NAN,
            deviation_factor: 1.0,
        }),
        0
    );
    assert_eq!(events::latest_events(10), projection);
    assert_eq!(events::try_load_events_from_file(10).unwrap(), merged);
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn ocla_producers_commit_all_variants_once_and_reject_failed_writes() {
    if run_isolated("ocla_producers_commit_all_variants_once_and_reject_failed_writes") {
        return;
    }
    let expected = variants();
    assert_eq!(ocla_bus::emit(expected[0].clone()), 0);
    assert_eq!(ocla_bus::total_emitted(), 0);
    ocla_bus::enable();
    for event in &expected {
        assert_ne!(ocla_bus::emit(event.clone()), 0);
    }
    assert_ne!(ocla_bus::emit_and_bridge(expected[0].clone()), 0);
    let runtime = lean_ctx::core::context_os::try_runtime().unwrap();
    let before = runtime.bus.read_local_observations(100).unwrap();
    let projection = ocla_bus::events_since(0);
    assert_eq!(before.len(), expected.len() + 1);
    assert_eq!(projection.len(), before.len());
    assert_eq!(ocla_bus::total_emitted(), before.len() as u64);
    assert!(
        runtime
            .bus
            .read_legacy_observations(100)
            .unwrap()
            .is_empty()
    );
    for (row, record) in before.iter().zip(&projection) {
        assert_eq!(row.source_id, record.id);
        assert_eq!(row.source_timestamp, record.timestamp_ms.to_string());
        assert_eq!(
            row.payload,
            LocalObservationPayload::Ocla(record.event.clone())
        );
    }
    assert_eq!(
        ocla_bus::emit(OclaEvent::IntentClassified {
            tier: "invalid".into(),
            confidence: f64::NAN,
            reasoning: "fixture".into(),
        }),
        0
    );
    let data = std::path::PathBuf::from(std::env::var_os("LEAN_CTX_DATA_DIR").unwrap());
    let connection = rusqlite::Connection::open(data.join("context-os/context-os.db")).unwrap();
    let shared: i64 = connection
        .query_row("SELECT COUNT(*) FROM context_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        shared, 0,
        "private observations must not become shared traffic"
    );
    connection.execute_batch("CREATE TRIGGER reject_fixture BEFORE INSERT ON context_local_observations BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    assert_eq!(ocla_bus::emit(expected[0].clone()), 0);
    assert_eq!(runtime.bus.read_local_observations(100).unwrap(), before);
    assert_eq!(
        serde_json::to_value(ocla_bus::events_since(0)).unwrap(),
        serde_json::to_value(projection).unwrap()
    );
    assert_eq!(ocla_bus::total_emitted(), before.len() as u64);
    connection
        .execute_batch("DROP TRIGGER reject_fixture;")
        .unwrap();
    assert_ne!(ocla_bus::emit(expected[0].clone()), 0);
    assert_eq!(ocla_bus::total_emitted(), before.len() as u64 + 1);
}

#[test]
fn observation_redelivery_is_idempotent_and_conflicts_fail_closed() {
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("context-os.db");
    let bus = ContextBus::try_open_at(path.clone()).unwrap();
    let producer = uuid::Uuid::new_v4().to_string();
    let record = ocla_bus::OclaBusRecord {
        id: u64::MAX,
        timestamp_ms: u64::MAX,
        event: variants()[0].clone(),
    };
    let cursor = bus.append_ocla_observation(&producer, &record).unwrap();
    drop(bus);
    let bus = ContextBus::try_open_at(path.clone()).unwrap();
    assert_eq!(
        bus.append_ocla_observation(&producer, &record).unwrap(),
        cursor
    );
    let mut conflicting = record.clone();
    conflicting.event = variants()[1].clone();
    assert!(matches!(
        bus.append_ocla_observation(&producer, &conflicting),
        Err(lean_ctx::core::context_os::ObservationPersistenceError::IdentityConflict)
    ));
    let rows = bus.read_local_observations(10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source_id, u64::MAX);
    assert_eq!(rows[0].source_timestamp, u64::MAX.to_string());
    assert_eq!(rows[0].payload, LocalObservationPayload::Ocla(record.event));

    let legacy = LeanCtxEvent {
        id: u64::MAX,
        timestamp: "2026-09-16T08:00:00.000".into(),
        kind: EventKind::CacheHit {
            path: "fixture.rs".into(),
            saved_tokens: 7,
        },
    };
    bus.append_legacy_observation(&producer, &legacy).unwrap();
    assert_eq!(
        bus.read_legacy_observations(10).unwrap(),
        vec![legacy.clone()]
    );
    assert_eq!(bus.read_local_observations(10).unwrap().len(), 2);

    // Independent corruption must not become a plausible partial history.
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute("UPDATE context_local_observations SET source_timestamp = 'contradiction' WHERE source_kind = 'legacy'", []).unwrap();
    assert!(matches!(
        bus.read_local_observations(10),
        Err(ObservationPersistenceError::InvalidRow(_))
    ));
    assert!(matches!(
        bus.read_legacy_observations(10),
        Err(ObservationPersistenceError::InvalidRow(_))
    ));
    conn.execute(
        "UPDATE context_local_observations SET source_timestamp = ?1 WHERE source_kind = 'legacy'",
        [&legacy.timestamp],
    )
    .unwrap();
    assert_eq!(bus.read_legacy_observations(10).unwrap(), vec![legacy]);
    assert_eq!(bus.read_local_observations(10).unwrap().len(), 2);
}
