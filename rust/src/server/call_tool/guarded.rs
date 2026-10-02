use super::super::{
    CallToolRequestParams, CallToolResult, CrpMode, ErrorData, LeanCtxServer, elicitation, helpers,
    is_shell_tool_name, permission_inheritance, post_process,
};
use super::dispatch_and_post_process;
use crate::core::ocla::response_cache::{
    CachedResponse, ResponseCache, ResponseCacheKey, global_response_cache,
};
use rmcp::model::{ContentBlock, Meta};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::time::{Duration, Instant};

const CACHEABLE_TOOLS: [&str; 3] = ["ctx_search", "ctx_tree", "ctx_glob"];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedCallToolResult {
    content: Vec<ContentBlock>,
    #[serde(default)]
    structured_content: Option<Value>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default, rename = "_meta")]
    meta: Option<Meta>,
}

pub(super) fn response_cache_key(
    tool_name: &str,
    arguments: Option<&Map<String, Value>>,
    project_root: &str,
) -> Option<ResponseCacheKey> {
    CACHEABLE_TOOLS.contains(&tool_name).then(|| {
        let mut input = Vec::with_capacity(project_root.len() + 1);
        input.extend_from_slice(project_root.as_bytes());
        input.push(0);
        input.extend_from_slice(
            &serde_json::to_vec(&arguments).expect("JSON arguments must serialize"),
        );
        let digest = blake3::hash(&input);
        let mut hash_bytes = [0; 8];
        hash_bytes.copy_from_slice(&digest.as_bytes()[..8]);
        let arguments_hash = u64::from_be_bytes(hash_bytes);
        ResponseCacheKey::new(tool_name, arguments_hash, 0.0, 0)
    })
}

fn cache_allowed_for_current_egress(config: &crate::core::config::Config) -> bool {
    !crate::core::policy::runtime::is_active()
        && !config.sensitivity_effective().enabled_effective()
        && crate::core::redaction::redaction_enabled_for_active_role()
}

pub(super) fn cached_call_result(
    cache: &ResponseCache,
    key: &ResponseCacheKey,
) -> Option<CallToolResult> {
    let response = cache.get(key);
    crate::core::telemetry::global_metrics().record_cache(response.is_some());
    response.and_then(|cached| {
        serde_json::from_slice::<CachedCallToolResult>(&cached.body)
            .ok()
            .map(|cached| {
                let mut result = CallToolResult::success(cached.content);
                result.structured_content = cached.structured_content;
                result.is_error = cached.is_error;
                result.meta = cached.meta;
                result
            })
    })
}

pub(super) fn cache_call_result(
    cache: &ResponseCache,
    key: ResponseCacheKey,
    result: &CallToolResult,
) {
    if result.is_error == Some(true) {
        return;
    }
    let Ok(body) = serde_json::to_vec(result) else {
        return;
    };
    let tokens = crate::core::tokens::count_tokens(&String::from_utf8_lossy(&body))
        .try_into()
        .unwrap_or(u64::MAX);
    cache.put(
        key,
        CachedResponse {
            body,
            status: 200,
            tokens,
            created_at: Instant::now(),
            ttl: Duration::ZERO,
        },
    );
}

