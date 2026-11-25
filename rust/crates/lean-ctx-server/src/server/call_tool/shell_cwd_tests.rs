// SPDX-License-Identifier: Apache-2.0
//! #2262: Claude Code subagents share one MCP connection. A `cwd` argument or a
//! `cd` in one agent's ctx_shell call must not change where the next call runs,
//! nor what a relative path in ctx_read or ctx_patch names.

use rmcp::model::CallToolResult;
use serde_json::{Value, json};

use crate::tools::LeanCtxServer;

async fn call(server: &LeanCtxServer, tool: &str, args: Value) -> CallToolResult {
    super::dispatch_and_post_process(
        server,
        tool,
        args.as_object(),
        false,
        lean_ctx::core::config::Config::load_arc(),
        false,
        None,
        None,
        "shell-cwd-2262".to_string(),
        None,
    )
    .await
    .expect("a tool result, not a transport error")
}

fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A project root holding a Claude Code worktree (own `.git` file) nested at
/// `.claude/worktrees/wt`, with `FILE.txt` in both trees and a `sub` directory.
struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    wt: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = lean_ctx::core::pathutil::safe_canonicalize_or_self(dir.path());
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("FILE.txt"), "ROOT_COPY_2262\n").unwrap();
    let wt = root.join(".claude/worktrees/wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: ../../../.git/worktrees/wt\n").unwrap();
    std::fs::write(wt.join("FILE.txt"), "WORKTREE_COPY_2262\n").unwrap();
    Fixture {
        _dir: dir,
        root,
        wt,
    }
}

fn server_for(fixture: &Fixture) -> LeanCtxServer {
    LeanCtxServer::new_with_project_root(Some(&fixture.root.to_string_lossy()))
}

/// The directory `pwd` printed, as one whole output line.
fn ran_in(result: &CallToolResult, dir: &std::path::Path) -> bool {
    let want = dir.to_string_lossy();
    text(result).lines().any(|line| line.trim() == want)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_cwd_applies_to_its_own_call_only() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let f = fixture();
    let server = server_for(&f);
    let shell_cwd_before = server.session.read().await.shell_cwd.clone();

    let first = call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": f.wt.to_string_lossy()}),
    )
    .await;
    assert!(ran_in(&first, &f.wt), "{}", text(&first));

    let second = call(&server, "ctx_shell", json!({"command": "pwd"})).await;
    assert!(ran_in(&second, &f.root), "{}", text(&second));
    assert_eq!(server.session.read().await.shell_cwd, shell_cwd_before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cd_inside_a_command_applies_to_that_command_only() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let f = fixture();
    let server = server_for(&f);

    let first = call(&server, "ctx_shell", json!({"command": "cd sub && pwd"})).await;
    assert!(ran_in(&first, &f.root.join("sub")), "{}", text(&first));

    let second = call(&server, "ctx_shell", json!({"command": "pwd"})).await;
    assert!(ran_in(&second, &f.root), "{}", text(&second));

    // Through the same dispatch, a relative read after the `cd` still names
    // the project root, not `sub/`.
    let read = call(
        &server,
        "ctx_read",
        json!({"path": "FILE.txt", "mode": "full"}),
    )
    .await;
    assert!(text(&read).contains("ROOT_COPY_2262"), "{}", text(&read));
}

