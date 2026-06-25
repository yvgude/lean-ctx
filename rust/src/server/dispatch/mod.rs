use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData;
use serde_json::Value;

use crate::server::helpers::get_str;
use crate::server::tool_trait::{McpTool, ShellOutcome, ToolContext, ToolOutput};
use crate::tools::LeanCtxServer;

impl LeanCtxServer {
    /// Returns (`output_text`, `saved_tokens`, `shell_outcome`). `saved_tokens` > 0
    /// indicates the tool already applied internal compression (shell engine,
    /// cache deltas, etc.). `shell_outcome` is `Some` for shell-executing tools
    /// so the caller can populate MCP error metadata (#389).
    pub(super) async fn dispatch_tool(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<(String, usize, Option<ShellOutcome>), ErrorData> {
        fn format_rate_limited(
            tool: &str,
            agent_id: &str,
            retry_after_ms: u64,
            args: Option<&serde_json::Map<String, Value>>,
        ) -> String {
            let as_json = get_str(args, "format").as_deref() == Some("json");
            if as_json {
                serde_json::json!({
                    "error": "rate_limited",
                    "tool": tool,
                    "agent_id": agent_id,
                    "retry_after_ms": retry_after_ms,
                })
                .to_string()
            } else {
                format!("[RATE LIMITED] tool={tool} retry_after_ms={retry_after_ms}")
            }
        }

        let agent_id = self
            .agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        {
            if let crate::core::a2a::rate_limiter::RateLimitResult::Limited { retry_after_ms } =
                crate::core::a2a::rate_limiter::check_rate_limit(&agent_id, name)
            {
                return Ok((
                    format_rate_limited(name, &agent_id, retry_after_ms, args),
                    0,
                    None,
                ));
            }
        }

        match name {
            "ctx_call" => {
                let inner = get_str(args, "name")
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                if inner == "ctx_call" {
                    return Err(ErrorData::invalid_params(
                        "ctx_call cannot invoke itself",
                        None,
                    ));
                }

                let arg_map = match args.and_then(|m| m.get("arguments")) {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(map)) => Some(map.clone()),
                    Some(_) => {
                        return Err(ErrorData::invalid_params(
                            "arguments must be an object",
                            None,
                        ));
                    }
                };

                if let crate::core::a2a::rate_limiter::RateLimitResult::Limited { retry_after_ms } =
                    crate::core::a2a::rate_limiter::check_rate_limit(&agent_id, &inner)
                {
                    return Ok((
                        format_rate_limited(&inner, &agent_id, retry_after_ms, arg_map.as_ref()),
                        0,
                        None,
                    ));
                }

                let inner_role_check = crate::server::role_guard::check_tool_access(&inner);
                if let Some(denied) =
                    crate::server::role_guard::into_call_tool_result(&inner_role_check)
                {
                    let msg = denied
                        .content
                        .first()
                        .and_then(|c| c.as_text())
                        .map_or_else(|| "Blocked by role policy".to_string(), |t| t.text.clone());
                    return Ok((msg, 0, None));
                }

                if !super::WORKFLOW_PASSTHROUGH_TOOLS.contains(&inner.as_str()) {
                    let active = self.workflow.read().await.clone();
                    if let Some(run) = active {
                        if run.current == "done" || super::is_workflow_stale(&run) {
                            let mut wf = self.workflow.write().await;
                            *wf = None;
                            let _ = crate::core::workflow::clear_active();
                        } else if let Some(state) = run.spec.state(&run.current)
                            && let Some(allowed) = &state.allowed_tools
                        {
                            let ok = allowed.iter().any(|t| t == &inner);
                            if !ok {
                                let mut shown = allowed.clone();
                                shown.sort();
                                shown.truncate(30);
                                return Ok((
                                    format!(
                                        "Tool '{inner}' blocked by workflow '{}' (state: {}). Allowed: {}. Use ctx_workflow(action=\"stop\") to exit.",
                                        run.spec.name,
                                        run.current,
                                        shown.join(", ")
                                    ),
                                    0,
                                    None,
                                ));
                            }
                        }
                    }
                }

                let result = self
                    .dispatch_inner(&inner, arg_map.as_ref(), minimal)
                    .await?;
                self.record_call("ctx_call", 0, 0, Some(inner)).await;
                Ok(result)
            }
            _ => self.dispatch_inner(name, args, minimal).await,
        }
    }