impl LeanCtxServer {
    pub(crate) async fn call_tool_guarded(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, ErrorData> {
        self.ensure_session_presence().await.map_err(|error| {
            ErrorData::internal_error(
                format!("agent bus registration is required before tool execution: {error}"),
                None,
            )
        })?;
        self.check_idle_expiry().await;
        self.resolve_roots_once().await;
        elicitation::increment_call();
        // Background cadence, counted here because this is the one point every
        // call passes regardless of how it is served: guard denials and
        // response-cache hits below return before `dispatch_and_post_process`.
        // It previously hung off `call_count`, which only advances in
        // `record_checkpoint` — skipped entirely when `minimal_overhead`
        // (default true) is set. Between the two, the daily telemetry flush
        // never ran in a default install. The first call sends the day's first
        // batch, so installs whose sessions stay short are still counted; every
        // tenth call persists counters and resends the grown day totals once
        // the aggregate admits it (spacing and daily cap live there).
        let tick = self
            .background_tick
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if tick == 1 || tick.is_multiple_of(100) {
            std::thread::spawn(crate::cloud_sync::cloud_background_tasks);
        } else if tick.is_multiple_of(10) {
            std::thread::spawn(|| {
                crate::cloud_sync::send_telemetry(
                    crate::core::telemetry_aggregate::SendTrigger::Periodic,
                );
            });
        }

        let original_name = request.name.as_ref().to_string();
        // Plan mode: extract interactionMode before arguments are consumed.
        let meta_interaction_mode = request
            .meta
            .as_ref()
            .and_then(|m| m.0.get("interactionMode"))
            .and_then(|v| v.as_str())
            .and_then(crate::tools::InteractionMode::from_meta_str);
        let (resolved_name, resolved_args) = if original_name == "ctx" {
            let sub = request
                .arguments
                .as_ref()
                .and_then(|a| a.get("tool"))
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
                .ok_or_else(|| {
                    ErrorData::invalid_params("'tool' is required for ctx meta-tool", None)
                })?;
            let tool_name = if sub.starts_with("ctx_") {
                sub
            } else {
                format!("ctx_{sub}")
            };
            let mut args = request.arguments.unwrap_or_default();
            args.remove("tool");
            (tool_name, Some(args))
        } else {
            (original_name, request.arguments)
        };
        let name = resolved_name.as_str();
        let args = resolved_args.as_ref();
        let query = args
            .and_then(|values| values.get("query").and_then(serde_json::Value::as_str))
            .or_else(|| {
                args.and_then(|values| values.get("task").and_then(serde_json::Value::as_str))
            });
        let session_id = self.session.read().await.id.clone();
        let agent_id = match self.agent_id.read().await.clone() {
            Some(agent_id) => agent_id,
            None => self
                .presence_agent_id
                .read()
                .await
                .clone()
                .unwrap_or_else(|| "mcp-agent".to_owned()),
        };
        let config = crate::core::config::Config::load_arc();
        let decision_context = config
            .decision_loop
            .enabled
            .then(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::core::decision_loop_runtime::DecisionLoopRuntime::get_or_init()
                        .on_tool_start(name, query, &session_id, &agent_id)
                }))
                .map_err(|_| tracing::warn!(tool = name, "decision loop start panicked"))
                .ok()
            })
            .flatten();
        *self.task_envelope.write().await = crate::core::task_spine::TaskSpine::current();

        if let Some(denied) = Self::guard_role_and_policy(name) {
            finish_decision_loop(decision_context.as_ref(), args, &denied);
            return Ok(denied);
        }

        // ctx_call is a meta-dispatcher: the egress DLP and permission-
        // inheritance gates below must inspect the INNER tool + arguments, or
        // the universal invoker becomes a policy bypass (#1008 security pass).
        // Role/rate/workflow gates for the inner tool already run inside the
        // dispatch layer; these two ran only on the wrapper name before.
        let inner_call: Option<(String, Option<serde_json::Map<String, serde_json::Value>>)> =
            if name == "ctx_call" {
                helpers::get_str(args, "name").map(|inner_name| {
                    let inner_args = args
                        .and_then(|m| m.get("arguments"))
                        .and_then(serde_json::Value::as_object)
                        .cloned();
                    (inner_name, inner_args)
                })
            } else {
                None
            };
        let (guard_name, guard_args): (&str, Option<&serde_json::Map<_, _>>) = match &inner_call {
            Some((n, a)) => (n.as_str(), a.as_ref()),
            None => (name, args),
        };

        // Plan mode enforcement: update session state and notify on transitions.
        if let Some(mode) = meta_interaction_mode {
            let prev = self
                .interaction_mode
                .swap(mode as u8, std::sync::atomic::Ordering::Relaxed);
            if prev != mode as u8 {
                tracing::info!(
                    ?mode,
                    "interaction mode changed — sending tools/list_changed"
                );
                if let Some(peer) = self.peer.read().await.as_ref() {
                    crate::server::notifications::send_tools_list_changed(peer).await;
                }
            }
        }

        // Plan mode guard: block non-readonly tools (including via ctx_call).
        {
            use crate::tools::InteractionMode;
            let current = InteractionMode::from_u8(
                self.interaction_mode
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
            if current == InteractionMode::Plan {
                let plan_tools = crate::core::editor_registry::plan_mode::plan_mode_tools();
                if !plan_tools.contains(&guard_name) {
                    let result = CallToolResult::error(vec![ContentBlock::text(format!(
                        "[PLAN MODE] Tool '{guard_name}' is not available in plan/readonly mode.                          Switch to agent/edit mode to use this tool."
                    ))]);
                    finish_decision_loop(decision_context.as_ref(), args, &result);
                    return Ok(result);
                }
            }
        }

        // #1765: a session admitted read-only — the machine-wide mutating cap
        // was full when it registered — serves the plan-mode (read-only) tool
        // set as-is and retries its real role before the first mutating call.
        if self
            .presence_read_only
            .load(std::sync::atomic::Ordering::Relaxed)
            && !crate::core::editor_registry::plan_mode::plan_mode_tools().contains(&guard_name)
            && let Err(refusal) = self.try_upgrade_presence(guard_name).await
        {
            let result = CallToolResult::error(vec![ContentBlock::text(refusal)]);
            finish_decision_loop(decision_context.as_ref(), args, &result);
            return Ok(result);
        }

        if let Some(blocked) = Self::guard_egress(guard_name, guard_args) {
            finish_decision_loop(decision_context.as_ref(), args, &blocked);
            return Ok(blocked);
        }

        if let Some(blocked) = self.guard_workflow(name).await {
            finish_decision_loop(decision_context.as_ref(), args, &blocked);
            return Ok(blocked);
        }

        // #794: cost cap guard — block tool calls when session cost exceeds the
        // configured limit. ctx_session is exempt so the agent can inspect
        // budget status and override the cap.
        if name != "ctx_session"
            && let Some(cap_msg) =
                crate::core::budget_tracker::BudgetTracker::global().cost_cap_message()
        {
            let result = CallToolResult::error(vec![ContentBlock::text(cap_msg)]);
            finish_decision_loop(decision_context.as_ref(), args, &result);
            return Ok(result);
        }

        // #990: determine machine-readability *before* the once-per-session
        // decorations below. A machine-readable invocation (e.g. ctx_outline
        // format=json) must reach the client byte-exact and parseable, so every
        // prose decoration and terse compression is suppressed and the pure
        // pre-decoration body is restored at the end (see the `machine_readable`
        // guard near the end of this function). Computing it here — not after
        // dispatch — means such a call also never *consumes* a latched
        // once-per-session flag (auto-context briefing, rules tip) whose prose
        // we would then discard, so those surface on the next human-facing call.
        //
        // `ctx_call` is a meta-dispatcher: the contract belongs to its *inner*
        // tool + inner arguments, not to ctx_call itself. Unwrap one level so
        // JSON reached via the lazy `ctx_call` path (the default advertised
        // surface, where ctx_outline is not a top-level tool) is just as
        // byte-exact as a direct call. This also covers JSON error envelopes
        // from the early rate-limit path, which the first-call auto-context
        // briefing would otherwise corrupt.
        let (mr_name, mr_args): (
            Option<String>,
            Option<&serde_json::Map<String, serde_json::Value>>,
        ) = if name == "ctx_call" {
            (
                helpers::get_str(args, "name"),
                args.and_then(|m| m.get("arguments"))
                    .and_then(serde_json::Value::as_object),
            )
        } else {
            (Some(name.to_string()), args)
        };
        let machine_readable = mr_name
            .as_deref()
            .and_then(|n| self.registry.as_ref().and_then(|r| r.get_arc(n)))
            .is_some_and(|tool| tool.produces_machine_readable(mr_args));

        // Skip the session wake-up briefing for machine-readable calls: the
        // pre-hook latches `session_initialized` via compare-exchange, so calling
        // it here would burn the once-per-session slot for a briefing we then
        // throw away. Deferring keeps the briefing intact for the next call.
        let auto_context = if machine_readable {
            None
        } else {
            let task = {
                let session = self.session.read().await;
                session.task.as_ref().map(|t| t.description.clone())
            };
            let project_root = {
                let session = self.session.read().await;
                session.project_root.clone()
            };
            let cache_timeout =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.cache.write()).await;
            if let Ok(mut cache) = cache_timeout {
                crate::tools::autonomy::session_lifecycle_pre_hook(
                    &self.autonomy,
                    name,
                    &mut cache,
                    task.as_deref(),
                    project_root.as_deref(),
                    CrpMode::effective(),
                )
            } else {
                tracing::warn!("pre-dispatch: cache write-lock timeout (5s), skipping autonomy");
                None
            }
        };

        let args_fp = args
            .map(|a| {
                crate::core::loop_detection::LoopDetector::fingerprint(&serde_json::Value::Object(
                    a.clone(),
                ))
            })
            .unwrap_or_default();
        let throttle_result = {
            let fp = &args_fp;
            let detector_timeout = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                self.loop_detector.write(),
            )
            .await;
            if let Ok(mut detector) = detector_timeout {
                let is_search = crate::core::loop_detection::LoopDetector::is_search_tool(name);
                let is_search_shell = name == "ctx_shell" && {
                    let cmd = args
                        .as_ref()
                        .and_then(|a| a.get("command"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    crate::core::loop_detection::LoopDetector::is_search_shell_command(cmd)
                };

                if is_search || is_search_shell {
                    let search_pattern = args.and_then(|a| {
                        a.get("pattern")
                            .or_else(|| a.get("query"))
                            .and_then(|v| v.as_str())
                    });
                    let shell_pattern = if is_search_shell {
                        args.and_then(|a| a.get("command"))
                            .and_then(|v| v.as_str())
                            .and_then(helpers::extract_search_pattern_from_command)
                    } else {
                        None
                    };
                    let pat = search_pattern.or(shell_pattern.as_deref());
                    detector.record_search(name, fp, pat)
                } else {
                    detector.record_call(name, fp)
                }
            } else {
                tracing::warn!("pre-dispatch: loop_detector write-lock timeout (3s), skipping");
                crate::core::loop_detection::ThrottleResult::default()
            }
        };

        if throttle_result.level == crate::core::loop_detection::ThrottleLevel::Blocked {
            let msg = throttle_result.message.unwrap_or_default();
            let result = CallToolResult::success(vec![ContentBlock::text(msg)]);
            finish_decision_loop(decision_context.as_ref(), args, &result);
            return Ok(result);
        }

        let throttle_warning =
            if throttle_result.level == crate::core::loop_detection::ThrottleLevel::Reduced {
                throttle_result.message.clone()
            } else {
                None
            };

        let minimal = config.minimal_overhead_effective();

        // IDE permission inheritance: when enabled, mirror the host IDE's
        // bash/read/edit/grep permission rules onto the matching lean-ctx tool so
        // e.g. `ctx_shell` honors a `rm *: ask` rule instead of bypassing it.
        // Gated on the cheap effective() check so the default (off) pays no lock
        // cost on the hot path. Checks the ctx_call-unwrapped inner tool (#1008)
        // so the invoker cannot side-step an IDE deny.
        if config.permission_inheritance_effective()
            == crate::core::config::PermissionInheritance::On
        {
            let client_name = self.client_name.read().await.clone();
            let project_root = self.session.read().await.project_root.clone();
            let perm = permission_inheritance::check(
                &client_name,
                guard_name,
                guard_args,
                project_root.as_deref(),
                &config,
            );
            if let Some(blocked) = permission_inheritance::into_call_tool_result(&perm) {
                tracing::warn!(tool = guard_name, "held back by IDE permission inheritance");
                finish_decision_loop(decision_context.as_ref(), args, &blocked);
                return Ok(blocked);
            }
        }

        if let Some(msg) = post_process::budget_exhausted_message(name) {
            tracing::warn!(tool = name, "{msg}");
            let result = CallToolResult::success(vec![ContentBlock::text(msg)]);
            finish_decision_loop(decision_context.as_ref(), args, &result);
            return Ok(result);
        }

        if is_shell_tool_name(name) {
            crate::core::budget_tracker::BudgetTracker::global().record_shell();
        }

        let project_root = self
            .session
            .read()
            .await
            .project_root
            .clone()
            .unwrap_or_default();
        let cache_key = cache_allowed_for_current_egress(&config)
            .then(|| response_cache_key(name, args, &project_root))
            .flatten();
        let call_start = std::time::Instant::now();
        if let Some(cached) = cache_key
            .as_ref()
            .and_then(|key| cached_call_result(global_response_cache(), key))
        {
            finish_decision_loop(decision_context.as_ref(), args, &cached);
            self.record_tool_usage(name, call_start, cached.is_error != Some(true));
            return Ok(cached);
        }

        let result = dispatch_and_post_process(
            self,
            name,
            args,
            minimal,
            config,
            machine_readable,
            auto_context,
            throttle_warning,
            args_fp,
            decision_context,
        )
        .await;
        self.record_tool_usage(
            name,
            call_start,
            result
                .as_ref()
                .is_ok_and(|response| response.is_error != Some(true)),
        );
        if let (Some(key), Ok(response)) = (cache_key, &result) {
            cache_call_result(global_response_cache(), key, response);
        }
        result
    }

    /// Feed the daily telemetry aggregate. Only registered tools are counted,
    /// under the registry's own name; calls stopped by a guard never reach
    /// here and are not usage.
    fn record_tool_usage(&self, name: &str, started: std::time::Instant, success: bool) {
        let Some(tool) = self
            .registry
            .as_ref()
            .and_then(|registry| registry.static_name(name))
        else {
            return;
        };
        let latency_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        crate::core::telemetry::global_metrics().record_named_tool_call(tool, latency_us, success);
    }
}

fn finish_decision_loop(
    context: Option<&crate::core::decision_loop_runtime::TaskContext>,
    args: Option<&Map<String, Value>>,
    result: &CallToolResult,
) {
    let Some(context) = context else {
        return;
    };
    let input_tokens = args
        .and_then(|args| serde_json::to_string(args).ok())
        .map_or(0, |input| (input.len() / 4) as u64);
    let output_tokens = format!("{result:?}").len() as u64 / 4;
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::core::decision_loop_runtime::DecisionLoopRuntime::get_or_init().on_tool_end(
            context,
            input_tokens,
            output_tokens,
            "mcp-tool",
            result.is_error != Some(true),
        );
    }))
    .is_err()
    {
        tracing::warn!("decision loop end panicked");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CallToolResult, ContentBlock, Duration, Meta, ResponseCache, Value, cache_call_result,
        cached_call_result, response_cache_key,
    };

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
        for tool in ["ctx_read", "ctx_search", "ctx_tree", "ctx_overview"] {
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
}
