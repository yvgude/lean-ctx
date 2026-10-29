// SPDX-License-Identifier: Apache-2.0

use super::super::{
    CallToolRequestParams, CallToolResult, ErrorData, LeanCtxServer, elicitation, helpers,
    permission_inheritance,
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
pub(super) mod replay_lineage;

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

/// A gate refusal (role, policy, workflow, loop throttle, budget) on a call to
/// an internal host hook. The host replaces a native tool's output with the
/// hook's success body, so the refusal must read as an error: the host then
/// keeps the original output instead of the block message (#2089).
fn host_hook_refusal(mut result: CallToolResult) -> CallToolResult {
    result.is_error = Some(true);
    result
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
    // A cached response does not carry the Workspace revision it was served
    // under; a detached root must be gone on the next call (#2120).
    if crate::core::workspace_authority::is_configured() {
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
    // The response cache keeps whole tool results without their sources.
    // Where stored output needs a source authority (policy pack, governed or
    // sovereign gateway) it is neither read nor filled: an answer cached
    // before the mode was tightened is not served afterwards.
    !crate::core::context_admission::stored::source_authority_required()
        && !crate::core::policy::runtime::is_active()
        && !config.sensitivity_effective().enabled_effective()
        && crate::core::redaction::redaction_enabled_for_active_role()
}

/// Under protection the response cache is not closed but BOUND: an entry is
/// stored only together with the lineage of the dispatch that produced it
/// (a complete captured source set, or an attested source-free output) and
/// served only while that lineage re-admits under the current policy, role
/// and project and the gateway policy is unchanged
/// (`replay_lineage::{bind_cache_entry, cache_entry_current}`). The other
/// reasons that close the cache — sensitivity floor, redaction switched off
/// for the role — close the bound cache as well.
fn bound_cache_allowed(config: &crate::core::config::Config) -> bool {
    crate::core::context_admission::stored::source_authority_required()
        && !config.sensitivity_effective().enabled_effective()
        && crate::core::redaction::redaction_enabled_for_active_role()
}

/// The lineage record of a response-cache entry.
fn cache_lineage_id(key: &ResponseCacheKey) -> String {
    format!("{}:{:016x}", key.model, key.hash)
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
    /// A call keeps the session instance it was bound to once its roots were
    /// resolved, so a root change while it runs cannot redirect its late
    /// writes into the next project's session (AP04-G1).
    pub(crate) async fn call_tool_guarded(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, ErrorData> {
        // One Workspace snapshot per call (#2120): every path check of this
        // call sees one revision, and the next call sees a detached root gone.
        let authority = crate::core::workspace_authority::current();
        crate::core::workspace_authority::scope(
            authority,
            self.session
                .scope(Box::pin(self.call_tool_guarded_bound(request))),
        )
        .await
    }

    async fn call_tool_guarded_bound(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, ErrorData> {
        let project = self
            .session
            .read()
            .await
            .project_root
            .as_ref()
            .map(std::path::PathBuf::from);
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
        if tick == 1 || tick.is_multiple_of(10) {
            // The first and every hundredth call also record the running
            // version, as the daemon's tick does.
            let record_version = tick == 1 || tick.is_multiple_of(100);
            std::thread::spawn(move || {
                if record_version
                    && crate::core::telemetry_send::telemetry_send_eligible()
                    && let Err(error) = crate::core::telemetry_aggregate::record_current_version()
                {
                    tracing::debug!("telemetry version aggregate unavailable: {error}");
                }
                crate::core::telemetry_send::send_telemetry(
                    crate::core::telemetry_aggregate::SendTrigger::Periodic,
                );
            });
        }
        crate::core::policy::runtime::REQUEST_PROJECT
            .scope(std::cell::RefCell::new(project), async {
                // Establish client-root authority before early receipt/identity
                // checks can return, as well as before lifecycle replay lookup.
                self.resolve_roots_once().await;
                self.session.pin();
                let project = self.session.read().await.project_root.clone();
                crate::core::policy::runtime::bind_request_project(project.as_deref());
                // One Decision Receipt per call: every admission inside this
                // call (handler and its read workers) lands in this capture.
                // It carries the call's ONE policy snapshot, taken here.
                let admissions =
                    crate::core::context_admission::capture::AdmissionCapture::for_call(
                        crate::core::context_admission::capture::CallSnapshot::take(),
                    );
                let tool_name = request.name.to_string();
                let result = crate::core::context_admission::capture::ADMISSIONS
                    .scope(
                        Some(admissions.clone()),
                        self.call_tool_guarded_scoped(request),
                    )
                    .await;
                // Under a pack an error leaves as one fixed text. Replaced
                // before the receipt, so the receipt and its observation name
                // the bytes that actually leave (AP10-C3).
                let result = result.map_err(|error| {
                    if crate::core::policy::runtime::is_active() {
                        ErrorData::internal_error(
                            "Tool request failed under the current policy",
                            None,
                        )
                    } else {
                        error
                    }
                });
                // The receipt is stored before the result leaves (G08).
                self.finish_gateway_receipt(admissions, result, &tool_name)
                    .await
            })
            .await
    }

    async fn call_tool_guarded_scoped(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, ErrorData> {
        // A withheld Hive tool (release switch off) answers one fixed line
        // before any lifecycle, ledger or tool state is touched.
        if let Some(line) = crate::server::dynamic_tools::withheld_hive_tool_refusal(
            &request.name,
            request.arguments.as_ref(),
        ) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(line)]));
        }
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
        // The replay key is not changed: it only names the lineage record the
        // dispatch writes and a later replay admission re-authorizes.
        let replay_key = idempotency_key.clone();
        let run = lifecycle.run_with_task_identity(
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
        );
        let outcome = replay_lineage::scope(replay_key, run)
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
            });
        // Both channels pass the release point: an error's message and data
        // reach the host as surely as a result does (AP10-C1).
        crate::server::policy_guard::release_outcome(&release_name, outcome)
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

        // Hive bus control (C5b): `hold`/`stop` refuse mutating tools,
        // including via ctx_call. The agent id is copied out first so no
        // lock is held across the file check.
        let agent = match self.agent_id.read().await.clone() {
            Some(agent) => Some(agent),
            None => self.presence_agent_id.read().await.clone(),
        };
        if let Some(message) =
            crate::core::hive::bus::control::tool_refusal(agent.as_deref(), guard_name)
        {
            return Ok(PreparedCallResult::Terminal(
                CallToolResult::error(vec![ContentBlock::text(message)]),
                "hive control denied",
            ));
        }
        // A maintainer vetoed this agent's lock or handoff (§3.7.4): its next
        // ctx_tasks / ctx_agent call says so, once.
        if let Some(message) =
            crate::core::hive::bus::veto::take_refusal(agent.as_deref(), guard_name)
        {
            return Ok(PreparedCallResult::Terminal(
                CallToolResult::error(vec![ContentBlock::text(message)]),
                "hive veto",
            ));
        }

        if let Some(blocked) = Self::guard_egress(guard_name, guard_args) {
            return Ok(PreparedCallResult::Terminal(blocked, "egress denied"));
        }

        if let Some(blocked) = self.guard_workflow(name).await {
            return Ok(PreparedCallResult::Terminal(blocked, "workflow denied"));
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
        // wake-up briefing latches `session_briefed` via compare-exchange, so calling
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
            if crate::server::dynamic_tools::INTERNAL_HOST_TOOLS.contains(&guard_name) {
                // A host hook shapes output of a native call the agent already
                // made; repeating `cargo test` is not a lean-ctx loop (#2089).
                crate::core::loop_detection::ThrottleResult::default()
            } else if let Ok(mut detector) = detector_timeout {
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

        let project_root = self
            .session
            .read()
            .await
            .project_root
            .clone()
            .unwrap_or_default();
        // A cached answer is egress too: never served while a policy pack or
        // the sensitivity floor could withhold what it contains.
        let bound_cache = bound_cache_allowed(&config);
        let cache_key = (bound_cache || cache_allowed_for_current_egress(&config))
            .then(|| response_cache_key(name, args, &project_root))
            .flatten();
        let call_start = std::time::Instant::now();
        if let Some(cached) = cache_key
            .as_ref()
            // Under protection a hit needs a lineage that re-admits now.
            .filter(|key| {
                !bound_cache || replay_lineage::cache_entry_current(&cache_lineage_id(key))
            })
            .and_then(|key| cached_call_result(global_response_cache(), key))
        {
            self.record_tool_usage(
                name,
                call_start,
                super::pipeline::result_failure(&cached, ""),
            );
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
    /// here and are not usage. A failure carries only its class and template.
    fn record_tool_usage(
        &self,
        name: &str,
        started: std::time::Instant,
        failure: Option<crate::core::telemetry_failure::Failure>,
    ) {
        let Some(tool) = self
            .registry
            .as_ref()
            .and_then(|registry| registry.static_name(name))
        else {
            return;
        };
        let latency_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        crate::core::telemetry::global_metrics()
            .record_named_tool_failure(tool, latency_us, failure);
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
        let host_hook = crate::server::dynamic_tools::targets_internal_host_tool(
            request.name.as_ref(),
            request.arguments.as_ref(),
        );
        let prepared = self.server.prepare_tool_call(request, entitlements).await?;
        self.prepared = Some(match prepared {
            PreparedCallResult::Terminal(result, reason) if host_hook => {
                PreparedCallResult::Terminal(host_hook_refusal(result), reason)
            }
            other => other,
        });
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
        // A current admission denial wins; otherwise the stored output is
        // returned only while every source it was derived from is still
        // authorized. Loss of rights yields a typed denial — the completed
        // operation is never executed again.
        Ok(self
            .prepared
            .take()
            .expect("MCP replay admission consumed once")
            .into_replay_override()
            .or_else(replay_lineage::denial))
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
            // Built from this caller's own session instance only (I07/S03).
            call.auto_context =
                crate::core::session::caller::scope(Some(self.server.session.instance()), || {
                    crate::tools::autonomy::session_wakeup_briefing(
                        &self.server.autonomy,
                        &call.name,
                        task.as_deref(),
                        project_root.as_deref(),
                    )
                });
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
                if let Some(mut denied) = LeanCtxServer::guard_role_and_policy(&guard_name)
                    .or_else(|| LeanCtxServer::guard_egress(&guard_name, guard_args.as_ref()))
                {
                    if crate::server::dynamic_tools::INTERNAL_HOST_TOOLS
                        .contains(&guard_name.as_str())
                    {
                        denied = host_hook_refusal(denied);
                    }
                    return Ok((
                        McpDispatched::Terminal(denied, "policy changed before dispatch"),
                        crate::core::execution_lifecycle::StageDisposition::Applied,
                    ));
                }
                // A command refused only for unlisted programs is put to the
                // user here, after every policy check and before dispatch.
                let shell_command = (guard_name == "ctx_shell")
                    .then(|| {
                        guard_args
                            .as_ref()
                            .and_then(|args| args.get("command"))
                            .and_then(Value::as_str)
                    })
                    .flatten();
                // ctx_execute code in a language other than shell, likewise
                // (GHSA-rxv5-4c3x-g4q6).
                let execute_items = (guard_name == "ctx_execute")
                    .then(|| {
                        guard_args
                            .as_ref()
                            .map(crate::server::shell_approval::execute_items)
                    })
                    .flatten();
                let approval = match (shell_command, execute_items) {
                    (Some(command), _) => Some(self.server.shell_approval(command).await),
                    (None, Some(items)) => Some(self.server.execute_approval(&items).await),
                    (None, None) => None,
                };
                let shell_grant: Arc<[String]> = match approval {
                    Some(outcome) => match outcome {
                        crate::server::shell_approval::Outcome::Granted(programs) => {
                            Arc::from(programs)
                        }
                        crate::server::shell_approval::Outcome::Refused(message) => {
                            return Ok((
                                McpDispatched::Terminal(
                                    CallToolResult::error(vec![ContentBlock::text(message)]),
                                    "shell command not approved",
                                ),
                                crate::core::execution_lifecycle::StageDisposition::Applied,
                            ));
                        }
                        crate::server::shell_approval::Outcome::NotNeeded => Arc::from(Vec::new()),
                    },
                    None => Arc::from(Vec::new()),
                };
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
                let primitive = crate::server::shell_approval::SHELL_GRANT
                    .scope(
                        shell_grant,
                        crate::server::native_receipts::NATIVE_RECEIPT_CAPTURE.scope(
                            self.native_receipt_capture.clone(),
                            crate::core::context_kernel::bridge::runtime::KERNEL_PLANNING_HANDOFF
                                .scope(call.kernel_handoff.clone(), dispatch),
                        ),
                    )
                    .await;
                self.server.record_tool_usage(
                    &call.name,
                    dispatch_start,
                    match &primitive {
                        Ok(primitive) => primitive.failure(),
                        Err(error) => Some(super::pipeline::error_failure(error)),
                    },
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
            // Under protection a result is cached only together with the
            // lineage of the dispatch that produced it; unknown lineage is
            // not cached at all.
            let bindable = !crate::core::context_admission::stored::source_authority_required()
                || replay_lineage::bind_cache_entry(&cache_lineage_id(&key));
            if bindable {
                cache_call_result(global_response_cache(), key, output);
            }
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
#[path = "guarded_tests.rs"]
mod tests;
