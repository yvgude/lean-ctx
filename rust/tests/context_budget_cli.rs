// SPDX-License-Identifier: Apache-2.0

//! Exercise the real CLI fallback, without daemon or developer state.
use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

fn client(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .env("LEAN_CTX_ACTIVE", "1")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env("DO_NOT_TRACK", "1")
        .env_remove("CLAUDECODE")
        .env("LEAN_CTX_CONVERSATION_SCOPE", "0");
    for (name, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("LEAN_CTX_CONFIG_DIR", "config"),
        ("LEAN_CTX_DATA_DIR", "data"),
        ("LEAN_CTX_STATE_DIR", "state"),
        ("LEAN_CTX_CACHE_DIR", "cache"),
    ] {
        let path = root.join(directory);
        std::fs::create_dir_all(&path).unwrap();
        command.env(name, path);
    }
    command
}

fn successful(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn persisted_context_respects_numeric_budget_without_a_daemon() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let source = root.join("source.txt");
    std::fs::write(
        &source,
        "Context evidence has an actual token cost.\n".repeat(100),
    )
    .unwrap();
    // Seed the documented legacy persisted input, not an Engine read journey:
    // a one-shot `call ctx_read` does not persist the MCP post-dispatch ledger.
    let _isolated = client(&root);
    let ledger = serde_json::json!({
        "window_size":12000,"total_tokens_sent":1000,"total_tokens_saved":0,
        "entries":[{"path":source,"mode":"full","original_tokens":1000,
            "sent_tokens":1000,"timestamp":chrono::Utc::now().timestamp(),"phi":0.8}]
    });
    std::fs::write(
        root.join("state/context_ledger.json"),
        serde_json::to_vec(&ledger).unwrap(),
    )
    .unwrap();
    let pinned = successful(
        client(&root)
            .args(["control", "pin"])
            .arg(&source)
            .args(["--scope", "session"])
            .output()
            .unwrap(),
    );
    assert!(pinned.contains("[ctx_control] pinned"));
    let admitted = successful(
        client(&root)
            .args(["compile", "--mode", "compressed", "--budget", "10000"])
            .output()
            .unwrap(),
    );
    assert!(admitted.contains("Selected: 1 items"), "{admitted}");
    assert!(admitted.contains("[pinned]"), "{admitted}");
    for budget in ["0", "1"] {
        let compiled = successful(
            client(&root)
                .args(["compile", "--mode", "compressed", "--budget", budget])
                .output()
                .unwrap(),
        );
        assert!(
            compiled.contains(&format!("0/{budget} tokens")),
            "{compiled}"
        );
        assert!(
            compiled.contains("Selected: 0 items, Excluded: 1"),
            "{compiled}"
        );
        assert!(
            compiled.contains("budget cannot fit a declared pinned view"),
            "{compiled}"
        );
    }
    let equals = successful(
        client(&root)
            .args(["compile", "--mode", "compressed", "--budget=1"])
            .output()
            .unwrap(),
    );
    assert!(equals.contains("0/1 tokens"), "{equals}");
    let plan = successful(
        client(&root)
            .args(["plan", "source", "--budget=37"])
            .output()
            .unwrap(),
    );
    assert!(plan.contains("/37 tokens"), "{plan}");
}

#[test]
fn invalid_budgets_never_fall_back_to_a_default() {
    for operation in ["compile", "plan"] {
        for arguments in [
            vec!["--budget"],
            vec!["--budget", "--mode", "compressed"],
            vec!["--budget", "-1"],
            vec!["--budget", "not-a-budget"],
            vec!["--budget", "18446744073709551616"],
            vec!["--budget="],
            vec!["--budget=1", "--budget", "2"],
            vec!["--budget", "1", "--budget", "2"],
        ] {
            let directory = tempfile::tempdir().unwrap();
            let output = client(directory.path())
                .arg(operation)
                .args(arguments)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_budget:"));
            assert!(!directory.path().join("state/context_ledger.json").exists());
        }
    }
}
