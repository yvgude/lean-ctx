// SPDX-License-Identifier: Apache-2.0

//! A successful file read is operational evidence, never task acceptance.
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::mcp_process;

const SOURCE: &str = "fn main() { println!(\"observed, not accepted\"); }\n";

fn isolated_command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("APPDATA", root.join("config"))
        .env("LOCALAPPDATA", root.join("data"))
        .env("DO_NOT_TRACK", "1")
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .env("LEAN_CTX_ACTIVE", "1")
        .env("LEAN_CTX_HEADLESS", "1")
        .env("LEAN_CTX_CONVERSATION_SCOPE", "0")
        .env("LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD", "0")
        .env("LEAN_CTX_PROJECT_ROOT", root)
        .current_dir(root);
    for suffix in ["DATA", "CONFIG", "STATE", "CACHE"] {
        let path = root.join(suffix.to_lowercase());
        command.env(format!("LEAN_CTX_{suffix}_DIR"), &path);
        command.env(format!("XDG_{suffix}_HOME"), path);
    }
    // Windows needs its OS root even in a clean child environment.
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    command
}

fn assert_observations(root: &Path, legacy: Option<&[u8]>) {
    let feedback = root.join("state/feedback.json");
    match legacy {
        Some(bytes) => assert_eq!(std::fs::read(feedback).unwrap(), bytes),
        None => assert!(!feedback.exists(), "read fabricated completion feedback"),
    }
    let heatmap: lean_ctx::core::heatmap::HeatMap =
        serde_json::from_slice(&std::fs::read(root.join("state/heatmap.json")).unwrap()).unwrap();
    let source = root.join("sample.rs").canonicalize().unwrap();
    let access = heatmap
        .entries
        .values()
        .find(|entry| {
            root.join(&entry.path)
                .canonicalize()
                .is_ok_and(|path| path == source)
        })
        .expect("fixture source access must be persisted");
    assert_eq!(access.access_count, 1, "one read must be accounted once");
    assert!(access.total_original_tokens > 0, "source tokens missing");

    let stats: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("data/mode_stats.json")).unwrap()).unwrap();
    let history: Vec<(
        lean_ctx::core::mode_predictor::FileSignature,
        Vec<lean_ctx::core::mode_predictor::ModeOutcome>,
    )> = serde_json::from_value(stats["history"].clone()).unwrap();
    assert!(
        history.iter().any(|(signature, outcomes)| {
            signature.ext == "rs"
                && outcomes.iter().any(|outcome| {
                    outcome.mode == "full"
                        && outcome.tokens_in > 0
                        && outcome.tokens_out > 0
                        && outcome.density.is_finite()
                })
        }),
        "real full-read observation missing"
    );
}

#[test]
fn cli_read_preserves_legacy_feedback_without_fabricating_acceptance() {
    for legacy in [
        None,
        Some(b"{legacy feedback retained verbatim}".as_slice()),
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let feedback = state.join("feedback.json");
        if let Some(bytes) = legacy {
            std::fs::write(&feedback, bytes).unwrap();
        }
        std::fs::write(fixture.path().join("sample.rs"), SOURCE).unwrap();
        let output = isolated_command(fixture.path())
            .args(["read", "sample.rs", "--mode", "full"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains(SOURCE.trim()));
        assert_observations(fixture.path(), legacy);
    }
}

fn read_lines(
    stream: impl std::io::Read + Send + 'static,
) -> (Receiver<String>, std::thread::JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            if sender.send(line.expect("child output line")).is_err() {
                break;
            }
        }
    });
    (receiver, thread)
}

fn wait_line(receiver: &Receiver<String>, matches: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = receiver
            .recv_timeout(remaining)
            .expect("child response deadline");
        if matches(&line) {
            return line;
        }
    }
}

fn response(receiver: &Receiver<String>, id: u64) -> serde_json::Value {
    let line = wait_line(receiver, |line| {
        serde_json::from_str::<serde_json::Value>(line).is_ok_and(|message| message["id"] == id)
    });
    serde_json::from_str(&line).unwrap()
}

#[test]
fn mcp_read_worker_preserves_legacy_feedback_without_fabricating_acceptance() {
    for legacy in [
        None,
        Some(b"{legacy feedback retained verbatim}".as_slice()),
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir(root.join("state")).unwrap();
        std::fs::create_dir(root.join("home")).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join("sample.rs"), SOURCE).unwrap();
        if let Some(bytes) = legacy {
            std::fs::write(root.join("state/feedback.json"), bytes).unwrap();
        }
        let mut child = isolated_command(root)
            .env("LEAN_CTX_LOG", "lean_ctx::read_observation=debug")
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut process = mcp_process::ChildGuard::new(&mut child);
        let mut stdin = process.child_mut().stdin.take().unwrap();
        let (stdout, stdout_thread) = read_lines(process.child_mut().stdout.take().unwrap());
        let (stderr, stderr_thread) = read_lines(process.child_mut().stderr.take().unwrap());
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": {"name": "read-observation-test", "version": "1"}}
            })
        )
        .unwrap();
        assert!(response(&stdout, 1)["result"]["serverInfo"].is_object());
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0", "method": "notifications/initialized"
            })
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "ctx_read", "arguments": {
                    "path": root.join("sample.rs"), "mode": "full"}}
            })
        )
        .unwrap();
        let result = response(&stdout, 2);
        assert!(result["error"].is_null(), "{result}");
        assert_ne!(result["result"]["isError"], true, "{result}");
        assert!(
            result["result"]["content"]
                .as_array()
                .unwrap()
                .iter()
                .any(|part| {
                    part["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(SOURCE.trim()))
                }),
            "{result}"
        );
        // Unlike a response or a timed sleep, this event is emitted only after
        // the real detached worker's entire non-panicking closure has returned.
        wait_line(&stderr, |line| {
            line.contains("read_observation_worker_completed")
        });
        assert_observations(root, legacy);
        drop(stdin);
        assert!(
            process
                .wait_for_exit(Duration::from_secs(5))
                .unwrap()
                .success()
        );
        mcp_process::join_bounded(stdout_thread, Duration::from_secs(2)).unwrap();
        mcp_process::join_bounded(stderr_thread, Duration::from_secs(2)).unwrap();
    }
}
