// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn protected_output_checks_text_copies_and_withholds_nontext() {
    let source = "name = 'sdk-output'\nversion = '1.0.0'\ndescription = 'test'\n[redaction]\ncustomer = 'CUS-[0-9]{4}'\n";
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::resolve(&crate::core::policy::parse(source).unwrap()).unwrap(),
    ));
    let output = ToolOutput {
        text: "CUS-1234".into(),
        content_blocks: Some(vec![rmcp::model::ContentBlock::text("CUS-5678")]),
        ..ToolOutput::simple(String::new())
    };
    let protected = protect_output("ctx_read", output).unwrap_or_else(|_| panic!("text allowed"));
    assert_eq!(protected.text, "[REDACTED:customer]");
    assert!(
        !serde_json::to_string(&protected.content_blocks)
            .unwrap()
            .contains("CUS-")
    );

    let image = serde_json::from_value(serde_json::json!({
        "type": "image", "data": "Q1VTLTEyMzQ=", "mimeType": "image/png",
    }))
    .unwrap();
    assert!(
        protect_output(
            "ctx_read",
            ToolOutput {
                content_blocks: Some(vec![image]),
                ..ToolOutput::simple(String::new())
            }
        )
        .is_err()
    );
}

#[test]
fn release_rechecks_tool_permission_and_parallel_metadata() {
    let source = "name = 'sdk-release'\nversion = '1.0.0'\ndescription = 'test'\n[context]\ndeny_tools = ['ctx_read']\n[redaction]\ncustomer = 'CUS-[0-9]{4}'\n";
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::resolve(&crate::core::policy::parse(source).unwrap()).unwrap(),
    ));
    assert!(protect_output("ctx_read", ToolOutput::simple(String::new())).is_err());
    assert!(
        protect_output(
            "ctx_tree",
            ToolOutput {
                mode: Some("CUS-1234".into()),
                ..ToolOutput::simple(String::new())
            }
        )
        .is_err()
    );
}

/// D9 / Q2: without any policy pack the Agent Tools wrapper returned every
/// output untouched. The gateway's release now runs here as well: clean
/// output keeps its bytes, developer mode delivers a redacted text with
/// the notice, governed mode with blocking secrets withholds.
#[test]
fn output_passes_the_gateway_release_without_a_policy_pack() {
    // Split so the repository's secret scanners never see a literal key.
    let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let raw = format!("aws_key = {key}\nrest of the output\n");
    let configured = |config: &str| {
        let data = crate::core::data_dir::isolated_data_dir();
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(dir.join("config.toml"), config).expect("config");
        data
    };
    {
        let _data = configured("");
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let clean = protect_output("ctx_shell", ToolOutput::simple("fn main() {}\n".into()))
            .unwrap_or_else(|_| panic!("clean output is delivered"));
        assert_eq!(
            clean.text, "fn main() {}\n",
            "allowed content keeps its bytes"
        );

        let redacted = protect_output("ctx_shell", ToolOutput::simple(raw.clone()))
            .unwrap_or_else(|_| panic!("developer mode delivers a redacted text"));
        assert!(!redacted.text.contains(key), "old code returned the key");
        assert!(redacted.text.contains("rest of the output"));
        assert!(
            redacted.text.contains("secret(s) redacted"),
            "{}",
            redacted.text
        );

        let block = ToolOutput {
            content_blocks: Some(vec![rmcp::model::ContentBlock::text(&raw)]),
            ..ToolOutput::simple("ok".into())
        };
        let released = protect_output("ctx_shell", block)
            .unwrap_or_else(|_| panic!("a text block is released like the text"));
        assert!(
            !serde_json::to_string(&released.content_blocks)
                .unwrap()
                .contains(key)
        );
        assert_eq!(released.text, "ok");
    }
    let _data = configured("[context_gateway]\nmode = \"governed\"\nsecrets = \"block\"\n");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let refused = protect_output("ctx_shell", ToolOutput::simple(raw));
    assert!(
        matches!(&refused, Err(error) if error.message == OUTPUT_WITHHELD),
        "governed mode without a pack withholds"
    );
    assert!(
        protect_output("ctx_shell", ToolOutput::simple("fn main() {}\n".into())).is_ok(),
        "control: clean output is still delivered in governed mode"
    );
}

