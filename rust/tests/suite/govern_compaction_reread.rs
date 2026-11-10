// SPDX-License-Identifier: Apache-2.0

//! GitHub #2147 in the governor profile: Claude's PreCompact/PostCompact go to
//! `hook govern` (not `hook observe`). After a compaction the model no longer
//! has the file, so the MCP server's next `ctx_read` must deliver the full
//! content again, never an "unchanged" stub pointing into the lost context.
//! Runs the real binary: `lean-ctx mcp` over stdio and `lean-ctx hook govern`
//! with the host's hook payload on stdin, in one isolated environment.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use super::mcp_process;
use super::read_observation_acceptance::{isolated_command, read_lines, response};

fn source() -> String {
    (0..60).fold(String::new(), |mut s, i| {
        s.push_str(&format!(
            "pub fn governed_{i}() -> usize {{ {i} * 7 + 3 }} // compaction re-read\n"
        ));
        s
    })
}

fn call_read(stdin: &mut impl Write, id: u64, path: &Path) {
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            // No mode: an explicit `full` always reads fresh and never stubs; the
            // default read of a small file delivers it whole and stubs a re-read.
            "params": {"name": "ctx_read", "arguments": {"path": path}}
        })
    )
    .unwrap();
}

fn text_of(result: &serde_json::Value) -> String {
    assert!(result["error"].is_null(), "{result}");
    assert_ne!(result["result"]["isError"], true, "{result}");
    result["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The host fires a hook event through the real `hook govern` entry point.
fn govern(root: &Path, payload: &serde_json::Value) {
    let mut child = isolated_command(root)
        .args(["hook", "govern"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "hook govern failed: {out:?}");
}

#[test]
fn reread_after_a_governed_compaction_delivers_full_content() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    for dir in ["state", "home", ".git"] {
        std::fs::create_dir(root.join(dir)).unwrap();
    }
    let body = source();
    let file = root.join("governed.rs");
    std::fs::write(&file, &body).unwrap();
    let marker_line = "pub fn governed_59() -> usize { 59 * 7 + 3 }";

    let mut child = isolated_command(root)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut process = mcp_process::ChildGuard::new(&mut child);
    let mut stdin = process.child_mut().stdin.take().unwrap();
    let (stdout, stdout_thread) = read_lines(process.child_mut().stdout.take().unwrap());
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "governed-compaction-test", "version": "1"}}
        })
    )
    .unwrap();
    assert!(response(&stdout, 1)["result"]["serverInfo"].is_object());
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();

    call_read(&mut stdin, 2, &file);
    let first = text_of(&response(&stdout, 2));
    assert!(first.contains(marker_line), "first read is full: {first}");

    // Precondition: an unchanged re-read inside the same context is a stub.
    call_read(&mut stdin, 3, &file);
    let again = text_of(&response(&stdout, 3));
    assert!(
        !again.contains(marker_line),
        "re-read before compaction is expected to be a stub: {again}"
    );

    // The host compacts: PreCompact, then PostCompact, both to `hook govern`.
    let session = "governed-compaction-session";
    govern(
        root,
        &serde_json::json!({"session_id": session, "hook_event_name": "PreCompact",
            "trigger": "auto", "custom_instructions": ""}),
    );
    govern(
        root,
        &serde_json::json!({"session_id": session, "hook_event_name": "PostCompact",
            "trigger": "auto"}),
    );

    call_read(&mut stdin, 4, &file);
    let after = text_of(&response(&stdout, 4));
    assert!(
        after.contains(marker_line),
        "re-read after a governed compaction must deliver the full content, not a stub: {after}"
    );

    drop(stdin);
    assert!(
        process
            .wait_for_exit(Duration::from_secs(5))
            .unwrap()
            .success()
    );
    mcp_process::join_bounded(stdout_thread, Duration::from_secs(2)).unwrap();
}
