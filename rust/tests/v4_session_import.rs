// SPDX-License-Identifier: Apache-2.0
//! Exercise the actual session tool and disk owner in isolated child processes.
use lean_ctx::core::session::{SessionState, TaskInfo};
use lean_ctx::tools::ctx_session::{self, SessionToolOptions};
use serde_json::{Value, json};
use std::{path::Path, process::Command};

fn options(path: Option<&str>) -> SessionToolOptions<'_> {
    SessionToolOptions {
        format: Some("json"),
        path,
        write: false,
        privacy: Some("redacted"),
        terse: None,
        agent_id: None,
    }
}

fn session(root: &Path, id: &str, task: &str) -> SessionState {
    let mut state = SessionState::new();
    state.id = id.into();
    state.project_root = Some(root.to_str().unwrap().into());
    state.task = Some(TaskInfo {
        description: task.into(),
        intent: None,
        progress_pct: Some(25),
    });
    state.save().unwrap();
    state
}

fn import(state: &mut SessionState, path: &Path) -> String {
    ctx_session::handle(state, &[], "import", None, None, options(path.to_str()))
}

fn write_bundle(path: &Path, bundle: &Value) {
    std::fs::write(path, serde_json::to_vec(bundle).unwrap()).unwrap();
}

fn assert_rejected(state: &mut SessionState, path: &Path) {
    let before = serde_json::to_value(&*state).unwrap();
    let result = import(state, path);
    assert!(
        !result.starts_with("CCP session bundle imported."),
        "{result}"
    );
    assert_eq!(serde_json::to_value(state).unwrap(), before);
}

#[test]
fn session_import_is_scoped_and_preserves_existing_archives() {
    if std::env::var_os("V4_SESSION_IMPORT_CHILD").is_none() {
        for scoped in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(fixture.path()).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .env_clear()
                .current_dir(&root)
                .args([
                    "--exact",
                    "session_import_is_scoped_and_preserves_existing_archives",
                    "--nocapture",
                ])
                .env("V4_SESSION_IMPORT_CHILD", "1")
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

    let root = std::env::current_dir().unwrap();
    let source_root = root.join("source");
    let target_root = root.join("target");
    for directory in [&source_root, &target_root] {
        std::fs::create_dir(directory).unwrap();
        std::fs::write(directory.join(".lean-ctx-id"), "session-import-component").unwrap();
    }
    let mut source = session(&source_root, "source-archive", "Keep source work");
    source.next_steps = vec!["Continue source work".into()];
    source.save().unwrap();
    let bundle: Value = serde_json::from_str(&ctx_session::handle(
        &mut source,
        &[],
        "export",
        None,
        None,
        options(None),
    ))
    .unwrap();
    let sessions = root.join("data/sessions");
    let source_archive = std::fs::read(sessions.join("source-archive.json")).unwrap();
    let mut target = session(&target_root, "target-archive", "Keep target work");
    let target_archive = std::fs::read(sessions.join("target-archive.json")).unwrap();
    let path = target_root.join("import.json");
    write_bundle(&path, &bundle);
    let original_bundle = std::fs::read(&path).unwrap();

    // Same explicit project identity in a different checkout remains portable.
    let result = import(&mut target, &path);
    assert!(
        result.starts_with("CCP session bundle imported."),
        "{result}"
    );
    assert_ne!(target.id, "source-archive");
    assert_ne!(target.id, "target-archive");
    assert_eq!(
        target.task.as_ref().unwrap().description,
        "Keep source work"
    );
    assert_eq!(target.next_steps, source.next_steps);
    assert_eq!(target.project_root.as_deref(), target_root.to_str());
    let loaded = SessionState::load_by_id(&target.id).unwrap();
    assert_eq!(loaded.task.unwrap().description, "Keep source work");
    let provenance: Value = serde_json::from_str(
        target
            .evidence
            .iter()
            .find(|entry| entry.key == "session_import_source")
            .unwrap()
            .value
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(provenance["source_session_id"], "source-archive");
    assert!(
        provenance["source_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(
        std::fs::read(sessions.join("source-archive.json")).unwrap(),
        source_archive
    );
    assert_eq!(
        std::fs::read(sessions.join("target-archive.json")).unwrap(),
        target_archive
    );
    assert_eq!(std::fs::read(&path).unwrap(), original_bundle);

    // A contradictory stable identity must not fall back to a matching root hash.
    let target_bundle: Value = serde_json::from_str(&ctx_session::handle(
        &mut target,
        &[],
        "export",
        None,
        None,
        options(None),
    ))
    .unwrap();
    let mut foreign = target_bundle.clone();
    foreign["project"]["project_identity_hash"] = json!("0".repeat(32));
    write_bundle(&path, &foreign);
    assert_rejected(&mut target, &path);
    foreign["project"]["project_identity_hash"] = Value::Null;
    foreign["project"]["project_root_hash"] = Value::Null;
    write_bundle(&path, &foreign);
    assert_rejected(&mut target, &path);

    std::fs::write(&path, vec![b' '; 250_001]).unwrap();
    assert_rejected(&mut target, &path);
    std::fs::write(&path, b"{invalid").unwrap();
    assert_rejected(&mut target, &path);
    assert_rejected(&mut target, &target_root);

    // The source ID is provenance only, never a path to overwrite.
    let sentinel = root.join("outside.json");
    std::fs::write(&sentinel, "do not overwrite").unwrap();
    let mut traversal = target_bundle.clone();
    traversal["session"]["id"] = json!("../../outside");
    traversal["session"]["shell_cwd"] = json!(root.join("outside"));
    write_bundle(&path, &traversal);
    let result = import(&mut target, &path);
    assert!(
        result.starts_with("CCP session bundle imported."),
        "{result}"
    );
    assert!(!target.id.contains('/'));
    assert!(target.shell_cwd.is_none());
    assert_eq!(target.effective_cwd(None), target_root.to_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "do not overwrite"
    );

    // Failure after primary publication must not claim success or swap live state.
    write_bundle(&path, &target_bundle);
    let pointer = sessions.join("latest.json");
    std::fs::rename(&pointer, root.join("saved-pointer.json")).unwrap();
    std::fs::create_dir(&pointer).unwrap();
    let before = serde_json::to_value(&target).unwrap();
    let failure = import(&mut target, &path);
    assert!(failure.starts_with("Import failed:"), "{failure}");
    assert!(failure.contains("recovery candidate:"));
    assert_eq!(serde_json::to_value(&target).unwrap(), before);
    assert_eq!(
        std::fs::read(sessions.join("source-archive.json")).unwrap(),
        source_archive
    );
    assert_eq!(
        std::fs::read(sessions.join("target-archive.json")).unwrap(),
        target_archive
    );

    let mut invalid = target.clone();
    invalid.id = "../forbidden-session".into();
    assert!(invalid.prepare_save().is_err());
    assert!(!root.join("data/forbidden-session.json").exists());
}
