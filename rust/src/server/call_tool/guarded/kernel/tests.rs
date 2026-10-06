// SPDX-License-Identifier: Apache-2.0

use super::super::{McpDriver, PreparedCallResult};
use super::*;
use crate::core::{
    context_kernel::autopilot::PlannerTier,
    data_dir::isolated_data_dir,
    execution_lifecycle::{
        ExecutionDriver, ExecutionLifecycle, LifecycleStage, ProductEntitlements, RuntimeContext,
        ToolRequest, ToolSurface,
    },
    knowledge::ProjectKnowledge,
    memory_policy::MemoryPolicy,
};

fn call(name: &str, args: serde_json::Value) -> PreparedMcpCall {
    PreparedMcpCall {
        name: name.to_owned(),
        args: match args {
            serde_json::Value::Object(arguments) => Some(arguments),
            _ => None,
        },
        minimal: true,
        config: Arc::new(crate::core::config::Config::default()),
        machine_readable: true,
        auto_context: None,
        throttle_warning: None,
        args_fp: String::new(),
        decision_context: None,
        kernel_handoff: Default::default(),
        cache_key: None,
    }
}

fn context(root: &str, name: &str) -> TaskContext {
    ExecutionLifecycle::default().begin(
        ToolRequest {
            tool_name: name.to_owned(),
            query: Some("quasar immutable".to_owned()),
            session_id: "kernel-handoff-test".to_owned(),
            agent_id: "kernel-handoff-test".to_owned(),
            surface: ToolSurface::Mcp,
            idempotency_key: None,
        },
        RuntimeContext {
            project_root: Some(root.to_owned()),
            ..Default::default()
        },
        ProductEntitlements {
            autopilot: true,
            personalized_learning: false,
        },
    )
}

