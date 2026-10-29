// SPDX-License-Identifier: Apache-2.0

use chrono::TimeZone as _;
use ed25519_dalek::SigningKey;

use super::*;
use crate::core::session::{Decision, FileTouched, TaskInfo};

fn session(task: &str, progress: Option<u8>, files: &[(&str, bool)]) -> SessionState {
    let mut s = SessionState::new();
    s.id = format!("ses-{}", task.len());
    s.started_at = Utc.timestamp_millis_opt(1_792_796_400_000).unwrap();
    s.updated_at = Utc.timestamp_millis_opt(1_792_799_940_000).unwrap();
    s.task = Some(TaskInfo {
        description: task.into(),
        intent: None,
        progress_pct: progress,
    });
    for (path, modified) in files {
        s.files_touched.push(FileTouched {
            path: (*path).into(),
            file_ref: None,
            read_count: 1,
            modified: *modified,
            last_mode: "full".into(),
            tokens: 0,
            stale: false,
            context_item_id: None,
            summary: None,
        });
    }
    s
}

fn commit(sha: &str, subject: &str) -> Commit {
    Commit {
        sha: sha.into(),
        time: None,
        subject: subject.into(),
        body: String::new(),
    }
}

fn schema() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/schemas/event.schema.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// The body has every required field, no other field, and anchors of the
/// contract's shape.
fn assert_contract_body(body: &Value, schema: &Value) {
    let def = &schema["$defs"]["episode_body"];
    let allowed = def["properties"].as_object().unwrap();
    let obj = body.as_object().unwrap();
    for key in obj.keys() {
        assert!(allowed.contains_key(key), "unknown field {key}");
    }
    for key in def["required"].as_array().unwrap() {
        assert!(obj.contains_key(key.as_str().unwrap()), "missing {key}");
    }
    let anchor = schema["$defs"]["anchor"]["properties"].as_object().unwrap();
    for t in body["touched"].as_array().into_iter().flatten() {
        assert!(
            t.as_object()
                .unwrap()
                .keys()
                .all(|k| anchor.contains_key(k))
        );
        assert_eq!(t["digest"].as_str().unwrap().len(), 64);
    }
    let outcomes = def["properties"]["outcome"]["enum"].as_array().unwrap();
    assert!(outcomes.contains(&body["outcome"]));
}

#[test]
fn the_outcome_and_goal_come_from_the_last_task() {
    let cases = [
        (
            "Fix the relay [100%]",
            None,
            Outcome::Succeeded,
            "Fix the relay",
        ),
        (
            "Fix the relay",
            Some(100),
            Outcome::Succeeded,
            "Fix the relay",
        ),
        (
            "Fix the relay [done]",
            None,
            Outcome::Succeeded,
            "Fix the relay",
        ),
        (
            "Fix the relay [45%]",
            Some(45),
            Outcome::Partial,
            "Fix the relay",
        ),
        (
            "Fix the relay: failed [100%]",
            None,
            Outcome::Failed,
            "Fix the relay: failed",
        ),
        (
            "Port the parser, abandoned",
            None,
            Outcome::Abandoned,
            "Port the parser, abandoned",
        ),
        (
            "Unfailedness is no keyword",
            None,
            Outcome::Partial,
            "Unfailedness is no keyword",
        ),
    ];
    for (task, progress, outcome, goal) in cases {
        assert_eq!(Outcome::of(task, progress), outcome, "{task}");
        assert_eq!(goal_of(task), goal, "{task}");
    }
}

