use md5::{Digest, Md5};
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData;
use serde_json::Value;

use crate::tools::{CrpMode, LeanCtxServer};

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
        let all_tools = if std::env::var("LEAN_CTX_UNIFIED").is_ok()
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
        let result_text = match name {
            "ctx_read" => {
                let path = match get_str(args, "path") {
                    Some(p) => self.resolve_path(&p).await,
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
                let mut cache = self.cache.write().await;
                let output = if fresh {
                    crate::tools::ctx_read::handle_fresh_with_task(
                        &mut cache,
                        &path,
                        &effective_mode,
                        self.crp_mode,
                        task_ref,
                    )
                } else {
                    crate::tools::ctx_read::handle_with_task(
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
                    session.touch_file(&path, file_ref.as_deref(), &effective_mode, original);
                    if is_cache_hit {
                        session.record_cache_hit();
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
                self.record_call("ctx_read", original, saved, Some(mode.clone()))
                    .await;
                crate::core::heatmap::record_file_access(&path, original, saved);
                {
                    let sig =
                        crate::core::mode_predictor::FileSignature::from_path(&path, original);
                    let density = if output_tokens > 0 {
                        original as f64 / output_tokens as f64
                    } else {
                        1.0
                    };
                    let outcome = crate::core::mode_predictor::ModeOutcome {
                        mode: mode.clone(),
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
                    paths.push(self.resolve_path(&p).await);
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
                    .await;
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
                    return Ok(CallToolResult::success(vec![Content::text(rejection)]));
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
                let (output, real_exit_code) =
                    tokio::task::spawn_blocking(move || execute_command_in(&cmd_clone, &cwd_clone))
                        .await
                        .unwrap_or_else(|e| (format!("ERROR: shell task failed: {e}"), 1));

                if raw {
                    let original = crate::core::tokens::count_tokens(&output);
                    self.record_call("ctx_shell", original, 0, None).await;
                    output
                } else {
                    let result = crate::tools::ctx_shell::handle(&command, &output, self.crp_mode);
                    let original = crate::core::tokens::count_tokens(&output);
                    let sent = crate::core::tokens::count_tokens(&result);
                    let saved = original.saturating_sub(sent);
                    self.record_call("ctx_shell", original, saved, None).await;

                    let cfg = crate::core::config::Config::load();
                    let tee_hint = match cfg.tee_mode {
                        crate::core::config::TeeMode::Always => {
                            crate::shell::save_tee(&command, &output)
                                .map(|p| format!("\n[full output: {p}]"))
                                .unwrap_or_default()
                        }
                        crate::core::config::TeeMode::Failures
                            if !output.trim().is_empty() && output.contains("error")
                                || output.contains("Error")
                                || output.contains("ERROR") =>
                        {
                            crate::shell::save_tee(&command, &output)
                                .map(|p| format!("\n[full output: {p}]"))
                                .unwrap_or_default()
                        }
                        _ => String::new(),
                    };

                    let savings_note = if saved > 0 {
                        format!("\n[saved {saved} tokens vs native Shell]")
                    } else {
                        String::new()
                    };

                    // Bug Memory: detect errors / resolve pending
                    {
                        let sess = self.session.read().await;
                        let root = sess.project_root.clone();
                        let sid = sess.id.clone();
                        let files: Vec<String> = sess
                            .files_touched
                            .iter()
                            .map(|ft| ft.path.clone())
                            .collect();
                        drop(sess);

                        if let Some(ref root) = root {
                            let mut store = crate::core::gotcha_tracker::GotchaStore::load(root);

                            if real_exit_code != 0 {
                                store.detect_error(&output, &command, real_exit_code, &files, &sid);
                            } else {
                                // Success: check if any injected gotchas prevented a repeat
                                let relevant = store.top_relevant(&files, 7);
                                let relevant_ids: Vec<String> =
                                    relevant.iter().map(|g| g.id.clone()).collect();
                                for gid in &relevant_ids {
                                    store.mark_prevented(gid);
                                }

                                if store.try_resolve_pending(&command, &files, &sid).is_some() {
                                    store.cross_session_boost();
                                }

                                // Promote mature gotchas to ProjectKnowledge
                                let promotions = store.check_promotions();
                                if !promotions.is_empty() {
                                    let mut knowledge =
                                        crate::core::knowledge::ProjectKnowledge::load_or_create(
                                            root,
                                        );
                                    for (cat, trigger, resolution, conf) in &promotions {
                                        knowledge.remember(
                                            &format!("gotcha-{cat}"),
                                            trigger,
                                            resolution,
                                            &sid,
                                            *conf,
                                        );
                                    }
                                    let _ = knowledge.save();
                                }
                            }

                            let _ = store.save(root);
                        }
                    }

                    format!("{result}{savings_note}{tee_hint}")
                }
            }
            "ctx_search" => {
                let pattern = get_str(args, "pattern")
                    .ok_or_else(|| ErrorData::invalid_params("pattern is required", None))?;
                let path = self
                    .resolve_path(&get_str(args, "path").unwrap_or_else(|| ".".to_string()))
                    .await;
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
                        return Ok(CallToolResult::success(vec![Content::text(msg)]));
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
                    Some(p) => self.resolve_path(&p).await,
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
                let result = crate::tools::ctx_metrics::handle(&cache, &calls, self.crp_mode);
                drop(cache);
                drop(calls);
                self.record_call("ctx_metrics", 0, 0, None).await;
                result
            }
            "ctx_analyze" => {
                let path = match get_str(args, "path") {
                    Some(p) => self.resolve_path(&p).await,
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
                    Some(p) => self.resolve_path(&p).await,
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
                    Some(p) => self.resolve_path(&p).await,
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
                    Some(p) => self.resolve_path(&p).await,
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
                    paths.push(self.resolve_path(&p).await);
                }
                let budget = get_int(args, "budget")
                    .ok_or_else(|| ErrorData::invalid_params("budget is required", None))?
                    as usize;
                let mut cache = self.cache.write().await;
                let output =
                    crate::tools::ctx_fill::handle(&mut cache, &paths, budget, self.crp_mode);
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
                    Some(p) => Some(self.resolve_path(&p).await),
                    None => None,
                };
                let root = self
                    .resolve_path(&get_str(args, "project_root").unwrap_or_else(|| ".".to_string()))
                    .await;
                let mut cache = self.cache.write().await;
                let result = crate::tools::ctx_graph::handle(
                    &action,
                    path.as_deref(),
                    &root,
                    &mut cache,
                    self.crp_mode,
                );
                drop(cache);
                self.record_call("ctx_graph", 0, 0, Some(action)).await;
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
                            Some(p) => self.resolve_path(&p).await,
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
                        return Ok(CallToolResult::success(vec![Content::text(
                            "ERROR: trigger and resolution are required for gotcha action",
                        )]));
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
                    return Ok(CallToolResult::success(vec![Content::text(msg)]));
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
                    Some(p) => Some(self.resolve_path(&p).await),
                    None => {
                        let session = self.session.read().await;
                        session.project_root.clone()
                    }
                };
                let cache = self.cache.read().await;
                let result = crate::tools::ctx_overview::handle(
                    &cache,
                    task.as_deref(),
                    resolved_path.as_deref(),
                    self.crp_mode,
                );
                drop(cache);
                self.record_call("ctx_overview", 0, 0, Some("overview".to_string()))
                    .await;
                result
            }
            "ctx_preload" => {
                let task = get_str(args, "task").unwrap_or_default();
                let resolved_path = match get_str(args, "path") {
                    Some(p) => Some(self.resolve_path(&p).await),
                    None => {
                        let session = self.session.read().await;
                        session.project_root.clone()
                    }
                };
                let mut cache = self.cache.write().await;
                let result = crate::tools::ctx_preload::handle(
                    &mut cache,
                    &task,
                    resolved_path.as_deref(),
                    self.crp_mode,
                );
                drop(cache);
                self.record_call("ctx_preload", 0, 0, Some("preload".to_string()))
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
                    .await;
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
                    let path = get_str(args, "path").ok_or_else(|| {
                        ErrorData::invalid_params("path is required for action=file", None)
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
                    .await;
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
                    .await;
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
        };

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
                .resolve_path(&get_str(args, "path").unwrap_or_default())
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

            {
                let mut session = self.session.write().await;
                session.record_tool_receipt(
                    name,
                    action.as_deref(),
                    &input_md5,
                    &output_md5,
                    agent_id.as_deref(),
                    Some(&client_name),
                );
                if session.should_save() {
                    let _ = session.save();
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

fn get_str_array(args: &Option<serde_json::Map<String, Value>>, key: &str) -> Option<Vec<String>> {
    let arr = args.as_ref()?.get(key)?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v.as_str()?.to_string();
        out.push(s);
    }
    Some(out)
}

fn get_str(args: &Option<serde_json::Map<String, Value>>, key: &str) -> Option<String> {
    args.as_ref()?.get(key)?.as_str().map(|s| s.to_string())
}

fn get_int(args: &Option<serde_json::Map<String, Value>>, key: &str) -> Option<i64> {
    args.as_ref()?.get(key)?.as_i64()
}

fn get_bool(args: &Option<serde_json::Map<String, Value>>, key: &str) -> Option<bool> {
    args.as_ref()?.get(key)?.as_bool()
}

fn md5_hex(s: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn canonicalize_json(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                if let Some(val) = map.get(k) {
                    out.insert(k.clone(), canonicalize_json(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize_json).collect()),
        other => other.clone(),
    }
}

fn canonical_args_string(args: &Option<serde_json::Map<String, Value>>) -> String {
    let v = args
        .as_ref()
        .map(|m| Value::Object(m.clone()))
        .unwrap_or(Value::Null);
    let canon = canonicalize_json(&v);
    serde_json::to_string(&canon).unwrap_or_default()
}

fn extract_search_pattern_from_command(command: &str) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let cmd = parts[0];
    if cmd == "grep" || cmd == "rg" || cmd == "ag" || cmd == "ack" {
        for (i, part) in parts.iter().enumerate().skip(1) {
            if !part.starts_with('-') {
                return Some(part.to_string());
            }
            if (*part == "-e" || *part == "--regexp" || *part == "-m") && i + 1 < parts.len() {
                return Some(parts[i + 1].to_string());
            }
        }
    }
    if cmd == "find" || cmd == "fd" {
        for (i, part) in parts.iter().enumerate() {
            if (*part == "-name" || *part == "-iname") && i + 1 < parts.len() {
                return Some(
                    parts[i + 1]
                        .trim_matches('\'')
                        .trim_matches('"')
                        .to_string(),
                );
            }
        }
        if cmd == "fd" && parts.len() >= 2 && !parts[1].starts_with('-') {
            return Some(parts[1].to_string());
        }
    }
    None
}

fn execute_command_in(command: &str, cwd: &str) -> (String, i32) {
    let (shell, flag) = crate::shell::shell_and_flag();
    let normalized_cmd = crate::tools::ctx_shell::normalize_command_for_shell(command);
    let dir = std::path::Path::new(cwd);
    let mut cmd = std::process::Command::new(&shell);
    cmd.arg(&flag)
        .arg(&normalized_cmd)
        .env("LEAN_CTX_ACTIVE", "1");
    if dir.is_dir() {
        cmd.current_dir(dir);
    }
    let output = cmd.output();

    match output {
        Ok(out) => {
            let code = out.status.code().unwrap_or(1);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let text = if stdout.is_empty() {
                stderr.to_string()
            } else if stderr.is_empty() {
                stdout.to_string()
            } else {
                format!("{stdout}\n{stderr}")
            };
            (text, code)
        }
        Err(e) => (format!("ERROR: {e}"), 1),
    }
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
        let disabled = vec!["ctx_graph".to_string(), "ctx_agent".to_string()];
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
        let disabled = vec!["ctx_nonexistent_tool".to_string()];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total);
    }
}
