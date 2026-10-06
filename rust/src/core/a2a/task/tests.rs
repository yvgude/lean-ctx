// SPDX-License-Identifier: Apache-2.0

use super::*;
use chrono::Utc;

#[test]
fn task_lifecycle_happy_path() {
    let mut task = Task::new("agent-a", "agent-b", "fix the bug");
    assert_eq!(task.state, TaskState::Created);

    task.transition(TaskState::Working, Some("started"))
        .unwrap();
    assert_eq!(task.state, TaskState::Working);

    task.transition(TaskState::Completed, Some("done")).unwrap();
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.history.len(), 3);
}

#[test]
fn task_lifecycle_with_input_required() {
    let mut task = Task::new("a", "b", "deploy");
    task.transition(TaskState::Working, None).unwrap();
    task.transition(TaskState::InputRequired, Some("need credentials"))
        .unwrap();
    task.transition(TaskState::Working, Some("got them"))
        .unwrap();
    task.transition(TaskState::Completed, None).unwrap();
    assert_eq!(task.history.len(), 5);
}

#[test]
fn invalid_transitions_rejected() {
    let mut task = Task::new("a", "b", "test");
    task.transition(TaskState::Working, None).unwrap();
    task.transition(TaskState::Completed, None).unwrap();

    let err = task.transition(TaskState::Working, None);
    assert!(err.is_err());
}

#[test]
fn task_store_operations() {
    let mut store = TaskStore::default();
    let id = store.create_task("agent-a", "agent-b", "review PR");
    assert_eq!(store.tasks.len(), 1);

    let task = store.get_task(&id).unwrap();
    assert_eq!(task.from_agent, "agent-a");

    let pending = store.pending_tasks_for("agent-b");
    assert_eq!(pending.len(), 1);

    store
        .get_task_mut(&id)
        .unwrap()
        .transition(TaskState::Working, None)
        .unwrap();
    store
        .get_task_mut(&id)
        .unwrap()
        .transition(TaskState::Completed, None)
        .unwrap();

    let pending = store.pending_tasks_for("agent-b");
    assert_eq!(pending.len(), 0);
}

#[test]
fn history_limit_reserves_terminal_capacity_without_bypassing_state_rules() {
    let mut task = Task::new("owner", "receiver", "bounded work");
    while task.history.len() < MAX_HISTORY - 2 {
        let next = if task.state == TaskState::Working {
            TaskState::InputRequired
        } else {
            TaskState::Working
        };
        task.transition(next, None).unwrap();
    }
    task.transition(TaskState::InputRequired, None).unwrap();
    assert_eq!(task.history.len(), MAX_HISTORY - 1);
    let before = serde_json::to_vec(&task).unwrap();
    for denied in [TaskState::Working, TaskState::Completed] {
        assert!(task.transition(denied, None).is_err());
        assert_eq!(serde_json::to_vec(&task).unwrap(), before);
    }
    for terminal in [TaskState::Canceled, TaskState::Failed] {
        let mut ended = task.clone();
        ended.transition(terminal.clone(), None).unwrap();
        assert_eq!(ended.state, terminal);
        assert_eq!(ended.history.len(), MAX_HISTORY);
        assert_eq!(ended.history.last().unwrap().timestamp, ended.updated_at);
    }
    // A historical full nonterminal history is legal to restore, not extend.
    task.history.push(TaskTransition {
        from: task.state.clone(),
        to: TaskState::Working,
        timestamp: task.updated_at,
        reason: None,
    });
    task.state = TaskState::Working;
    let before = serde_json::to_vec(&task).unwrap();
    for denied in [
        TaskState::Canceled,
        TaskState::Failed,
        TaskState::Completed,
        TaskState::InputRequired,
    ] {
        assert!(task.transition(denied, None).is_err());
        assert_eq!(serde_json::to_vec(&task).unwrap(), before);
    }
}

#[test]
fn transition_uses_one_timestamp_and_rejects_clock_rollback_without_mutation() {
    let mut task = Task::new("owner", "receiver", "clock safety");
    task.transition(TaskState::Working, None).unwrap();
    assert_eq!(task.history.last().unwrap().timestamp, task.updated_at);
    task.updated_at = Utc::now() + chrono::Duration::hours(1);
    let before = serde_json::to_vec(&task).unwrap();
    assert_eq!(
        task.transition(TaskState::Canceled, None).unwrap_err(),
        "task clock precedes stored state"
    );
    assert_eq!(serde_json::to_vec(&task).unwrap(), before);
}

#[test]
fn invalid_mutations_cannot_replace_a_readable_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let mut future = Task::new("owner", "receiver", "restored from a fast clock");
    let timestamp = Utc::now() + chrono::Duration::hours(1);
    future.created_at = timestamp;
    future.updated_at = timestamp;
    future.history[0].timestamp = timestamp;
    future.messages[0].timestamp = timestamp;
    TaskStore::mutate_locked(&path, |store| {
        store.tasks.push(future);
        store.create_task("owner", "receiver", "unrelated task");
        Ok::<_, std::io::Error>(())
    })
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    for case in 0..4 {
        let error = TaskStore::mutate_locked(&path, |store| {
            let task = &mut store.tasks[0];
            match case {
                0 => task.add_message(
                    "owner",
                    vec![TaskPart::Text {
                        text: "later".into(),
                    }],
                ),
                1 => task.add_artifact(TaskPart::Text {
                    text: "artifact".into(),
                }),
                2 => task.history[0].reason = Some("x".repeat(MAX_DESCRIPTION_BYTES + 1)),
                3 => task.state = TaskState::Working,
                _ => unreachable!(),
            }
            Ok::<_, std::io::Error>(())
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            TaskStore::read_locked(&path, |store| store.tasks.len()).unwrap(),
            2
        );
    }
}