    /// Dispatches a single tool via the trait-based registry.
    /// Returns (`output_text`, `saved_tokens`, `shell_outcome`).
    async fn dispatch_inner(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<(String, usize, Option<ShellOutcome>), ErrorData> {
        // #454: when the user prefers their host's native editor, lean-ctx edit
        // operations are fully disabled — refused here so neither a direct call
        // nor `ctx_call` can reach them (list_tools already hides them).
        if crate::core::config::Config::load().edit_tool_blocked(name) {
            return Ok((
                format!(
                    "[disabled] '{name}' is turned off (prefer_native_editor): use your editor's \
                     built-in edit tool. Re-enable with `lean-ctx config set prefer_native_editor false`."
                ),
                0,
                None,
            ));
        }

        if let Some(tool) = self.registry.as_ref().and_then(|r| r.get_arc(name)) {
            let empty = serde_json::Map::new();
            let args_map = args.unwrap_or(&empty);
            let project_root = {
                let session = self.session.read().await;
                session.project_root.clone().unwrap_or_default()
            };

            // Index warming removed — indexes are SQLite-backed.

            let mut resolved_paths = std::collections::HashMap::new();
            let mut path_errors: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for key in PATH_LIKE_KEYS {
                if let Some(val) = args_map.get(*key) {
                    if let Some(raw) = val.as_str() {
                        match self.resolve_path(raw).await {
                            Ok(resolved) => {
                                if !["path", "project_root", "root"].contains(key) {
                                    tracing::trace!(
                                        "[pathjail] resolved non-standard path key '{key}': {raw} -> {resolved}"
                                    );
                                }
                                resolved_paths.insert(key.to_string(), resolved);
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "[dispatch] path resolution failed for '{key}' = '{raw}': {e}"
                                );
                                path_errors.insert(key.to_string(), e);
                            }
                        }
                    } else {
                        let type_name = match val {
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::Bool(_) => "boolean",
                            serde_json::Value::Array(_) => "array",
                            serde_json::Value::Object(_) => "object",
                            serde_json::Value::Null => "null",
                            serde_json::Value::String(_) => unreachable!(),
                        };
                        path_errors.insert(
                            key.to_string(),
                            format!("{key} must be a string, got {type_name}"),
                        );
                    }
                }
            }

            let crp_mode = crate::tools::CrpMode::effective();
            let pressure_snapshot = {
                let ledger = self.ledger.read().await;
                Some(ledger.pressure())
            };
            let extra_roots = self.session.read().await.extra_roots.clone();
            let ctx = crate::server::tool_trait::ToolContext {
                project_root,
                extra_roots,
                minimal,
                resolved_paths,
                crp_mode,
                cache: Some(self.cache.clone()),
                session: Some(self.session.clone()),
                tool_calls: Some(self.tool_calls.clone()),
                agent_id: Some(self.agent_id.clone()),
                workflow: Some(self.workflow.clone()),
                ledger: Some(self.ledger.clone()),
                client_name: Some(self.client_name.clone()),
                pipeline_stats: Some(self.pipeline_stats.clone()),
                call_count: Some(self.call_count.clone()),
                autonomy: Some(self.autonomy.clone()),
                pressure_snapshot,
                path_errors,
                progress_sender: Some(self.progress_sender.clone()),
            };
            // Run the (synchronous) handler on the dedicated blocking pool under
            // a watchdog deadline (#271). `block_in_place` would pin one of the
            // few core workers and — being synchronous — cannot be interrupted
            // by `tokio::time::timeout` from the same task, so a hung handler
            // would silently swallow the JSON-RPC response and the MCP client
            // would crash with "Cannot read properties of undefined (reading
            // 'invoke')". `spawn_blocking` keeps the core workers free and lets
            // the watchdog always return a response.
            let output = self.run_tool_handler(name, tool, args_map, ctx).await?;

            if output.changed
                && let Some(peer) = self.peer.read().await.as_ref()
            {
                super::notifications::send_tools_list_changed(peer).await;
            }

            let headers_only =
                crate::core::config::ResponseVerbosity::effective().is_headers_only();
            let header_line = if headers_only {
                Some(output.to_header_line(name))
            } else {
                None
            };

            let output_token_estimate = crate::core::tokens::count_tokens(&output.text) as u32;

            if let Some(ref path) = output.path {
                {
                    // Skip ledger record for ctx_read — it's recorded in post_dispatch
                    // with correct final token counts after terse compression.
                    if name != "ctx_read" {
                        let sent_tokens = if output.original_tokens > 0 {
                            output.original_tokens.saturating_sub(output.saved_tokens)
                        } else {
                            crate::core::tokens::count_tokens(&output.text)
                        };
                        let orig = if output.original_tokens > 0 {
                            output.original_tokens
                        } else {
                            sent_tokens
                        };
                        let mode_str = output.mode.as_deref().unwrap_or("full");
                        let mut ledger = self.ledger.write().await;
                        ledger.record(path, mode_str, orig, sent_tokens);
                        ledger.save_debounced();
                    }
                }
                self.record_call_with_path(
                    name,
                    output.original_tokens,
                    output.saved_tokens,
                    output.mode,
                    Some(path),
                )
                .await;
            } else {
                self.record_call(
                    name,
                    output.original_tokens,
                    output.saved_tokens,
                    output.mode,
                )
                .await;
            }

            let agent_id = self
                .agent_id
                .read()
                .await
                .clone()
                .unwrap_or_else(|| "unknown".into());
            let role = crate::core::roles::active_role_name();
            {
                let input_hash = crate::core::audit_trail::hash_input(args_map);
                crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
                    agent_id: agent_id.clone(),
                    tool: name.to_string(),
                    action: None,
                    input_hash,
                    output_tokens: output_token_estimate,
                    role: role.clone(),
                    event_type: crate::core::audit_trail::AuditEventType::ToolCall,
                });
            }

