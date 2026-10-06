#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::suspicious,
    clippy::nursery,
    unused
)]
//! Local MCP integration coverage for the decision-loop ingress-to-value path.

use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};

use super::{decision_loop_runtime::DecisionLoopRuntime, session::EvidenceKind};

fn request(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_owned()).with_arguments(
        arguments
            .as_object()
            .cloned()
            .expect("test tool arguments must be a JSON object"),
    )
}

async fn call(server: &crate::tools::LeanCtxServer, name: &str, arguments: Value) -> String {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        server.call_tool_guarded(request(name, arguments)),
    )
    .await
    .unwrap_or_else(|_| panic!("{name} MCP call must complete within 15 seconds"))
    .unwrap_or_else(|error| panic!("{name} MCP call must complete: {error}"));
    assert_ne!(
        result.is_error,
        Some(true),
        "{name} MCP call must return a successful outcome"
    );

    server
        .task_envelope
        .read()
        .await
        .as_ref()
        .map(|envelope| envelope.task_id.as_str().to_owned())
        .expect("guarded ingress must create and retain a task envelope")
}

async fn assert_completed_task(
    server: &crate::tools::LeanCtxServer,
    task_id: &str,
    expected_tool: &str,
) {
    let envelope = server.task_envelope.read().await.clone();
    let envelope = envelope.expect("MCP ingress must retain the task envelope");
    assert_eq!(envelope.task_id.as_str(), task_id);
    let task_uuid = task_id
        .strip_prefix("mcp-task-")
        .expect("ingress task id must use the mcp-task UUID format");
    uuid::Uuid::parse_str(task_uuid).expect("ingress task id must contain a valid UUID");
    assert!(
        envelope
            .intent
            .as_deref()
            .is_some_and(|intent| !intent.is_empty()),
        "triage must produce an intent-backed task profile"
    );
    assert!(
        envelope
            .task_class
            .as_deref()
            .is_some_and(|class| !class.is_empty()),
        "triage must enrich the ingress envelope with a task class"
    );

    let receipt_matches_task = server.session.read().await.evidence.iter().any(|receipt| {
        matches!(&receipt.kind, EvidenceKind::ToolCall)
            && receipt.tool.as_deref() == Some(expected_tool)
            && receipt.task_id.as_deref() == Some(task_id)
    });
    assert!(
        receipt_matches_task,
        "execution receipt for {expected_tool} must retain the ingress task id"
    );

    let runtime = DecisionLoopRuntime::get_or_init();
    let outcome = runtime
        .outcome_for(task_id)
        .expect("completed MCP call must record a canonical outcome");
    assert_eq!(outcome.accepted_outcome.task_id.as_str(), task_id);
    assert_eq!(
        outcome.accepted_outcome.accepted,
        lean_ctx_protocol::AcceptanceState::Unknown,
        "tool success without acceptance evidence must remain unknown"
    );
    assert!(outcome.assessment.is_none());
    assert!(runtime.assessment_for(task_id).is_none());
}

async fn server_with_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    crate::tools::LeanCtxServer,
) {
    let data_dir = tempfile::tempdir().expect("create isolated data directory");
    let project_dir = tempfile::tempdir().expect("create local project fixture");
    std::fs::write(
        project_dir.path().join("fixture.txt"),
        "decision loop fixture\n",
    )
    .expect("write local fixture");
    let root = project_dir.path().to_string_lossy().to_string();
    let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
    (data_dir, project_dir, server)
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decision_loop_integration_simple_read() {
    let _lock = crate::core::data_dir::test_env_lock();
    let (data_dir, project_dir, server) = server_with_fixture().await;
    // SAFETY: test_env_lock serializes process-wide data directory changes.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

    let task_id = call(
        &server,
        "ctx_read",
        json!({"path": project_dir.path().join("fixture.txt"), "mode": "full"}),
    )
    .await;
    assert_completed_task(&server, &task_id, "ctx_read").await;

    // #1484: `ctx_read` carries neither `query` nor `task`, so the ingress must
    // hand the triage no task text at all. Classifying the tool name instead
    // ("ctx_read: ctx_read") yields a confident SingleFile profile and filter
    // level 2 — the regression this binds shut.
    let session_id = server.session.read().await.id.clone();
    let profile = DecisionLoopRuntime::get_or_init()
        .profile_for_session(&session_id)
        .expect("a tool call must record a triage profile for its session");
    assert_eq!(
        profile.confidence_milli,
        crate::core::triage::confidence::RULES_FALLBACK_MILLI,
        "a call without a task text must reach rules::fallback(), not a classification"
    );
    assert_eq!(
        crate::server::context_gate::triage_filter_level(&profile),
        0,
        "a task-text-less profile must pass output through unfiltered"
    );

    // SAFETY: test_env_lock serializes process-wide data directory changes.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decision_loop_integration_shell() {
    let _lock = crate::core::data_dir::test_env_lock();
    let (data_dir, _project_dir, server) = server_with_fixture().await;
    // SAFETY: test_env_lock serializes process-wide data directory changes.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

    let task_id = call(
        &server,
        "ctx_shell",
        json!({"command": "printf decision-loop-shell", "workdir": "."}),
    )
    .await;
    assert_completed_task(&server, &task_id, "ctx_shell").await;

    // SAFETY: test_env_lock serializes process-wide data directory changes.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decision_loop_integration_multi_step_operation() {
    let _lock = crate::core::data_dir::test_env_lock();
    let (data_dir, project_dir, server) = server_with_fixture().await;
    // SAFETY: test_env_lock serializes process-wide data directory changes.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

    let first_task_id = call(
        &server,
        "ctx_read",
        json!({"path": project_dir.path().join("fixture.txt"), "mode": "full"}),
    )
    .await;
    assert_completed_task(&server, &first_task_id, "ctx_read").await;

    let shell_task_id = call(
        &server,
        "ctx_shell",
        json!({"command": "printf decision-loop-step", "workdir": "."}),
    )
    .await;
    assert_completed_task(&server, &shell_task_id, "ctx_shell").await;

    let final_task_id = call(
        &server,
        "ctx_read",
        json!({"path": project_dir.path().join("fixture.txt"), "mode": "full"}),
    )
    .await;
    assert_completed_task(&server, &final_task_id, "ctx_read").await;
    assert_ne!(first_task_id, shell_task_id);
    assert_ne!(shell_task_id, final_task_id);

    // SAFETY: test_env_lock serializes process-wide data directory changes.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
