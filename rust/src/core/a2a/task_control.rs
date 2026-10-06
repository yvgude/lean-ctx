// SPDX-License-Identifier: Apache-2.0

//! Receiver-side authorization for signed task control operations.
//!
//! This is the storage boundary, not a wire response: the transport must sign
//! the returned status and bind it to the request before publishing it.

use std::path::Path;

use super::task::{
    TASK_ACTION_CANCEL, TASK_ACTION_GET, Task, TaskAuthorityConfigV1, TaskAuthorityError,
    TaskAuthorityExpectationV1, TaskControlDescriptorV1, TaskState, TaskStatusV1, TaskStore,
};

#[derive(Debug)]
pub enum TaskControlError {
    Authority(TaskAuthorityError),
    Storage(std::io::Error),
    /// Deliberately identical for an absent task and any target-binding mismatch.
    NotFound,
    /// An owned task cannot make the requested terminal transition.
    InvalidTransition,
}

impl From<std::io::Error> for TaskControlError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(error)
    }
}

/// Authenticate the operation before touching storage; then enforce target
/// ownership under the same lock as the status read. Never return task payloads
/// or artifact references from this status-only boundary.
pub fn get_status(
    path: &Path,
    request: &TaskControlDescriptorV1,
    policy: &TaskAuthorityConfigV1,
    expected: &TaskAuthorityExpectationV1<'_>,
) -> Result<TaskStatusV1, TaskControlError> {
    if request.action != TASK_ACTION_GET {
        return Err(TaskControlError::Authority(
            TaskAuthorityError::UnsupportedAction,
        ));
    }
    request
        .verify_authority(policy, expected)
        .map_err(TaskControlError::Authority)?;
    TaskStore::read_locked(path, |store| {
        store
            .get_task(&request.task_id)
            .filter(|task| matches_target(task, request))
            .map(Task::status_v1)
            .ok_or(TaskControlError::NotFound)
    })
    .map_err(TaskControlError::Storage)?
}

/// Persist an authenticated owner's cancellation before acknowledging it.
/// Repeated cancellation returns the original terminal status and adds no
/// history entry; a completed or failed task cannot be rewritten as canceled.
/// Like get_status, this is a storage operation, not a signed wire response.
pub fn cancel(
    path: &Path,
    request: &TaskControlDescriptorV1,
    policy: &TaskAuthorityConfigV1,
    expected: &TaskAuthorityExpectationV1<'_>,
) -> Result<TaskStatusV1, TaskControlError> {
    if request.action != TASK_ACTION_CANCEL {
        return Err(TaskControlError::Authority(
            TaskAuthorityError::UnsupportedAction,
        ));
    }
    request
        .verify_authority(policy, expected)
        .map_err(TaskControlError::Authority)?;
    TaskStore::mutate_locked(path, |store| {
        let task = store
            .get_task_mut(&request.task_id)
            .filter(|task| matches_target(task, request))
            .ok_or(TaskControlError::NotFound)?;
        if task.state != TaskState::Canceled {
            task.transition(TaskState::Canceled, request.reason.as_deref())
                .map_err(|_| TaskControlError::InvalidTransition)?;
        }
        Ok(task.status_v1())
    })
}

