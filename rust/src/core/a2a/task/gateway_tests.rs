// SPDX-License-Identifier: Apache-2.0
//! The task store at the gateway boundary: new caller text is admitted at
//! the mutators, stored text leaves only as the served copy. Credential
//! fixtures are split with `concat!` so the repository's own secret scanners
//! never see a literal key.

use std::path::Path;

use chrono::{Duration, Utc};

use super::{
    ARTIFACT_REF_VERSION, AgentArtifactRefV1, TASK_ACTION_SEND, Task, TaskDescriptorV1, TaskPart,
    TaskState, TaskStore,
};
use crate::core::a2a::a2a_compat::{JsonRpcRequest, JsonRpcResponse, handle_a2a_jsonrpc_at_path};
use crate::core::context_admission::carrier::Withheld;
use crate::core::context_admission::stored::lineage_support::governed;
use crate::core::policy::runtime::TestPolicyOverride;

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
const RESTRICTED: &str = "*** TOP SECRET ***\nmerger plan";

fn notice() -> String {
    Withheld::no_source_authority().notice()
}

fn peer(path: &Path, method: &str, params: serde_json::Value) -> JsonRpcResponse {
    handle_a2a_jsonrpc_at_path(
        &JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: 1.into(),
            method: method.to_owned(),
            params,
        },
        path,
    )
}

fn peer_send(path: &Path, text: &str) -> JsonRpcResponse {
    peer(
        path,
        "message/send",
        serde_json::json!({
            "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] }
        }),
    )
}

fn result_text(response: &JsonRpcResponse) -> String {
    response.result.as_ref().expect("result").to_string()
}

fn result_id(response: &JsonRpcResponse) -> String {
    response.result.as_ref().expect("result")["id"]
        .as_str()
        .expect("task id")
        .to_owned()
}

/// While protection is active a task has no source authority: its text is
/// stored, but every leaving point delivers the typed notice in its place.
#[test]
fn task_text_is_withheld_at_every_leaving_point_while_protection_is_active() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let notice = notice();

    // A peer over the legacy route: send, get, cancel.
    let directory = tempfile::tempdir().expect("store directory");
    let path = directory.path().join("tasks.json");
    let sent = peer_send(&path, "private payload for the peer");
    let peer_id = result_id(&sent);
    let got = peer(&path, "tasks/get", serde_json::json!({ "id": peer_id }));
    let canceled = peer(&path, "tasks/cancel", serde_json::json!({ "id": peer_id }));
    for response in [&sent, &got, &canceled] {
        let body = result_text(response);
        assert!(body.contains(&notice), "{body}");
        assert!(!body.contains("private payload"), "{body}");
    }
    assert!(
        result_text(&canceled).contains("canceled via A2A"),
        "fixed reasons stay"
    );
    let disk = std::fs::read_to_string(&path).expect("peer store");
    assert!(disk.contains("private payload for the peer"), "{disk}");

    // The copy itself: every kind of text, in place, shape unchanged.
    let mut task = Task::new("agent-a", "agent-b", "describe the outage");
    task.add_artifact(TaskPart::Data {
        mime_type: "text/plain".to_owned(),
        data: "postmortem body".to_owned(),
    });
    task.metadata
        .insert("ticket".to_owned(), "OPS-7 outage".to_owned());
    let served = task.served();
    assert_eq!(served.description, notice);
    assert_eq!(served.served_description(), notice);
    assert!(matches!(
        &served.messages[0].parts[0],
        TaskPart::Text { text } if *text == notice
    ));
    assert!(matches!(
        &served.artifacts[0],
        TaskPart::Text { text } if *text == notice
    ));
    assert_eq!(served.metadata["ticket"], notice);
    assert_eq!(served.history[0].reason.as_deref(), Some("task created"));
    assert_eq!(served.id, task.id);
    assert_eq!(served.state, TaskState::Created);
    assert_eq!(served.created_at, task.created_at);
    assert_eq!(task.description, "describe the outage", "the stored task");
}

/// Developer mode: clean text is delivered byte for byte at the same points.
#[test]
fn clean_task_text_is_served_unchanged_in_developer_mode() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);

    let directory = tempfile::tempdir().expect("store directory");
    let path = directory.path().join("tasks.json");
    let sent = peer_send(&path, "payload for the peer");
    let got = peer(
        &path,
        "tasks/get",
        serde_json::json!({ "id": result_id(&sent) }),
    );
    for response in [&sent, &got] {
        let result = response.result.as_ref().expect("result");
        assert_eq!(
            result["messages"][0]["parts"][0]["text"],
            "payload for the peer"
        );
    }

    let mut task = Task::new("agent-a", "agent-b", "describe the outage");
    task.add_artifact(TaskPart::Data {
        mime_type: "text/plain".to_owned(),
        data: "postmortem body".to_owned(),
    });
    task.metadata
        .insert("ticket".to_owned(), "OPS-7 outage".to_owned());
    let served = task.served();
    assert_eq!(served.description, "describe the outage");
    assert!(matches!(
        &served.artifacts[0],
        TaskPart::Data { mime_type, data } if mime_type == "text/plain" && data == "postmortem body"
    ));
    assert_eq!(served.metadata["ticket"], "OPS-7 outage");
}

