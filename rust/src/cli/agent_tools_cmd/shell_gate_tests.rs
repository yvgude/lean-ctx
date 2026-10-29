// SPDX-License-Identifier: Apache-2.0
//! Q13: an agent-tool shell call never runs before its guard is ready, and a
//! call that cannot get a guard never runs at all.

use serde_json::{Map, Value};

use super::tests::session;
use crate::shell::containment::tests::{
    caller_killed_before_its_guard, caller_standin_marker, external_fixture, run_external_fixture,
};

fn marking_call(marker: &std::path::Path) -> Map<String, Value> {
    Map::from_iter([
        (
            "argv".into(),
            serde_json::json!([
                "sh",
                "-c",
                format!("echo ran > '{}'; sleep 60", marker.display())
            ]),
        ),
        ("cwd".into(), Value::String(".".into())),
        ("env".into(), serde_json::json!({})),
        ("timeout_ms".into(), Value::from(20_000)),
    ])
}

#[test]
fn a_shell_call_never_runs_before_its_guard_is_ready() {
    run_external_fixture(module_path!(), "shell_call_caller_killed_fixture");
}

/// Body of the test above: the stand-in below is killed between "the call's
/// process exists" and "attach its guard".
#[test]
#[ignore = "fixture body of a_shell_call_never_runs_before_its_guard_is_ready"]
fn shell_call_caller_killed_fixture() {
    let fixture = external_fixture();
    caller_killed_before_its_guard(module_path!(), "shell_call_caller_standin");
    fixture.complete();
}

/// Stands in for an Engine in the middle of `call_shell`.
#[test]
#[ignore = "stand-in caller of shell_call_caller_killed_fixture"]
fn shell_call_caller_standin() {
    let marker = caller_standin_marker();
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), false, true);
    session.policy.allowed_executables = vec!["sh".to_string()];
    let _ = session.call_shell(&marking_call(&marker));
}

/// No guard, no call: the refusal is the fixed text, and the call's program
/// was never executed.
#[test]
fn a_shell_call_without_a_guard_is_refused_and_never_runs() {
    use crate::shell::containment::tests::{GuardFault, set_guard_fault_for_test};
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("ran");
    let mut session = session(root.path(), false, true);
    session.policy.allowed_executables = vec!["sh".to_string()];
    for fault in [GuardFault::CannotJoin, GuardFault::WrongGroup] {
        set_guard_fault_for_test(Some(fault));
        let refused = session.call_shell(&marking_call(&marker));
        set_guard_fault_for_test(None);
        let Err(error) = refused else {
            panic!("{fault:?}: a call without a guard is refused");
        };
        assert_eq!(error.message, "shell process could not be contained");
        assert!(!marker.exists(), "the refused call's program ran");
    }
}

/// G08: in a governed session a `call` frame is a receipted delivery — the
/// receipt binds the frame's bytes, the sidecar names the tool session, and
/// the observation exists because the frame was written. Where the receipt
/// cannot be stored, the result does not reach the wire: a `tool_error`
/// frame does.
#[test]
fn a_governed_session_call_is_receipted_as_its_frame_or_not_written() {
    use crate::core::context_admission::receipt_store::{self, observed, trace};
    const SESSION: &str = concat!(
        r#"{"id":"1","op":"hello","schema_version":1,"transport_version":1,"#,
        r#""agent_tools_interface_version":"1.0.0","sdk_version":"1.1.0"}"#,
        "\n",
        r#"{"id":"2","op":"call","tool":"ctx_read","#,
        r#""arguments":{"path":"sample.txt","mode":"full"}}"#,
        "\n",
    );
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config dir");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("config");
    let root = tempfile::tempdir().expect("project");
    let root = root.path().canonicalize().expect("canonical root");
    std::fs::write(root.join("sample.txt"), "session_marker alpha\nbeta\n").expect("source");
    let session = session(&root, false, false);
    let root = root.to_string_lossy().into_owned();
    let call_frame = |output: &[u8]| -> Vec<u8> {
        let frames: Vec<&[u8]> = output.split_inclusive(|byte| *byte == b'\n').collect();
        assert_eq!(frames.len(), 2, "hello and call");
        frames[1].to_vec()
    };

    let mut output = Vec::new();
    super::serve(&session, SESSION.as_bytes(), &mut output).expect("served");
    let frame = call_frame(&output);
    let answer: Value = serde_json::from_slice(&frame).expect("frame");
    assert_eq!(answer["ok"], true, "{answer}");
    let found = receipt_store::by_output(&root, &receipt_store::output_digest(&frame));
    assert_eq!(found.len(), 1, "one receipt binds the frame's bytes");
    let stored = found[0].1.as_ref().expect("a verified receipt");
    let trace::StoredTrace::Bound { sidecar, .. } = trace::load(&stored.digest) else {
        panic!("a mandatory receipt has its sidecar");
    };
    assert_eq!(sidecar.transport, trace::Transport::ToolSession);
    assert_eq!(
        sidecar.release_prepared.channel,
        trace::Channel::SessionFrame
    );
    assert!(matches!(
        observed::load(&stored.digest),
        observed::StoredObservation::Bound {
            observed: observed::Observed::ReturnedToTransport { .. },
            ..
        }
    ));
    // The hello frame is protocol data: no receipt.
    let hello = output.len() - frame.len();
    let hello = receipt_store::output_digest(&output[..hello]);
    assert!(receipt_store::by_output(&root, &hello).is_empty());

    // The store cannot persist: a regular file where its directory would be.
    let gateway = crate::core::data_dir::lean_ctx_data_dir()
        .expect("data dir")
        .join("gateway");
    std::fs::remove_dir_all(&gateway).expect("the store of the first call");
    std::fs::write(&gateway, b"not a directory").expect("blocked store");
    let mut output = Vec::new();
    super::serve(&session, SESSION.as_bytes(), &mut output).expect("served");
    let answer: Value = serde_json::from_slice(&call_frame(&output)).expect("frame");
    assert_eq!(answer["ok"], false, "{answer}");
    assert_eq!(answer["error"]["code"], "tool_error");
    assert!(
        answer["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(receipt_store::NOT_PERSISTED_CODE)),
        "{answer}"
    );
    assert!(!String::from_utf8_lossy(&output).contains("session_marker"));
}
