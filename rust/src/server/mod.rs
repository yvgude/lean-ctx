mod dispatch;
mod execute;
pub mod helpers;

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData;

use crate::tools::{CrpMode, LeanCtxServer};

use helpers::{canonical_args_string, extract_search_pattern_from_command, get_str, md5_hex};

impl ServerHandler for LeanCtxServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder().enable_tools().build();

        let instructions = crate::instructions::build_instructions(self.crp_mode);

        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("lean-ctx", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let name = request.client_info.name.clone();
        tracing::info!("MCP client connected: {:?}", name);
        *self.client_name.write().await = name.clone();

        tokio::task::spawn_blocking(|| {
            if let Some(home) = dirs::home_dir() {
                let _ = crate::rules_inject::inject_all_rules(&home);
            }
            crate::hooks::refresh_installed_hooks();
            crate::core::version_check::check_background();
        });

        let instructions =
            crate::instructions::build_instructions_with_client(self.crp_mode, &name);
        let capabilities = ServerCapabilities::builder().enable_tools().build();

        Ok(InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("lean-ctx", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let all_tools = if crate::tool_defs::is_lazy_mode() {
            crate::tool_defs::lazy_tool_defs()
        } else if std::env::var("LEAN_CTX_UNIFIED").is_ok()
            && std::env::var("LEAN_CTX_FULL_TOOLS").is_err()
        {
            crate::tool_defs::unified_tool_defs()
        } else {
            crate::tool_defs::granular_tool_defs()
        };

        let disabled = crate::core::config::Config::load().disabled_tools_effective();
        let tools = if disabled.is_empty() {
            all_tools
        } else {
            all_tools
                .into_iter()
                .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
                .collect()
        };

        let tools = {
            let active = self.workflow.read().await.clone();
            if let Some(run) = active {
                if let Some(state) = run.spec.state(&run.current) {
                    if let Some(allowed) = &state.allowed_tools {
                        let mut allow: std::collections::HashSet<&str> =
                            allowed.iter().map(|s| s.as_str()).collect();
                        allow.insert("ctx");
                        allow.insert("ctx_workflow");
                        return Ok(ListToolsResult {
                            tools: tools
                                .into_iter()
                                .filter(|t| allow.contains(t.name.as_ref()))
                                .collect(),
                            ..Default::default()
                        });
                    }
                }
            }
            tools
        };

        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.check_idle_expiry().await;

        let original_name = request.name.as_ref().to_string();
        let (resolved_name, resolved_args) = if original_name == "ctx" {
            let sub = request
                .arguments
                .as_ref()
                .and_then(|a| a.get("tool"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
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
        let args = &resolved_args;

        if name != "ctx_workflow" {
            let active = self.workflow.read().await.clone();
            if let Some(run) = active {
                if let Some(state) = run.spec.state(&run.current) {
                    if let Some(allowed) = &state.allowed_tools {
                        let allowed_ok = allowed.iter().any(|t| t == name) || name == "ctx";
                        if !allowed_ok {
                            let mut shown = allowed.clone();
                            shown.sort();
                            shown.truncate(30);
                            return Ok(CallToolResult::success(vec![Content::text(format!(
                                "Tool '{name}' blocked by workflow '{}' (state: {}). Allowed ({} shown): {}",
                                run.spec.name,
                                run.current,
                                shown.len(),
                                shown.join(", ")
                            ))]));
                        }
                    }
                }
            }
        }

        let auto_context = {
            let task = {
                let session = self.session.read().await;
                session.task.as_ref().map(|t| t.description.clone())
            };
            let project_root = {
                let session = self.session.read().await;
                session.project_root.clone()
            };
            let mut cache = self.cache.write().await;
            crate::tools::autonomy::session_lifecycle_pre_hook(
                &self.autonomy,
                name,
                &mut cache,
                task.as_deref(),
                project_root.as_deref(),
                self.crp_mode,
            )
        };

        let throttle_result = {
            let fp = args
                .as_ref()
                .map(|a| {
                    crate::core::loop_detection::LoopDetector::fingerprint(
                        &serde_json::Value::Object(a.clone()),
                    )
                })
                .unwrap_or_default();
            let mut detector = self.loop_detector.write().await;

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
                let search_pattern = args.as_ref().and_then(|a| {
                    a.get("pattern")
                        .or_else(|| a.get("query"))
                        .and_then(|v| v.as_str())
                });
                let shell_pattern = if is_search_shell {
                    args.as_ref()
                        .and_then(|a| a.get("command"))
                        .and_then(|v| v.as_str())
                        .and_then(extract_search_pattern_from_command)
                } else {
                    None
                };
                let pat = search_pattern.or(shell_pattern.as_deref());
                detector.record_search(name, &fp, pat)
            } else {
                detector.record_call(name, &fp)
            }
        };

        if throttle_result.level == crate::core::loop_detection::ThrottleLevel::Blocked {
            let msg = throttle_result.message.unwrap_or_default();
            return Ok(CallToolResult::success(vec![Content::text(msg)]));
        }

        let throttle_warning =
            if throttle_result.level == crate::core::loop_detection::ThrottleLevel::Reduced {
                throttle_result.message.clone()
            } else {
                None
            };

        let tool_start = std::time::Instant::now();
        let result_text = self.dispatch_tool(name, args).await?;

        let mut result_text = result_text;

        {
            let config = crate::core::config::Config::load();
            let density = crate::core::config::OutputDensity::effective(&config.output_density);
            result_text = crate::core::protocol::compress_output(&result_text, &density);
        }

        if let Some(ctx) = auto_context {
            result_text = format!("{ctx}\n\n{result_text}");
        }

        if let Some(warning) = throttle_warning {
            result_text = format!("{result_text}\n\n{warning}");
        }

        if name == "ctx_read" {
            let read_path = self
                .resolve_path_or_passthrough(&get_str(args, "path").unwrap_or_default())
                .await;
            let project_root = {
                let session = self.session.read().await;
                session.project_root.clone()
            };
            let mut cache = self.cache.write().await;
            let enrich = crate::tools::autonomy::enrich_after_read(
                &self.autonomy,
                &mut cache,
                &read_path,
                project_root.as_deref(),
            );
            if let Some(hint) = enrich.related_hint {
                result_text = format!("{result_text}\n{hint}");
            }

            crate::tools::autonomy::maybe_auto_dedup(&self.autonomy, &mut cache);
        }

        if name == "ctx_shell" {
            let cmd = get_str(args, "command").unwrap_or_default();
            let output_tokens = crate::core::tokens::count_tokens(&result_text);
            let calls = self.tool_calls.read().await;
            let last_original = calls.last().map(|c| c.original_tokens).unwrap_or(0);
            drop(calls);
            if let Some(hint) = crate::tools::autonomy::shell_efficiency_hint(
                &self.autonomy,
                &cmd,
                last_original,
                output_tokens,
            ) {
                result_text = format!("{result_text}\n{hint}");
            }
        }

        {
            let input = canonical_args_string(args);
            let input_md5 = md5_hex(&input);
            let output_md5 = md5_hex(&result_text);
            let action = get_str(args, "action");
            let agent_id = self.agent_id.read().await.clone();
            let client_name = self.client_name.read().await.clone();
            let mut explicit_intent: Option<(
                crate::core::intent_protocol::IntentRecord,
                Option<String>,
                String,
            )> = None;

            {
                let empty_args = serde_json::Map::new();
                let args_map = args.as_ref().unwrap_or(&empty_args);
                let mut session = self.session.write().await;
                session.record_tool_receipt(
                    name,
                    action.as_deref(),
                    &input_md5,
                    &output_md5,
                    agent_id.as_deref(),
                    Some(&client_name),
                );

                if let Some(intent) = crate::core::intent_protocol::infer_from_tool_call(
                    name,
                    action.as_deref(),
                    args_map,
                    session.project_root.as_deref(),
                ) {
                    let is_explicit =
                        intent.source == crate::core::intent_protocol::IntentSource::Explicit;
                    let root = session.project_root.clone();
                    let sid = session.id.clone();
                    session.record_intent(intent.clone());
                    if is_explicit {
                        explicit_intent = Some((intent, root, sid));
                    }
                }
                if session.should_save() {
                    let _ = session.save();
                }
            }

            if let Some((intent, root, session_id)) = explicit_intent {
                crate::core::intent_protocol::apply_side_effects(
                    &intent,
                    root.as_deref(),
                    &session_id,
                );
            }

            if self.autonomy.is_enabled() {
                let (calls, project_root) = {
                    let session = self.session.read().await;
                    (session.stats.total_tool_calls, session.project_root.clone())
                };

                if let Some(root) = project_root {
                    if crate::tools::autonomy::should_auto_consolidate(&self.autonomy, calls) {
                        let root_clone = root.clone();
                        tokio::task::spawn_blocking(move || {
                            let _ = crate::core::consolidation_engine::consolidate_latest(
                                &root_clone,
                                crate::core::consolidation_engine::ConsolidationBudgets::default(),
                            );
                        });
                    }
                }
            }

            let agent_key = agent_id.unwrap_or_else(|| "unknown".to_string());
            let input_tokens = crate::core::tokens::count_tokens(&input) as u64;
            let output_tokens = crate::core::tokens::count_tokens(&result_text) as u64;
            let mut store = crate::core::a2a::cost_attribution::CostStore::load();
            store.record_tool_call(&agent_key, &client_name, name, input_tokens, output_tokens);
            let _ = store.save();
        }

        let skip_checkpoint = matches!(
            name,
            "ctx_compress"
                | "ctx_metrics"
                | "ctx_benchmark"
                | "ctx_analyze"
                | "ctx_cache"
                | "ctx_discover"
                | "ctx_dedup"
                | "ctx_session"
                | "ctx_knowledge"
                | "ctx_agent"
                | "ctx_share"
                | "ctx_wrapped"
                | "ctx_overview"
                | "ctx_preload"
                | "ctx_cost"
                | "ctx_gain"
                | "ctx_heatmap"
                | "ctx_task"
                | "ctx_impact"
                | "ctx_architecture"
                | "ctx_workflow"
        );

        if !skip_checkpoint && self.increment_and_check() {
            if let Some(checkpoint) = self.auto_checkpoint().await {
                let combined = format!(
                    "{result_text}\n\n--- AUTO CHECKPOINT (every {} calls) ---\n{checkpoint}",
                    self.checkpoint_interval
                );
                return Ok(CallToolResult::success(vec![Content::text(combined)]));
            }
        }

        let tool_duration_ms = tool_start.elapsed().as_millis() as u64;
        if tool_duration_ms > 100 {
            LeanCtxServer::append_tool_call_log(
                name,
                tool_duration_ms,
                0,
                0,
                None,
                &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            );
        }

        let current_count = self.call_count.load(std::sync::atomic::Ordering::Relaxed);
        if current_count > 0 && current_count.is_multiple_of(100) {
            std::thread::spawn(crate::cloud_sync::cloud_background_tasks);
        }

        Ok(CallToolResult::success(vec![Content::text(result_text)]))
    }
}

pub fn build_instructions_for_test(crp_mode: CrpMode) -> String {
    crate::instructions::build_instructions(crp_mode)
}

pub fn build_claude_code_instructions_for_test() -> String {
    crate::instructions::claude_code_instructions()
}

pub fn tool_descriptions_for_test() -> Vec<(&'static str, &'static str)> {
    crate::tool_defs::list_all_tool_defs()
        .into_iter()
        .map(|(name, desc, _)| (name, desc))
        .collect()
}

pub fn tool_schemas_json_for_test() -> String {
    crate::tool_defs::list_all_tool_defs()
        .iter()
        .map(|(name, _, schema)| format!("{}: {}", name, schema))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_unified_tool_count() {
        let tools = crate::tool_defs::unified_tool_defs();
        assert_eq!(tools.len(), 5, "Expected 5 unified tools");
    }

    #[test]
    fn test_granular_tool_count() {
        let tools = crate::tool_defs::granular_tool_defs();
        assert!(tools.len() >= 25, "Expected at least 25 granular tools");
    }

    #[test]
    fn disabled_tools_filters_list() {
        let all = crate::tool_defs::granular_tool_defs();
        let total = all.len();
        let disabled = ["ctx_graph".to_string(), "ctx_agent".to_string()];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total - 2);
        assert!(!filtered.iter().any(|t| t.name.as_ref() == "ctx_graph"));
        assert!(!filtered.iter().any(|t| t.name.as_ref() == "ctx_agent"));
    }

    #[test]
    fn empty_disabled_tools_returns_all() {
        let all = crate::tool_defs::granular_tool_defs();
        let total = all.len();
        let disabled: Vec<String> = vec![];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total);
    }

    #[test]
    fn misspelled_disabled_tool_is_silently_ignored() {
        let all = crate::tool_defs::granular_tool_defs();
        let total = all.len();
        let disabled = ["ctx_nonexistent_tool".to_string()];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total);
    }
}