fn remember(knowledge: &mut ProjectKnowledge, value: &str) {
    knowledge.remember(
        "architecture",
        "quasar",
        value,
        "kernel-handoff-test",
        1.0,
        &MemoryPolicy::default(),
    );
    knowledge.save().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_mcp_dispatch_consumes_the_pre_dispatch_snapshot() {
    // The actual ctx_call dispatcher enforces process-global role policy.
    // Other suite tests switch that policy; exercise it in an isolated process.
    if std::env::var_os("LEAN_CTX_KERNEL_SNAPSHOT_SUBPROCESS").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "server::call_tool::guarded::kernel::tests::real_mcp_dispatch_consumes_the_pre_dispatch_snapshot", "--nocapture"])
            .env("LEAN_CTX_KERNEL_SNAPSHOT_SUBPROCESS", "1")
            .env("LEAN_CTX_ROLE", "coder")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _data = isolated_data_dir();
    for (inner_name, mut args, wrapper) in [
        (
            "ctx_compose",
            serde_json::json!({"task":"quasar immutable", "task_aware":false}),
        ),
        (
            "ctx_search",
            serde_json::json!({"query":"quasar immutable"}),
        ),
        ("ctx_read", serde_json::json!({"paths":[], "mode":"full"})),
    ]
    .into_iter()
    .flat_map(|(name, args)| {
        ["direct", "nested", "flat"].map(|wrapper| (name, args.clone(), wrapper))
    }) {
        let directory = tempfile::tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(directory.path()).unwrap();
        let root = canonical_root.to_str().unwrap();
        let server = LeanCtxServer::new_with_project_root(Some(root));
        server
            .session
            .write()
            .await
            .set_task("quasar immutable", Some("investigation"));
        if inner_name == "ctx_read" {
            let first = canonical_root.join("first.rs");
            let second = canonical_root.join("second.rs");
            std::fs::write(&first, "fn quasar() {}\n").unwrap();
            std::fs::write(&second, "fn immutable() {}\n").unwrap();
            args["paths"] = serde_json::json!([first, second]);
        }
        let mut knowledge = ProjectKnowledge::new(root);
        remember(
            &mut knowledge,
            &format!("quasar immutable ORIGINAL snapshot {wrapper}"),
        );
        let context = context(root, inner_name);
        let name = match wrapper {
            "nested" => {
                args = serde_json::json!({"name":inner_name, "arguments":args});
                "ctx_call"
            }
            "flat" => {
                args["name"] = serde_json::json!(inner_name);
                "ctx_call"
            }
            _ => inner_name,
        };
        context.advance_through(LifecycleStage::GatherContextStrategy);
        let mut driver = McpDriver {
            server: &server,
            request: None,
            entitlements: None,
            prepared: Some(PreparedCallResult::Ready(Box::new(call(name, args)))),
            checkpoint: None,
            pending_cache_key: None,
            native_receipt_capture: None,
            native_receipt_metadata: None,
            input_tokens: 0,
        };
        assert_eq!(
            driver.ask_autopilot(&context).await.unwrap(),
            StageDisposition::Applied
        );
        let planned = context.autopilot_decision().unwrap();
        assert_eq!(
            planned.context_projection().task_id,
            context.envelope.task_id
        );
        assert_eq!(planned.decision().tier, PlannerTier::Community);
        assert!(
            planned
                .decision()
                .context_plan
                .selected
                .iter()
                .any(|entry| entry.reason.contains("ORIGINAL snapshot")),
            "planning must select the persisted fixture"
        );
        let identity = planned.canonical_bytes().unwrap();
        assert!(
            !context
                .completed_stages()
                .contains(&LifecycleStage::DispatchPrimitive)
        );
        // A second planner during dispatch would select REPLACED instead.
        remember(&mut knowledge, "quasar immutable REPLACED after admission");
        tokio::task::yield_now().await;
        let (primitive, _) = driver.dispatch_primitive(&context).await.unwrap();
        let (processed, _) = driver
            .reversible_post_process(&context, primitive)
            .await
            .unwrap();
        let output = serde_json::to_string(&processed.result).unwrap();
        assert!(output.contains("ORIGINAL snapshot"), "{name}: {output}");
        assert!(
            !output.contains("REPLACED after admission"),
            "{name}: {output}"
        );
        assert_eq!(
            output.matches("ORIGINAL snapshot").count(),
            1,
            "one supplement per task including batch reads"
        );
        assert_eq!(
            context
                .autopilot_decision()
                .unwrap()
                .canonical_bytes()
                .unwrap(),
            identity
        );
        assert!(
            context.outcome().is_none(),
            "planning and dispatch are not accepted evidence"
        );
    }
}

#[tokio::test]
async fn planning_failure_suppresses_fallback_without_claiming_application() {
    let _data = isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap();
    let server = LeanCtxServer::new_with_project_root(Some(root));
    let context = context(root, "ctx_compose");
    context.advance_through(LifecycleStage::GatherContextStrategy);
    let invalid = call(
        "ctx_compose",
        serde_json::json!({"task":"quasar", "path":42}),
    );
    let (disposition, handoff) = plan(&server, &invalid, &context).await;
    assert_eq!(
        disposition,
        StageDisposition::Skipped("kernel planning input unavailable")
    );
    assert!(matches!(handoff, KernelPlanningHandoff::Suppressed));
    assert!(context.autopilot_decision().is_none());

    let raw = call(
        "ctx_read",
        serde_json::json!({"paths":["first.rs"], "mode":"raw"}),
    );
    let (disposition, handoff) = plan(&server, &raw, &context).await;
    assert_eq!(
        disposition,
        StageDisposition::Skipped("kernel planning input unavailable")
    );
    assert!(matches!(handoff, KernelPlanningHandoff::Suppressed));
    assert!(context.autopilot_decision().is_none());

    std::fs::write(
        directory.path().join(".lean-ctx.toml"),
        "[kernel]\nmax_supplement_tokens = 0\n",
    )
    .unwrap();
    let ready = call("ctx_compose", serde_json::json!({"task":"quasar"}));
    let (disposition, handoff) = plan(&server, &ready, &context).await;
    assert_eq!(
        disposition,
        StageDisposition::Skipped("kernel planning unavailable")
    );
    assert!(matches!(handoff, KernelPlanningHandoff::Suppressed));
    assert!(context.autopilot_decision().is_none());
}