            let saved = output.saved_tokens;
            let raw_text = header_line.unwrap_or(output.text);
            let final_text = crate::core::output_sanitizer::sanitize(&raw_text);

            // Context immune system: scan for prompt-injection patterns in tool output.
            let injection_signals = crate::core::output_sanitizer::detect_injection(&final_text);
            if !injection_signals.is_empty() {
                tracing::warn!(
                    tool = name,
                    signals = injection_signals.len(),
                    "prompt-injection patterns detected in tool output"
                );
                crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
                    agent_id: agent_id.clone(),
                    tool: name.to_string(),
                    action: None,
                    input_hash: String::new(),
                    output_tokens: 0,
                    role: role.clone(),
                    event_type: crate::core::audit_trail::AuditEventType::SecurityViolation,
                });
            }

            let reference_enabled = std::env::var("LEAN_CTX_REFERENCE_RESULTS").map_or_else(
                |_| crate::core::config::Config::load().reference_results,
                |v| v == "1" || v == "true",
            );

            // An explicit file read must always return its content — never a
            // stored-reference stub — so the agent can edit against the lines. The
            // firewall already exempts reads; the reference-results path honours the
            // same rule via `is_protected_read` (otherwise enabling reference_results
            // silently turns `ctx_read` into an un-editable "Output stored …" preview).
            if reference_enabled
                && !crate::core::firewall::is_protected_read(name)
                && final_text.len() > REFERENCE_THRESHOLD
            {
                let ref_id = super::reference_store::store(final_text.clone());
                let mut preview_end = final_text.len().min(200);
                while preview_end > 0 && !final_text.is_char_boundary(preview_end) {
                    preview_end -= 1;
                }
                let summary = format!(
                    "[Reference: {ref_id}] Output stored ({} chars, ~{} tokens). Resolve: /v1/references/{ref_id}\nPreview: {}...",
                    final_text.len(),
                    final_text.len() / 4,
                    &final_text[..preview_end]
                );
                // The outcome must survive the reference-store substitution —
                // a failed shell command stays a failure even when its output
                // is delivered out-of-band (#389).
                return Ok((summary, saved, output.shell_outcome));
            }

            return Ok((final_text, saved, output.shell_outcome));
        }

        Err(ErrorData::invalid_params(
            format!("Unknown tool: {name}"),
            None,
        ))
    }

    /// Execute a (synchronous) tool handler on the blocking pool under a
    /// watchdog deadline (#271).
    ///
    /// The handler is `Send + 'static` (it owns an `Arc<dyn McpTool>`, a cloned
    /// arg map and the `ToolContext`), so it runs via `spawn_blocking` on the
    /// dedicated blocking-thread pool. That keeps the few core async workers free
    /// to keep driving the stdio JSON-RPC loop, and — crucially — lets the
    /// watchdog `timeout` actually fire (a synchronous `block_in_place` on the
    /// same task can never be timed out, because the task's own timer cannot be
    /// polled while it blocks).
    async fn run_tool_handler(
        &self,
        name: &str,
        tool: Arc<dyn McpTool>,
        args_map: &serde_json::Map<String, Value>,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let args_owned = args_map.clone();
        let join = tokio::task::spawn_blocking(move || tool.handle(&args_owned, &ctx));
        Self::watchdog_join(name, join, Self::handler_watchdog(name)).await
    }

    /// Await a blocking handler's join handle, enforcing an optional watchdog.
    ///
    /// On timeout the join handle is abandoned (the blocking-pool thread keeps
    /// running but never touches the response path again) and a clean error is
    /// returned, so the MCP client always receives a JSON-RPC reply. A handler
    /// panic is isolated on the blocking pool and surfaced as an error too — the
    /// server process stays alive either way.
    async fn watchdog_join(
        name: &str,
        join: tokio::task::JoinHandle<Result<ToolOutput, ErrorData>>,
        watchdog: Option<Duration>,
    ) -> Result<ToolOutput, ErrorData> {
        let Some(limit) = watchdog else {
            return Self::unwrap_join(name, join.await);
        };
        match tokio::time::timeout(limit, join).await {
            Ok(joined) => Self::unwrap_join(name, joined),
            Err(_elapsed) => {
                crate::core::io_health::record_freeze();
                tracing::error!(
                    tool = name,
                    timeout_secs = limit.as_secs(),
                    "tool watchdog fired — abandoning blocking handler to keep the MCP server responsive (#271)"
                );
                Err(ErrorData::internal_error(
                    format!(
                        "tool '{name}' exceeded its {}s watchdog and was abandoned. \
                         The MCP server is still running — retry or narrow the request.",
                        limit.as_secs()
                    ),
                    None,
                ))
            }
        }
    }

    /// Collapse a `spawn_blocking` join result into the handler result.
    /// A `JoinError` (handler panic) becomes a clean error instead of crashing
    /// the request task.
    fn unwrap_join(
        name: &str,
        joined: Result<Result<ToolOutput, ErrorData>, tokio::task::JoinError>,
    ) -> Result<ToolOutput, ErrorData> {
        match joined {
            Ok(inner) => inner,
            Err(join_err) => {
                tracing::error!(
                    tool = name,
                    is_panic = join_err.is_panic(),
                    "tool handler did not complete (panic isolated on the blocking pool)"
                );
                Err(ErrorData::internal_error(
                    format!(
                        "tool '{name}' failed unexpectedly. The MCP server is still running \
                         — retry or use a different approach."
                    ),
                    None,
                ))
            }
        }
    }

    /// Watchdog deadline for a single tool handler, or `None` to disable it.
    ///
    /// `ctx_shell` / `ctx_execute` run arbitrary user commands (builds, long
    /// test suites) and already enforce their own command timeouts, so a generic
    /// watchdog would wrongly abort a legitimate long-running command. Every
    /// other tool is bounded so a hang can never swallow the JSON-RPC response.
    /// Tunable via `LEAN_CTX_TOOL_TIMEOUT_SECS` (`0` disables the watchdog).
    fn handler_watchdog(name: &str) -> Option<Duration> {
        if super::is_shell_tool_name(name) {
            return None;
        }
        watchdog_from_secs(read_watchdog_secs())
    }
}