fn policy(write: bool, exec: bool) -> PolicyV1 {
    PolicyV1 {
        schema_version: 1,
        allow_write: write,
        allow_exec: exec,
        allowed_executables: if exec {
            vec!["git".to_string()]
        } else {
            Vec::new()
        },
        allowed_env: Vec::new(),
        max_timeout_ms: 30_000,
        selected_gitlab: None,
    }
}

pub(super) fn session(root: &Path, write: bool, exec: bool) -> Session {
    let root = root.to_string_lossy().into_owned();
    let mut state = SessionState::new();
    state.project_root = Some(root.clone());
    state.shell_cwd = Some(root.clone());
    Session {
        root,
        policy: policy(write, exec),
        registry: build_registry(),
        cache: Arc::new(RwLock::new(SessionCache::new())),
        state: Arc::new(RwLock::new(state)),
        bm25_cache: Arc::new(Mutex::new(None)),
        autonomy: crate::core::autonomy::AutonomyState::new(),
        accepted_calls: std::sync::atomic::AtomicU32::new(0),
        source_authority: None,
        frame_call: Mutex::new(None),
    }
}

fn request(id: &str, op: &str) -> RequestV1 {
    RequestV1 {
        id: id.to_string(),
        op: op.to_string(),
        schema_version: None,
        transport_version: None,
        agent_tools_interface_version: None,
        sdk_version: None,
        tool: None,
        arguments: None,
    }
}

#[test]
fn permissions_are_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), false, false);
    assert_eq!(
        session.call("ctx_patch", &Map::new()).err().unwrap().code,
        "permission_denied"
    );
    assert_eq!(
        session.call("ctx_shell", &Map::new()).err().unwrap().code,
        "permission_denied"
    );
    assert_eq!(
        session
            .call("ctx_provider", &Map::new())
            .err()
            .unwrap()
            .code,
        "unsupported_capability"
    );
}

#[test]
fn selected_source_rejects_relaxed_and_removed_policy() {
    // Env before policy (the crate-wide lock order): the protected read
    // resolves config and project state that concurrent tests move.
    let _env = crate::core::data_dir::test_env_lock();
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".lean-ctx")).unwrap();
    let path = root.path().join(".lean-ctx/policy.toml");
    let rules = "name=\"source-test\"\nversion=\"1.0.0\"\ndescription=\"test\"\n[redaction]\ncustomer=\"CUS-[0-9]{4}\"\n";
    fs::write(&path, rules).unwrap();
    fs::write(root.path().join("note.txt"), "CUS-1234 useful context").unwrap();
    let mut session = session(root.path(), false, false);
    session.policy.selected_gitlab = Some(
        serde_json::from_value(serde_json::json!({
            "host":"gitlab.example.test", "project":5, "namespace":"group/project",
            "glab":"/usr/local/bin/glab"
        }))
        .unwrap(),
    );
    session.source_authority =
        Some(crate::core::policy::runtime::protected_policy_digest(root.path()).unwrap());
    let args = serde_json::json!({"path":"note.txt", "mode":"full"})
        .as_object()
        .unwrap()
        .clone();
    // The test pins authority without reading credentials or making a source request.
    let initial = session.call("ctx_read", &args);
    assert!(
        initial.is_ok(),
        "pinned selected-source read failed: {:?}",
        initial.err()
    );
    fs::write(
        &path,
        "name=\"relaxed\"\nversion=\"1.0.0\"\ndescription=\"test\"\n",
    )
    .unwrap();
    assert_eq!(
        session.call("ctx_read", &args).err().unwrap().code,
        "permission_denied"
    );
    fs::remove_file(&path).unwrap();
    assert_eq!(
        session.call("ctx_read", &args).err().unwrap().code,
        "permission_denied"
    );
    fs::write(&path, rules).unwrap();
    assert!(session.call("ctx_read", &args).is_ok());
}