#[tokio::test]
async fn task_binding_is_once_only_and_rejects_wrong_lineage_and_late_attachment() {
    let _data = isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap();
    let context = context(root, "ctx_compose");
    let prepared = Arc::new(
        PreparedKernelContext::plan(
            context.envelope.task_id.clone(),
            "quasar".to_owned(),
            root.to_owned(),
            100,
            context.autopilot_task_class(),
            None,
        )
        .unwrap(),
    );
    assert!(context.attach_autopilot(prepared.clone()).is_err());
    context.advance_through(LifecycleStage::GatherContextStrategy);
    let wrong = Arc::new(
        PreparedKernelContext::plan(
            lean_ctx_protocol::TaskId::new("different-task").unwrap(),
            "quasar".to_owned(),
            root.to_owned(),
            100,
            context.autopilot_task_class(),
            None,
        )
        .unwrap(),
    );
    assert!(context.attach_autopilot(wrong).is_err());
    context.attach_autopilot(prepared.clone()).unwrap();
    assert!(context.attach_autopilot(prepared.clone()).is_err());
    context.advance(LifecycleStage::AskAutopilot);
    context.advance(LifecycleStage::DispatchPrimitive);
    assert!(context.attach_autopilot(prepared).is_err());
}

#[test]
fn semantic_whole_response_cache_cannot_bypass_context_planning() {
    for semantic in [
        serde_json::json!({"action":"semantic", "query":"quasar"}),
        serde_json::json!({"action":" SEARCH ", "query":"quasar"}),
        serde_json::json!({"query":"quasar"}),
        serde_json::json!({"action":"unknown", "query":"quasar"}),
    ] {
        assert!(
            super::super::response_cache_key("ctx_search", semantic.as_object(), "/root").is_none()
        );
    }
    let regex = serde_json::json!({"action":"regex", "pattern":"quasar"});
    assert!(super::super::response_cache_key("ctx_search", regex.as_object(), "/root").is_some());
}

