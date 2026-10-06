// SPDX-License-Identifier: Apache-2.0

use super::super::{
    CallToolRequestParams, CallToolResult, CrpMode, ErrorData, LeanCtxServer, elicitation, helpers,
    is_shell_tool_name, permission_inheritance, post_process,
};
use crate::core::ocla::response_cache::{
    CachedResponse, ResponseCache, ResponseCacheKey, global_response_cache,
};
use rmcp::model::{ContentBlock, Meta};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

mod kernel;

#[cfg(test)]
mod task_lineage_tests;

const CACHEABLE_TOOLS: [&str; 3] = ["ctx_search", "ctx_tree", "ctx_glob"];

struct McpEntitlements {
    config: std::sync::Arc<crate::core::config::Config>,
}

struct PreparedMcpCall {
    name: String,
    args: Option<Map<String, Value>>,
    minimal: bool,
    config: std::sync::Arc<crate::core::config::Config>,
    machine_readable: bool,
    auto_context: Option<String>,
    throttle_warning: Option<String>,
    args_fp: String,
    decision_context: Option<crate::core::execution_lifecycle::TaskContext>,
    kernel_handoff: crate::core::context_kernel::bridge::runtime::KernelPlanningHandoff,
    cache_key: Option<ResponseCacheKey>,
}

