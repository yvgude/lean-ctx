// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx doctor --json` must write exactly one JSON document to stdout
//! (#2179): daemon diagnostics (systemd unit state, linger, crash-loop log)
//! used to be printed before the `{` and broke every JSON consumer.
use std::process::{Command, Stdio};

#[test]
fn doctor_json_stdout_is_a_single_json_document() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .current_dir(&root)
        .stdin(Stdio::null())
        .args(["doctor", "--json"])
        .env("LEAN_CTX_ACTIVE", "1")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env("DO_NOT_TRACK", "1")
        .env("NO_COLOR", "1")
        .env_remove("CLAUDECODE");
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
    // A crash-looping MCP server: the diagnostic that fires on every OS (the
    // systemd/linger lines only exist on Linux).
    std::fs::write(
        root.join("data").join(".mcp-server-starts.log"),
        "1\n2\n3\n4\n5\n6\n",
    )
    .unwrap();

    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON document ({error}):\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let checks = report["checks"].as_array().expect("checks array");
    assert!(
        checks.iter().any(|check| check["name"]
            .as_str()
            .is_some_and(|name| name.contains("Crash-loop log"))
            && check["scored"] == false),
        "the crash-loop diagnostic belongs in `checks`, unscored: {checks:?}"
    );
}
