// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cli::context_execution::ExecutionRoute;

use super::{
    CommandOutput, LocalOperation, Recording, observe_deps, observe_diff, observe_find,
    observe_glob, observe_grep, observe_ls, observe_read,
};

struct NoDaemonGuard {
    previous: Option<OsString>,
}

impl NoDaemonGuard {
    fn new() -> Self {
        let previous = std::env::var_os("__LEAN_CTX_NO_DAEMON");
        crate::test_env::set_var("__LEAN_CTX_NO_DAEMON", "1");
        Self { previous }
    }
}

impl Drop for NoDaemonGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            crate::test_env::set_var("__LEAN_CTX_NO_DAEMON", value);
        } else {
            crate::test_env::remove_var("__LEAN_CTX_NO_DAEMON");
        }
    }
}

fn without_daemon<T>(run: impl FnOnce() -> T) -> T {
    let _lock = crate::core::data_dir::test_env_lock();
    let _no_daemon = NoDaemonGuard::new();
    run()
}

fn marked_runner<'a>(marker: Arc<AtomicBool>) -> impl FnOnce(LocalOperation<'a>) -> CommandOutput {
    move |operation| {
        marker.store(true, Ordering::SeqCst);
        operation()
    }
}

#[test]
fn invalid_requests_are_rejected_before_local_dispatch() {
    let read = observe_read(&[], |_operation| panic!("invalid read dispatched"));
    assert_eq!(read.route, ExecutionRoute::NotDispatched);
    assert_eq!(read.exit_code, 1);

    let diff = observe_diff(&[], |_operation| panic!("invalid diff dispatched"));
    assert_eq!(diff.route, ExecutionRoute::NotDispatched);
    assert_eq!(diff.exit_code, 1);

    let grep = observe_grep(&[], |_operation| panic!("invalid grep dispatched"));
    assert_eq!(grep.route, ExecutionRoute::NotDispatched);
    assert_eq!(grep.exit_code, 1);

    let glob = observe_glob(&[], |_operation| panic!("invalid glob dispatched"));
    assert_eq!(glob.route, ExecutionRoute::NotDispatched);
    assert_eq!(glob.exit_code, 1);

    let find = observe_find(&[], |_operation| panic!("invalid find dispatched"));
    assert_eq!(find.route, ExecutionRoute::NotDispatched);
    assert_eq!(find.exit_code, 1);

    let ls = observe_ls(&["--unsupported".to_string()], |_operation| {
        panic!("invalid ls dispatched")
    });
    assert_eq!(ls.route, ExecutionRoute::NotDispatched);
    assert_eq!(ls.exit_code, 1);
}

#[test]
fn read_local_success_and_cache_return_one_recording_each() {
    without_daemon(|| {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("sample.rs");
        std::fs::write(&file, "fn sample() { println!(\"ok\"); }\n").unwrap();
        let args = vec![
            file.to_string_lossy().into_owned(),
            "--mode".to_string(),
            "full".to_string(),
            // `lean-ctx read` is jailed to its project root (#1903).
            format!("--root={}", directory.path().display()),
        ];

        let first_called = Arc::new(AtomicBool::new(false));
        let first = observe_read(&args, marked_runner(Arc::clone(&first_called)));
        assert!(first_called.load(Ordering::SeqCst));
        assert_eq!(first.route, ExecutionRoute::Local);
        assert_eq!(first.exit_code, 0);
        assert!(matches!(
            first.recording.as_ref(),
            Some(Recording::Read {
                cache_hit: false,
                ..
            })
        ));

        let second_called = Arc::new(AtomicBool::new(false));
        let second = observe_read(&args, marked_runner(Arc::clone(&second_called)));
        assert!(second_called.load(Ordering::SeqCst));
        assert_eq!(second.route, ExecutionRoute::Local);
        assert_eq!(second.exit_code, 0);
        assert!(matches!(
            second.recording.as_ref(),
            Some(Recording::Read {
                cache_hit: true,
                ..
            })
        ));

        crate::core::cli_cache::invalidate(file.to_string_lossy().as_ref()).unwrap();
    });
}

