// SPDX-License-Identifier: Apache-2.0
use super::{
    CallToolResult, ContentBlock, Duration, Meta, PreparedCallResult, ResponseCache, Value,
    cache_call_result, cached_call_result, response_cache_key,
};

#[test]
fn response_cache_cannot_reuse_a_result_under_changed_rules() {
    use crate::core::policy::{parse, resolve, runtime::TestPolicyOverride};
    let cache = ResponseCache::new(16, Duration::from_mins(1));
    let policy = |prefix: &str| {
        resolve(&parse(&format!(
        "name = 'cache'\nversion = '1.0.0'\ndescription = 'test'\n[redaction]\ncustomer = '{prefix}-[0-9]{{4}}'\n"
    )).unwrap()).unwrap()
    };
    let key = {
        let _policy = TestPolicyOverride::set(Some(policy("CUS")));
        response_cache_key("ctx_tree", None, "/fixture").unwrap()
    };
    cache_call_result(
        &cache,
        key.clone(),
        &CallToolResult::success(vec![ContentBlock::text("ACC-1234")]),
    );
    assert!(cached_call_result(&cache, &key).is_some());
    let _policy = TestPolicyOverride::set(Some(policy("ACC")));
    let changed = response_cache_key("ctx_tree", None, "/fixture").unwrap();
    assert!(cached_call_result(&cache, &changed).is_none());
}