#[test]
fn terminal_states_correct() {
    assert!(TaskState::Completed.is_terminal());
    assert!(TaskState::Failed.is_terminal());
    assert!(TaskState::Canceled.is_terminal());
    assert!(!TaskState::Created.is_terminal());
    assert!(!TaskState::Working.is_terminal());
    assert!(!TaskState::InputRequired.is_terminal());
}

#[test]
fn corrupt_store_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    std::fs::write(&path, b"{not-json").unwrap();

    assert!(TaskStore::read_locked(&path, |store| store.tasks.len()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{not-json");
}

#[test]
fn restored_store_rejects_duplicate_task_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let task = Task::new("sender", "receiver", "bounded task");
    let store = TaskStore {
        tasks: vec![task.clone(), task],
        updated_at: Some(Utc::now()),
        idempotency: Vec::new(),
    };
    std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();

    let error = TaskStore::read_locked(&path, |_| ()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("duplicate task ids"));
}

#[test]
fn restored_store_rejects_oversized_fields_and_collections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let mut task = Task::new("sender", "receiver", "bounded task");
    task.from_agent = "x".repeat(MAX_ID_BYTES + 1);
    let store = TaskStore {
        tasks: vec![task],
        updated_at: Some(Utc::now()),
        idempotency: Vec::new(),
    };
    std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
    assert!(TaskStore::read_locked(&path, |_| ()).is_err());

    let mut task = Task::new("sender", "receiver", "bounded task");
    task.messages = vec![task.messages[0].clone(); MAX_MESSAGES + 1];
    let store = TaskStore {
        tasks: vec![task],
        updated_at: Some(Utc::now()),
        idempotency: Vec::new(),
    };
    std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
    assert!(TaskStore::read_locked(&path, |_| ()).is_err());
}

#[test]
fn restored_store_rejects_inconsistent_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let mut task = Task::new("sender", "receiver", "bounded task");
    task.state = TaskState::Completed;
    let store = TaskStore {
        tasks: vec![task],
        updated_at: Some(Utc::now()),
        idempotency: Vec::new(),
    };
    std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();

    let error = TaskStore::read_locked(&path, |_| ()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("does not match history"));

    let mut task = Task::new("sender", "receiver", "bounded task");
    task.messages[0].timestamp = task.created_at - chrono::Duration::seconds(1);
    let store = TaskStore {
        tasks: vec![task],
        updated_at: Some(Utc::now()),
        idempotency: Vec::new(),
    };
    std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
    let error = TaskStore::read_locked(&path, |_| ()).unwrap_err();
    assert!(error.to_string().contains("outside task lifetime"));
}

#[cfg(unix)]
#[test]
fn symlink_store_and_parent_fail_closed() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.json");
    std::fs::write(&target, b"{}").unwrap();
    let store_link = dir.path().join("tasks.json");
    symlink(&target, &store_link).unwrap();
    assert!(TaskStore::read_locked(&store_link, |_| ()).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"{}");

    let real_parent = dir.path().join("real-parent");
    std::fs::create_dir(&real_parent).unwrap();
    let parent_link = dir.path().join("linked-parent");
    symlink(&real_parent, &parent_link).unwrap();
    let path = parent_link.join("tasks.json");
    assert!(TaskStore::mutate_locked(&path, |_| Ok::<_, std::io::Error>(())).is_err());
    assert!(!real_parent.join("tasks.json").exists());

    let intermediate_target = dir.path().join("intermediate-target");
    std::fs::create_dir(&intermediate_target).unwrap();
    let intermediate_link = dir.path().join("intermediate-link");
    symlink(&intermediate_target, &intermediate_link).unwrap();
    let nested_path = intermediate_link.join("nested").join("tasks.json");
    assert!(TaskStore::mutate_locked(&nested_path, |_| Ok::<_, std::io::Error>(())).is_err());
    assert!(!intermediate_target.join("nested").exists());
}

#[test]
fn concurrent_mutations_do_not_lose_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let workers = 12;
    let handles = (0..workers)
        .map(|index| {
            let path = path.clone();
            std::thread::spawn(move || {
                TaskStore::mutate_locked(&path, |store| {
                    store.create_task("sender", "receiver", &format!("task-{index}"));
                    Ok::<_, std::io::Error>(())
                })
                .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }

    let ids = TaskStore::read_locked(&path, |store| {
        store
            .tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<std::collections::HashSet<_>>()
    })
    .unwrap();
    assert_eq!(ids.len(), workers);
}

#[cfg(unix)]
#[test]
fn task_store_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    TaskStore::mutate_locked(&path, |store| {
        store.create_task("sender", "receiver", "private task");
        Ok::<_, std::io::Error>(())
    })
    .unwrap();

    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn fifo_store_is_rejected_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c_path` is a live, NUL-terminated pathname and mode is valid.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

    let started = std::time::Instant::now();
    assert!(TaskStore::read_locked(&path, |_| ()).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}
