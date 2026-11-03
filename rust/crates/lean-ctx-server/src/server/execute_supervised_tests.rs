// SPDX-License-Identifier: Apache-2.0
//! The protected supervisor (`core::protected_execution`) running brokered
//! `ctx_shell` commands with this layer's executor, over a real socket pair
//! and real child processes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lean_ctx::core::protected_execution::supervised_for_test;

/// The supervisor runs brokered shell commands with the registered executor.
fn supervised<T>(
    denials: &str,
    exercise: impl FnOnce(&lean_ctx::core::protected_execution::Client) -> T,
) -> T {
    super::register_protected_shell_runner();
    supervised_for_test(denials, exercise)
}

#[test]
fn ordinary_work_succeeds_and_exit_codes_survive_the_channel() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::core::protected_store::unpin_synthetic_session();
    let temp = tempfile::tempdir().expect("temp");
    std::fs::write(temp.path().join("ordinary.txt"), "project-content").unwrap();
    let cwd = temp.path().to_str().unwrap().to_string();
    supervised("", |client| {
        let (text, code) = client.shell(
            "cat ordinary.txt",
            &cwd,
            &std::collections::HashMap::new(),
            Some(30_000),
            None,
            false,
            None,
        );
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("project-content"), "{text}");

        let (_, failing) = client.shell(
            "exit 17",
            &cwd,
            &std::collections::HashMap::new(),
            Some(30_000),
            None,
            false,
            None,
        );
        assert_eq!(failing, 17, "the child's real exit code must survive");
    });
}

#[test]
fn a_supervised_child_is_denied_the_store_but_keeps_its_project_work() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::core::protected_store::unpin_synthetic_session();
    let temp = tempfile::tempdir().expect("temp");
    let store = temp.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let store = store.canonicalize().unwrap();
    let secret = store.join("knowledge.json");
    std::fs::write(&secret, "synthetic-provider-fact").unwrap();
    let work = temp.path().join("project");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("ordinary.txt"), "project-content").unwrap();
    let denials = format!("(deny file-read* (subpath \"{}\"))\n", store.display());
    let cwd = work.to_str().unwrap().to_string();
    let secret_path = secret.to_str().unwrap().to_string();

    supervised(&denials, |client| {
        let (text, code) = client.shell(
            "cat ordinary.txt",
            &cwd,
            &std::collections::HashMap::new(),
            Some(30_000),
            None,
            false,
            None,
        );
        assert_eq!(code, 0, "ordinary project work must still run: {text}");

        let (denied, code) = client.shell(
            &format!("cat {secret_path}"),
            &cwd,
            &std::collections::HashMap::new(),
            Some(30_000),
            None,
            false,
            None,
        );
        assert_ne!(code, 0, "the store must be denied: {denied}");
        assert!(
            !denied.contains("synthetic-provider-fact"),
            "no store content may cross the channel"
        );
        let (_, stderr, code) = client
            .session_program("/usr/bin/true", &[], &[], 10, None)
            .unwrap();
        assert_eq!(code, 0, "{stderr}");
        let (text, stderr, code) = client
            .session_program("/bin/cat", &[&secret_path], &[], 10, None)
            .unwrap();
        assert_ne!(code, 0, "{text} {stderr}");
        assert!(!text.contains("synthetic-provider-fact"));
    });
    assert_eq!(
        std::fs::read_to_string(&secret).unwrap(),
        "synthetic-provider-fact"
    );
}

#[test]
fn cancellation_terminates_the_whole_process_group() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::core::protected_store::unpin_synthetic_session();
    let cancel = AtomicBool::new(false);
    supervised("", |client| {
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(400));
                cancel.store(true, Ordering::Release);
            });
            let started = Instant::now();
            let (_, code) = client.shell(
                "sleep 120 & sleep 120",
                "/tmp",
                &std::collections::HashMap::new(),
                Some(120_000),
                Some(&cancel),
                false,
                None,
            );
            assert_eq!(code, 130, "a cancelled command reports cancellation");
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "cancellation must not wait for the command's own timeout"
            );
        });
    });
}
