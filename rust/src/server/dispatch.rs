use rmcp::ErrorData;
use serde_json::Value;

use super::execute::execute_command_in;
use super::helpers::*;
use crate::tools::LeanCtxServer;

impl LeanCtxServer {
    pub(super) async fn dispatch_tool(
        &self,
        name: &str,
        args: &Option<serde_json::Map<String, Value>>,
    ) -> Result<String, ErrorData> {
        Ok(match name {
            "ctx_read" => {
                let path = match get_str(args, "path") {
                    Some(p) => self
                        .resolve_path(&p)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?,
                    None => return Err(ErrorData::invalid_params("path is required", None)),
                };
                let current_task = {
                    let session = self.session.read().await;
                    session.task.as_ref().map(|t| t.description.clone())
                };
                let task_ref = current_task.as_deref();
                let mut mode = match get_str(args, "mode") {
                    Some(m) => m,
                    None => {
                        let cache = self.cache.read().await;
                        crate::tools::ctx_smart_read::select_mode_with_task(&cache, &path, task_ref)
                    }
                };
                let fresh = get_bool(args, "fresh").unwrap_or(false);
                let start_line = get_int(args, "start_line");
                if let Some(sl) = start_line {
                    let sl = sl.max(1_i64);
                    mode = format!("lines:{sl}-999999");
                }
                let stale = self.is_prompt_cache_stale().await;
                let effective_mode = LeanCtxServer::upgrade_mode_if_stale(&mode, stale).to_string();
                let read_start = std::time::Instant::now();
                let mut cache = self.cache.write().await;
                let (output, resolved_mode) = if fresh {
                    crate::tools::ctx_read::handle_fresh_with_task_resolved(
                        &mut cache,
                        &path,
                        &effective_mode,
                        self.crp_mode,
                        task_ref,
                    )
                } else {
                    crate::tools::ctx_read::handle_with_task_resolved(
                        &mut cache,
                        &path,
                        &effective_mode,
                        self.crp_mode,
                        task_ref,
                    )
                };
                let stale_note = if effective_mode != mode {
                    format!("[cache stale, {mode}→{effective_mode}]\n")
                } else {
                    String::new()
                };
                let original = cache.get(&path).map_or(0, |e| e.original_tokens);
                let output_tokens = crate::core::tokens::count_tokens(&output);
                let saved = original.saturating_sub(output_tokens);
                let is_cache_hit = output.contains(" cached ");
                let output = format!("{stale_note}{output}");
                let file_ref = cache.file_ref_map().get(&path).cloned();
                drop(cache);
                let mut ensured_root: Option<String> = None;
                {
                    let mut session = self.session.write().await;
                    session.touch_file(&path, file_ref.as_deref(), &resolved_mode, original);
                    if is_cache_hit {
                        session.record_cache_hit();
                    }
                    if session.active_structured_intent.is_none()
                        && session.files_touched.len() >= 2
                    {
                        let touched: Vec<String> = session
                            .files_touched
                            .iter()
                            .map(|f| f.path.clone())
                            .collect();
                        let inferred =
                            crate::core::intent_engine::StructuredIntent::from_file_patterns(
                                &touched,
                            );
                        if inferred.confidence >= 0.4 {
                            session.active_structured_intent = Some(inferred);
                        }
                    }
                    let root_missing = session
                        .project_root
                        .as_deref()
                        .map(|r| r.trim().is_empty())
                        .unwrap_or(true);
                    if root_missing {
                        if let Some(root) = crate::core::protocol::detect_project_root(&path) {
                            session.project_root = Some(root.clone());
                            ensured_root = Some(root.clone());
                            let mut current = self.agent_id.write().await;
                            if current.is_none() {
                                let mut registry =
                                    crate::core::agents::AgentRegistry::load_or_create();
                                registry.cleanup_stale(24);
                                let role = std::env::var("LEAN_CTX_AGENT_ROLE").ok();
                                let id = registry.register("mcp", role.as_deref(), &root);
                                let _ = registry.save();
                                *current = Some(id);
                            }
                        }
                    }
                }
                if let Some(root) = ensured_root.as_deref() {
                    crate::core::index_orchestrator::ensure_all_background(root);
                }
                self.record_call("ctx_read", original, saved, Some(resolved_mode.clone()))
                    .await;
                crate::core::heatmap::record_file_access(&path, original, saved);
                {
                    let mut ledger = self.ledger.write().await;
                    ledger.record(&path, &resolved_mode, original, output_tokens);
                    ledger.save();
                }
                {
                    let mut stats = self.pipeline_stats.write().await;
                    stats.record_single(
                        crate::core::pipeline::LayerKind::Compression,
                        original,
                        output_tokens,
                        read_start.elapsed(),
                    );
                    stats.save();
                }
                {
                    let sig =
                        crate::core::mode_predictor::FileSignature::from_path(&path, original);
                    let density = if output_tokens > 0 {
                        original as f64 / output_tokens as f64
                    } else {
                        1.0
                    };
                    let outcome = crate::core::mode_predictor::ModeOutcome {
                        mode: resolved_mode.clone(),
                        tokens_in: original,
                        tokens_out: output_tokens,
                        density: density.min(1.0),
                    };
                    let mut predictor = crate::core::mode_predictor::ModePredictor::new();
                    predictor.record(sig, outcome);
                    predictor.save();

                    let ext = std::path::Path::new(&path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_string();
                    let thresholds = crate::core::adaptive_thresholds::thresholds_for_path(&path);
                    let cache = self.cache.read().await;
                    let stats = cache.get_stats();
                    let feedback_outcome = crate::core::feedback::CompressionOutcome {
                        session_id: format!("{}", std::process::id()),
                        language: ext,
                        entropy_threshold: thresholds.bpe_entropy,
                        jaccard_threshold: thresholds.jaccard,
                        total_turns: stats.total_reads as u32,
                        tokens_saved: saved as u64,
                        tokens_original: original as u64,
                        cache_hits: stats.cache_hits as u32,
                        total_reads: stats.total_reads as u32,
                        task_completed: true,
                        timestamp: chrono::Local::now().to_rfc3339(),
                    };
                    drop(cache);
                    let mut store = crate::core::feedback::FeedbackStore::load();
                    store.record_outcome(feedback_outcome);
                }
                output
            }
            "ctx_multi_read" => {
                let raw_paths = get_str_array(args, "paths")
                    .ok_or_else(|| ErrorData::invalid_params("paths array is required", None))?;
                let mut paths = Vec::with_capacity(raw_paths.len());
                for p in raw_paths {
                    paths.push(
                        self.resolve_path(&p)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?,
                    );
                }
                let mode = get_str(args, "mode").unwrap_or_else(|| "full".to_string());
                let current_task = {
                    let session = self.session.read().await;
                    session.task.as_ref().map(|t| t.description.clone())
                };
                let mut cache = self.cache.write().await;
                let output = crate::tools::ctx_multi_read::handle_with_task(
                    &mut cache,
                    &paths,
                    &mode,
                    self.crp_mode,
                    current_task.as_deref(),
                );
                let mut total_original: usize = 0;
                for path in &paths {
                    total_original = total_original
                        .saturating_add(cache.get(path).map(|e| e.original_tokens).unwrap_or(0));
                }
                let tokens = crate::core::tokens::count_tokens(&output);
                drop(cache);
                self.record_call(
                    "ctx_multi_read",
                    total_original,
                    total_original.saturating_sub(tokens),
                    Some(mode),
                )
                .await;
                output
            }
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
                self.record_call("ctx_tree", original, saved, None).await;
                let savings_note = if saved > 0 {
                    format!("\n[saved {saved} tokens vs native ls]")
                } else {
                    String::new()
                };
                format!("{result}{savings_note}")
            }
            "ctx_shell" => {
                let command = get_str(args, "command")
                    .ok_or_else(|| ErrorData::invalid_params("command is required", None))?;

                if let Some(rejection) = crate::tools::ctx_shell::validate_command(&command) {
                    self.record_call("ctx_shell", 0, 0, None).await;
                    return Ok(rejection);
                }

                let explicit_cwd = get_str(args, "cwd");
                let effective_cwd = {
                    let session = self.session.read().await;
                    session.effective_cwd(explicit_cwd.as_deref())
                };

                let ensured_root = {
                    let mut session = self.session.write().await;
                    session.update_shell_cwd(&command);
                    let root_missing = session
                        .project_root
                        .as_deref()
                        .map(|r| r.trim().is_empty())
                        .unwrap_or(true);
                    if !root_missing {
                        None
                    } else {
                        let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string());
                        crate::core::protocol::detect_project_root(&effective_cwd).and_then(|r| {
                            if home.as_deref() == Some(r.as_str()) {
                                None
                            } else {
                                session.project_root = Some(r.clone());
                                Some(r)
                            }
                        })
                    }
                };
                if let Some(root) = ensured_root.as_deref() {
                    crate::core::index_orchestrator::ensure_all_background(root);
                    let mut current = self.agent_id.write().await;
                    if current.is_none() {
                        let mut registry = crate::core::agents::AgentRegistry::load_or_create();
                        registry.cleanup_stale(24);
                        let role = std::env::var("LEAN_CTX_AGENT_ROLE").ok();
                        let id = registry.register("mcp", role.as_deref(), root);
                        let _ = registry.save();
                        *current = Some(id);
                    }
                }

                let raw = get_bool(args, "raw").unwrap_or(false)
                    || std::env::var("LEAN_CTX_DISABLED").is_ok();
                let cmd_clone = command.clone();
                let cwd_clone = effective_cwd.clone();
                let crp_mode = self.crp_mode;

                let (result_out, original, saved, tee_hint) =
                    tokio::task::spawn_blocking(move || {
                        let (output, _real_exit_code) = execute_command_in(&cmd_clone, &cwd_clone);

                        // Perform heavy token counting and compression here, off the main thread
                        if raw {
                            let tokens = crate::core::tokens::count_tokens(&output);
                            (output, tokens, 0, String::new())
                        } else {
                            let result =
                                crate::tools::ctx_shell::handle(&cmd_clone, &output, crp_mode);
                            let original = crate::core::tokens::count_tokens(&output);
                            let sent = crate::core::tokens::count_tokens(&result);
                            let saved = original.saturating_sub(sent);

                            let cfg = crate::core::config::Config::load();
                            let tee_hint = match cfg.tee_mode {
                                crate::core::config::TeeMode::Always => {
                                    crate::shell::save_tee(&cmd_clone, &output)
                                        .map(|p| format!("\n[full output: {p}]"))
                                        .unwrap_or_default()
                                }
                                crate::core::config::TeeMode::Failures
                                    if !output.trim().is_empty()
                                        && (output.contains("error")
                                            || output.contains("Error")
                                            || output.contains("ERROR")) =>
                                {
                                    crate::shell::save_tee(&cmd_clone, &output)
                                        .map(|p| format!("\n[full output: {p}]"))
                                        .unwrap_or_default()
                                }
                                _ => String::new(),
                            };

                            // Gotcha detection logic (moved inside blocking task)
                            // Note: We don't have access to session here easily,
                            // but we can pass the relevant data if needed.
                            // For now, focusing on the core perf fix.

                            (result, original, saved, tee_hint)
                        }
                    })
                    .await
                    .unwrap_or_else(|e| {
                        (
                            format!("ERROR: shell task failed: {e}"),
                            0,
                            0,
                            String::new(),
                        )
                    });

                self.record_call("ctx_shell", original, saved, None).await;

                let savings_note = if !raw && saved > 0 {
                    format!("\n[saved {saved} tokens vs native Shell]")
                } else {
                    String::new()
                };

                format!("{result_out}{savings_note}{tee_hint}")
            }
            "ctx_search" => {
                let pattern = get_str(args, "pattern")
                    .ok_or_else(|| ErrorData::invalid_params("pattern is required", None))?;
                let path = self
                    .resolve_path(&get_str(args, "path").unwrap_or_else(|| ".".to_string()))
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let ext = get_str(args, "ext");
                let max = get_int(args, "max_results").unwrap_or(20) as usize;
                let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);
                let crp = self.crp_mode;
                let respect = !no_gitignore;
                let search_result = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    tokio::task::spawn_blocking(move || {
                        crate::tools::ctx_search::handle(
                            &pattern,
                            &path,
                            ext.as_deref(),
                            max,
                            crp,
                            respect,
                        )
                    }),
                )
                .await;
                let (result, original) = match search_result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        return Err(ErrorData::internal_error(
                            format!("search task failed: {e}"),
                            None,
                        ))
                    }
                    Err(_) => {
                        let msg = "ctx_search timed out after 30s. Try narrowing the search:\n\
                                   • Use a more specific pattern\n\
                                   • Specify ext= to limit file types\n\
                                   • Specify a subdirectory in path=";
                        self.record_call("ctx_search", 0, 0, None).await;
                        return Ok(msg.to_string());
                    }
                };
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);
                self.record_call("ctx_search", original, saved, None).await;
                let savings_note = if saved > 0 {
                    format!("\n[saved {saved} tokens vs native Grep]")
                } else {
                    String::new()
                };
                format!("{result}{savings_note}")
            }
            "ctx_compress" => {
                let include_sigs = get_bool(args, "include_signatures").unwrap_or(true);
                let cache = self.cache.read().await;
                let result =
                    crate::tools::ctx_compress::handle(&cache, include_sigs, self.crp_mode);
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
                    crate::tools::ctx_benchmark::handle(&path, self.crp_mode)
                };
                self.record_call("ctx_benchmark", 0, 0, None).await;
                result
            }
            "ctx_metrics" => {
                let cache = self.cache.read().await;
                let calls = self.tool_calls.read().await;
                let mut result = crate::tools::ctx_metrics::handle(&cache, &calls, self.crp_mode);
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
                let result = crate::tools::ctx_analyze::handle(&path, self.crp_mode);
                self.record_call("ctx_analyze", 0, 0, None).await;
                result
            }
            "ctx_discover" => {
                let limit = get_int(args, "limit").unwrap_or(15) as usize;
                let history = crate::cli::load_shell_history_pub();
                let result = crate::tools::ctx_discover::discover_from_history(&history, limit);
                self.record_call("ctx_discover", 0, 0, None).await;
                result
            }
            "ctx_smart_read" => {
                let path = match get_str(args, "path") {
                    Some(p) => self
                        .resolve_path(&p)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?,
                    None => return Err(ErrorData::invalid_params("path is required", None)),
                };
                let mut cache = self.cache.write().await;
                let output = crate::tools::ctx_smart_read::handle(&mut cache, &path, self.crp_mode);
                let original = cache.get(&path).map_or(0, |e| e.original_tokens);
                let tokens = crate::core::tokens::count_tokens(&output);
                drop(cache);
                self.record_call(
                    "ctx_smart_read",
                    original,
                    original.saturating_sub(tokens),
                    Some("auto".to_string()),
                )
                .await;
                output
            }
            "ctx_delta" => {
                let path = match get_str(args, "path") {
                    Some(p) => self
                        .resolve_path(&p)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?,
                    None => return Err(ErrorData::invalid_params("path is required", None)),
                };
                let mut cache = self.cache.write().await;
                let output = crate::tools::ctx_delta::handle(&mut cache, &path);
                let original = cache.get(&path).map_or(0, |e| e.original_tokens);
                let tokens = crate::core::tokens::count_tokens(&output);
                drop(cache);
                {
                    let mut session = self.session.write().await;
                    session.mark_modified(&path);
                }
                self.record_call(
                    "ctx_delta",
                    original,
                    original.saturating_sub(tokens),
                    Some("delta".to_string()),
                )
                .await;
                output
            }
            "ctx_edit" => {
                let path = match get_str(args, "path") {
                    Some(p) => self
                        .resolve_path(&p)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?,
                    None => return Err(ErrorData::invalid_params("path is required", None)),
                };
                let old_string = get_str(args, "old_string").unwrap_or_default();
                let new_string = get_str(args, "new_string")
                    .ok_or_else(|| ErrorData::invalid_params("new_string is required", None))?;
                let replace_all = args
                    .as_ref()
                    .and_then(|a| a.get("replace_all"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let create = args
                    .as_ref()
                    .and_then(|a| a.get("create"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let mut cache = self.cache.write().await;
                let output = crate::tools::ctx_edit::handle(
                    &mut cache,
                    crate::tools::ctx_edit::EditParams {
                        path: path.clone(),
                        old_string,
                        new_string,
                        replace_all,
                        create,
                    },
                );
                drop(cache);

                {
                    let mut session = self.session.write().await;
                    session.mark_modified(&path);
                }
                self.record_call("ctx_edit", 0, 0, None).await;
                output
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
            "ctx_fill" => {
                let raw_paths = get_str_array(args, "paths")
                    .ok_or_else(|| ErrorData::invalid_params("paths array is required", None))?;
                let mut paths = Vec::with_capacity(raw_paths.len());
                for p in raw_paths {
                    paths.push(
                        self.resolve_path(&p)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?,
                    );
                }
                let budget = get_int(args, "budget")
                    .ok_or_else(|| ErrorData::invalid_params("budget is required", None))?
                    as usize;
                let task = get_str(args, "task");
                let mut cache = self.cache.write().await;
                let output = crate::tools::ctx_fill::handle(
                    &mut cache,
                    &paths,
                    budget,
                    self.crp_mode,
                    task.as_deref(),
                );
                drop(cache);
                self.record_call("ctx_fill", 0, 0, Some(format!("budget:{budget}")))
                    .await;
                output
            }
            "ctx_intent" => {
                let query = get_str(args, "query")
                    .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
                let root = get_str(args, "project_root").unwrap_or_else(|| ".".to_string());
                let mut cache = self.cache.write().await;
                let output =
                    crate::tools::ctx_intent::handle(&mut cache, &query, &root, self.crp_mode);
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
                let output = crate::tools::ctx_response::handle(&text, self.crp_mode);
                self.record_call("ctx_response", 0, 0, None).await;
                output
            }
            "ctx_context" => {
                let cache = self.cache.read().await;
                let turn = self.call_count.load(std::sync::atomic::Ordering::Relaxed);
                let result = crate::tools::ctx_context::handle_status(&cache, turn, self.crp_mode);
                drop(cache);
                self.record_call("ctx_context", 0, 0, None).await;
                result
            }
            "ctx_graph" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let path = match get_str(args, "path") {
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
                let crp_mode = self.crp_mode;
                let action_for_record = action.clone();
                let mut cache = self.cache.write().await;
                let result = crate::tools::ctx_graph::handle(
                    &action,
                    path.as_deref(),
                    &root,
                    &mut cache,
                    crp_mode,
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
                                    .map(|s| s.as_str())
                                    .unwrap_or("F?");
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
            "ctx_session" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let value = get_str(args, "value");
                let sid = get_str(args, "session_id");
                let mut session = self.session.write().await;
                let result = crate::tools::ctx_session::handle(
                    &mut session,
                    &action,
                    value.as_deref(),
                    sid.as_deref(),
                );
                drop(session);
                self.record_call("ctx_session", 0, 0, Some(action)).await;
                result
            }
            "ctx_knowledge" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let category = get_str(args, "category");
                let key = get_str(args, "key");
                let value = get_str(args, "value");
                let query = get_str(args, "query");
                let pattern_type = get_str(args, "pattern_type");
                let examples = get_str_array(args, "examples");
                let confidence: Option<f32> = args
                    .as_ref()
                    .and_then(|a| a.get("confidence"))
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32);

                let session = self.session.read().await;
                let session_id = session.id.clone();
                let project_root = session.project_root.clone().unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| "unknown".to_string())
                });
                drop(session);

                if action == "gotcha" {
                    let trigger = get_str(args, "trigger").unwrap_or_default();
                    let resolution = get_str(args, "resolution").unwrap_or_default();
                    let severity = get_str(args, "severity").unwrap_or_default();
                    let cat = category.as_deref().unwrap_or("convention");

                    if trigger.is_empty() || resolution.is_empty() {
                        self.record_call("ctx_knowledge", 0, 0, Some(action)).await;
                        return Ok(
                            "ERROR: trigger and resolution are required for gotcha action"
                                .to_string(),
                        );
                    }

                    let mut store = crate::core::gotcha_tracker::GotchaStore::load(&project_root);
                    let msg = match store.report_gotcha(
                        &trigger,
                        &resolution,
                        cat,
                        &severity,
                        &session_id,
                    ) {
                        Some(gotcha) => {
                            let conf = (gotcha.confidence * 100.0) as u32;
                            let label = gotcha.category.short_label();
                            format!("Gotcha recorded: [{label}] {trigger} (confidence: {conf}%)")
                        }
                        None => format!(
                            "Gotcha noted: {trigger} (evicted by higher-confidence entries)"
                        ),
                    };
                    let _ = store.save(&project_root);
                    self.record_call("ctx_knowledge", 0, 0, Some(action)).await;
                    return Ok(msg);
                }

                let result = crate::tools::ctx_knowledge::handle(
                    &project_root,
                    &action,
                    category.as_deref(),
                    key.as_deref(),
                    value.as_deref(),
                    query.as_deref(),
                    &session_id,
                    pattern_type.as_deref(),
                    examples,
                    confidence,
                );
                self.record_call("ctx_knowledge", 0, 0, Some(action)).await;
                result
            }
            "ctx_agent" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let agent_type = get_str(args, "agent_type");
                let role = get_str(args, "role");
                let message = get_str(args, "message");
                let category = get_str(args, "category");
                let to_agent = get_str(args, "to_agent");
                let status = get_str(args, "status");

                let session = self.session.read().await;
                let project_root = session.project_root.clone().unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| "unknown".to_string())
                });
                drop(session);

                let current_agent_id = self.agent_id.read().await.clone();
                let result = crate::tools::ctx_agent::handle(
                    &action,
                    agent_type.as_deref(),
                    role.as_deref(),
                    &project_root,
                    current_agent_id.as_deref(),
                    message.as_deref(),
                    category.as_deref(),
                    to_agent.as_deref(),
                    status.as_deref(),
                );

                if action == "register" {
                    if let Some(id) = result.split(':').nth(1) {
                        let id = id.split_whitespace().next().unwrap_or("").to_string();
                        if !id.is_empty() {
                            *self.agent_id.write().await = Some(id);
                        }
                    }

                    let agent_role = crate::core::agents::AgentRole::from_str_loose(
                        role.as_deref().unwrap_or("coder"),
                    );
                    let depth = crate::core::agents::ContextDepthConfig::for_role(agent_role);
                    let depth_hint = format!(
                        "\n[context] role={:?} preferred_mode={} max_full={} max_sig={} budget_ratio={:.0}%",
                        agent_role,
                        depth.preferred_mode,
                        depth.max_files_full,
                        depth.max_files_signatures,
                        depth.context_budget_ratio * 100.0,
                    );
                    self.record_call("ctx_agent", 0, 0, Some(action)).await;
                    return Ok(format!("{result}{depth_hint}"));
                }

                self.record_call("ctx_agent", 0, 0, Some(action)).await;
                result
            }
            "ctx_share" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let to_agent = get_str(args, "to_agent");
                let paths = get_str(args, "paths");
                let message = get_str(args, "message");

                let from_agent = self.agent_id.read().await.clone();
                let cache = self.cache.read().await;
                let result = crate::tools::ctx_share::handle(
                    &action,
                    from_agent.as_deref(),
                    to_agent.as_deref(),
                    paths.as_deref(),
                    message.as_deref(),
                    &cache,
                );
                drop(cache);

                self.record_call("ctx_share", 0, 0, Some(action)).await;
                result
            }
            "ctx_overview" => {
                let task = get_str(args, "task");
                let resolved_path = match get_str(args, "path") {
                    Some(p) => Some(
                        self.resolve_path(&p)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?,
                    ),
                    None => {
                        let session = self.session.read().await;
                        session.project_root.clone()
                    }
                };
                let cache = self.cache.read().await;
                let crp_mode = self.crp_mode;
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
                let resolved_path = match get_str(args, "path") {
                    Some(p) => Some(
                        self.resolve_path(&p)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?,
                    ),
                    None => {
                        let session = self.session.read().await;
                        session.project_root.clone()
                    }
                };
                let mut cache = self.cache.write().await;
                let mut result = crate::tools::ctx_preload::handle(
                    &mut cache,
                    &task,
                    resolved_path.as_deref(),
                    self.crp_mode,
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
                let root = match get_str(args, "root") {
                    Some(r) => self
                        .resolve_path(&r)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e, None))?,
                    None => {
                        let session = self.session.read().await;
                        session
                            .project_root
                            .clone()
                            .unwrap_or_else(|| ".".to_string())
                    }
                };
                let task = get_str(args, "task");
                let changed_files = get_str_array(args, "changed_files");
                let budget_tokens = get_int(args, "budget_tokens")
                    .map(|n| n.max(0) as usize)
                    .unwrap_or(3000);
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
                    self.crp_mode,
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
                let result = if action == "reindex" {
                    crate::tools::ctx_semantic_search::handle_reindex(&path)
                } else {
                    crate::tools::ctx_semantic_search::handle(
                        &query,
                        &path,
                        top_k,
                        self.crp_mode,
                        languages,
                        path_glob.as_deref(),
                        mode.as_deref(),
                    )
                };
                self.record_call("ctx_semantic_search", 0, 0, Some("semantic".to_string()))
                    .await;
                result
            }
            "ctx_execute" => {
                let action = get_str(args, "action").unwrap_or_default();

                let result = if action == "batch" {
                    let items_str = get_str(args, "items").ok_or_else(|| {
                        ErrorData::invalid_params("items is required for batch", None)
                    })?;
                    let items: Vec<serde_json::Value> =
                        serde_json::from_str(&items_str).map_err(|e| {
                            ErrorData::invalid_params(format!("Invalid items JSON: {e}"), None)
                        })?;
                    let batch: Vec<(String, String)> = items
                        .iter()
                        .filter_map(|item| {
                            let lang = item.get("language")?.as_str()?.to_string();
                            let code = item.get("code")?.as_str()?.to_string();
                            Some((lang, code))
                        })
                        .collect();
                    crate::tools::ctx_execute::handle_batch(&batch)
                } else if action == "file" {
                    let raw_path = get_str(args, "path").ok_or_else(|| {
                        ErrorData::invalid_params("path is required for action=file", None)
                    })?;
                    let path = self.resolve_path(&raw_path).await.map_err(|e| {
                        ErrorData::invalid_params(format!("path rejected: {e}"), None)
                    })?;
                    let intent = get_str(args, "intent");
                    crate::tools::ctx_execute::handle_file(&path, intent.as_deref())
                } else {
                    let language = get_str(args, "language")
                        .ok_or_else(|| ErrorData::invalid_params("language is required", None))?;
                    let code = get_str(args, "code")
                        .ok_or_else(|| ErrorData::invalid_params("code is required", None))?;
                    let intent = get_str(args, "intent");
                    let timeout = get_int(args, "timeout").map(|t| t as u64);
                    crate::tools::ctx_execute::handle(&language, &code, intent.as_deref(), timeout)
                };

                self.record_call("ctx_execute", 0, 0, Some(action)).await;
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
                self.record_call("ctx_symbol", original, saved, kind).await;
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
                let result = crate::tools::ctx_graph_diagram::handle(
                    file.as_deref(),
                    depth,
                    kind.as_deref(),
                    &project_root,
                );
                self.record_call("ctx_graph_diagram", 0, 0, kind).await;
                result
            }
            "ctx_expand" => {
                let args_val = args
                    .as_ref()
                    .map(|m| serde_json::Value::Object(m.clone()))
                    .unwrap_or(serde_json::Value::Null);
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
                self.record_call("ctx_outline", original, saved, kind).await;
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
                let limit = get_int(args, "limit")
                    .map(|n| n.max(1) as usize)
                    .unwrap_or(500);
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
                        let result = crate::tools::ctx_feedback::record(ev)
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
            "ctx_handoff" => {
                let action = get_str(args, "action").unwrap_or_else(|| "list".to_string());
                match action.as_str() {
                    "list" => {
                        let items = crate::core::handoff_ledger::list_ledgers();
                        let result = crate::tools::ctx_handoff::format_list(&items);
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                    "clear" => {
                        let removed =
                            crate::core::handoff_ledger::clear_ledgers().unwrap_or_default();
                        let result = crate::tools::ctx_handoff::format_clear(removed);
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                    "show" => {
                        let path = get_str(args, "path").ok_or_else(|| {
                            ErrorData::invalid_params("path is required for action=show", None)
                        })?;
                        let path = self
                            .resolve_path(&path)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?;
                        let ledger =
                            crate::core::handoff_ledger::load_ledger(std::path::Path::new(&path))
                                .map_err(|e| {
                                ErrorData::internal_error(format!("load ledger: {e}"), None)
                            })?;
                        let result = crate::tools::ctx_handoff::format_show(
                            std::path::Path::new(&path),
                            &ledger,
                        );
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                    "create" => {
                        let curated_paths = get_str_array(args, "paths").unwrap_or_default();
                        let mut curated_refs: Vec<(String, String)> = Vec::new();
                        if !curated_paths.is_empty() {
                            let mut cache = self.cache.write().await;
                            for p in curated_paths.into_iter().take(20) {
                                let abs = self
                                    .resolve_path(&p)
                                    .await
                                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                                let text = crate::tools::ctx_read::handle_with_task(
                                    &mut cache,
                                    &abs,
                                    "signatures",
                                    self.crp_mode,
                                    None,
                                );
                                curated_refs.push((abs, text));
                            }
                        }

                        let session = { self.session.read().await.clone() };
                        let active_intent = session.active_structured_intent.clone();
                        let tool_calls = { self.tool_calls.read().await.clone() };
                        let workflow = { self.workflow.read().await.clone() };
                        let agent_id = { self.agent_id.read().await.clone() };
                        let client_name = { self.client_name.read().await.clone() };
                        let project_root = session.project_root.clone();

                        let (ledger, path) = crate::core::handoff_ledger::create_ledger(
                            crate::core::handoff_ledger::CreateLedgerInput {
                                agent_id,
                                client_name: Some(client_name),
                                project_root,
                                session,
                                tool_calls,
                                workflow,
                                curated_refs,
                            },
                        )
                        .map_err(|e| {
                            ErrorData::internal_error(format!("create ledger: {e}"), None)
                        })?;

                        let ctx_ledger = self.ledger.read().await;
                        let package = crate::core::handoff_ledger::HandoffPackage::build(
                            ledger.clone(),
                            active_intent.as_ref(),
                            if ctx_ledger.entries.is_empty() {
                                None
                            } else {
                                Some(&*ctx_ledger)
                            },
                        );
                        drop(ctx_ledger);

                        let mut output = crate::tools::ctx_handoff::format_created(&path, &ledger);
                        let compact = package.format_compact();
                        if !compact.is_empty() {
                            output.push_str("\n\n");
                            output.push_str(&compact);
                        }

                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        output
                    }
                    "pull" => {
                        let path = get_str(args, "path").ok_or_else(|| {
                            ErrorData::invalid_params("path is required for action=pull", None)
                        })?;
                        let path = self
                            .resolve_path(&path)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?;
                        let ledger =
                            crate::core::handoff_ledger::load_ledger(std::path::Path::new(&path))
                                .map_err(|e| {
                                ErrorData::internal_error(format!("load ledger: {e}"), None)
                            })?;

                        let apply_workflow = get_bool(args, "apply_workflow").unwrap_or(true);
                        let apply_session = get_bool(args, "apply_session").unwrap_or(true);
                        let apply_knowledge = get_bool(args, "apply_knowledge").unwrap_or(true);

                        if apply_workflow {
                            let mut wf = self.workflow.write().await;
                            *wf = ledger.workflow.clone();
                        }

                        if apply_session {
                            let mut session = self.session.write().await;
                            if let Some(t) = ledger.session.task.as_deref() {
                                session.set_task(t, None);
                            }
                            for d in &ledger.session.decisions {
                                session.add_decision(d, None);
                            }
                            for f in &ledger.session.findings {
                                session.add_finding(None, None, f);
                            }
                            session.next_steps = ledger.session.next_steps.clone();
                            let _ = session.save();
                        }

                        let mut knowledge_imported = 0u32;
                        let mut contradictions = 0u32;
                        if apply_knowledge {
                            let root = if let Some(r) = ledger.project_root.as_deref() {
                                r.to_string()
                            } else {
                                let session = self.session.read().await;
                                session
                                    .project_root
                                    .clone()
                                    .unwrap_or_else(|| ".".to_string())
                            };
                            let session_id = {
                                let s = self.session.read().await;
                                s.id.clone()
                            };
                            let mut knowledge =
                                crate::core::knowledge::ProjectKnowledge::load_or_create(&root);
                            for fact in &ledger.knowledge.facts {
                                let c = knowledge.remember(
                                    &fact.category,
                                    &fact.key,
                                    &fact.value,
                                    &session_id,
                                    fact.confidence,
                                );
                                if c.is_some() {
                                    contradictions += 1;
                                }
                                knowledge_imported += 1;
                            }
                            let _ = knowledge.run_memory_lifecycle();
                            let _ = knowledge.save();
                        }

                        let lines = [
                            "ctx_handoff pull".to_string(),
                            format!(" path: {}", path),
                            format!(" md5: {}", ledger.content_md5),
                            format!(" applied_workflow: {}", apply_workflow),
                            format!(" applied_session: {}", apply_session),
                            format!(" imported_knowledge: {}", knowledge_imported),
                            format!(" contradictions: {}", contradictions),
                        ];
                        let result = lines.join("\n");
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                    _ => {
                        let result =
                            "Unknown action. Use: create, show, list, pull, clear".to_string();
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
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
            "ctx_task" => {
                let action = get_str(args, "action").unwrap_or_else(|| "list".to_string());
                let current_agent_id = { self.agent_id.read().await.clone() };
                let task_id = get_str(args, "task_id");
                let to_agent = get_str(args, "to_agent");
                let description = get_str(args, "description");
                let state = get_str(args, "state");
                let message = get_str(args, "message");
                let result = crate::tools::ctx_task::handle(
                    &action,
                    current_agent_id.as_deref(),
                    task_id.as_deref(),
                    to_agent.as_deref(),
                    description.as_deref(),
                    state.as_deref(),
                    message.as_deref(),
                );
                self.record_call("ctx_task", 0, 0, Some(action)).await;
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
            "ctx_workflow" => {
                let action = get_str(args, "action").unwrap_or_else(|| "status".to_string());
                let result = {
                    let mut session = self.session.write().await;
                    crate::tools::ctx_workflow::handle_with_session(args, &mut session)
                };
                *self.workflow.write().await = crate::core::workflow::load_active().ok().flatten();
                self.record_call("ctx_workflow", 0, 0, Some(action)).await;
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