/// Read the configured watchdog budget in seconds (defaults to
/// [`DEFAULT_TOOL_TIMEOUT_SECS`]). Kept separate from the policy so the pure
/// `secs -> Option<Duration>` mapping stays trivially testable.
fn read_watchdog_secs() -> u64 {
    std::env::var("LEAN_CTX_TOOL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
}

/// Map a watchdog budget in seconds to a duration; `0` disables the watchdog.
fn watchdog_from_secs(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

const REFERENCE_THRESHOLD: usize = 4000;

/// Default per-tool watchdog budget (#271). Long enough that no legitimate
/// read/search/graph call ever hits it, short enough that a hang degrades to a
/// clean error instead of a dropped request.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 120;

const PATH_LIKE_KEYS: &[&str] = &[
    "path",
    "project_root",
    "root",
    "file",
    "directory",
    "dir",
    "target",
    "source",
    "destination",
    "old_path",
    "new_path",
    "from",
    "to",
    "base_path",
    "config_path",
    "output",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_like_keys_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for key in PATH_LIKE_KEYS {
            assert!(seen.insert(*key), "duplicate PATH_LIKE_KEYS entry: {key}");
        }
    }

    #[test]
    fn path_like_keys_includes_primary_keys() {
        for primary in &["path", "project_root", "root"] {
            assert!(
                PATH_LIKE_KEYS.contains(primary),
                "primary key '{primary}' missing from PATH_LIKE_KEYS"
            );
        }
    }

    #[test]
    fn path_like_keys_all_non_empty() {
        for key in PATH_LIKE_KEYS {
            assert!(!key.is_empty(), "PATH_LIKE_KEYS contains empty string");
        }
        assert!(
            PATH_LIKE_KEYS.len() >= 3,
            "PATH_LIKE_KEYS must have at least the 3 primary keys"
        );
    }

    #[test]
    fn watchdog_disabled_for_long_running_shell_tools() {
        assert!(
            LeanCtxServer::handler_watchdog("ctx_shell").is_none(),
            "ctx_shell runs arbitrary user commands and must not be watchdog-bounded"
        );
        assert!(
            LeanCtxServer::handler_watchdog("ctx_execute").is_none(),
            "ctx_execute must not be watchdog-bounded"
        );
    }

    #[test]
    fn watchdog_from_secs_zero_disables() {
        assert!(watchdog_from_secs(0).is_none());
    }

    #[test]
    fn watchdog_from_secs_positive_maps_to_duration() {
        assert_eq!(watchdog_from_secs(5).unwrap().as_secs(), 5);
        assert_eq!(
            watchdog_from_secs(DEFAULT_TOOL_TIMEOUT_SECS)
                .unwrap()
                .as_secs(),
            120
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_returns_error_on_hung_handler() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // A hung handler must surface as a clean error (not a dropped request),
        // which is the core #271 guarantee. The stop flag lets the simulated
        // hang exit promptly after the assertion so the runtime shuts down fast.
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_in = stop.clone();
        let join = tokio::task::spawn_blocking(move || {
            for _ in 0..400 {
                if stop_in.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(ToolOutput::simple("late".to_string()))
        });
        let start = std::time::Instant::now();
        let result =
            LeanCtxServer::watchdog_join("mock", join, Some(Duration::from_millis(200))).await;
        let elapsed = start.elapsed();
        stop.store(true, Ordering::Relaxed);
        assert!(result.is_err(), "hung handler must surface as an error");
        assert!(
            elapsed < Duration::from_secs(2),
            "watchdog must fire promptly, took {elapsed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_passes_through_fast_handler() {
        let join = tokio::task::spawn_blocking(|| Ok(ToolOutput::simple("ok".to_string())));
        let result = LeanCtxServer::watchdog_join("mock", join, Some(Duration::from_secs(5))).await;
        assert_eq!(result.unwrap().text, "ok");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_none_awaits_handler_to_completion() {
        let join = tokio::task::spawn_blocking(|| Ok(ToolOutput::simple("ok".to_string())));
        let result = LeanCtxServer::watchdog_join("ctx_shell", join, None).await;
        assert_eq!(result.unwrap().text, "ok");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_panic_becomes_error_not_crash() {
        let join: tokio::task::JoinHandle<Result<ToolOutput, ErrorData>> =
            tokio::task::spawn_blocking(|| panic!("simulated handler panic"));
        let result = LeanCtxServer::watchdog_join("mock", join, Some(Duration::from_secs(5))).await;
        assert!(
            result.is_err(),
            "a handler panic must surface as an error; the server process survives"
        );
    }
}