/// The batch forms resolve like a single call: `ctx_read` `paths[]` and
/// `ctx_patch` `ops[]` with relative paths name the project root after a
/// worktree `cwd`, never the worktree copy.
#[tokio::test(flavor = "multi_thread")]
async fn batch_read_and_patch_stay_in_the_project_root_after_a_worktree_cwd() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let f = fixture();
    let server = server_for(&f);
    std::fs::write(f.root.join("SECOND.txt"), "ROOT_SECOND_2262\n").unwrap();
    std::fs::write(f.wt.join("SECOND.txt"), "WORKTREE_SECOND_2262\n").unwrap();
    call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": f.wt.to_string_lossy()}),
    )
    .await;

    let read = call(
        &server,
        "ctx_read",
        json!({"paths": ["FILE.txt", "SECOND.txt"], "mode": "full"}),
    )
    .await;
    for want in ["ROOT_COPY_2262", "ROOT_SECOND_2262"] {
        assert!(text(&read).contains(want), "{want}: {}", text(&read));
    }
    for unwanted in ["WORKTREE_COPY_2262", "WORKTREE_SECOND_2262"] {
        assert!(
            !text(&read).contains(unwanted),
            "{unwanted}: {}",
            text(&read)
        );
    }

    let patch = call(
        &server,
        "ctx_patch",
        json!({"ops": [
            {"path": "FILE.txt", "op": "replace_unique",
             "old_text": "ROOT_COPY_2262", "new_text": "ROOT_PATCHED_2262"},
            {"path": "SECOND.txt", "op": "replace_unique",
             "old_text": "ROOT_SECOND_2262", "new_text": "ROOT_SECOND_PATCHED_2262"}
        ]}),
    )
    .await;
    assert_ne!(patch.is_error, Some(true), "{}", text(&patch));
    assert_eq!(
        std::fs::read_to_string(f.root.join("FILE.txt")).unwrap(),
        "ROOT_PATCHED_2262\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("SECOND.txt")).unwrap(),
        "ROOT_SECOND_PATCHED_2262\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.wt.join("FILE.txt")).unwrap(),
        "WORKTREE_COPY_2262\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.wt.join("SECOND.txt")).unwrap(),
        "WORKTREE_SECOND_2262\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn relative_read_and_patch_stay_in_the_project_root_after_a_worktree_cwd() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let f = fixture();
    let server = server_for(&f);
    call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": f.wt.to_string_lossy()}),
    )
    .await;

    let read = call(
        &server,
        "ctx_read",
        json!({"path": "FILE.txt", "mode": "full"}),
    )
    .await;
    assert!(text(&read).contains("ROOT_COPY_2262"), "{}", text(&read));
    assert!(
        !text(&read).contains("WORKTREE_COPY_2262"),
        "{}",
        text(&read)
    );

    let patch = call(
        &server,
        "ctx_patch",
        json!({"path": "FILE.txt", "op": "replace_unique",
               "old_text": "ROOT_COPY_2262", "new_text": "ROOT_PATCHED_2262"}),
    )
    .await;
    assert_ne!(patch.is_error, Some(true), "{}", text(&patch));
    assert_eq!(
        std::fs::read_to_string(f.root.join("FILE.txt")).unwrap(),
        "ROOT_PATCHED_2262\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.wt.join("FILE.txt")).unwrap(),
        "WORKTREE_COPY_2262\n"
    );
}

/// The #2262 repro table: after `cwd=<worktree>`, every way of naming a file
/// reaches the same file in ctx_read and ctx_patch — none is doubled.
#[tokio::test(flavor = "multi_thread")]
async fn read_and_patch_agree_on_every_path_of_the_repro_table() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let f = fixture();
    let server = server_for(&f);
    call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": f.wt.to_string_lossy()}),
    )
    .await;

    // One file per row, so no row reads a file an earlier row patched.
    let wt_abs = |name: &str| f.wt.join(name).to_string_lossy().to_string();
    let root_abs = |name: &str| f.root.join(name).to_string_lossy().to_string();
    let table = [
        (".claude/worktrees/wt/A.txt".to_string(), wt_abs("A.txt")),
        (wt_abs("B.txt"), wt_abs("B.txt")),
        ("C.txt".to_string(), root_abs("C.txt")),
        (root_abs("D.txt"), root_abs("D.txt")),
    ];
    for (path, file) in &table {
        std::fs::write(file, "MARKER_2262\n").unwrap();
        let read = call(&server, "ctx_read", json!({"path": path, "mode": "full"})).await;
        assert!(
            text(&read).contains("MARKER_2262"),
            "{path}: {}",
            text(&read)
        );
        assert!(
            !text(&read).contains("does not exist"),
            "{path}: {}",
            text(&read)
        );

        let patch = call(
            &server,
            "ctx_patch",
            json!({"path": path, "op": "replace_unique",
                   "old_text": "MARKER_2262", "new_text": "PATCHED_2262"}),
        )
        .await;
        assert_ne!(patch.is_error, Some(true), "{path}: {}", text(&patch));
        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            "PATCHED_2262\n",
            "{path}"
        );
    }
}