#[tokio::test]
async fn policy_revoked_after_preparation_prevents_primitive_dispatch() {
    use crate::core::execution_lifecycle::{
        ExecutionDriver, ExecutionLifecycle, ProductEntitlements, RuntimeContext, ToolRequest,
        ToolSurface,
    };
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let policy = root.path().join(".lean-ctx/policy.toml");
    std::fs::create_dir_all(policy.parent().unwrap()).unwrap();
    let base = "name = 'revoke'\nversion = '1.0.0'\ndescription = 'test'\n";
    std::fs::write(&policy, base).unwrap();
    let source = root.path().join("sample.txt");
    std::fs::write(&source, "CUS-1234").unwrap();
    let server = crate::tools::LeanCtxServer::new_with_project_root(root.path().to_str());
    crate::core::policy::runtime::REQUEST_PROJECT
        .scope(std::cell::RefCell::new(Some(root.path().into())), async {
            let request = serde_json::from_value(serde_json::json!({"name":"ctx_read",
                "arguments":{"path":source,"mode":"full"}}))
            .unwrap();
            let prepared = server
                .prepare_tool_call(
                    request,
                    super::McpEntitlements {
                        config: std::sync::Arc::new(crate::core::config::Config::default()),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(prepared, PreparedCallResult::Ready(_)));
            let lifecycle = ExecutionLifecycle::default();
            let context = lifecycle.begin(
                ToolRequest {
                    tool_name: "ctx_read".into(),
                    query: None,
                    session_id: "revoke".into(),
                    agent_id: "revoke".into(),
                    surface: ToolSurface::Mcp,
                    idempotency_key: None,
                },
                RuntimeContext {
                    project_root: root.path().to_str().map(str::to_owned),
                    ..RuntimeContext::default()
                },
                ProductEntitlements {
                    autopilot: false,
                    personalized_learning: false,
                },
            );
            let mut driver = super::McpDriver {
                server: &server,
                request: None,
                entitlements: None,
                prepared: Some(prepared),
                checkpoint: None,
                pending_cache_key: None,
                native_receipt_capture: None,
                native_receipt_metadata: None,
                input_tokens: 0,
            };
            tokio::task::yield_now().await;
            std::fs::write(
                &policy,
                format!("{base}[context]\ndeny_tools = ['ctx_read']\n"),
            )
            .unwrap();
            let (primitive, _) = driver.dispatch_primitive(&context).await.unwrap();
            let super::McpDispatched::Terminal(result, reason) = primitive else {
                panic!("revoked request must never dispatch the primitive");
            };
            assert_eq!(reason, "policy changed before dispatch");
            assert!(!serde_json::to_string(&result).unwrap().contains("CUS-1234"));
        })
        .await;
}

#[tokio::test]
async fn mcp_context_attachment_is_not_an_autopilot_decision() {
    use crate::core::execution_lifecycle::{
        ExecutionDriver, ExecutionLifecycle, ProductEntitlements, RuntimeContext, StageDisposition,
        ToolRequest, ToolSurface,
    };

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap();
    let server = crate::tools::LeanCtxServer::new_with_project_root(Some(root));
    let lifecycle = ExecutionLifecycle::default();
    for enabled in [false, true] {
        let context = lifecycle.begin(
            ToolRequest {
                tool_name: "ctx_read".to_owned(),
                query: Some("inspect lifecycle".to_owned()),
                session_id: "autopilot-stage-test".to_owned(),
                agent_id: "autopilot-stage-test".to_owned(),
                surface: ToolSurface::Mcp,
                idempotency_key: None,
            },
            RuntimeContext {
                project_root: Some(root.to_owned()),
                ..RuntimeContext::default()
            },
            ProductEntitlements {
                autopilot: enabled,
                personalized_learning: false,
            },
        );
        let mut driver = super::McpDriver {
            server: &server,
            request: None,
            entitlements: None,
            prepared: Some(PreparedCallResult::Ready(Box::new(
                super::PreparedMcpCall {
                    name: "ctx_read".to_owned(),
                    args: None,
                    minimal: true,
                    config: std::sync::Arc::new(crate::core::config::Config::default()),
                    machine_readable: false,
                    auto_context: None,
                    throttle_warning: None,
                    args_fp: String::new(),
                    decision_context: None,
                    kernel_handoff: Default::default(),
                    cache_key: None,
                },
            ))),
            checkpoint: None,
            pending_cache_key: None,
            native_receipt_capture: None,
            native_receipt_metadata: None,
            input_tokens: 0,
        };
        assert_eq!(
            driver.ask_autopilot(&context).await.unwrap(),
            StageDisposition::Skipped(if enabled {
                "no canonical autopilot decision"
            } else {
                "not entitled"
            })
        );
        let Some(PreparedCallResult::Ready(call)) = driver.prepared.as_ref() else {
            panic!("prepared call must remain available for dispatch");
        };
        assert_eq!(
            call.decision_context.as_ref().map(|value| &value.task_id),
            enabled.then_some(&context.task_id),
            "existing completion-accounting handoff must remain intact"
        );
        if enabled {
            assert_eq!(
                server.task_envelope.read().await.as_ref().unwrap().task_id,
                context.envelope.task_id
            );
        }
        assert!(context.outcome().is_none());
    }
}

#[test]
fn replay_admission_overrides_policy_terminal_but_not_response_cache() {
    let result = || CallToolResult::success(vec![ContentBlock::text("result")]);
    assert!(
        PreparedCallResult::Terminal(result(), "policy denied")
            .into_replay_override()
            .is_some()
    );
    assert!(
        PreparedCallResult::Cached(result())
            .into_replay_override()
            .is_none()
    );
}

#[test]
fn cached_result_preserves_meta() {
    let cache = ResponseCache::new(8, Duration::from_mins(1));
    let key = response_cache_key("ctx_search", None, "/project").expect("cacheable tool");
    let mut result = CallToolResult::success(vec![ContentBlock::text("cached")]);
    let mut meta = Meta::new();
    meta.0
        .insert("cache_hint".to_owned(), Value::String("stable".to_owned()));
    result.meta = Some(meta);

    cache_call_result(&cache, key.clone(), &result);

    let cached = cached_call_result(&cache, &key).expect("response should be cached");
    assert_eq!(cached.meta, result.meta);
}

#[test]
fn ctx_read_is_not_response_cached() {
    assert!(response_cache_key("ctx_read", None, "/project").is_none());
}

#[test]
fn remaining_response_cache_tools_are_cacheable() {
    for tool_name in ["ctx_search", "ctx_tree", "ctx_glob"] {
        assert!(response_cache_key(tool_name, None, "/project").is_some());
    }
}

#[test]
fn active_policy_disables_response_cache_reuse() {
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::ResolvedPolicy {
            name: "cache-egress-test".into(),
            version: "1.0.0".into(),
            description: "cache egress guard".into(),
            chain: vec![],
            default_read_mode: None,
            allow_tools: None,
            deny_tools: vec![],
            max_context_tokens: None,
            audit_retention_days: None,
            redaction: std::collections::BTreeMap::new(),
            filters: crate::core::policy::FilterRules::default(),
            egress: crate::core::policy::EgressRules::default(),
            routing: crate::core::policy::RoutingPolicyRules::default(),
            budgets: crate::core::policy::BudgetRules::default(),
        },
    ));

    assert!(!super::cache_allowed_for_current_egress(
        &crate::core::config::Config::default()
    ));
}