enum PreparedCallResult {
    Terminal(CallToolResult, &'static str),
    Cached(CallToolResult),
    Ready(Box<PreparedMcpCall>),
}

impl PreparedCallResult {
    fn into_replay_override(self) -> Option<CallToolResult> {
        match self {
            Self::Terminal(result, _) => Some(result),
            Self::Cached(_) | Self::Ready(_) => None,
        }
    }
}

enum McpDispatched {
    Terminal(CallToolResult, &'static str),
    Raw {
        call: Box<PreparedMcpCall>,
        primitive: super::pipeline::McpPrimitive,
    },
}

struct McpProcessedCall {
    result: CallToolResult,
    pipeline: Option<super::pipeline::McpProcessed>,
    cache_key: Option<ResponseCacheKey>,
    skip_reason: Option<&'static str>,
}

struct McpDriver<'a> {
    server: &'a LeanCtxServer,
    request: Option<CallToolRequestParams>,
    entitlements: Option<McpEntitlements>,
    prepared: Option<PreparedCallResult>,
    checkpoint: Option<super::pipeline::McpCheckpointIntent>,
    pending_cache_key: Option<ResponseCacheKey>,
    native_receipt_capture: Option<Arc<crate::server::native_receipts::NativeReceiptCapture>>,
    native_receipt_metadata: Option<Value>,
    input_tokens: u64,
}

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
    if crate::server::native_receipts::requested(tool_name, arguments) {
        // A new lifecycle task must execute and bind its own native plan.
        return None;
    }
    // Semantic results include mutable kernel context. Keep its underlying
    // search caches, but plan/reapply context before caching the whole response.
    if tool_name == "ctx_search"
        && arguments.is_some_and(crate::tools::registered::ctx_search::uses_kernel_context)
    {
        return None;
    }
    let policy = crate::core::policy::runtime::active();
    if policy
        .as_ref()
        .is_some_and(|policy| !policy.tool_allowed(tool_name))
    {
        return None;
    }
    CACHEABLE_TOOLS.contains(&tool_name).then(|| {
        let mut input = Vec::with_capacity(project_root.len() + 1);
        input.extend_from_slice(project_root.as_bytes());
        input.push(0);
        input.extend_from_slice(
            &serde_json::to_vec(&arguments).expect("JSON arguments must serialize"),
        );
        if let Some(policy) = policy {
            input.push(0);
            input.extend_from_slice(
                &serde_json::to_vec(&policy.resolved).expect("resolved policy is serializable"),
            );
        }
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
        let project = self
            .session
            .read()
            .await
            .project_root
            .as_ref()
            .map(std::path::PathBuf::from);
        crate::core::policy::runtime::REQUEST_PROJECT
            .scope(std::cell::RefCell::new(project), async {
                // Establish client-root authority before early receipt/identity
                // checks can return, as well as before lifecycle replay lookup.
                self.resolve_roots_once().await;
                let project = self.session.read().await.project_root.clone();
                crate::core::policy::runtime::bind_request_project(project.as_deref());
                // One Decision Receipt per call: every admission inside this
                // call (handler and its read workers) lands in this capture.
                let admissions = crate::core::context_admission::capture::AdmissionCapture::new();
                let tool_name = request.name.to_string();
                let result = crate::core::context_admission::capture::ADMISSIONS
                    .scope(
                        Some(admissions.clone()),
                        self.call_tool_guarded_scoped(request),
                    )
                    .await;
                self.finish_gateway_receipt(admissions, &result, &tool_name)
                    .await;
                result.map_err(|error| {
                    if crate::core::policy::runtime::is_active() {
                        ErrorData::internal_error(
                            "Tool request failed under the current policy",
                            None,
                        )
                    } else {
                        error
                    }
                })
            })
            .await
    }

    async fn call_tool_guarded_scoped(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, ErrorData> {
        if crate::server::native_receipts::requested(&request.name, request.arguments.as_ref())
            && let Some(authority) = &self.native_receipt_authority
        {
            // Includes idempotent replay: stale signer admission is never bypassed.
            authority
                .validate_current()
                .map_err(|code| ErrorData::internal_error(code, None))?;
        }
        let config = crate::core::config::Config::load_arc();
        let tool_name = request.name.as_ref().to_string();
        // Semantic identity is the actual tool, while replay keys and dispatch
        // retain the original wrapper request. Invalid envelopes still go
        // through the normal lifecycle admission/error path.
        let semantic_call =
            crate::tools::registered::ctx_call::resolve(&tool_name, request.arguments.as_ref())
                .ok();
        let query = semantic_call
            .as_ref()
            .and_then(|(_, args)| args.as_ref())
            .and_then(|args| {
                args.get("query")
                    .and_then(Value::as_str)
                    .or_else(|| args.get("task").and_then(Value::as_str))
            })
            .map(str::to_owned);
        let input_tokens = request
            .arguments
            .as_ref()
            .and_then(|arguments| serde_json::to_vec(arguments).ok())
            .map_or(0, |bytes| {
                u64::try_from(bytes.len() / 4).unwrap_or(u64::MAX)
            });
        let raw_idempotency_key = request.meta.as_ref().and_then(|meta| {
            meta.0
                .get("requestId")
                .or_else(|| meta.0.get("idempotencyKey"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let (session_id, project_root, stated_task) = {
            let session = self.session.read().await;
            let stated_task = session
                .task
                .as_ref()
                .filter(|task| {
                    crate::tools::registered::ctx_read::task_intent_steers_read(
                        task.intent.as_deref(),
                    )
                })
                .map(|task| task.description.clone());
            (
                session.id.clone(),
                session.project_root.clone(),
                stated_task,
            )
        };
        let query = query.or(stated_task);
        let task_identity = self
            .native_receipt_authority
            .as_ref()
            .map(|authority| authority.admit_task_identity(project_root.as_deref()))
            .transpose()
            .map_err(|code| ErrorData::internal_error(code, None))?
            .flatten();
        let agent_id = match self.agent_id.read().await.clone() {
            Some(agent_id) => agent_id,
            None => self
                .presence_agent_id
                .read()
                .await
                .clone()
                .unwrap_or_else(|| "mcp-agent".to_owned()),
        };
        let payload_fingerprint = request.arguments.as_ref().map_or_else(String::new, |args| {
            crate::core::loop_detection::LoopDetector::fingerprint(&Value::Object(args.clone()))
        });
        let idempotency_key = raw_idempotency_key.map(|key| {
            format!("mcp:{session_id}:{agent_id}:{tool_name}:{payload_fingerprint}:{key}")
        });
        let client_name = self.client_name.read().await.clone();
        let release_name = semantic_call
            .as_ref()
            .map_or(tool_name.as_str(), |(name, _)| name)
            .to_owned();
        let decision_loop_enabled = config.decision_loop.enabled;
        let lifecycle = crate::core::execution_lifecycle::ExecutionLifecycle::global();
        lifecycle
            .run_with_task_identity(
                crate::core::execution_lifecycle::ToolRequest {
                    tool_name: semantic_call.map_or(tool_name, |(name, _)| name),
                    query,
                    session_id,
                    agent_id,
                    surface: crate::core::execution_lifecycle::ToolSurface::Mcp,
                    idempotency_key,
                },
                crate::core::execution_lifecycle::RuntimeContext {
                    client_name: Some(client_name),
                    project_root,
                },
                crate::core::execution_lifecycle::ProductEntitlements {
                    autopilot: decision_loop_enabled,
                    personalized_learning: false,
                },
                McpDriver {
                    server: self,
                    request: Some(request),
                    entitlements: Some(McpEntitlements { config }),
                    prepared: None,
                    checkpoint: None,
                    pending_cache_key: None,
                    native_receipt_capture: None,
                    native_receipt_metadata: None,
                    input_tokens,
                },
                task_identity,
            )
            .await
            .map_err(|error| match error {
                crate::core::execution_lifecycle::LifecycleRunError::Dispatch(error) => error,
                crate::core::execution_lifecycle::LifecycleRunError::ReplayTypeMismatch => {
                    ErrorData::invalid_request("idempotent execution contract mismatch", None)
                }
                crate::core::execution_lifecycle::LifecycleRunError::Aborted(outcome) => {
                    ErrorData::internal_error(
                        format!(
                            "idempotent execution already terminated: {}",
                            outcome.accepted_outcome.outcome_id.as_str()
                        ),
                        None,
                    )
                }
            })
            .map(|result| crate::server::policy_guard::release_result(&release_name, result))
    }

    async fn prepare_tool_call(
        &self,
        request: CallToolRequestParams,
        entitlements: McpEntitlements,
    ) -> Result<PreparedCallResult, ErrorData> {
        elicitation::increment_call();

        let original_name = request.name.as_ref().to_string();
        // Plan mode: extract interactionMode before arguments are consumed.
        let meta_interaction_mode = request
            .meta
            .as_ref()
            .and_then(|m| m.0.get("interactionMode"))
            .and_then(|v| v.as_str())
            .and_then(crate::tools::InteractionMode::from_meta_str);
        let (resolved_name, resolved_args) = crate::tools::registered::ctx_call::normalize_outer(
            &original_name,
            request.arguments.as_ref(),
        )?;
        let name = resolved_name.as_str();
        let args = resolved_args.as_ref();
        let config = entitlements.config;
        if let Some(denied) = Self::guard_role_and_policy(name) {
            return Ok(PreparedCallResult::Terminal(
                denied,
                "role or policy denied",
            ));
        }

        // ctx_call is a meta-dispatcher: the egress DLP and permission-
        // inheritance gates below must inspect the INNER tool + arguments, or
        // the universal invoker becomes a policy bypass (#1008 security pass).
        // Role/rate/workflow gates for the inner tool already run inside the
        // dispatch layer; these two ran only on the wrapper name before.
        let inner_call = if name == "ctx_call" {
            match crate::tools::registered::ctx_call::resolve_inner(args) {
                Ok(inner) => Some(inner),
                // Preserve dispatch's soft INVALID_PARAMS contract for clients
                // that otherwise mistake an argument error for transport failure.
                Err(error) => {
                    return Ok(PreparedCallResult::Terminal(
                        CallToolResult::error(vec![ContentBlock::text(error.message.to_string())]),
                        "invalid params",
                    ));
                }
            }
        } else {
            None
        };
        let (guard_name, guard_args): (&str, Option<&serde_json::Map<_, _>>) = match &inner_call {
            Some((n, a)) => (n.as_str(), a.as_ref()),
            None => (name, args),
        };
        // A wrapper must not plan for a tool that direct admission forbids.
        // Dispatch still rechecks the inner role/rate/workflow boundaries.
        if inner_call.is_some() {
            if let Some(denied) = Self::guard_role_and_policy(guard_name) {
                return Ok(PreparedCallResult::Terminal(
                    denied,
                    "inner role or policy denied",
                ));
            }
            if let Some(denied) = self.guard_workflow(guard_name).await {
                return Ok(PreparedCallResult::Terminal(
                    denied,
                    "inner workflow denied",
                ));
            }
        }

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
                    return Ok(PreparedCallResult::Terminal(result, "plan mode denied"));
                }
            }
        }

        if let Some(blocked) = Self::guard_egress(guard_name, guard_args) {
            return Ok(PreparedCallResult::Terminal(blocked, "egress denied"));
        }

        if let Some(blocked) = self.guard_workflow(name).await {
            return Ok(PreparedCallResult::Terminal(blocked, "workflow denied"));
        }

        // #794: cost cap guard — block tool calls when session cost exceeds the
        // configured limit. ctx_session is exempt so the agent can inspect
        // budget status and override the cap.
        if name != "ctx_session"
            && let Some(cap_msg) =
                crate::core::budget_tracker::BudgetTracker::global().cost_cap_message()
        {
            let result = CallToolResult::error(vec![ContentBlock::text(cap_msg)]);
            return Ok(PreparedCallResult::Terminal(result, "cost cap reached"));
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
        let machine_readable = self
            .registry
            .as_ref()
            .and_then(|registry| registry.get_arc(guard_name))
            .is_some_and(|tool| tool.produces_machine_readable(guard_args));

        // Skip the session wake-up briefing for machine-readable calls: the
        // pre-hook latches `session_initialized` via compare-exchange, so calling
        // it here would burn the once-per-session slot for a briefing we then
        // throw away. Deferring keeps the briefing intact for the next call.
        let args_fp = args
            .map(|a| {
                crate::core::loop_detection::LoopDetector::fingerprint(&serde_json::Value::Object(
                    a.clone(),
                ))
            })
            .unwrap_or_default();
        let throttle_result = {
            let semantic_fp = guard_args
                .map(|args| {
                    crate::core::loop_detection::LoopDetector::fingerprint(&Value::Object(
                        args.clone(),
                    ))
                })
                .unwrap_or_default();
            let fp = &semantic_fp;
            let detector_timeout = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                self.loop_detector.write(),
            )
            .await;
            if let Ok(mut detector) = detector_timeout {
                let is_search =
                    crate::core::loop_detection::LoopDetector::is_search_tool(guard_name);
                let is_search_shell = guard_name == "ctx_shell" && {
                    let cmd = guard_args
                        .and_then(|a| a.get("command"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    crate::core::loop_detection::LoopDetector::is_search_shell_command(cmd)
                };

                if is_search || is_search_shell {
                    let search_pattern = guard_args.and_then(|a| {
                        a.get("pattern")
                            .or_else(|| a.get("query"))
                            .and_then(|v| v.as_str())
                    });
                    let shell_pattern = if is_search_shell {
                        guard_args
                            .and_then(|a| a.get("command"))
                            .and_then(|v| v.as_str())
                            .and_then(helpers::extract_search_pattern_from_command)
                    } else {
                        None
                    };
                    let pat = search_pattern.or(shell_pattern.as_deref());
                    detector.record_search(guard_name, fp, pat)
                } else {
                    detector.record_call(guard_name, fp)
                }
            } else {
                tracing::warn!("pre-dispatch: loop_detector write-lock timeout (3s), skipping");
                crate::core::loop_detection::ThrottleResult::default()
            }
        };

        if throttle_result.level == crate::core::loop_detection::ThrottleLevel::Blocked {
            let msg = throttle_result.message.unwrap_or_default();
            let result = CallToolResult::success(vec![ContentBlock::text(msg)]);
            return Ok(PreparedCallResult::Terminal(
                result,
                "loop throttle blocked",
            ));
        }

        let throttle_warning =
            if throttle_result.level == crate::core::loop_detection::ThrottleLevel::Reduced {
                throttle_result.message.clone()
            } else {
                None
            };

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
                return Ok(PreparedCallResult::Terminal(
                    blocked,
                    "permission inheritance denied",
                ));
            }
        }

        if let Some(msg) = post_process::budget_exhausted_message(name) {
            tracing::warn!(tool = name, "{msg}");
            let result = CallToolResult::success(vec![ContentBlock::text(msg)]);
            return Ok(PreparedCallResult::Terminal(result, "budget exhausted"));
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
        // A cached answer is egress too: never served while a policy pack or
        // the sensitivity floor could withhold what it contains.
        let cache_key = cache_allowed_for_current_egress(&config)
            .then(|| response_cache_key(name, args, &project_root))
            .flatten();
        let call_start = std::time::Instant::now();
        if let Some(cached) = cache_key
            .as_ref()
            .and_then(|key| cached_call_result(global_response_cache(), key))
        {
            self.record_tool_usage(name, call_start, cached.is_error != Some(true));
            return Ok(PreparedCallResult::Cached(cached));
        }

        Ok(PreparedCallResult::Ready(Box::new(PreparedMcpCall {
            name: name.to_owned(),
            args: args.cloned(),
            minimal: false,
            config,
            machine_readable,
            auto_context: None,
            throttle_warning,
            args_fp,
            decision_context: None,
            kernel_handoff:
                crate::core::context_kernel::bridge::runtime::KernelPlanningHandoff::Suppressed,
            cache_key,
        })))
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

#[async_trait::async_trait]
impl crate::core::execution_lifecycle::ExecutionDriver for McpDriver<'_> {
    type Primitive = McpDispatched;
    type Processed = McpProcessedCall;
    type Output = CallToolResult;
    type Error = ErrorData;

    async fn identify_caller(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        self.server
            .ensure_session_presence()
            .await
            .map_err(|error| {
                ErrorData::internal_error(
                    format!("agent bus registration is required before tool execution: {error}"),
                    None,
                )
            })?;
        self.server.check_idle_expiry().await;
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    async fn resolve_workspace(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        self.server.resolve_roots_once().await;
        let project = self.server.session.read().await.project_root.clone();
        crate::core::policy::runtime::bind_request_project(project.as_deref());
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    async fn resolve_entitlements(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        if self.entitlements.is_none() {
            return Err(ErrorData::internal_error(
                "MCP entitlements were not resolved",
                None,
            ));
        }
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    async fn apply_security_boundaries(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        let request = self.request.take().expect("MCP request consumed once");
        let entitlements = self
            .entitlements
            .take()
            .expect("MCP entitlements consumed once");
        self.prepared = Some(self.server.prepare_tool_call(request, entitlements).await?);
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    async fn validate_cached_replay(
        &mut self,
        context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<Option<Self::Output>, Self::Error> {
        self.identify_caller(context).await?;
        self.resolve_workspace(context).await?;
        self.resolve_entitlements(context).await?;
        self.apply_security_boundaries(context).await?;
        // Replays revalidate admission only. Context gathering/planning already
        // belongs to the cached execution and must not run a second time.
        Ok(self
            .prepared
            .take()
            .expect("MCP replay admission consumed once")
            .into_replay_override())
    }

    async fn gather_context_strategy(
        &mut self,
        context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        let Some(PreparedCallResult::Ready(call)) = self.prepared.as_mut() else {
            return Ok(crate::core::execution_lifecycle::StageDisposition::Skipped(
                "admission returned a terminal result",
            ));
        };
        *self.server.task_envelope.write().await = Some(context.envelope.clone());
        call.minimal = call.config.minimal_overhead_effective();
        if !call.machine_readable {
            let (task, project_root) = {
                let session = self.server.session.read().await;
                (
                    session.task.as_ref().map(|task| task.description.clone()),
                    session.project_root.clone(),
                )
            };
            let cache_timeout =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.server.cache.write())
                    .await;
            if let Ok(mut cache) = cache_timeout {
                call.auto_context = crate::tools::autonomy::session_lifecycle_pre_hook(
                    &self.server.autonomy,
                    &call.name,
                    &mut cache,
                    task.as_deref(),
                    project_root.as_deref(),
                    CrpMode::effective(),
                );
            } else {
                tracing::warn!("pre-dispatch: cache write-lock timeout (5s), skipping autonomy");
            }
        }
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    }

    async fn ask_autopilot(
        &mut self,
        context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        if !context.entitlements.autopilot {
            if let Some(PreparedCallResult::Ready(call)) = self.prepared.as_mut() {
                call.kernel_handoff =
                    crate::core::context_kernel::bridge::runtime::KernelPlanningHandoff::Suppressed;
            }
            return Ok(crate::core::execution_lifecycle::StageDisposition::Skipped(
                "not entitled",
            ));
        }
        let Some(PreparedCallResult::Ready(call)) = self.prepared.as_mut() else {
            return Ok(crate::core::execution_lifecycle::StageDisposition::Skipped(
                "admission returned a terminal result",
            ));
        };
        *self.server.task_envelope.write().await = Some(context.envelope.clone());
        call.decision_context = Some(context.clone());
        let (disposition, handoff) = kernel::plan(self.server, call, context).await;
        call.kernel_handoff = handoff;
        Ok(disposition)
    }

    async fn dispatch_primitive(
        &mut self,
        context: &crate::core::execution_lifecycle::TaskContext,
    ) -> Result<
        (
            Self::Primitive,
            crate::core::execution_lifecycle::StageDisposition,
        ),
        Self::Error,
    > {
        match self.prepared.take().expect("MCP admission consumed once") {
            PreparedCallResult::Terminal(result, reason) => Ok((
                McpDispatched::Terminal(result, reason),
                crate::core::execution_lifecycle::StageDisposition::Applied,
            )),
            PreparedCallResult::Cached(result) => Ok((
                McpDispatched::Terminal(result, "response cache hit"),
                crate::core::execution_lifecycle::StageDisposition::Applied,
            )),
            PreparedCallResult::Ready(call) => {
                // Context gathering can await locks/services after admission.
                // Recheck current tool and egress rules before any primitive,
                // including the inner tool carried by ctx_call, is dispatched.
                let (guard_name, guard_args) =
                    crate::tools::registered::ctx_call::resolve(&call.name, call.args.as_ref())?;
                if let Some(denied) = LeanCtxServer::guard_role_and_policy(&guard_name)
                    .or_else(|| LeanCtxServer::guard_egress(&guard_name, guard_args.as_ref()))
                {
                    return Ok((
                        McpDispatched::Terminal(denied, "policy changed before dispatch"),
                        crate::core::execution_lifecycle::StageDisposition::Applied,
                    ));
                }
                if crate::server::native_receipts::requested(&call.name, call.args.as_ref()) {
                    self.native_receipt_capture = self
                        .server
                        .native_receipt_authority
                        .as_ref()
                        .map(|authority| {
                            Arc::new(crate::server::native_receipts::NativeReceiptCapture::new(
                                authority.clone(),
                                context.envelope.clone(),
                            ))
                        });
                }
                let dispatch_start = std::time::Instant::now();
                let dispatch = super::pipeline::dispatch_primitive(
                    self.server,
                    &call.name,
                    call.args.as_ref(),
                    call.minimal,
                    &call.args_fp,
                );
                let primitive = crate::server::native_receipts::NATIVE_RECEIPT_CAPTURE
                    .scope(
                        self.native_receipt_capture.clone(),
                        crate::core::context_kernel::bridge::runtime::KERNEL_PLANNING_HANDOFF
                            .scope(call.kernel_handoff.clone(), dispatch),
                    )
                    .await;
                self.server.record_tool_usage(
                    &call.name,
                    dispatch_start,
                    primitive
                        .as_ref()
                        .is_ok_and(super::pipeline::McpPrimitive::succeeded),
                );
                let primitive = primitive?;
                Ok((
                    McpDispatched::Raw { call, primitive },
                    crate::core::execution_lifecycle::StageDisposition::Applied,
                ))
            }
        }
    }

    async fn reversible_post_process(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
        primitive: Self::Primitive,
    ) -> Result<
        (
            Self::Processed,
            crate::core::execution_lifecycle::StageDisposition,
        ),
        Self::Error,
    > {
        match primitive {
            McpDispatched::Terminal(result, reason) => Ok((
                McpProcessedCall {
                    result,
                    pipeline: None,
                    cache_key: None,
                    skip_reason: Some(reason),
                },
                crate::core::execution_lifecycle::StageDisposition::Skipped(reason),
            )),
            McpDispatched::Raw { call, primitive } => {
                let disposition = match &primitive {
                    super::pipeline::McpPrimitive::Raw(_) => {
                        crate::core::execution_lifecycle::StageDisposition::Applied
                    }
                    super::pipeline::McpPrimitive::Terminal(_, reason) => {
                        crate::core::execution_lifecycle::StageDisposition::Skipped(reason)
                    }
                };
                let mut processed = super::pipeline::reversible_post_process(
                    self.server,
                    &call.name,
                    call.args.as_ref(),
                    call.minimal,
                    call.config,
                    call.machine_readable,
                    call.auto_context,
                    call.throttle_warning,
                    call.decision_context,
                    primitive,
                )
                .await?;
                if self.native_receipt_capture.is_some() {
                    processed.freeze_receipt_delivery();
                }
                Ok((
                    McpProcessedCall {
                        result: processed.result.clone(),
                        pipeline: Some(processed),
                        cache_key: call.cache_key,
                        skip_reason: None,
                    },
                    disposition,
                ))
            }
        }
    }

    async fn record_context_ir(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
        processed: &Self::Processed,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        Ok(match processed.pipeline.as_ref() {
            Some(pipeline) => super::pipeline::record_context_ir(self.server, pipeline).await,
            None => crate::core::execution_lifecycle::StageDisposition::Skipped(
                processed.skip_reason.unwrap_or("terminal result"),
            ),
        })
    }

    async fn record_ledger(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
        processed: &Self::Processed,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        if let Some(capture) = &self.native_receipt_capture {
            self.native_receipt_metadata = Some(
                capture
                    .publish(&processed.result)
                    .map_err(|code| ErrorData::internal_error(code, None))?,
            );
        }
        Ok(match processed.pipeline.as_ref() {
            Some(pipeline) => super::pipeline::record_ledger(self.server, pipeline).await?,
            None => crate::core::execution_lifecycle::StageDisposition::Skipped(
                processed.skip_reason.unwrap_or("terminal result"),
            ),
        })
    }

    async fn checkpoint(
        &mut self,
        _context: &crate::core::execution_lifecycle::TaskContext,
        output: &mut Self::Output,
    ) -> Result<crate::core::execution_lifecycle::StageDisposition, Self::Error> {
        let disposition =
            super::pipeline::record_checkpoint(self.server, self.checkpoint.take(), output).await;
        if let Some(key) = self.pending_cache_key.take() {
            cache_call_result(global_response_cache(), key, output);
        }
        Ok(disposition)
    }

    fn output_from_processed(&mut self, mut processed: Self::Processed) -> Self::Output {
        if let Some(pipeline) = processed.pipeline.as_mut() {
            self.checkpoint = pipeline.checkpoint.take();
        }
        self.pending_cache_key = processed.cache_key;
        if let Some(receipt) = self.native_receipt_metadata.take() {
            processed
                .result
                .meta
                .get_or_insert_with(Meta::new)
                .0
                .insert("canonical_receipt".into(), receipt);
        }
        processed.result
    }

    fn observe(
        &self,
        result: &Result<
            Self::Output,
            crate::core::execution_lifecycle::LifecycleRunError<Self::Error>,
        >,
    ) -> crate::core::execution_lifecycle::CompletionObservation {
        let (output_tokens, success) = result.as_ref().map_or((0, false), |response| {
            (
                u64::try_from(format!("{response:?}").len() / 4).unwrap_or(u64::MAX),
                response.is_error != Some(true),
            )
        });
        crate::core::execution_lifecycle::CompletionObservation::tool_result(
            self.input_tokens,
            output_tokens,
            "mcp-tool",
            success,
        )
    }
}

#[cfg(test)]
mod tests {
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
            ExecutionDriver, ExecutionLifecycle, ProductEntitlements, RuntimeContext,
            StageDisposition, ToolRequest, ToolSurface,
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