#[test]
fn hello_is_required_and_close_is_explicit() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), false, false);
    let mut hello = false;
    let mut call = request("1", "call");
    call.tool = Some("ctx_tree".into());
    assert_eq!(
        handle_request(&session, call, &mut hello)
            .error
            .unwrap()
            .code,
        "invalid_state"
    );

    let mut greeting = request("2", "hello");
    greeting.schema_version = Some(1);
    greeting.transport_version = Some(1);
    greeting.agent_tools_interface_version = Some("1.0.0".into());
    greeting.sdk_version = Some("1.1.0".into());
    assert!(handle_request(&session, greeting, &mut hello).ok);
    assert!(handle_request(&session, request("3", "close"), &mut hello).ok);
}

#[test]
fn session_dispatches_real_tools_with_shared_cache() {
    // This Community cache fixture must not inherit another parallel test's
    // process-wide policy override while it exercises the real dispatcher.
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("sample.txt"), "alpha\nbeta\n").unwrap();
    let session = session(root.path(), false, false);
    let arguments = Map::from_iter([
        ("path".into(), Value::String("sample.txt".into())),
        ("mode".into(), Value::String("full".into())),
    ]);
    let first = session.call("ctx_read", &arguments).unwrap();
    let second = session.call("ctx_read", &arguments).unwrap();
    assert!(!first.text.is_empty(), "`first.text` is empty");
    assert!(!second.text.is_empty(), "`second.text` is empty");
    assert!(second.saved_tokens >= first.saved_tokens);
}

/// Agent Tools v1 is an SDK contract (`sdk/contracts/agent-tools-v1.json`):
/// a published SDK refuses a hello whose capabilities differ from it, so a
/// removed MCP tool must not shrink this set.
#[test]
fn capabilities_keep_the_agent_tools_v1_contract() {
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("lib.rs"),
        "pub fn ignite() -> u32 { 100 }\n",
    )
    .unwrap();
    let reader = session(root.path(), false, false);
    assert_eq!(
        reader.capabilities(),
        [
            "ctx_compose",
            "ctx_glob",
            "ctx_read",
            "ctx_search",
            "ctx_symbol",
            "ctx_tree"
        ]
    );
    let writer = session(root.path(), true, false);
    assert_eq!(
        writer.capabilities(),
        [
            "ctx_compose",
            "ctx_edit",
            "ctx_fill",
            "ctx_glob",
            "ctx_patch",
            "ctx_read",
            "ctx_search",
            "ctx_symbol",
            "ctx_tree"
        ]
    );
    let symbol = Map::from_iter([("name".into(), Value::String("ignite".into()))]);
    let found = reader.call("ctx_symbol", &symbol).err();
    assert!(
        found.is_none(),
        "ctx_symbol must run as a symbol search: {found:?}"
    );
    assert_eq!(
        writer.call("ctx_fill", &Map::new()).err().unwrap().code,
        "tool_error"
    );
}

#[test]
fn malformed_input_gets_a_framed_error() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), false, false);
    let mut output = Vec::new();
    serve(&session, "not-json\n".as_bytes(), &mut output).unwrap();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "invalid_request");
}

#[test]
fn oversized_frame_is_drained_before_the_next_request() {
    let mut input = vec![b'x'; MAX_REQUEST_BYTES + 1];
    input.extend_from_slice(b"\n{}\n");
    let mut reader = input.as_slice();
    assert!(matches!(
        read_frame(&mut reader).unwrap(),
        Some(InputFrame::TooLarge)
    ));
    let Some(InputFrame::Data(next)) = read_frame(&mut reader).unwrap() else {
        panic!("next frame missing");
    };
    assert_eq!(next, b"{}");
}