/// Two subagents on one connection, each with its own `cwd`, then a call
/// without one: it runs in the project root, not in either earlier cwd.
#[tokio::test(flavor = "multi_thread")]
async fn two_agents_with_different_cwds_leave_the_default_in_the_project_root() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let f = fixture();
    let server = server_for(&f);
    let sub = f.root.join("sub");

    let a = call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": f.wt.to_string_lossy()}),
    )
    .await;
    assert!(ran_in(&a, &f.wt), "{}", text(&a));
    let b = call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": sub.to_string_lossy()}),
    )
    .await;
    assert!(ran_in(&b, &sub), "{}", text(&b));

    let plain = call(&server, "ctx_shell", json!({"command": "pwd"})).await;
    assert!(ran_in(&plain, &f.root), "{}", text(&plain));
    assert_eq!(
        server.session.read().await.project_root.as_deref(),
        Some(&*f.root.to_string_lossy())
    );
}

/// A `cwd` in another checkout says, once and stably, that relative file-tool
/// paths still name the project root; a subdirectory or no `cwd` says nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_cwd_notes_where_relative_paths_resolve() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let f = fixture();
    let server = server_for(&f);
    let note = format!(
        "[note: relative paths in ctx_read/ctx_patch resolve to {}; use absolute paths for {}]",
        f.root.display(),
        f.wt.display()
    );

    let wt = call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": f.wt.to_string_lossy()}),
    )
    .await;
    assert_eq!(text(&wt).matches(&note).count(), 1, "{}", text(&wt));
    let sub = call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": f.root.join("sub").to_string_lossy()}),
    )
    .await;
    assert!(
        !text(&sub).contains("[note: relative paths"),
        "{}",
        text(&sub)
    );
    let plain = call(&server, "ctx_shell", json!({"command": "pwd"})).await;
    assert!(
        !text(&plain).contains("[note: relative paths"),
        "{}",
        text(&plain)
    );
}

/// A session without a project root learns it from its own start directory,
/// never from one call's `cwd` (another agent's checkout), and keeps it.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_root_is_learned_from_the_start_dir_not_a_calls_cwd() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let f = fixture();
    let server = server_for(&f);
    {
        let mut session = server.session.write().await;
        session.project_root = None;
        session.shell_cwd = Some(f.root.to_string_lossy().to_string());
    }
    let expected = lean_ctx::core::protocol::detect_project_root(&f.root.to_string_lossy());
    assert!(expected.is_some());
    // Another agent's checkout: a directory of a different repository that the
    // rootless jail admits (it lies under the process cwd, this crate).
    let other = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    assert_ne!(
        lean_ctx::core::protocol::detect_project_root(other),
        expected
    );

    // While the root is unknown the call runs in its own `cwd`, and the root
    // is learned from the start directory instead.
    let first = call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": other}),
    )
    .await;
    assert!(
        text(&first)
            .lines()
            .any(|line| line.trim().ends_with("/src")),
        "{}",
        text(&first)
    );
    assert_eq!(
        server.session.read().await.project_root,
        expected,
        "{}",
        text(&first)
    );

    // Once the root is known, the jail holds to it: the other checkout's `cwd`
    // is refused and the command does not run at all; the root is kept.
    let second = call(
        &server,
        "ctx_shell",
        json!({"command": "pwd", "cwd": other}),
    )
    .await;
    assert!(
        text(&second).contains("working directory rejected, command not run"),
        "{}",
        text(&second)
    );
    assert!(
        !text(&second)
            .lines()
            .any(|line| line.trim().ends_with("/src")),
        "{}",
        text(&second)
    );
    assert_eq!(
        server.session.read().await.project_root,
        expected,
        "{}",
        text(&second)
    );
}
