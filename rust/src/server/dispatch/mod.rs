use rmcp::ErrorData;
use serde_json::Value;

use crate::server::helpers::get_str;
use crate::tools::LeanCtxServer;

impl LeanCtxServer {
    /// Returns (output_text, saved_tokens). saved_tokens > 0 indicates the tool
    /// already applied internal compression (shell engine, cache deltas, etc.).
    pub(super) async fn dispatch_tool(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<(String, usize), ErrorData> {
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
                        ))
                    }
                };

                if let crate::core::a2a::rate_limiter::RateLimitResult::Limited { retry_after_ms } =
                    crate::core::a2a::rate_limiter::check_rate_limit(&agent_id, &inner)
                {
                    return Ok((
                        format_rate_limited(&inner, &agent_id, retry_after_ms, arg_map.as_ref()),
                        0,
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
                    return Ok((msg, 0));
                }

                if !super::WORKFLOW_PASSTHROUGH_TOOLS.contains(&inner.as_str()) {
                    let active = self.workflow.read().await.clone();
                    if let Some(run) = active {
                        if run.current == "done" || super::is_workflow_stale(&run) {
                            let mut wf = self.workflow.write().await;
                            *wf = None;
                            let _ = crate::core::workflow::clear_active();
                        } else if let Some(state) = run.spec.state(&run.current) {
                            if let Some(allowed) = &state.allowed_tools {
                                let ok = allowed.iter().any(|t| t == &inner);
                                if !ok {
                                    let mut shown = allowed.clone();
                                    shown.sort();
                                    shown.truncate(30);
                                    return Ok((format!(
                                        "Tool '{inner}' blocked by workflow '{}' (state: {}). Allowed: {}. Use ctx_workflow(action=\"stop\") to exit.",
                                        run.spec.name,
                                        run.current,
                                        shown.join(", ")
                                    ), 0));
                                }
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
    /// Returns (output_text, saved_tokens).
    async fn dispatch_inner(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<(String, usize), ErrorData> {
        if let Some(tool) = self.registry.as_ref().and_then(|r| r.get(name)) {
            let empty = serde_json::Map::new();
            let args_map = args.unwrap_or(&empty);
            let project_root = {
                let session = self.session.read().await;
                session.project_root.clone().unwrap_or_default()
            };

            // Lazy, demand-driven index warming (#152): only tools that actually
            // need a prebuilt index trigger a (background, once-per-root) scan.
            // The first heavy pre-warm also warms any configured extra roots once.
            if !project_root.is_empty()
                && crate::core::index_orchestrator::ensure_warm_for_tool(&project_root, name)
            {
                let extra_roots = self.session.read().await.extra_roots.clone();
                if !extra_roots.is_empty() {
                    let primary = project_root.clone();
                    std::thread::spawn(move || {
                        crate::core::index_orchestrator::ensure_extra_roots_background(
                            &primary,
                            &extra_roots,
                        );
                    });
                }
            }

            let mut resolved_paths = std::collections::HashMap::new();
            let mut path_errors: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for key in PATH_LIKE_KEYS {
                if let Some(val) = args_map.get(*key) {
                    if let Some(raw) = val.as_str() {
                        match self.resolve_path(raw).await {
                            Ok(resolved) => {
                                if !["path", "project_root", "root"].contains(key) {
                                    tracing::trace!("[pathjail] resolved non-standard path key '{key}': {raw} -> {resolved}");
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
            let ctx = crate::server::tool_trait::ToolContext {
                project_root,
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
                bm25_cache: Some(self.bm25_cache.clone()),
                progress_sender: Some(self.progress_sender.clone()),
            };
            let output = tokio::task::block_in_place(|| tool.handle(args_map, &ctx))?;

            if output.changed {
                if let Some(peer) = self.peer.read().await.as_ref() {
                    super::notifications::send_tools_list_changed(peer).await;
                }
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
                return Ok((summary, saved));
            }

            return Ok((final_text, saved));
        }

        Err(ErrorData::invalid_params(
            format!("Unknown tool: {name}"),
            None,
        ))
    }
}

const REFERENCE_THRESHOLD: usize = 4000;

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
}