/// New text the gateway masks is stored masked: the key never reaches the
/// store, at any mutator.
#[test]
fn key_shaped_new_text_is_stored_masked() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);

    let directory = tempfile::tempdir().expect("store directory");
    let path = directory.path().join("tasks.json");
    let sent = peer_send(&path, &format!("deploy with aws_key = {AWS_KEY}"));
    assert!(!result_text(&sent).contains(AWS_KEY));
    let followup = peer(
        &path,
        "message/send",
        serde_json::json!({
            "message": {
                "taskId": result_id(&sent),
                "parts": [{ "kind": "text", "text": format!("again aws_key = {AWS_KEY}") }]
            }
        }),
    );
    assert!(followup.error.is_none());
    let disk = std::fs::read_to_string(&path).expect("peer store");
    assert!(disk.contains("[REDACTED:"), "{disk}");
    assert!(!disk.contains(AWS_KEY), "{disk}");

    let mut task = Task::new("agent-a", "agent-b", "work");
    task.transition(TaskState::Working, Some(&format!("aws_key = {AWS_KEY}")))
        .expect("a masked reason is stored");
    let reason = task.history[1].reason.as_deref().expect("reason");
    assert!(reason.contains("[REDACTED:") && !reason.contains(AWS_KEY));
}

fn remote_descriptor(body: &str) -> TaskDescriptorV1 {
    let now = Utc::now();
    let mut descriptor = TaskDescriptorV1::new(
        "agent-a",
        "server-b",
        "tenant-a",
        "project-a",
        TASK_ACTION_SEND,
        now,
        now + Duration::minutes(30),
        "idem-1",
        body,
        vec![AgentArtifactRefV1 {
            schema_version: ARTIFACT_REF_VERSION,
            artifact_id: "artifact-1".to_owned(),
            digest: "sha256:abcdef".to_owned(),
            media_type: "application/json".to_owned(),
            size_bytes: 12,
            uri: None,
        }],
        "grant-a",
        "key-a",
    );
    descriptor.sign(&ed25519_dalek::SigningKey::from_bytes(&[7; 32]));
    descriptor
}

/// Restricted new text is refused at each mutator with a typed, content-free
/// answer, and nothing of it reaches the disk (E3).
#[test]
fn restricted_new_text_is_refused_at_the_mutator_and_absent_from_disk() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    // The legacy peer route: new task and follow-up message.
    let directory = tempfile::tempdir().expect("store directory");
    let path = directory.path().join("tasks.json");
    let refused = peer_send(&path, RESTRICTED);
    let error = refused.error.expect("typed refusal");
    assert_eq!(error.code, -32602);
    assert!(!error.message.contains("merger"), "{}", error.message);
    assert!(!path.exists(), "a refusal saves nothing");
    let sent = peer_send(&path, "an ordinary request");
    let refused = peer(
        &path,
        "message/send",
        serde_json::json!({
            "message": {
                "taskId": result_id(&sent),
                "parts": [{ "kind": "text", "text": RESTRICTED }]
            }
        }),
    );
    assert!(refused.error.is_some());
    let disk = std::fs::read_to_string(&path).expect("peer store");
    assert!(!disk.contains("merger"), "{disk}");

    // A signed remote descriptor: no task and no idempotency record.
    let remote = directory.path().join("remote.json");
    let error = TaskStore::materialize_remote_task(&remote, &remote_descriptor(RESTRICTED))
        .expect_err("restricted description");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!error.to_string().contains("merger"), "{error}");
    assert!(!remote.exists(), "a refusal saves nothing");
    let stored = TaskStore::materialize_remote_task(&remote, &remote_descriptor("ship it"))
        .expect("the same key is free: the refusal left no record");
    assert!(!stored.duplicate);

    // The part mutator itself, for every kind of part.
    let mut task = Task::new("agent-a", "agent-b", "work");
    for part in [
        TaskPart::Text {
            text: RESTRICTED.to_owned(),
        },
        TaskPart::Data {
            mime_type: "text/plain".to_owned(),
            data: RESTRICTED.to_owned(),
        },
        TaskPart::File {
            name: "plan.txt".to_owned(),
            mime_type: None,
            data: Some(RESTRICTED.to_owned()),
            uri: None,
        },
    ] {
        let clean = TaskPart::Text {
            text: "a clean part of the same message".to_owned(),
        };
        assert!(task.append_message("agent-b", &[clean, part]).is_err());
    }
    assert_eq!(
        task.messages.len(),
        1,
        "one refused part refuses the message"
    );
}

/// Admission is for NEW text only. A task whose stored text today's policy
/// would refuse still loads, takes clean messages and transitions, and is
/// saved — a later tightening never locks an existing task.
#[test]
fn an_existing_task_stays_mutable_after_a_policy_tightening() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    // Written as an earlier, laxer policy stored it.
    let old = Task::new("agent-a", "agent-b", RESTRICTED);
    let id = old.id.clone();
    let path = TaskStore::default_path().expect("task store path");
    TaskStore::mutate_locked(&path, |store| {
        store.tasks.push(old);
        Ok::<(), std::io::Error>(())
    })
    .expect("the store does not refuse on load or save");

    TaskStore::mutate_locked(&path, |store| {
        let task = store.get_task_mut(&id).expect("the old task loads");
        let clean = TaskPart::Text {
            text: "picking this up".to_owned(),
        };
        task.append_message("agent-b", &[clean])
            .expect("a clean message is admitted");
        task.transition(TaskState::Working, Some("in progress"))
            .expect("a clean transition");
        task.transition(TaskState::Canceled, None).expect("cancel");
        Ok::<(), std::io::Error>(())
    })
    .expect("the store saves the old task");

    let served = TaskStore::read_locked(&path, |store| {
        let task = store.get_task(&id).expect("task");
        assert_eq!(
            (&task.state, task.messages.len()),
            (&TaskState::Canceled, 2)
        );
        task.served()
    })
    .expect("store");
    let disk = std::fs::read_to_string(&path).expect("task store");
    assert!(disk.contains("merger plan"), "the old text is kept");
    // It is not delivered.
    assert_eq!(served.description, notice());
    assert!(!serde_json::to_string(&served).unwrap().contains("merger"));
}