/// The daily telemetry flush is scheduled off `background_tick`, so a call
/// rejected before dispatch must still advance it. Under the old gate
/// (`call_count`, only moved by `record_checkpoint`) a default install
/// never reached the cadence boundary and never sent a batch.
#[tokio::test(flavor = "multi_thread")]
async fn background_tick_counts_calls_rejected_before_dispatch() {
    use std::sync::atomic::Ordering;

    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let root = env!("CARGO_MANIFEST_DIR").to_string();
    let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
    // Skip bus registration: this test is about the cadence, not presence.
    *server.presence_agent_id.write().await = Some("tick-test".to_string());

    for expected in 1..=2 {
        // `ctx` without `tool` is rejected before any dispatch or checkpoint.
        let rejected = server
            .call_tool_guarded(rmcp::model::CallToolRequestParams::new("ctx"))
            .await;
        assert!(rejected.is_err());
        assert_eq!(server.background_tick.load(Ordering::Relaxed), expected);
    }
    assert_eq!(server.call_count.load(Ordering::Relaxed), 0);
}

/// #2089: Claude Code 2.1.296+ only lets a mod call listed tools, so the mod
/// reaches the unlisted ctx_shape through ctx_call. The reporter's native
/// Bash output must come back shaped, as a success, with no decoration the
/// mod would splice into stdout.
#[tokio::test(flavor = "multi_thread")]
async fn ctx_shape_through_ctx_call_shapes_native_output() {
    use std::fmt::Write as _;

    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let root = env!("CARGO_MANIFEST_DIR").to_string();
    let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
    *server.presence_agent_id.write().await = Some("shape-test".to_string());
    let mut output = String::new();
    for i in 1..=150 {
        let _ = writeln!(
            output,
            "2026-10-14T20:00:00Z INFO [build] compiling package github.com/example/pkg{} step {i} ok",
            i % 7
        );
    }
    let request: rmcp::model::CallToolRequestParams = serde_json::from_value(serde_json::json!({
        "name": "ctx_call",
        "arguments": {"name": "ctx_shape", "arguments": {
            "tool": "Bash",
            "command": "for i in $(seq 1 150); do echo \"… step $i ok\"; done",
            "output": output,
            "exit_code": 0,
        }},
    }))
    .unwrap();

    let result = server.call_tool_guarded(request).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect::<String>();
    assert!(
        text.len() < output.len(),
        "native output must shrink: {} >= {}",
        text.len(),
        output.len()
    );
    // The mod splices the body into stdout as-is: nothing may precede it.
    assert!(
        text.starts_with(output.lines().next().unwrap()),
        "the body must start with the native output, undecorated: {text}"
    );
}

#[test]
fn plan_mode_blocks_destructive_tools() {
    let plan_tools = crate::core::editor_registry::plan_mode::plan_mode_tools();
    for tool in crate::tool_defs::DESTRUCTIVE_TOOL_NAMES {
        assert!(
            !plan_tools.contains(tool),
            "destructive tool '{tool}' must not be in plan_mode_tools"
        );
    }
}

#[test]
fn plan_mode_allows_read_only_tools() {
    let plan_tools = crate::core::editor_registry::plan_mode::plan_mode_tools();
    for tool in ["ctx_read", "ctx_search", "ctx_tree", "ctx_graph"] {
        assert!(
            plan_tools.contains(&tool),
            "read-only tool '{tool}' must be in plan_mode_tools"
        );
    }
}

#[test]
fn interaction_mode_atomic_swap() {
    use crate::tools::InteractionMode;
    let mode = std::sync::atomic::AtomicU8::new(InteractionMode::Agent as u8);
    assert_eq!(
        InteractionMode::from_u8(mode.load(std::sync::atomic::Ordering::Relaxed)),
        InteractionMode::Agent,
    );
    let prev = mode.swap(
        InteractionMode::Plan as u8,
        std::sync::atomic::Ordering::Relaxed,
    );
    assert_eq!(prev, InteractionMode::Agent as u8);
    assert_eq!(
        InteractionMode::from_u8(mode.load(std::sync::atomic::Ordering::Relaxed)),
        InteractionMode::Plan,
    );
}
