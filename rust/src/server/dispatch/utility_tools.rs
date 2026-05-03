use rmcp::ErrorData;
use serde_json::Value;

use crate::server::helpers::{get_bool, get_int, get_str, get_str_array};
use crate::tools::LeanCtxServer;

impl LeanCtxServer {
    pub(crate) async fn dispatch_utility_tools(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<String, ErrorData> {
        Ok(match name {
            "ctx_tree" => {
                let path = self
                    .resolve_path(&get_str(args, "path").unwrap_or_else(|| ".".to_string()))
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let depth = get_int(args, "depth").unwrap_or(3) as usize;
                let show_hidden = get_bool(args, "show_hidden").unwrap_or(false);
                let (result, original) = crate::tools::ctx_tree::handle(&path, depth, show_hidden);
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);
                self.record_call_with_path("ctx_tree", original, saved, None, Some(&path))
                    .await;
                let savings_note = if !minimal && saved > 0 {
                    format!("\n[saved {saved} tokens vs native ls]")
                } else {
                    String::new()
                };
                format!("{result}{savings_note}")
            }
            "ctx_compress" => {
                let include_sigs = get_bool(args, "include_signatures").unwrap_or(true);
                let cache = self.cache.read().await;
                let result = crate::tools::ctx_compress::handle(
                    &cache,
                    include_sigs,
                    crate::tools::CrpMode::effective(),
                );
                drop(cache);
                self.record_call("ctx_compress", 0, 0, None).await;
                result
            }
            "ctx_benchmark" => {
                let path = match get_str(args, "path") {
                    Some(p) => self
                        .resolve_path(&p)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?,
                    None => return Err(ErrorData::invalid_params("path is required", None)),
                };
                let action = get_str(args, "action").unwrap_or_default();
                let result = if action == "project" {
                    let fmt = get_str(args, "format").unwrap_or_default();
                    let bench = crate::core::benchmark::run_project_benchmark(&path);
                    match fmt.as_str() {
                        "json" => crate::core::benchmark::format_json(&bench),
                        "markdown" | "md" => crate::core::benchmark::format_markdown(&bench),
                        _ => crate::core::benchmark::format_terminal(&bench),
                    }
                } else {
                    crate::tools::ctx_benchmark::handle(&path, crate::tools::CrpMode::effective())
                };
                self.record_call("ctx_benchmark", 0, 0, None).await;
                result
            }
            "ctx_metrics" => {
                let cache = self.cache.read().await;
                let calls = self.tool_calls.read().await;
                let mut result = crate::tools::ctx_metrics::handle(
                    &cache,
                    &calls,
                    crate::tools::CrpMode::effective(),
                );
                drop(cache);
                drop(calls);
                let stats = self.pipeline_stats.read().await;
                if stats.runs > 0 {
                    result.push_str("\n\n--- PIPELINE METRICS ---\n");
                    result.push_str(&stats.format_summary());
                }
                drop(stats);
                self.record_call("ctx_metrics", 0, 0, None).await;
                result
            }
            "ctx_analyze" => {
                let path = match get_str(args, "path") {
                    Some(p) => self
                        .resolve_path(&p)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?,
                    None => return Err(ErrorData::invalid_params("path is required", None)),
                };
                let result =
                    crate::tools::ctx_analyze::handle(&path, crate::tools::CrpMode::effective());
                self.record_call_with_path("ctx_analyze", 0, 0, None, Some(&path))
                    .await;
                result
            }
            "ctx_discover" => {
                let limit = get_int(args, "limit").unwrap_or(15) as usize;
                let history = crate::cli::load_shell_history_pub();
                let result = crate::tools::ctx_discover::discover_from_history(&history, limit);
                self.record_call("ctx_discover", 0, 0, None).await;
                result
            }
            "ctx_dedup" => {
                let action = get_str(args, "action").unwrap_or_default();
                if action == "apply" {
                    let mut cache = self.cache.write().await;
                    let result = crate::tools::ctx_dedup::handle_action(&mut cache, &action);
                    drop(cache);
                    self.record_call("ctx_dedup", 0, 0, None).await;
                    result
                } else {
                    let cache = self.cache.read().await;
                    let result = crate::tools::ctx_dedup::handle(&cache);
                    drop(cache);
                    self.record_call("ctx_dedup", 0, 0, None).await;
                    result
                }
            }
            "ctx_intent" => {
                let query = get_str(args, "query")
                    .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
                let root = get_str(args, "project_root").unwrap_or_else(|| ".".to_string());
                let mut cache = self.cache.write().await;
                let output = crate::tools::ctx_intent::handle(
                    &mut cache,
                    &query,
                    &root,
                    crate::tools::CrpMode::effective(),
                );
                drop(cache);
                {
                    let mut session = self.session.write().await;
                    session.set_task(&query, Some("intent"));
                }
                self.record_call("ctx_intent", 0, 0, Some("semantic".to_string()))
                    .await;
                output
            }
            "ctx_response" => {
                let text = get_str(args, "text")
                    .ok_or_else(|| ErrorData::invalid_params("text is required", None))?;
                let output =
                    crate::tools::ctx_response::handle(&text, crate::tools::CrpMode::effective());
                self.record_call("ctx_response", 0, 0, None).await;
                output
            }
            "ctx_context" => {
                let cache = self.cache.read().await;
                let turn = self.call_count.load(std::sync::atomic::Ordering::Relaxed);
                let result = crate::tools::ctx_context::handle_status(
                    &cache,
                    turn,
                    crate::tools::CrpMode::effective(),
                );
                drop(cache);
                self.record_call("ctx_context", 0, 0, None).await;
                result
            }
            "ctx_graph" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let path = match get_str(args, "path") {
                    Some(p) if action == "diagram" => Some(p),
                    Some(p) => Some(
                        self.resolve_path(&p)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?,
                    ),
                    None => None,
                };
                let root = self
                    .resolve_path(&get_str(args, "project_root").unwrap_or_else(|| ".".to_string()))
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let depth = get_int(args, "depth").map(|d| d as usize);
                let kind = get_str(args, "kind");
                let crp_mode = crate::tools::CrpMode::effective();
                let action_for_record = action.clone();
                let mut cache = self.cache.write().await;
                let result = crate::tools::ctx_graph::handle(
                    &action,
                    path.as_deref(),
                    &root,
                    &mut cache,
                    crp_mode,
                    depth,
                    kind.as_deref(),
                );
                drop(cache);
                self.record_call("ctx_graph", 0, 0, Some(action_for_record))
                    .await;
                result
            }
            "ctx_cache" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let mut cache = self.cache.write().await;
                let result = match action.as_str() {
                    "status" => {
                        let entries = cache.get_all_entries();
                        if entries.is_empty() {
                            "Cache empty — no files tracked.".to_string()
                        } else {
                            let mut lines = vec![format!("Cache: {} file(s)", entries.len())];
                            for (path, entry) in &entries {
                                let fref = cache
                                    .file_ref_map()
                                    .get(*path)
                                    .map_or("F?", std::string::String::as_str);
                                lines.push(format!(
                                    "  {fref}={} [{}L, {}t, read {}x]",
                                    crate::core::protocol::shorten_path(path),
                                    entry.line_count,
                                    entry.original_tokens,
                                    entry.read_count
                                ));
                            }
                            lines.join("\n")
                        }
                    }
                    "clear" => {
                        let count = cache.clear();
                        format!("Cache cleared — {count} file(s) removed. Next ctx_read will return full content.")
                    }
                    "invalidate" => {
                        let path = match get_str(args, "path") {
                            Some(p) => self
                                .resolve_path(&p)
                                .await
                                .map_err(|e| ErrorData::invalid_params(e, None))?,
                            None => {
                                return Err(ErrorData::invalid_params(
                                    "path is required for invalidate",
                                    None,
                                ))
                            }
                        };
                        if cache.invalidate(&path) {
                            format!(
                                "Invalidated cache for {}. Next ctx_read will return full content.",
                                crate::core::protocol::shorten_path(&path)
                            )
                        } else {
                            format!(
                                "{} was not in cache.",
                                crate::core::protocol::shorten_path(&path)
                            )
                        }
                    }
                    _ => "Unknown action. Use: status, clear, invalidate".to_string(),
                };
                drop(cache);
                self.record_call("ctx_cache", 0, 0, Some(action)).await;
                result
            }
            "ctx_overview" => {
                let task = get_str(args, "task");
                let resolved_path = if let Some(p) = get_str(args, "path") {
                    Some(
                        self.resolve_path(&p)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?,
                    )
                } else {
                    let session = self.session.read().await;
                    session.project_root.clone()
                };
                let cache = self.cache.read().await;
                let crp_mode = crate::tools::CrpMode::effective();
                let result = crate::tools::ctx_overview::handle(
                    &cache,
                    task.as_deref(),
                    resolved_path.as_deref(),
                    crp_mode,
                );
                drop(cache);
                self.record_call("ctx_overview", 0, 0, Some("overview".to_string()))
                    .await;
                result
            }
            "ctx_preload" => {
                let task = get_str(args, "task").unwrap_or_default();
                let resolved_path = if let Some(p) = get_str(args, "path") {
                    Some(
                        self.resolve_path(&p)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?,
                    )
                } else {
                    let session = self.session.read().await;
                    session.project_root.clone()
                };
                let mut cache = self.cache.write().await;
                let mut result = crate::tools::ctx_preload::handle(
                    &mut cache,
                    &task,
                    resolved_path.as_deref(),
                    crate::tools::CrpMode::effective(),
                );
                drop(cache);

                {
                    let mut session = self.session.write().await;
                    if session.active_structured_intent.is_none()
                        || session
                            .active_structured_intent
                            .as_ref()
                            .is_none_or(|i| i.confidence < 0.6)
                    {
                        session.set_task(&task, Some("preload"));
                    }
                }

                let session = self.session.read().await;
                if let Some(ref intent) = session.active_structured_intent {
                    let ledger = self.ledger.read().await;
                    if !ledger.entries.is_empty() {
                        let known: Vec<String> = session
                            .files_touched
                            .iter()
                            .map(|f| f.path.clone())
                            .collect();
                        let deficit =
                            crate::core::context_deficit::detect_deficit(&ledger, intent, &known);
                        if !deficit.suggested_files.is_empty() {
                            result.push_str("\n\n--- SUGGESTED FILES ---");
                            for s in &deficit.suggested_files {
                                result.push_str(&format!(
                                    "\n  {} ({:?}, ~{} tok, mode: {})",
                                    s.path, s.reason, s.estimated_tokens, s.recommended_mode
                                ));
                            }
                        }

                        let pressure = ledger.pressure();
                        if pressure.utilization > 0.7 {
                            let plan = ledger.reinjection_plan(intent, 0.6);
                            if !plan.actions.is_empty() {
                                result.push_str("\n\n--- REINJECTION PLAN ---");
                                result.push_str(&format!(
                                    "\n  Context pressure: {:.0}% -> target: 60%",
                                    pressure.utilization * 100.0
                                ));
                                for a in &plan.actions {
                                    result.push_str(&format!(
                                        "\n  {} : {} -> {} (frees ~{} tokens)",
                                        a.path, a.current_mode, a.new_mode, a.tokens_freed
                                    ));
                                }
                                result.push_str(&format!(
                                    "\n  Total freeable: {} tokens",
                                    plan.total_tokens_freed
                                ));
                            }
                        }
                    }
                }
                drop(session);

                self.record_call("ctx_preload", 0, 0, Some("preload".to_string()))
                    .await;
                result
            }
            "ctx_prefetch" => {
                let root = if let Some(r) = get_str(args, "root") {
                    self.resolve_path(&r)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?
                } else {
                    let session = self.session.read().await;
                    session
                        .project_root
                        .clone()
                        .unwrap_or_else(|| ".".to_string())
                };
                let task = get_str(args, "task");
                let changed_files = get_str_array(args, "changed_files");
                let budget_tokens =
                    get_int(args, "budget_tokens").map_or(3000, |n| n.max(0) as usize);
                let max_files = get_int(args, "max_files").map(|n| n.max(1) as usize);

                let mut resolved_changed: Option<Vec<String>> = None;
                if let Some(files) = changed_files {
                    let mut v = Vec::with_capacity(files.len());
                    for p in files {
                        v.push(
                            self.resolve_path(&p)
                                .await
                                .map_err(|e| ErrorData::invalid_params(e, None))?,
                        );
                    }
                    resolved_changed = Some(v);
                }

                let mut cache = self.cache.write().await;
                let result = crate::tools::ctx_prefetch::handle(
                    &mut cache,
                    &root,
                    task.as_deref(),
                    resolved_changed.as_deref(),
                    budget_tokens,
                    max_files,
                    crate::tools::CrpMode::effective(),
                );
                drop(cache);
                self.record_call("ctx_prefetch", 0, 0, Some("prefetch".to_string()))
                    .await;
                result
            }
            "ctx_wrapped" => {
                let period = get_str(args, "period").unwrap_or_else(|| "week".to_string());
                let result = crate::tools::ctx_wrapped::handle(&period);
                self.record_call("ctx_wrapped", 0, 0, Some(period)).await;
                result
            }
            "ctx_semantic_search" => {
                let query = get_str(args, "query")
                    .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
                let path = self
                    .resolve_path(&get_str(args, "path").unwrap_or_else(|| ".".to_string()))
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let top_k = get_int(args, "top_k").unwrap_or(10) as usize;
                let action = get_str(args, "action").unwrap_or_default();
                let mode = get_str(args, "mode");
                let languages = get_str_array(args, "languages");
                let path_glob = get_str(args, "path_glob");
                let workspace = get_bool(args, "workspace").unwrap_or(false);
                let artifacts = get_bool(args, "artifacts").unwrap_or(false);

                #[cfg(feature = "qdrant")]
                {
                    let mode_effective = mode
                        .as_deref()
                        .unwrap_or("hybrid")
                        .trim()
                        .to_ascii_lowercase();
                    if action != "reindex"
                        && !artifacts
                        && matches!(mode_effective.as_str(), "dense" | "hybrid")
                        && matches!(
                            crate::core::dense_backend::DenseBackendKind::try_from_env(),
                            Ok(crate::core::dense_backend::DenseBackendKind::Qdrant)
                        )
                    {
                        let value = format!(
                            "tool=ctx_semantic_search mode={mode_effective} workspace={workspace}"
                        );
                        let mut session = self.session.write().await;
                        session.record_manual_evidence("remote:qdrant_query", Some(&value));
                    }
                }

                let result = if action == "reindex" {
                    if artifacts {
                        crate::tools::ctx_semantic_search::handle_reindex_artifacts(
                            &path, workspace,
                        )
                    } else {
                        crate::tools::ctx_semantic_search::handle_reindex(&path)
                    }
                } else {
                    crate::tools::ctx_semantic_search::handle(
                        &query,
                        &path,
                        top_k,
                        crate::tools::CrpMode::effective(),
                        languages.as_deref(),
                        path_glob.as_deref(),
                        mode.as_deref(),
                        Some(workspace),
                        Some(artifacts),
                    )
                };
                self.record_call("ctx_semantic_search", 0, 0, Some("semantic".to_string()))
                    .await;
                result
            }
            "ctx_symbol" => {
                let sym_name = get_str(args, "name")
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                let file = get_str(args, "file");
                let kind = get_str(args, "kind");
                let session = self.session.read().await;
                let project_root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
                drop(session);
                let (result, original) = crate::tools::ctx_symbol::handle(
                    &sym_name,
                    file.as_deref(),
                    kind.as_deref(),
                    &project_root,
                );
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);
                self.record_call_with_path("ctx_symbol", original, saved, kind, file.as_deref())
                    .await;
                result
            }
            "ctx_graph_diagram" => {
                let file = get_str(args, "file");
                let depth = get_int(args, "depth").map(|d| d as usize);
                let kind = get_str(args, "kind");
                let session = self.session.read().await;
                let project_root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
                drop(session);
                let mut cache = self.cache.write().await;
                let graph_output = crate::tools::ctx_graph::handle(
                    "diagram",
                    file.as_deref(),
                    &project_root,
                    &mut cache,
                    crate::tools::CrpMode::effective(),
                    depth,
                    kind.as_deref(),
                );
                drop(cache);
                let result = format!(
                    "[DEPRECATED] Use ctx_graph with action='diagram' (supports same args).\n{graph_output}"
                );
                self.record_call("ctx_graph_diagram", 0, 0, kind).await;
                result
            }
            "ctx_expand" => {
                let args_val = args.map_or(serde_json::Value::Null, |m| {
                    serde_json::Value::Object(m.clone())
                });
                let result = crate::tools::ctx_expand::handle(&args_val);
                self.record_call("ctx_expand", 0, 0, None).await;
                result
            }
            "ctx_routes" => {
                let method = get_str(args, "method");
                let path_prefix = get_str(args, "path");
                let session = self.session.read().await;
                let project_root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
                drop(session);
                let result = crate::tools::ctx_routes::handle(
                    method.as_deref(),
                    path_prefix.as_deref(),
                    &project_root,
                );
                self.record_call("ctx_routes", 0, 0, None).await;
                result
            }
            "ctx_compress_memory" => {
                let path = self
                    .resolve_path(
                        &get_str(args, "path")
                            .ok_or_else(|| ErrorData::invalid_params("path is required", None))?,
                    )
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let result = crate::tools::ctx_compress_memory::handle(&path);
                self.record_call("ctx_compress_memory", 0, 0, None).await;
                result
            }
            "ctx_callers" => {
                let symbol = get_str(args, "symbol")
                    .ok_or_else(|| ErrorData::invalid_params("symbol is required", None))?;
                let file = get_str(args, "file");
                let session = self.session.read().await;
                let project_root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
                drop(session);
                let result =
                    crate::tools::ctx_callers::handle(&symbol, file.as_deref(), &project_root);
                self.record_call("ctx_callers", 0, 0, None).await;
                result
            }
            "ctx_callgraph" => {
                let symbol = get_str(args, "symbol")
                    .ok_or_else(|| ErrorData::invalid_params("symbol is required", None))?;
                let direction = get_str(args, "direction").unwrap_or_else(|| "callers".to_string());
                let file = get_str(args, "file");
                let session = self.session.read().await;
                let project_root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
                drop(session);
                let result = crate::tools::ctx_callgraph::handle(
                    &symbol,
                    file.as_deref(),
                    &project_root,
                    &direction,
                );
                self.record_call("ctx_callgraph", 0, 0, Some(direction))
                    .await;
                result
            }
            "ctx_review" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let path = get_str(args, "path");
                let depth = get_int(args, "depth").map(|d| d as usize);
                let session = self.session.read().await;
                let project_root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
                drop(session);
                let result = crate::tools::ctx_review::handle(
                    &action,
                    path.as_deref(),
                    &project_root,
                    depth,
                );
                self.record_call("ctx_review", 0, 0, Some(action)).await;
                result
            }
            "ctx_pack" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let base = get_str(args, "base");
                let format = get_str(args, "format");
                let depth = get_int(args, "depth").map(|d| d as usize);
                let diff = get_str(args, "diff");

                let project_root = if let Some(p) = get_str(args, "project_root") {
                    self.resolve_path(&p)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?
                } else {
                    let session = self.session.read().await;
                    let project_root = session
                        .project_root
                        .clone()
                        .unwrap_or_else(|| ".".to_string());
                    drop(session);
                    project_root
                };

                let result = crate::tools::ctx_pack::handle(
                    &action,
                    &project_root,
                    base.as_deref(),
                    format.as_deref(),
                    depth,
                    diff.as_deref(),
                );
                self.record_call("ctx_pack", 0, 0, Some(action)).await;
                result
            }
            "ctx_callees" => {
                let symbol = get_str(args, "symbol")
                    .ok_or_else(|| ErrorData::invalid_params("symbol is required", None))?;
                let file = get_str(args, "file");
                let session = self.session.read().await;
                let project_root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
                drop(session);
                let result =
                    crate::tools::ctx_callees::handle(&symbol, file.as_deref(), &project_root);
                self.record_call("ctx_callees", 0, 0, None).await;
                result
            }
            "ctx_outline" => {
                let path = self
                    .resolve_path(
                        &get_str(args, "path")
                            .ok_or_else(|| ErrorData::invalid_params("path is required", None))?,
                    )
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let kind = get_str(args, "kind");
                let (result, original) = crate::tools::ctx_outline::handle(&path, kind.as_deref());
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);
                self.record_call_with_path("ctx_outline", original, saved, kind, Some(&path))
                    .await;
                result
            }
            "ctx_cost" => {
                let action = get_str(args, "action").unwrap_or_else(|| "report".to_string());
                let agent_id = get_str(args, "agent_id");
                let limit = get_int(args, "limit").map(|n| n as usize);
                let result = crate::tools::ctx_cost::handle(&action, agent_id.as_deref(), limit);
                self.record_call("ctx_cost", 0, 0, Some(action)).await;
                result
            }
            "ctx_discover_tools" => {
                let query = get_str(args, "query").unwrap_or_default();
                let result = crate::tool_defs::discover_tools(&query);
                self.record_call("ctx_discover_tools", 0, 0, None).await;
                result
            }
            "ctx_gain" => {
                let action = get_str(args, "action").unwrap_or_else(|| "status".to_string());
                let period = get_str(args, "period");
                let model = get_str(args, "model");
                let limit = get_int(args, "limit").map(|n| n as usize);
                let result = crate::tools::ctx_gain::handle(
                    &action,
                    period.as_deref(),
                    model.as_deref(),
                    limit,
                );
                self.record_call("ctx_gain", 0, 0, Some(action)).await;
                result
            }
            "ctx_feedback" => {
                let action = get_str(args, "action").unwrap_or_else(|| "report".to_string());
                let limit = get_int(args, "limit").map_or(500, |n| n.max(1) as usize);
                match action.as_str() {
                    "record" => {
                        let current_agent_id = { self.agent_id.read().await.clone() };
                        let agent_id = get_str(args, "agent_id").or(current_agent_id);
                        let agent_id = agent_id.ok_or_else(|| {
                            ErrorData::invalid_params(
                                "agent_id is required (or register an agent via project_root detection first)",
                                None,
                            )
                        })?;

                        let (ctx_read_last_mode, ctx_read_modes) = {
                            let calls = self.tool_calls.read().await;
                            let mut last: Option<String> = None;
                            let mut modes: std::collections::BTreeMap<String, u64> =
                                std::collections::BTreeMap::new();
                            for rec in calls.iter().rev().take(50) {
                                if rec.tool != "ctx_read" {
                                    continue;
                                }
                                if let Some(m) = rec.mode.as_ref() {
                                    *modes.entry(m.clone()).or_insert(0) += 1;
                                    if last.is_none() {
                                        last = Some(m.clone());
                                    }
                                }
                            }
                            (last, if modes.is_empty() { None } else { Some(modes) })
                        };

                        let llm_input_tokens =
                            get_int(args, "llm_input_tokens").ok_or_else(|| {
                                ErrorData::invalid_params("llm_input_tokens is required", None)
                            })?;
                        let llm_output_tokens =
                            get_int(args, "llm_output_tokens").ok_or_else(|| {
                                ErrorData::invalid_params("llm_output_tokens is required", None)
                            })?;
                        if llm_input_tokens <= 0 || llm_output_tokens <= 0 {
                            return Err(ErrorData::invalid_params(
                                "llm_input_tokens and llm_output_tokens must be > 0",
                                None,
                            ));
                        }

                        let ev = crate::core::llm_feedback::LlmFeedbackEvent {
                            agent_id,
                            intent: get_str(args, "intent"),
                            model: get_str(args, "model"),
                            llm_input_tokens: llm_input_tokens as u64,
                            llm_output_tokens: llm_output_tokens as u64,
                            latency_ms: get_int(args, "latency_ms").map(|n| n.max(0) as u64),
                            note: get_str(args, "note"),
                            ctx_read_last_mode,
                            ctx_read_modes,
                            timestamp: chrono::Local::now().to_rfc3339(),
                        };
                        let result = crate::tools::ctx_feedback::record(&ev)
                            .unwrap_or_else(|e| format!("Error recording feedback: {e}"));
                        self.record_call("ctx_feedback", 0, 0, Some(action)).await;
                        result
                    }
                    "status" => {
                        let result = crate::tools::ctx_feedback::status();
                        self.record_call("ctx_feedback", 0, 0, Some(action)).await;
                        result
                    }
                    "json" => {
                        let result = crate::tools::ctx_feedback::json(limit);
                        self.record_call("ctx_feedback", 0, 0, Some(action)).await;
                        result
                    }
                    "reset" => {
                        let result = crate::tools::ctx_feedback::reset();
                        self.record_call("ctx_feedback", 0, 0, Some(action)).await;
                        result
                    }
                    _ => {
                        let result = crate::tools::ctx_feedback::report(limit);
                        self.record_call("ctx_feedback", 0, 0, Some(action)).await;
                        result
                    }
                }
            }
            "ctx_heatmap" => {
                let action = get_str(args, "action").unwrap_or_else(|| "status".to_string());
                let path = get_str(args, "path");
                let result = crate::tools::ctx_heatmap::handle(&action, path.as_deref());
                self.record_call("ctx_heatmap", 0, 0, Some(action)).await;
                result
            }
            "ctx_impact" => {
                let action = get_str(args, "action").unwrap_or_else(|| "analyze".to_string());
                let path = get_str(args, "path");
                let depth = get_int(args, "depth").map(|d| d as usize);
                let root = if let Some(r) = get_str(args, "root") {
                    r
                } else {
                    let session = self.session.read().await;
                    session
                        .project_root
                        .clone()
                        .unwrap_or_else(|| ".".to_string())
                };
                let result =
                    crate::tools::ctx_impact::handle(&action, path.as_deref(), &root, depth);
                self.record_call("ctx_impact", 0, 0, Some(action)).await;
                result
            }
            "ctx_architecture" => {
                let action = get_str(args, "action").unwrap_or_else(|| "overview".to_string());
                let path = get_str(args, "path");
                let root = if let Some(r) = get_str(args, "root") {
                    r
                } else {
                    let session = self.session.read().await;
                    session
                        .project_root
                        .clone()
                        .unwrap_or_else(|| ".".to_string())
                };
                let result =
                    crate::tools::ctx_architecture::handle(&action, path.as_deref(), &root);
                self.record_call("ctx_architecture", 0, 0, Some(action))
                    .await;
                result
            }
            _ => {
                return Err(ErrorData::invalid_params(
                    format!("Unknown tool: {name}"),
                    None,
                ));
            }
        })
    }
}
