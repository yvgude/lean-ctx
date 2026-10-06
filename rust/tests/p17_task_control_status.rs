// SPDX-License-Identifier: Apache-2.0

use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use lean_ctx::core::a2a::task::*;
use lean_ctx::core::a2a::task_control::{TaskControlError, get_status};
use std::fmt::Write;

#[test]
fn signed_get_reads_only_the_bound_task_without_mutating_storage() {
    let root = tempfile::tempdir().expect("temporary directory");
    let path = root.path().join("tasks.json");
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7; 32]);
    let mut public_key = String::with_capacity(64);
    for byte in key.verifying_key().to_bytes() {
        write!(&mut public_key, "{byte:02x}").expect("write to String");
    }
    let policy = TaskAuthorityConfigV1 {
        schema_version: 1,
        peers: vec![TaskPeerTrustV1 {
            schema_version: 1,
            key_id: "key".into(),
            agent_id: "owner".into(),
            public_key,
            allowed_actions: vec![TASK_ACTION_SEND.into(), TASK_ACTION_GET.into()],
            allowed_scopes: vec![TaskScopeV1 {
                schema_version: 1,
                tenant_id: "tenant".into(),
                project_id: "project".into(),
            }],
            not_before: now - Duration::minutes(1),
            expires_at: now + Duration::hours(1),
            revoked: false,
        }],
        grants: [TASK_ACTION_SEND, TASK_ACTION_GET]
            .into_iter()
            .map(|action| TaskCapabilityGrantV1 {
                schema_version: 1,
                grant_id: action.replace('/', "-"),
                key_id: "key".into(),
                action: action.into(),
                tenant_id: "tenant".into(),
                project_id: "project".into(),
                not_before: now - Duration::minutes(1),
                expires_at: now + Duration::hours(1),
                revoked: false,
            })
            .collect(),
    };
    let expected = TaskAuthorityExpectationV1 {
        sender: "owner",
        recipient: "receiver",
        tenant_id: "tenant",
        project_id: "project",
        now,
    };
    let mut send = TaskDescriptorV1::new(
        "owner",
        "receiver",
        "tenant",
        "project",
        TASK_ACTION_SEND,
        now,
        now + Duration::minutes(10),
        "send-nonce",
        "private description",
        vec![],
        "tasks-send",
        "key",
    );
    send.sign(&key);
    send.verify_authority(&policy, &expected)
        .expect("authorized fixture send");
    let accepted = TaskStore::materialize_remote_task(&path, &send).expect("persist send");
    let mut get = TaskControlDescriptorV1::new(
        "owner",
        "receiver",
        "tenant",
        "project",
        TASK_ACTION_GET,
        &accepted.task_id,
        &send.content_digest(),
        now,
        now + Duration::minutes(1),
        "get-nonce",
        None,
        "tasks-get",
        "key",
    );
    get.sign(&key);
    let before = std::fs::read(&path).expect("stored bytes");
    let status = get_status(&path, &get, &policy, &expected).expect("authorized status");
    assert_eq!(status.state, TaskState::Created);
    assert_eq!(std::fs::read(&path).expect("stored bytes"), before);
    let encoded = serde_json::to_string(&status).expect("status JSON");
    assert!(!encoded.contains("private description"));
    assert!(!encoded.contains("artifact"));

    let original = TaskStore::read_locked(&path, |store| {
        store
            .get_task(&accepted.task_id)
            .expect("fixture task")
            .clone()
    })
    .expect("read fixture");
    for dimension in 0..6 {
        let mut changed = original.clone();
        match dimension {
            0 => changed.from_agent = "foreign-owner".into(),
            1 => changed.to_agent = "foreign-receiver".into(),
            2 => changed.tenant_id = Some("foreign-tenant".into()),
            3 => changed.project_id = Some("foreign-project".into()),
            4 => changed.descriptor_digest = Some(format!("sha256:{}", "b".repeat(64))),
            5 => changed.authority_key_id = None,
            _ => unreachable!(),
        }
        TaskStore::mutate_locked(&path, |store| {
            *store.get_task_mut(&accepted.task_id).expect("fixture task") = changed;
            Ok::<_, std::io::Error>(())
        })
        .expect("prepare foreign binding");
        let bytes = std::fs::read(&path).expect("stored bytes");
        assert!(
            matches!(
                get_status(&path, &get, &policy, &expected),
                Err(TaskControlError::NotFound)
            ),
            "dimension {dimension}"
        );
        assert_eq!(std::fs::read(&path).expect("stored bytes"), bytes);
    }
    get.task_id = "unknown-task".into();
    get.sign(&key);
    let before_unknown = std::fs::read(&path).expect("stored bytes");
    assert!(matches!(
        get_status(&path, &get, &policy, &expected),
        Err(TaskControlError::NotFound)
    ));
    assert_eq!(std::fs::read(&path).expect("stored bytes"), before_unknown);
}