#[tokio::test]
async fn batch_planning_uses_task_snapshot_not_later_session_task() {
    let _data = isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap();
    let server = LeanCtxServer::new_with_project_root(Some(root));
    let context = context(root, "ctx_read");
    server
        .session
        .write()
        .await
        .set_task("different later task", Some("investigation"));
    let batch = call("ctx_read", serde_json::json!({"paths":["first.rs"]}));
    let input = inputs(&server, &batch, &context).await.unwrap().unwrap();
    assert_eq!(input.query, "quasar immutable");
    let missing = call("ctx_compose", serde_json::json!({}));
    let (_, handoff) = plan(&server, &missing, &context).await;
    assert!(matches!(handoff, KernelPlanningHandoff::Suppressed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_lifecycle_replay_retains_original_plan_and_cached_result() {
    use super::super::McpEntitlements;
    // Role/policy registries are process-global and other suite tests mutate
    // them. Exercise actual admission in a fresh process with a normal role.
    if std::env::var_os("LEAN_CTX_KERNEL_REPLAY_SUBPROCESS").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "server::call_tool::guarded::kernel::tests::real_lifecycle_replay_retains_original_plan_and_cached_result", "--nocapture"])
            .env("LEAN_CTX_KERNEL_REPLAY_SUBPROCESS", "1")
            .env("LEAN_CTX_ROLE", "coder")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _data = isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let canonical_root = std::fs::canonicalize(directory.path()).unwrap();
    let root = canonical_root.to_str().unwrap();
    let server = LeanCtxServer::new_with_project_root(Some(root));
    let mut knowledge = ProjectKnowledge::new(root);
    remember(&mut knowledge, "quasar immutable ORIGINAL replay snapshot");
    let lifecycle = ExecutionLifecycle::default();
    let request = ToolRequest {
        tool_name: "ctx_compose".to_owned(),
        query: Some("quasar immutable".to_owned()),
        session_id: "replay-test".to_owned(),
        agent_id: "replay-test".to_owned(),
        surface: ToolSurface::Mcp,
        idempotency_key: Some("kernel-replay-key".to_owned()),
    };
    let runtime = RuntimeContext {
        project_root: Some(root.to_owned()),
        ..Default::default()
    };
    let entitlements = ProductEntitlements {
        autopilot: true,
        personalized_learning: false,
    };
    let captured = lifecycle.begin(request.clone(), runtime.clone(), entitlements);
    let driver = || {
        McpDriver { server: &server,
        request: Some(serde_json::from_value(serde_json::json!({"name":"ctx_compose", "arguments":{"task":"quasar immutable", "task_aware":false}})).unwrap()),
        entitlements: Some(McpEntitlements { config: Arc::new(crate::core::config::Config::default()) }),
        prepared: None, checkpoint: None, pending_cache_key: None,
        native_receipt_capture: None, native_receipt_metadata: None, input_tokens: 0 }
    };
    let first = lifecycle
        .run(request.clone(), runtime.clone(), entitlements, driver())
        .await
        .unwrap();
    let planned = captured.autopilot_decision().unwrap_or_else(|| {
        panic!(
            "actual lifecycle did not plan: {:?}; {}",
            captured.stage_executions(),
            serde_json::to_string(&first).unwrap()
        )
    });
    let before = planned.canonical_bytes().unwrap();
    assert_eq!(
        captured.stage_executions()[5].disposition,
        StageDisposition::Applied
    );
    remember(
        &mut knowledge,
        "quasar immutable REPLACED after original execution",
    );
    let (second, concurrent) = tokio::join!(
        lifecycle.run(request.clone(), runtime.clone(), entitlements, driver()),
        lifecycle.run(request.clone(), runtime.clone(), entitlements, driver()),
    );
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(second.unwrap()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(concurrent.unwrap()).unwrap()
    );
    assert_eq!(
        captured
            .autopilot_decision()
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        before
    );
    assert_eq!(
        captured.completed_stages(),
        crate::core::execution_lifecycle::LIFECYCLE_STAGE_ORDER
    );
    assert_eq!(
        captured
            .autopilot_planning_attempts()
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "replay admission must not invoke the planner again"
    );
    crate::core::roles::set_active_role("reviewer").unwrap();
    let denied = lifecycle
        .run(request, runtime, entitlements, driver())
        .await
        .unwrap();
    // Role denial is intentionally a soft MCP result; assert its content,
    // not the transport success bit or a fabricated acceptance outcome.
    let denied_text = serde_json::to_string(&denied).unwrap();
    assert!(denied_text.contains("ROLE DENIED"));
    assert!(!denied_text.contains("ORIGINAL replay snapshot"));
    assert_eq!(
        captured
            .autopilot_planning_attempts()
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "security revalidation must not gather or plan again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advertised_invokers_bind_inner_task_and_preserve_replay_identity() {
    if std::env::var_os("LEAN_CTX_INVOKER_SUBPROCESS").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "server::call_tool::guarded::kernel::tests::advertised_invokers_bind_inner_task_and_preserve_replay_identity", "--nocapture"])
            .env("LEAN_CTX_INVOKER_SUBPROCESS", "1")
            .env("LEAN_CTX_ROLE", "coder")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _data = isolated_data_dir();
    for (name, args) in [
        (
            "ctx_compose",
            serde_json::json!({"task":"quasar immutable", "task_aware":false}),
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_compose", "task":"outer decoy", "arguments":{"task":"quasar immutable", "task_aware":false}}),
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_compose", "task":"quasar immutable", "task_aware":false}),
        ),
        (
            "ctx",
            serde_json::json!({"tool":"compose", "task":"quasar immutable", "task_aware":false}),
        ),
        (
            "ctx",
            serde_json::json!({"tool":"call", "name":"ctx_compose", "arguments":{"task":"quasar immutable", "task_aware":false}}),
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(directory.path()).unwrap();
        let root = canonical.to_str().unwrap();
        let server = LeanCtxServer::new_with_project_root(Some(root));
        server
            .session
            .write()
            .await
            .set_task("session decoy", Some("investigation"));
        let mut knowledge = ProjectKnowledge::new(root);
        remember(&mut knowledge, "quasar immutable ORIGINAL invoker snapshot");
        let request: rmcp::model::CallToolRequestParams =
            serde_json::from_value(serde_json::json!({
                "name":name, "arguments":args, "_meta":{"idempotencyKey":"invoker-replay"}
            }))
            .unwrap();
        *server.agent_id.write().await = Some("invoker-test".into());
        let session_id = server.session.read().await.id.clone();
        let fingerprint = crate::core::loop_detection::LoopDetector::fingerprint(
            &serde_json::Value::Object(request.arguments.clone().unwrap()),
        );
        let captured = ExecutionLifecycle::global().begin(
            ToolRequest {
                tool_name: "ctx_compose".into(),
                query: Some("quasar immutable".into()),
                session_id: session_id.clone(),
                agent_id: "invoker-test".into(),
                surface: ToolSurface::Mcp,
                idempotency_key: Some(format!(
                    "mcp:{session_id}:invoker-test:{name}:{fingerprint}:invoker-replay"
                )),
            },
            RuntimeContext {
                client_name: Some(server.client_name.read().await.clone()),
                project_root: server.session.read().await.project_root.clone(),
            },
            ProductEntitlements {
                autopilot: true,
                personalized_learning: false,
            },
        );
        let first = server.call_tool_guarded(request.clone()).await.unwrap();
        let envelope = server
            .task_envelope
            .read()
            .await
            .clone()
            .expect("admitted task");
        assert_eq!(
            envelope.task_id, captured.envelope.task_id,
            "inner query/name must identify the same lifecycle run"
        );
        assert!(captured.autopilot_decision().is_some());
        let text = serde_json::to_string(&first).unwrap();
        assert!(text.contains("ORIGINAL invoker snapshot"), "{name}: {text}");
        remember(&mut knowledge, "quasar immutable REPLACED invoker snapshot");
        let replay = server.call_tool_guarded(request).await.unwrap();
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
        assert_eq!(
            server.task_envelope.read().await.as_ref().unwrap().task_id,
            envelope.task_id
        );
        assert_eq!(
            captured
                .autopilot_planning_attempts()
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    verify_invoker_boundary_parity().await;
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::ResolvedPolicy {
            name: "invoker-admission-test".into(),
            version: "1.0.0".into(),
            description: "inner admission parity".into(),
            chain: vec![],
            default_read_mode: None,
            allow_tools: None,
            deny_tools: vec!["ctx_compose".into()],
            max_context_tokens: None,
            audit_retention_days: None,
            redaction: Default::default(),
            filters: Default::default(),
            routing: Default::default(),
            budgets: Default::default(),
            egress: crate::core::policy::EgressRules {
                forbidden_patterns: vec!["blocked-fixture-value".into()],
                ..Default::default()
            },
        },
    ));
    let directory = tempfile::tempdir().unwrap();
    let server = LeanCtxServer::new_with_project_root(directory.path().to_str());
    for (name, args, reason) in [
        (
            "ctx_edit",
            serde_json::json!({"path":"untouched.rs", "new_string":"blocked-fixture-value"}),
            "egress denied",
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_edit", "path":"untouched.rs", "new_string":"blocked-fixture-value"}),
            "egress denied",
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_edit", "arguments":{"path":"untouched.rs", "new_string":"blocked-fixture-value"}}),
            "egress denied",
        ),
        (
            "ctx_compose",
            serde_json::json!({"task":"quasar"}),
            "role or policy denied",
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_compose", "arguments":{"task":"quasar"}}),
            "inner role or policy denied",
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_compose", "task":"quasar"}),
            "inner role or policy denied",
        ),
    ] {
        let request =
            serde_json::from_value(serde_json::json!({"name":name,"arguments":args})).unwrap();
        let prepared = server
            .prepare_tool_call(
                request,
                super::super::McpEntitlements {
                    config: Arc::new(crate::core::config::Config::default()),
                },
            )
            .await
            .unwrap();
        match prepared {
            PreparedCallResult::Terminal(_, actual) => assert_eq!(actual, reason),
            _ => panic!("{name} bypassed {reason}"),
        }
        assert!(
            server.task_envelope.read().await.is_none(),
            "denied before gathering/planning"
        );
    }
}

async fn verify_invoker_boundary_parity() {
    use crate::core::{config::LoopDetectionConfig, loop_detection::LoopDetector};
    let directory = tempfile::tempdir().unwrap();
    let server = LeanCtxServer::new_with_project_root(directory.path().to_str());
    let prepare = |name: &str, args: serde_json::Value| {
        let request =
            serde_json::from_value(serde_json::json!({"name":name,"arguments":args})).unwrap();
        server.prepare_tool_call(
            request,
            super::super::McpEntitlements {
                config: Arc::new(crate::core::config::Config::default()),
            },
        )
    };
    for (name, args) in [
        ("ctx_outline", serde_json::json!({"format":"json"})),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_outline", "arguments":{"format":"json"}}),
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_outline", "format":"json"}),
        ),
    ] {
        let PreparedCallResult::Ready(call) = prepare(name, args).await.unwrap() else {
            panic!("outline admission failed");
        };
        assert!(
            call.machine_readable,
            "JSON contract must use actual inner arguments"
        );
    }
    for args in [
        serde_json::json!({"name":"ctx_search", "arguments":42}),
        serde_json::json!({"name":"ctx_search", "args":{"query":"quasar"}}),
        serde_json::json!({"name":"ctx_call"}),
        serde_json::json!({"tool":"ctx_search"}),
    ] {
        let PreparedCallResult::Terminal(result, reason) = prepare("ctx_call", args).await.unwrap()
        else {
            panic!("invalid envelope admitted");
        };
        assert_eq!(reason, "invalid params");
        assert_eq!(
            result.is_error,
            Some(true),
            "argument errors remain soft MCP tool errors"
        );
    }
    *server.loop_detector.write().await = LoopDetector::with_config(&LoopDetectionConfig {
        blocked_threshold: 100,
        search_group_limit: 3,
        ..Default::default()
    });
    for (index, (name, args)) in [
        ("ctx_search", serde_json::json!({"query":"first"})),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_search", "arguments":{"query":"second"}}),
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_search", "query":"third"}),
        ),
        (
            "ctx_call",
            serde_json::json!({"name":"ctx_shell", "arguments":{"command":"rg fourth"}}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        match prepare(name, args).await.unwrap() {
            PreparedCallResult::Ready(_) => assert!(index < 3),
            PreparedCallResult::Terminal(_, reason) => {
                assert_eq!(index, 3);
                assert_eq!(reason, "loop throttle blocked");
            }
            PreparedCallResult::Cached(_) => panic!("unexpected cached fixture"),
        }
    }
    *server.loop_detector.write().await = LoopDetector::new();
    let _first = prepare("ctx_compose", serde_json::json!({})).await.unwrap();
    let PreparedCallResult::Ready(second) = prepare(
        "ctx_call",
        serde_json::json!({"name":"ctx_compose", "arguments":{}}),
    )
    .await
    .unwrap() else {
        panic!("valid envelope must reach inner argument validation");
    };
    let counts = server.loop_detector.read().await.stats();
    assert_eq!(
        counts.len(),
        1,
        "direct and wrapped fingerprints must share one key"
    );
    assert!(counts[0].0.starts_with("ctx_compose:"));
    assert_eq!(counts[0].1, 2);
    // Missing task is an actual inner-tool error; undo the same semantic key.
    crate::server::call_tool::pipeline::dispatch_primitive(
        &server,
        &second.name,
        second.args.as_ref(),
        second.minimal,
        &second.args_fp,
    )
    .await
    .unwrap();
    assert!(server.loop_detector.read().await.stats().is_empty());
}