fn matches_target(task: &Task, request: &TaskControlDescriptorV1) -> bool {
    // A rotated authorized key may represent the same durable owner. Check
    // signed-task provenance, not equality with the original signing key.
    task.authority_key_id.is_some()
        && task.id == request.task_id
        && task.from_agent == request.sender
        && task.to_agent == request.recipient
        && task.tenant_id.as_deref() == Some(request.tenant_id.as_str())
        && task.project_id.as_deref() == Some(request.project_id.as_str())
        && task.descriptor_digest.as_deref() == Some(request.task_descriptor_digest.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn fixture() -> (Task, TaskControlDescriptorV1) {
        let mut task = Task::new("owner", "receiver", "private payload");
        task.tenant_id = Some("tenant".into());
        task.project_id = Some("project".into());
        task.authority_key_id = Some("original-key".into());
        task.descriptor_digest = Some(format!("sha256:{}", "a".repeat(64)));
        let now = Utc::now();
        let request = TaskControlDescriptorV1::new(
            "owner",
            "receiver",
            "tenant",
            "project",
            TASK_ACTION_GET,
            &task.id,
            task.descriptor_digest.as_deref().expect("fixture digest"),
            now,
            now + Duration::minutes(1),
            "nonce",
            None,
            "grant",
            "rotated-key",
        );
        (task, request)
    }

    #[test]
    fn target_binding_requires_all_persisted_authority_dimensions() {
        let (task, request) = fixture();
        assert!(matches_target(&task, &request));
        for field in 0..7 {
            let mut changed = task.clone();
            match field {
                0 => changed.id.push('x'),
                1 => changed.from_agent.push('x'),
                2 => changed.to_agent.push('x'),
                3 => changed.tenant_id = None,
                4 => changed.project_id = None,
                5 => changed.descriptor_digest = None,
                6 => changed.authority_key_id = None,
                _ => unreachable!(),
            }
            assert!(!matches_target(&changed, &request), "dimension {field}");
        }
    }

    #[test]
    fn unauthenticated_status_request_never_touches_storage() {
        let (_, request) = fixture();
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("absent-parent").join("tasks.json");
        let policy = TaskAuthorityConfigV1::default();
        let expected = TaskAuthorityExpectationV1 {
            sender: "owner",
            recipient: "receiver",
            tenant_id: "tenant",
            project_id: "project",
            now: Utc::now(),
        };
        assert!(matches!(
            get_status(&path, &request, &policy, &expected),
            Err(TaskControlError::Authority(_))
        ));
        assert!(!path.parent().expect("parent").exists());
    }

    fn authorized_cancel() -> (Task, TaskControlDescriptorV1, TaskAuthorityConfigV1) {
        use super::super::task::{TaskCapabilityGrantV1, TaskPeerTrustV1, TaskScopeV1};
        let (task, mut request) = fixture();
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        request.action = TASK_ACTION_CANCEL.into();
        request.reason = Some("owner requested cancellation".into());
        request.sign(&key);
        let policy = TaskAuthorityConfigV1 {
            schema_version: 1,
            peers: vec![TaskPeerTrustV1 {
                schema_version: 1,
                key_id: request.key_id.clone(),
                agent_id: request.sender.clone(),
                public_key: crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes()),
                allowed_actions: vec![TASK_ACTION_CANCEL.into()],
                allowed_scopes: vec![TaskScopeV1 {
                    schema_version: 1,
                    tenant_id: request.tenant_id.clone(),
                    project_id: request.project_id.clone(),
                }],
                not_before: request.issued_at - Duration::minutes(1),
                expires_at: request.expires_at + Duration::minutes(1),
                revoked: false,
            }],
            grants: vec![TaskCapabilityGrantV1 {
                schema_version: 1,
                grant_id: request.grant_ref.grant_id.clone(),
                key_id: request.key_id.clone(),
                action: TASK_ACTION_CANCEL.into(),
                tenant_id: request.tenant_id.clone(),
                project_id: request.project_id.clone(),
                not_before: request.issued_at - Duration::minutes(1),
                expires_at: request.expires_at + Duration::minutes(1),
                revoked: false,
            }],
        };
        (task, request, policy)
    }

    fn expected(request: &TaskControlDescriptorV1) -> TaskAuthorityExpectationV1<'_> {
        TaskAuthorityExpectationV1 {
            sender: "owner",
            recipient: "receiver",
            tenant_id: "tenant",
            project_id: "project",
            now: request.issued_at,
        }
    }

    fn persist(path: &Path, task: Task) -> Vec<u8> {
        TaskStore::mutate_locked(path, |store| {
            store.tasks = vec![task];
            Ok::<(), std::io::Error>(())
        })
        .expect("persist fixture");
        std::fs::read(path).expect("fixture bytes")
    }

    #[test]
    fn signed_cancel_is_durable_concurrent_and_hidden_from_legacy() {
        let (task, request, policy) = authorized_cancel();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        persist(&path, task);
        let operation = || cancel(&path, &request, &policy, &expected(&request)).unwrap();
        let (left, right) = std::thread::scope(|scope| {
            let first = scope.spawn(operation);
            let second = scope.spawn(operation);
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_eq!(left, right);
        assert_eq!(left.state, TaskState::Canceled);
        TaskStore::read_locked(&path, |store| {
            let task = store.get_task(&request.task_id).unwrap();
            assert_eq!(task.status_v1(), left);
            assert_eq!(task.history.len(), 2);
            assert_eq!(task.history[1].reason, request.reason);
        })
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        for method in [TASK_ACTION_GET, TASK_ACTION_CANCEL] {
            let legacy = super::super::a2a_compat::JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: 1.into(),
                method: method.into(),
                params: serde_json::json!({"id": request.task_id}),
            };
            let response = super::super::a2a_compat::handle_a2a_jsonrpc_at_path(&legacy, &path);
            assert_eq!(response.error.unwrap().message, "task not found");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn signed_cancel_denies_every_target_mismatch_without_writing() {
        let (task, request, policy) = authorized_cancel();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        for field in 0..7 {
            let mut changed = task.clone();
            match field {
                0 => changed.id.push('x'),
                1 => changed.from_agent.push('x'),
                2 => changed.to_agent.push('x'),
                3 => changed.tenant_id = None,
                4 => changed.project_id = None,
                5 => changed.descriptor_digest = None,
                6 => changed.authority_key_id = None,
                _ => unreachable!(),
            }
            let before = persist(&path, changed);
            assert!(
                matches!(
                    cancel(&path, &request, &policy, &expected(&request)),
                    Err(TaskControlError::NotFound)
                ),
                "dimension {field}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn signed_cancel_rejects_invalid_authority_before_storage() {
        let (_, request, policy) = authorized_cancel();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent-parent/tasks.json");
        for field in 0..5 {
            let mut request = request.clone();
            let mut policy = policy.clone();
            match field {
                0 => request.action = TASK_ACTION_GET.into(),
                1 => request.signature = "00".repeat(64),
                2 => policy.peers[0].revoked = true,
                3 => policy.grants[0].revoked = true,
                4 => policy.grants.clear(),
                _ => unreachable!(),
            }
            assert!(matches!(
                cancel(&path, &request, &policy, &expected(&request)),
                Err(TaskControlError::Authority(_))
            ));
            assert!(!path.parent().unwrap().exists());
        }
    }

    #[test]
    fn signed_cancel_preserves_other_terminal_states_and_corrupt_storage() {
        let (task, request, policy) = authorized_cancel();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        for state in [TaskState::Completed, TaskState::Failed] {
            let mut task = task.clone();
            task.transition(TaskState::Working, None).unwrap();
            task.transition(state, None).unwrap();
            let before = persist(&path, task);
            assert!(matches!(
                cancel(&path, &request, &policy, &expected(&request)),
                Err(TaskControlError::InvalidTransition)
            ));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        std::fs::write(&path, "invalid fixture").unwrap();
        assert!(matches!(
            cancel(&path, &request, &policy, &expected(&request)),
            Err(TaskControlError::Storage(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid fixture");
    }

    #[test]
    fn signed_cancel_never_persists_an_unreadable_history_overflow() {
        let (mut task, request, policy) = authorized_cancel();
        for index in 0..2046 {
            task.transition(
                if index % 2 == 0 {
                    TaskState::Working
                } else {
                    TaskState::InputRequired
                },
                None,
            )
            .unwrap();
        }
        // Reconstruct the last entry of a legacy full history explicitly: new
        // authoring reserves it for a terminal state. Preserve the denial test.
        task.history.push(super::super::task::TaskTransition {
            from: task.state.clone(),
            to: TaskState::Working,
            timestamp: task.updated_at,
            reason: None,
        });
        task.state = TaskState::Working;
        assert_eq!(task.history.len(), 2048);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let before = persist(&path, task);
        assert!(matches!(
            cancel(&path, &request, &policy, &expected(&request)),
            Err(TaskControlError::InvalidTransition)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(TaskStore::read_locked(&path, |_| ()).is_ok());
    }

    #[test]
    fn signed_cancel_uses_reserved_capacity_and_retries_without_growth() {
        let (mut task, request, policy) = authorized_cancel();
        while task.history.len() < 2047 {
            let next = if task.state == TaskState::Working {
                TaskState::InputRequired
            } else {
                TaskState::Working
            };
            task.transition(next, None).unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        persist(&path, task);
        let first = cancel(&path, &request, &policy, &expected(&request)).unwrap();
        assert_eq!(first.state, TaskState::Canceled);
        let stored = TaskStore::read_locked(&path, |store| store.tasks[0].clone()).unwrap();
        assert_eq!(stored.history.len(), 2048);
        assert_eq!(stored.history.last().unwrap().timestamp, stored.updated_at);
        assert_eq!(
            cancel(&path, &request, &policy, &expected(&request)).unwrap(),
            first
        );
        let retry = TaskStore::read_locked(&path, |store| store.tasks[0].clone()).unwrap();
        assert_eq!(
            serde_json::to_vec(&retry).unwrap(),
            serde_json::to_vec(&stored).unwrap()
        );
    }

    #[test]
    fn signed_cancel_rejects_backward_clock_without_touching_storage() {
        let (mut task, request, policy) = authorized_cancel();
        task.updated_at = Utc::now() + Duration::hours(1);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let before = persist(&path, task);
        assert!(matches!(
            cancel(&path, &request, &policy, &expected(&request)),
            Err(TaskControlError::InvalidTransition)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(TaskStore::read_locked(&path, |_| ()).is_ok());
    }
}