#[test]
fn twenty_recorded_sessions_become_contract_episodes() {
    let repo = crate::core::hive::capture::tests::repo();
    let root = repo.path();
    let abs = root.join("src/lib.rs").to_string_lossy().into_owned();
    let schema = schema();
    let mut checkout = Checkout::new(root);
    let tasks = [
        "Find why refunds double-post on retries [100%]",
        "Speed up the relay handshake [60%]",
        "Rewrite the outbox retry loop: failed",
        "Port the scheduler to async, abandoned",
        "Add sealed vault key rotation [done]",
    ];
    let mut built = 0;
    for i in 0..20 {
        let task = tasks[i % tasks.len()];
        let mut s = session(
            task,
            None,
            &[
                (abs.as_str(), i % 2 == 0),
                ("src/lib.rs", true),
                ("src/missing.rs", true),
                ("/elsewhere/outside.rs", true),
                ("README.md", false),
            ],
        );
        if i % 3 == 0 {
            s.decisions.push(Decision {
                summary: "Keyed the refund calls by request id.".into(),
                rationale: None,
                timestamp: s.updated_at,
            });
        }
        let commits = if i % 4 == 0 {
            vec![
                commit(
                    "3e9f0c1a7b3e9f0c1a7b3e9f0c1a7b3e9f0c1a7b",
                    "fix: key refunds",
                ),
                commit(
                    "4a551fc4524a551fc4524a551fc4524a551fc452",
                    "Revert \"feat: retry refunds twice\"",
                ),
            ]
        } else {
            vec![]
        };
        let dead = vec![
            "Suspected clock skew between workers".to_string(),
            "Build failed at 2026-10-24T14:02 with code 101".to_string(),
            "Two\nlines".to_string(),
        ];
        let e = assemble(&s, root, &dead, &commits, &mut checkout).unwrap();
        assert_eq!(e.touched.len(), 1, "only HEAD files inside the checkout");
        assert_eq!(e.touched[0]["path"], "src/lib.rs");
        assert_eq!(
            e.dead_ends[0], "Suspected clock skew between workers",
            "log text and multi-line dead ends are left out"
        );
        assert_eq!(e.dead_ends.len(), if i % 4 == 0 { 2 } else { 1 });
        assert!(e.session_ref.starts_with("ses_"));
        assert_contract_body(&e.body().unwrap(), &schema);
        built += 1;
    }
    assert_eq!(built, 20);
}

#[test]
fn an_episode_without_task_or_reference_is_not_captured() {
    let repo = crate::core::hive::capture::tests::repo();
    let mut checkout = Checkout::new(repo.path());
    let mut s = session("Look", None, &[("src/lib.rs", false)]);
    assert_eq!(
        assemble(&s, repo.path(), &[], &[], &mut checkout),
        Err(ReasonCode::EpisodeShape),
        "a goal under 8 characters"
    );
    s.task.as_mut().unwrap().description = "Look around the relay code".into();
    assert_eq!(
        assemble(&s, repo.path(), &[], &[], &mut checkout),
        Err(ReasonCode::NoReference),
        "nothing changed, nothing committed"
    );
    s.task = None;
    assert_eq!(
        assemble(&s, repo.path(), &[], &[], &mut checkout),
        Err(ReasonCode::EpisodeShape)
    );
}

#[test]
fn an_episode_is_a_signed_node_add() {
    let repo = crate::core::hive::capture::tests::repo();
    let mut checkout = Checkout::new(repo.path());
    let s = session(
        "Find why refunds double-post [100%]",
        None,
        &[("src/lib.rs", true)],
    );
    let e = assemble(&s, repo.path(), &[], &[], &mut checkout).unwrap();
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let place = Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: [0; 32],
        causal_base: 5,
    };
    let signer = Signer {
        key: &key,
        hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
        worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
    };
    let (event, target) = e
        .node_add(
            &place,
            Stamp {
                wall_ms: 1_792_800_006_000,
                counter: 0,
            },
            &signer,
        )
        .unwrap();
    assert_eq!(event["node_type"], "episode");
    assert_eq!(event["target"], target.as_str());
    assert_eq!(event["capture"]["evidence"], "observed");
    assert!(sign::verify(&event, key.verifying_key().as_bytes()));
    assert_eq!(
        event["body_commit"],
        sign::body_commit(&place.pk_id, &event["body"]).unwrap()
    );
    assert!(
        event["body"].get("anchors").is_none(),
        "episodes carry no anchors"
    );
}

#[test]
fn dead_ends_are_kept_per_session_and_private() {
    let data = tempfile::tempdir().unwrap();
    record_dead_end(data.path(), "s1", "Suspected clock skew between workers").unwrap();
    record_dead_end(data.path(), "s1", "Suspected clock skew between workers").unwrap();
    record_dead_end(data.path(), "s1", "Tried a bigger retry budget").unwrap();
    record_dead_end(data.path(), "s2", "Other session").unwrap();
    assert_eq!(
        dead_ends(data.path(), "s1"),
        vec![
            "Suspected clock skew between workers".to_string(),
            "Tried a bigger retry budget".to_string()
        ]
    );
    assert!(dead_ends(data.path(), "s3").is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = data.path().join("capture/dead_ends");
        for entry in std::fs::read_dir(dir).unwrap() {
            let mode = entry.unwrap().metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "no group or world access");
        }
    }
}