#[test]
fn shell_policy_accepts_only_structured_allowed_argv() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), false, true);
    let arguments = Map::from_iter([
        (
            "argv".into(),
            serde_json::json!(["git", "status", "--short"]),
        ),
        ("cwd".into(), Value::String(".".into())),
        ("env".into(), serde_json::json!({})),
        ("timeout_ms".into(), Value::from(1_000)),
    ]);
    let prepared = session.prepare_shell(&arguments).unwrap();
    assert_eq!(
        prepared["command"],
        crate::shell::platform::join_command(&[
            "git".to_string(),
            "status".to_string(),
            "--short".to_string(),
        ])
    );
    assert!(prepared.get("argv").is_none());

    let mut denied = arguments;
    denied.insert("argv".into(), serde_json::json!(["python", "-V"]));
    assert_eq!(
        session.prepare_shell(&denied).unwrap_err().code,
        "permission_denied"
    );
    for executable in ["/tmp/git", r"C:\tools\git"] {
        denied.insert("argv".into(), serde_json::json!([executable, "status"]));
        assert_eq!(
            session.prepare_shell(&denied).unwrap_err().code,
            "permission_denied"
        );
    }
}

#[test]
fn allowed_executable_resolves_to_absolute_path_outside_project() {
    let root = tempfile::tempdir().unwrap();
    let executable = resolve_allowed_executable("git", root.path().to_str().unwrap()).unwrap();
    assert!(executable.is_absolute());
    assert!(executable.is_file());
    assert!(!executable.starts_with(root.path()));
}

#[cfg(unix)]
#[test]
fn shell_child_cannot_consume_protocol_stdin() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), false, true);
    session.policy.allowed_executables = vec!["sh".to_string()];
    let arguments = Map::from_iter([
        (
            "argv".into(),
            serde_json::json!([
                "sh",
                "-c",
                "if read value; then exit 9; else printf closed; fi"
            ]),
        ),
        ("cwd".into(), Value::String(".".into())),
        ("env".into(), serde_json::json!({})),
        ("timeout_ms".into(), Value::from(1_000)),
    ]);
    let result = session.call_shell(&arguments).unwrap();
    assert_eq!(result.shell_outcome, Some(ShellOutcome::Exit(0)));
    assert!(result.text.contains("closed"));
}

/// Q13: a process-group end that the guard could not confirm does not
/// fail the call; the caller sees the one fixed notice as the first line.
/// The guard runs for real; only its reported outcome is replaced (test
/// seam in `shell::containment`, for this thread).
#[cfg(unix)]
#[test]
fn an_unconfirmed_group_end_is_the_first_line_and_the_call_succeeds() {
    use crate::shell::containment::{END_NOTICE, GuardOutcome, force_outcome_for_test};
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), false, true);
    session.policy.allowed_executables = vec!["sh".to_string()];
    // Runs long enough to still be running when the guard is attached.
    let arguments = Map::from_iter([
        (
            "argv".into(),
            serde_json::json!(["sh", "-c", "sleep 3; printf done"]),
        ),
        ("cwd".into(), Value::String(".".into())),
        ("env".into(), serde_json::json!({})),
        ("timeout_ms".into(), Value::from(20_000)),
    ]);
    force_outcome_for_test(Some(GuardOutcome::Unconfirmed));
    let result = session.call_shell(&arguments);
    force_outcome_for_test(None);
    let result = result.unwrap();
    assert_eq!(result.shell_outcome, Some(ShellOutcome::Exit(0)));
    assert_eq!(result.text.lines().next(), Some(END_NOTICE));
    assert!(result.text.contains("done"));

    // Control: a confirmed end adds nothing.
    let result = session.call_shell(&arguments).unwrap();
    assert!(!result.text.contains(END_NOTICE));
    assert!(result.text.contains("done"));
}

#[test]
fn foreground_shell_exit_is_explicit_for_sdk_clients() {
    assert_eq!(
        agent_shell_outcome(Some(&ShellOutcome::Exit(0))),
        Some(serde_json::json!({ "exitCode": 0 }))
    );
    assert_eq!(
        agent_shell_outcome(Some(&ShellOutcome::Exit(7))),
        Some(serde_json::json!({ "exitCode": 7 }))
    );
}

#[test]
fn loader_search_paths_are_forbidden_but_explicit_safe_env_names_remain_valid() {
    for name in [
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "ld_library_path",
        "dYlD_LiBrArY_PaTh",
    ] {
        assert!(!is_env_name(name), "{name} must not be passed to a child");
    }
    assert!(is_env_name("LANG"));
    assert!(is_env_name("APP_FEATURE_FLAG"));
}
