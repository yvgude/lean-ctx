//! Fast-initialize contract (GH #669).
//!
//! VS Code's start-on-demand MCP lifecycle races the first tool call of a
//! fresh conversation against server startup (microsoft/vscode#321150). The
//! server-side mitigation: nothing that spawns processes or opens sockets may
//! run in front of the `initialize` handshake — housekeeping (orphan sweep,
//! proxy autostart, publish throttle) is deferred onto the blocking pool.
//!
//! Two guards, real binary over real stdio JSON-RPC in an isolated HOME:
//!   1. spawn → initialize-response stays under a conservative wall-clock
//!      bound (catches a future re-introduction of synchronous pre-serve work
//!      such as a crash-backoff sleep or a network wait),
//!   2. a tools/call fired IMMEDIATELY after the initialized notification —
//!      the exact VS Code race pattern — succeeds on the first attempt.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Generous for CI (debug binary, cold cache, shared runners) yet far below
/// the pathological regressions this guards against (30s crash-loop backoff,
/// TCP timeouts, N×ps orphan sweeps in front of the handshake).
const INITIALIZE_DEADLINE: Duration = Duration::from_secs(10);

struct TestEnv {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    data: std::path::PathBuf,
    project: std::path::PathBuf,
}

fn test_env() -> TestEnv {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::write(project.join("hello.txt"), "hello fast init\n").unwrap();
    TestEnv {
        _tmp: tmp,
        home,
        data,
        project,
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "HOME-override isolation is Unix-only (dirs::home_dir uses the Win32 API)"
)]
fn initialize_answers_fast_and_first_call_succeeds() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let env = test_env();

    let spawn_at = Instant::now();
    let mut child: Child = Command::new(bin)
        .arg("mcp")
        .current_dir(&env.project)
        .env("HOME", &env.home)
        .env("LEAN_CTX_DATA_DIR", &env.data)
        .env("CODEX_HOME", env.home.join(".codex"))
        .env("LEAN_CTX_HEADLESS", "1")
        // Root detection must derive from the temp project's cwd. When the
        // suite itself runs inside an IDE/agent session these carry the HOST
        // workspace and would hijack the project root (→ path-jail rejects
        // the temp file).
        .env_remove("LEAN_CTX_PROJECT_ROOT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("WORKSPACE_FOLDER_PATHS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("mcp server spawn");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let recv_response = |id: u64, deadline: Duration| -> serde_json::Value {
        let needle = format!("\"id\":{id}");
        let until = Instant::now() + deadline;
        loop {
            let remaining = until
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("timeout waiting for JSON-RPC response id={id}"));
            let line = rx
                .recv_timeout(remaining)
                .unwrap_or_else(|e| panic!("no response id={id} within deadline: {e}"));
            if line.contains(&needle) {
                return serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("invalid JSON-RPC line: {e}\n{line}"));
            }
        }
    };

    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "vscode", "version": "1.109.0" }
        }
    });
    writeln!(stdin, "{init}").expect("write initialize");
    let init_res = recv_response(1, INITIALIZE_DEADLINE);
    let elapsed = spawn_at.elapsed();
    assert!(
        init_res["result"]["serverInfo"]["name"].is_string(),
        "initialize must return serverInfo; got: {init_res}"
    );
    assert!(
        elapsed < INITIALIZE_DEADLINE,
        "spawn → initialize-response took {elapsed:?} (bound {INITIALIZE_DEADLINE:?}) — \
         synchronous pre-serve work crept back in front of the handshake (#669)"
    );

    // The VS Code race pattern: initialized notification and the first tool
    // call back-to-back, no grace period. It must succeed first try.
    writeln!(
        stdin,
        "{}",
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .expect("write initialized");
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "ctx_read",
            "arguments": { "path": env.project.join("hello.txt").to_string_lossy() }
        }
    });
    writeln!(stdin, "{call}").expect("write tools/call");
    let call_res = recv_response(2, Duration::from_secs(30));
    assert!(
        call_res["error"].is_null(),
        "first tools/call immediately after initialized must succeed; got: {call_res}"
    );
    let text = call_res["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("hello fast init"),
        "ctx_read must deliver the file on the first post-initialize call; got: {call_res}"
    );

    drop(stdin); // EOF → clean server shutdown
    let _ = child.wait();
    let _ = reader.join();
}