#[test]
fn diff_local_success_invokes_callback_and_records_stats() {
    without_daemon(|| {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.txt");
        let second_path = directory.path().join("second.txt");
        std::fs::write(&first_path, "alpha\n").unwrap();
        std::fs::write(&second_path, "beta\n").unwrap();
        let args = vec![
            first_path.to_string_lossy().into_owned(),
            second_path.to_string_lossy().into_owned(),
        ];
        let called = Arc::new(AtomicBool::new(false));
        let result = observe_diff(&args, marked_runner(Arc::clone(&called)));
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(result.route, ExecutionRoute::Local);
        assert_eq!(result.exit_code, 0);
        assert!(matches!(
            result.recording.as_ref(),
            Some(Recording::Stats {
                tool: "cli_diff",
                ..
            })
        ));
    });
}

#[test]
fn grep_local_no_match_preserves_nonzero_status() {
    without_daemon(|| {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("sample.txt");
        std::fs::write(&file, "present\n").unwrap();
        let args = vec![
            "definitely-not-present".to_string(),
            file.to_string_lossy().into_owned(),
        ];
        let called = Arc::new(AtomicBool::new(false));
        let result = observe_grep(&args, marked_runner(Arc::clone(&called)));
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(result.route, ExecutionRoute::Local);
        assert_eq!(result.exit_code, 1);
        assert!(matches!(
            result.recording.as_ref(),
            Some(Recording::Search { .. })
        ));
    });
}

#[test]
fn glob_local_success_records_stats() {
    without_daemon(|| {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("sample.rs"), "fn main() {}\n").unwrap();
        let args = vec![
            "*.rs".to_string(),
            directory.path().to_string_lossy().into_owned(),
        ];
        let called = Arc::new(AtomicBool::new(false));
        let result = observe_glob(&args, marked_runner(Arc::clone(&called)));
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(result.route, ExecutionRoute::Local);
        assert_eq!(result.exit_code, 0);
        assert!(matches!(
            result.recording.as_ref(),
            Some(Recording::Stats {
                tool: "cli_glob",
                ..
            })
        ));
    });
}

#[test]
fn find_local_success_records_stats() {
    without_daemon(|| {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("needle.rs"), "fn main() {}\n").unwrap();
        let args = vec![
            "needle.rs".to_string(),
            directory.path().to_string_lossy().into_owned(),
        ];
        let called = Arc::new(AtomicBool::new(false));
        let result = observe_find(&args, marked_runner(Arc::clone(&called)));
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(result.route, ExecutionRoute::Local);
        assert_eq!(result.exit_code, 0);
        assert!(matches!(
            result.recording.as_ref(),
            Some(Recording::Stats {
                tool: "cli_find",
                ..
            })
        ));
    });
}

#[test]
fn ls_local_success_records_tree_tokens() {
    without_daemon(|| {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("sample.txt"), "sample\n").unwrap();
        let args = vec![
            directory.path().to_string_lossy().into_owned(),
            "--depth".to_string(),
            "1".to_string(),
        ];
        let called = Arc::new(AtomicBool::new(false));
        let result = observe_ls(&args, marked_runner(Arc::clone(&called)));
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(result.route, ExecutionRoute::Local);
        assert_eq!(result.exit_code, 0);
        assert!(matches!(
            result.recording.as_ref(),
            Some(Recording::Tree { .. })
        ));
    });
}

#[test]
fn deps_local_success_records_stats() {
    without_daemon(|| {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("package.json"),
            "{\"name\":\"fixture\",\"version\":\"1.0.0\",\"dependencies\":{\"serde\":\"1\"}}",
        )
        .unwrap();
        let args = vec![directory.path().to_string_lossy().into_owned()];
        let called = Arc::new(AtomicBool::new(false));
        let result = observe_deps(&args, marked_runner(Arc::clone(&called)));
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(result.route, ExecutionRoute::Local);
        assert_eq!(result.exit_code, 0);
        assert!(matches!(
            result.recording.as_ref(),
            Some(Recording::Stats {
                tool: "cli_deps",
                ..
            })
        ));
    });
}
