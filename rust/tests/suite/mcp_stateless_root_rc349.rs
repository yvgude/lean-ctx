// SPDX-License-Identifier: Apache-2.0

//! Stateless MCP (2026-07-28) binds the session's project root (rc3-49).
//!
//! Claude Code >= 2.1.330 never sends `initialize`: it opens with
//! `server/discover`, then `subscriptions/listen`, the list requests and tool
//! calls, each carrying the client's info in `_meta`. The root selection
//! lived only in `initialize`, so such a session kept no project root and
//! every project store (tasks, knowledge, rules) was keyed by the empty root,
//! shared by every folder without a project marker.
//!
//! This test drives the REAL binary over stdio with the request sequence
//! captured from Claude Code 2.1.330, in a folder without any project marker,
//! and asserts:
//!   1. `subscriptions/listen` is acknowledged, not answered with -32601,
//!   2. the task store the tool call opens is keyed by the folder Claude Code
//!      names (`CLAUDE_PROJECT_DIR`), not by the empty root.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const RESPONSE_DEADLINE: Duration = Duration::from_secs(20);

fn meta() -> serde_json::Value {
    serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
            "name": "claude-code",
            "title": "Claude Code",
            "version": "2.1.330"
        },
        "io.modelcontextprotocol/clientCapabilities": {
            "roots": { "listChanged": true },
            "elicitation": { "form": {}, "url": {} }
        }
    })
}

fn request(id: &serde_json::Value, method: &str, params: serde_json::Value) -> String {
    let mut params = params;
    params["_meta"] = meta();
    let msg = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    format!("{msg}\n")
}

#[test]
fn stateless_client_without_initialize_binds_its_project_root() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    // No marker: no .git, no manifest, no .lean-ctx.toml.
    let project = tmp.path().join("plain-folder");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("notes.txt"), "rc3-49\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .arg("mcp")
        .current_dir(&project)
        .env("HOME", &home)
        .env("LEAN_CTX_DATA_DIR", &data)
        .env("CODEX_HOME", home.join(".codex"))
        .env("LEAN_CTX_HEADLESS", "1")
        .env("CLAUDE_PROJECT_DIR", &project)
        .env_remove("LEAN_CTX_PROJECT_ROOT")
        .env_remove("CURSOR_PROJECT_DIR")
        .env_remove("WORKSPACE_FOLDER_PATHS")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("mcp server spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<serde_json::Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });

    let sequence = [
        request(
            &serde_json::json!("server-discover-probe-1"),
            "server/discover",
            serde_json::json!({}),
        ),
        request(
            &serde_json::json!("listen:0"),
            "subscriptions/listen",
            serde_json::json!({ "notifications": { "toolsListChanged": true } }),
        ),
        request(&serde_json::json!(0), "prompts/list", serde_json::json!({})),
        request(
            &serde_json::json!(1),
            "resources/list",
            serde_json::json!({}),
        ),
        request(&serde_json::json!(2), "tools/list", serde_json::json!({})),
        request(
            &serde_json::json!(3),
            "tools/call",
            serde_json::json!({ "name": "ctx_tasks", "arguments": { "action": "list" } }),
        ),
    ];
    for msg in &sequence {
        stdin.write_all(msg.as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    let deadline = Instant::now() + RESPONSE_DEADLINE;
    let mut seen = Vec::new();
    let mut call_done = false;
    while !call_done {
        let left = deadline.saturating_duration_since(Instant::now());
        let msg = rx
            .recv_timeout(left)
            .unwrap_or_else(|e| panic!("no tools/call reply within deadline ({e}); saw {seen:?}"));
        if msg["id"] == serde_json::json!(3) {
            call_done = true;
        }
        seen.push(msg);
    }

    let discover = seen
        .iter()
        .find(|m| m["id"] == serde_json::json!("server-discover-probe-1"))
        .expect("server/discover answered");
    assert!(
        discover.get("error").is_none(),
        "discover failed: {discover}"
    );
    let listen_refused = seen
        .iter()
        .any(|m| m["id"] == serde_json::json!("listen:0") && m["error"]["code"] == -32601);
    assert!(
        !listen_refused,
        "subscriptions/listen must be acknowledged; saw {seen:?}"
    );

    let tasks_dir = data.join("tasks");
    let stores: Vec<String> = std::fs::read_dir(&tasks_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let empty_root = lean_ctx::core::project_hash::hash_project_root("");
    let given = lean_ctx::core::project_hash::hash_project_root(&project.to_string_lossy());
    let canonical = lean_ctx::core::project_hash::hash_project_root(
        &project.canonicalize().unwrap().to_string_lossy(),
    );
    assert!(
        !stores.contains(&empty_root),
        "task store keyed by the empty root: {stores:?}"
    );
    assert!(
        stores.contains(&given) || stores.contains(&canonical),
        "task store must be keyed by the project folder ({given} / {canonical}); found {stores:?}"
    );

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}
