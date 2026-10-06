// SPDX-License-Identifier: Apache-2.0
//! Receiving-project admission for the real session handler and disk store.
use lean_ctx::core::session::{SessionState, TaskInfo};
use lean_ctx::tools::ctx_session::{self, SessionToolOptions};
use std::{path::Path, process::Command};

fn load(state: &mut SessionState, id: Option<&str>) -> String {
    ctx_session::handle(
        state,
        &[],
        "load",
        None,
        id,
        SessionToolOptions {
            format: None,
            path: None,
            write: false,
            privacy: None,
            terse: None,
            agent_id: None,
        },
    )
}

fn session(root: &Path, id: &str) -> SessionState {
    let mut state = SessionState::new();
    state.id = id.into();
    state.project_root = Some(root.to_str().unwrap().into());
    state.task = Some(TaskInfo {
        description: format!("Private task for {id}"),
        intent: None,
        progress_pct: Some(25),
    });
    state
}

fn oneshot_status(root: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(root)
        .args(["call", "ctx_session", "--project-root"])
        .arg(root)
        .args(["--json", r#"{"action":"status"}"#])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn rejected(state: &mut SessionState, id: Option<&str>) {
    let before = serde_json::to_value(&*state).unwrap();
    let result = load(state, id);
    assert!(result.starts_with("ERROR:"), "{result}");
    assert!(!result.contains("Private task"), "{result}");
    assert_eq!(serde_json::to_value(state).unwrap(), before);
}

#[test]
fn session_load_uses_receiver_scope_without_expanding_authority() {
    if std::env::var_os("V4_SESSION_LOAD_CHILD").is_none() {
        for scoped in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(fixture.path()).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .env_clear()
                .current_dir(&root)
                .args([
                    "--exact",
                    "session_load_uses_receiver_scope_without_expanding_authority",
                    "--nocapture",
                ])
                .env("V4_SESSION_LOAD_CHILD", "1")
                .env("LEAN_CTX_ACTIVE", "1")
                .env("__LEAN_CTX_SKIP_EVENTS", "1")
                .env("__LEAN_CTX_NO_DAEMON", "1")
                .env("DO_NOT_TRACK", "1")
                .env("LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD", "0")
                .env(
                    "LEAN_CTX_CONVERSATION_SCOPE",
                    if scoped { "1" } else { "0" },
                );
            if scoped {
                command.env("CLAUDECODE", "1");
            }
            #[cfg(not(windows))]
            command.env("PATH", "/usr/bin:/bin");
            #[cfg(windows)]
            {
                let system = std::env::var_os("SystemRoot").unwrap();
                command
                    .env("SystemRoot", &system)
                    .env("PATH", std::path::PathBuf::from(system).join("System32"));
            }
            for (name, part) in [
                ("HOME", "home"),
                ("USERPROFILE", "home"),
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_DATA_HOME", "data"),
                ("XDG_STATE_HOME", "state"),
                ("XDG_CACHE_HOME", "cache"),
                ("LEAN_CTX_CONFIG_DIR", "config"),
                ("LEAN_CTX_DATA_DIR", "data"),
                ("LEAN_CTX_STATE_DIR", "state"),
                ("LEAN_CTX_CACHE_DIR", "cache"),
            ] {
                let directory = root.join(part);
                std::fs::create_dir_all(&directory).unwrap();
                command.env(name, directory);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "scope={scoped}\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }

    let fixture = std::env::current_dir().unwrap();
    let project = fixture.join("receiver");
    let foreign = fixture.join("foreign");
    let legacy = fixture.join(".codex/old-agent");
    let trusted_extra = fixture.join("trusted-extra");
    for directory in [&project, &foreign, &legacy, &trusted_extra] {
        std::fs::create_dir_all(directory).unwrap();
    }
    for directory in [&project, &foreign] {
        std::fs::write(directory.join(".lean-ctx-id"), "same-logical-project").unwrap();
    }
    let mut saved = session(&project, "same-root");
    saved.shell_cwd = Some(foreign.to_str().unwrap().into());
    saved.extra_roots = vec![foreign.to_str().unwrap().into()];
    saved.next_steps = vec!["Continue saved work".into()];
    saved.save().unwrap();
    let mut outsider = session(&foreign, "foreign-root");
    outsider.save().unwrap();
    let mut receiver = session(&project, "receiving-session");
    receiver.shell_cwd = Some(project.to_str().unwrap().into());
    receiver.extra_roots = vec![trusted_extra.to_str().unwrap().into()];
    let extra = receiver.extra_roots.clone();
    let cwd = receiver.shell_cwd.clone();
    let archive = fixture.join("data/sessions/same-root.json");
    let archive_before = std::fs::read(&archive).unwrap();

    let result = load(&mut receiver, Some("same-root"));
    assert!(result.starts_with("Session loaded"), "{result}");
    assert_eq!(receiver.id, saved.id);
    assert_eq!(receiver.next_steps, saved.next_steps);
    assert_eq!(receiver.project_root, saved.project_root);
    assert_eq!(receiver.extra_roots, extra);
    assert_eq!(receiver.shell_cwd, cwd);
    assert_eq!(std::fs::read(&archive).unwrap(), archive_before);

    // Identical logical project IDs are not authority to replace the live root.
    rejected(&mut receiver, Some("foreign-root"));
    // Neither a legacy cwd heuristic nor load-time temp-root repair grants scope.
    outsider.project_root = Some(legacy.to_str().unwrap().into());
    outsider.shell_cwd = Some(project.to_str().unwrap().into());
    outsider.id = "legacy-foreign".into();
    // Seed historical/untrusted metadata: today's writer already rejects agent roots.
    std::fs::write(
        fixture.join("data/sessions/legacy-foreign.json"),
        serde_json::to_vec(&outsider).unwrap(),
    )
    .unwrap();
    rejected(&mut receiver, Some("legacy-foreign"));

    // Missing IDs, malformed storage and traversal attempts never clobber state.
    rejected(&mut receiver, Some("missing"));
    rejected(&mut receiver, Some("../../outside"));
    std::fs::write(fixture.join("data/sessions/corrupt.json"), "{invalid").unwrap();
    rejected(&mut receiver, Some("corrupt"));
    for root in [
        None,
        Some(""),
        Some("relative"),
        Some(fixture.ancestors().last().unwrap().to_str().unwrap()),
    ] {
        let mut unsafe_receiver = receiver.clone();
        unsafe_receiver.project_root = root.map(str::to_owned);
        rejected(&mut unsafe_receiver, Some("same-root"));
        rejected(&mut unsafe_receiver, None);
    }

    // Save a valid latest project entry after the deliberately malformed candidate.
    saved.save().unwrap();
    let mut latest = session(&project, "fresh-process-view");
    latest.extra_roots.clone_from(&extra);
    // Process cwd is fixture, deliberately not the receiving project's root.
    let result = load(&mut latest, None);
    assert!(result.starts_with("Session loaded"), "{result}");
    assert_eq!(latest.id, saved.id);
    assert_eq!(latest.extra_roots, extra);
    assert!(latest.shell_cwd.is_none());
    assert_eq!(std::fs::read(&archive).unwrap(), archive_before);
    assert!(oneshot_status(&project).contains("Private task for same-root"));

    // Ordinary reroot/save leaves the old index behind; it must not admit B into A.
    let mut rerouted = session(&project, "rerouted-session");
    rerouted.save().unwrap();
    rerouted.project_root = Some(foreign.to_str().unwrap().into());
    rerouted.shell_cwd = Some(foreign.to_str().unwrap().into());
    rerouted.save().unwrap();
    let output = oneshot_status(&project);
    assert!(!output.contains("Private task"), "{output}");

    // Same key as the product (session/persistence.rs): the normalized root,
    // which drops Windows' `\\?\` prefix that fs::canonicalize adds.
    let key_root = lean_ctx::core::pathutil::safe_canonicalize_or_self(&project);
    let index_path = fixture.join("data/sessions/project-index").join(format!(
        "{}.json",
        blake3::hash(key_root.to_string_lossy().as_bytes()).to_hex()
    ));
    let mut index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
    index["session_ids"] = serde_json::json!(["legacy-foreign"]);
    std::fs::write(index_path, serde_json::to_vec(&index).unwrap()).unwrap();
    let output = oneshot_status(&project);
    assert!(!output.contains("Private task"), "{output}");
    println!(
        "receiver-scoped explicit/latest load, 13 fail-closed attempts, authority preservation: PASS"
    );
}
