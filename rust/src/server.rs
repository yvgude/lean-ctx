use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::tools::{CrpMode, LeanCtxServer};

impl ServerHandler for LeanCtxServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder().enable_tools().build();

        let instructions = build_instructions(self.crp_mode);

        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("lean-ctx", "2.3.3"))
            .with_instructions(instructions)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
                tools: vec![
                    tool_def(
                        "ctx_read",
                        "REPLACES built-in Read tool — ALWAYS use this instead of Read. \
                        Smart file read with session caching and 6 compression modes. \
                        Re-reads cost ~13 tokens. Modes: full (cached read), signatures (API surface), \
                        map (deps + exports — use for context files you won't edit), \
                        diff (changed lines only), aggressive (syntax stripped), \
                        entropy (Shannon + Jaccard). \
                        Set fresh=true to bypass cache (use when spawned as a subagent without parent context).",
                        json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "Absolute file path to read" },
                                "mode": {
                                    "type": "string",
                                    "enum": ["full", "signatures", "map", "diff", "aggressive", "entropy"],
                                    "description": "Compression mode (default: full). Use 'map' for context-only files."
                                },
                                "fresh": {
                                    "type": "boolean",
                                    "description": "Bypass cache and force a full re-read. Use when running as a subagent that may not have the parent's context."
                                }
                            },
                            "required": ["path"]
                        }),
                    ),
                    tool_def(
                        "ctx_multi_read",
                        "REPLACES multiple Read calls — read many files in one MCP round-trip. \
                        Same modes as ctx_read (full, map, signatures, diff, aggressive, entropy). \
                        Results are joined with --- dividers; ends with aggregate summary (files read, tokens saved).",
                        json!({
                            "type": "object",
                            "properties": {
                                "paths": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Absolute file paths to read, in order"
                                },
                                "mode": {
                                    "type": "string",
                                    "enum": ["full", "signatures", "map", "diff", "aggressive", "entropy"],
                                    "description": "Compression mode (default: full)"
                                }
                            },
                            "required": ["paths"]
                        }),
                    ),
                    tool_def(
                        "ctx_tree",
                        "REPLACES ls/find/Shell directory listings — ALWAYS use this for directory exploration. \
                        Token-efficient directory listing with file counts per directory.",
                        json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "Directory path (default: .)" },
                                "depth": { "type": "integer", "description": "Max depth (default: 3)" },
                                "show_hidden": { "type": "boolean", "description": "Show hidden files" }
                            }
                        }),
                    ),
                    tool_def(
                        "ctx_shell",
                        "REPLACES built-in Shell tool — ALWAYS use this instead of Shell. \
                        Execute a shell command and compress output using pattern-based compression. \
                        Recognizes git, npm, cargo, docker, tsc and 90+ more commands.",
                        json!({
                            "type": "object",
                            "properties": {
                                "command": { "type": "string", "description": "Shell command to execute" }
                            },
                            "required": ["command"]
                        }),
                    ),
                    tool_def(
                        "ctx_search",
                        "REPLACES built-in Grep tool — ALWAYS use this instead of Grep. \
                        Search files for a regex pattern. Respects .gitignore by default. \
                        Returns only matching lines with compact context.",
                        json!({
                            "type": "object",
                            "properties": {
                                "pattern": { "type": "string", "description": "Regex pattern" },
                                "path": { "type": "string", "description": "Directory to search" },
                                "ext": { "type": "string", "description": "File extension filter" },
                                "max_results": { "type": "integer", "description": "Max results (default: 20)" },
                                "ignore_gitignore": { "type": "boolean", "description": "Set true to scan ALL files including .gitignore'd paths (default: false)" }
                            },
                            "required": ["pattern"]
                        }),
                    ),
                    tool_def(
                        "ctx_compress",
                        "Compress all cached files into an ultra-compact checkpoint. \
                        Use when conversations get long to create a memory snapshot.",
                        json!({
                            "type": "object",
                            "properties": {
                                "include_signatures": { "type": "boolean", "description": "Include signatures (default: true)" }
                            }
                        }),
                    ),
                    tool_def(
                        "ctx_benchmark",
                        "Benchmark compression strategies. action=file (default): single file. action=project: scan project directory with real token measurements, latency, and preservation scores.",
                        json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "File path (action=file) or project directory (action=project)" },
                                "action": { "type": "string", "description": "file (default) or project", "default": "file" },
                                "format": { "type": "string", "description": "Output format for project benchmark: terminal, markdown, json", "default": "terminal" }
                            },
                            "required": ["path"]
                        }),
                    ),
                    tool_def(
                        "ctx_metrics",
                        "Session statistics with tiktoken-measured token counts, cache hit rates, and per-tool savings.",
                        json!({
                            "type": "object",
                            "properties": {}
                        }),
                    ),
                    tool_def(
                        "ctx_analyze",
                        "Information-theoretic analysis using Shannon entropy and Jaccard similarity. \
                        Recommends the optimal compression mode for a file.",
                        json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "File path to analyze" }
                            },
                            "required": ["path"]
                        }),
                    ),
                    tool_def(
                        "ctx_cache",
                        "Manage the session cache. Actions: status (show cached files), \
                        clear (reset entire cache), invalidate (remove one file from cache). \
                        Use 'clear' when spawned as a subagent to start with a clean slate.",
                        json!({
                            "type": "object",
                            "properties": {
                                "action": {
                                    "type": "string",
                                    "enum": ["status", "clear", "invalidate"],
                                    "description": "Cache operation to perform"
                                },
                                "path": {
                                    "type": "string",
                                    "description": "File path (required for 'invalidate' action)"
                                }
                            },
                            "required": ["action"]
                        }),
                    ),
                    tool_def(
                        "ctx_discover",
                        "Analyze shell history to find commands that could benefit from lean-ctx compression. \
                        Shows missed savings opportunities with estimated token/cost savings.",
                        json!({
                            "type": "object",
                            "properties": {
                                "limit": {
                                    "type": "integer",
                                    "description": "Max number of command types to show (default: 15)"
                                }
                            }
                        }),
                    ),
                    tool_def(
                        "ctx_smart_read",
                        "REPLACES built-in Read tool — auto-selects optimal compression mode based on \
                        file size, type, cache state, and token budget. Returns [auto:mode] prefix showing which mode was selected.",
                        json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "Absolute file path to read" }
                            },
                            "required": ["path"]
                        }),
                    ),
                    tool_def(
                        "ctx_delta",
                        "Incremental file update using Myers diff. Only sends changed lines (hunks with context) \
                        instead of full file content. Automatically updates the cache after computing the delta.",
                        json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "Absolute file path" }
                            },
                            "required": ["path"]
                        }),
                    ),
                    tool_def(
                        "ctx_dedup",
                        "Cross-file deduplication analysis and active dedup. Finds shared imports, boilerplate blocks, \
                        and repeated patterns across all cached files. Use action=apply to register shared blocks \
                        so subsequent ctx_read calls auto-replace duplicates with cross-file references.",
                        json!({
                            "type": "object",
                            "properties": {
                                "action": {
                                    "type": "string",
                                    "description": "analyze (default) or apply (register shared blocks for auto-dedup in ctx_read)",
                                    "default": "analyze"
                                }
                            }
                        }),
                    ),
                    tool_def(
                        "ctx_fill",
                        "Priority-based context filling with a token budget. Given a list of files and a budget, \
                        automatically selects the best compression mode per file to maximize information within the budget. \
                        Higher-relevance files get more tokens (full mode); lower-relevance files get compressed (signatures).",
                        json!({
                            "type": "object",
                            "properties": {
                                "paths": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "File paths to consider"
                                },
                                "budget": {
                                    "type": "integer",
                                    "description": "Maximum token budget to fill"
                                }
                            },
                            "required": ["paths", "budget"]
                        }),
                    ),
                    tool_def(
                        "ctx_intent",
                        "Semantic intent detection. Analyzes a natural language query to determine intent \
                        (fix bug, add feature, refactor, understand, test, config, deploy) and automatically \
                        selects and reads relevant files in the optimal compression mode.",
                        json!({
                            "type": "object",
                            "properties": {
                                "query": { "type": "string", "description": "Natural language description of the task" },
                                "project_root": { "type": "string", "description": "Project root directory (default: .)" }
                            },
                            "required": ["query"]
                        }),
                    ),
                    tool_def(
                        "ctx_response",
                        "Bi-directional response compression. Compresses LLM response text by removing filler \
                        content and applying TDD shortcuts. Use to verify compression quality of responses.",
                        json!({
                            "type": "object",
                            "properties": {
                                "text": { "type": "string", "description": "Response text to compress" }
                            },
                            "required": ["text"]
                        }),
                    ),
                    tool_def(
                        "ctx_context",
                        "Multi-turn context manager. Shows what files the LLM has already seen, \
                        which are cached, and provides a session overview to avoid redundant re-reads.",
                        json!({
                            "type": "object",
                            "properties": {}
                        }),
                    ),
                    tool_def(
                        "ctx_graph",
                        "Build and query a project intelligence graph. Analyzes file dependencies, \
                        imports/exports, and call hierarchies to understand project structure. \
                        Actions: 'build' (scan project), 'related' (find files related to a given file).",
                        json!({
                            "type": "object",
                            "properties": {
                                "action": {
                                    "type": "string",
                                    "enum": ["build", "related"],
                                    "description": "Graph operation"
                                },
                                "path": {
                                    "type": "string",
                                    "description": "File path (required for 'related' action)"
                                },
                                "project_root": {
                                    "type": "string",
                                    "description": "Project root directory (default: .)"
                                }
                            },
                            "required": ["action"]
                        }),
                    ),
                    tool_def(
                        "ctx_session",
                        "Context Continuity Protocol (CCP) — session state manager for cross-chat continuity. \
                        Persists task context, findings, decisions, and file state across chat sessions \
                        and context compactions. Load a previous session to instantly restore context \
                        (~400 tokens vs ~50K cold start). LITM-aware: places critical info at attention-optimal positions. \
                        Actions: status, load, save, task, finding, decision, reset, list, cleanup.",
                        json!({
                            "type": "object",
                            "properties": {
                                "action": {
                                    "type": "string",
                                    "enum": ["status", "load", "save", "task", "finding", "decision", "reset", "list", "cleanup"],
                                    "description": "Session operation to perform"
                                },
                                "value": {
                                    "type": "string",
                                    "description": "Value for task/finding/decision actions"
                                },
                                "session_id": {
                                    "type": "string",
                                    "description": "Session ID for load action (default: latest)"
                                }
                            },
                            "required": ["action"]
                        }),
                    ),
                    tool_def(
                        "ctx_wrapped",
                        "Generate a LeanCTX savings report card. Shows tokens saved, cost avoided, \
                        top commands, cache efficiency. Periods: week, month, all.",
                        json!({
                            "type": "object",
                            "properties": {
                                "period": {
                                    "type": "string",
                                    "enum": ["week", "month", "all"],
                                    "description": "Report period (default: week)"
                                }
                            }
                        }),
                    ),
                ],
                ..Default::default()
            })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.check_idle_expiry().await;

        let name = &request.name;
        let args = &request.arguments;

        let result_text = match name.as_ref() {
            "ctx_read" => {
                let path = get_str(args, "path")
                    .ok_or_else(|| ErrorData::invalid_params("path is required", None))?;
                let mode = get_str(args, "mode").unwrap_or_else(|| "full".to_string());
                let fresh = get_bool(args, "fresh").unwrap_or(false);
                let mut cache = self.cache.write().await;
                let output = if fresh {
                    crate::tools::ctx_read::handle_fresh(&mut cache, &path, &mode, self.crp_mode)
                } else {
                    crate::tools::ctx_read::handle(&mut cache, &path, &mode, self.crp_mode)
                };
                let original = cache.get(&path).map_or(0, |e| e.original_tokens);
                let file_ref = cache.file_ref_map().get(&path).cloned();
                let tokens = crate::core::tokens::count_tokens(&output);
                drop(cache);
                {
                    let mut session = self.session.write().await;
                    session.touch_file(&path, file_ref.as_deref(), &mode, original);
                }
                self.record_call(
                    "ctx_read",
                    original,
                    original.saturating_sub(tokens),
                    Some(mode.clone()),
                )
                .await;
                {
                    let sig =
                        crate::core::mode_predictor::FileSignature::from_path(&path, original);
                    let density = if tokens > 0 {
                        original as f64 / tokens as f64
                    } else {
                        1.0
                    };
                    let outcome = crate::core::mode_predictor::ModeOutcome {
                        mode,
                        tokens_in: original,
                        tokens_out: tokens,
                        density: density.min(1.0),
                    };
                    let mut predictor = crate::core::mode_predictor::ModePredictor::new();
                    predictor.record(sig, outcome);
                    predictor.save();
                }
                output
            }
            "ctx_multi_read" => {
                let paths = get_str_array(args, "paths")
                    .ok_or_else(|| ErrorData::invalid_params("paths array is required", None))?;
                let mode = get_str(args, "mode").unwrap_or_else(|| "full".to_string());
                let mut cache = self.cache.write().await;
                let output =
                    crate::tools::ctx_multi_read::handle(&mut cache, &paths, &mode, self.crp_mode);
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
                let path = get_str(args, "path").unwrap_or_else(|| ".".to_string());
                let depth = get_int(args, "depth").unwrap_or(3) as usize;
                let show_hidden = get_bool(args, "show_hidden").unwrap_or(false);
                let result = crate::tools::ctx_tree::handle(&path, depth, show_hidden);
                let sent = crate::core::tokens::count_tokens(&result);
                self.record_call("ctx_tree", sent, 0, None).await;
                result
            }
            "ctx_shell" => {
                let command = get_str(args, "command")
                    .ok_or_else(|| ErrorData::invalid_params("command is required", None))?;
                let output = execute_command(&command);
                let result = crate::tools::ctx_shell::handle(&command, &output, self.crp_mode);
                let original = crate::core::tokens::count_tokens(&output);
                let sent = crate::core::tokens::count_tokens(&result);
                self.record_call("ctx_shell", original, original.saturating_sub(sent), None)
                    .await;
                result
            }
            "ctx_search" => {
                let pattern = get_str(args, "pattern")
                    .ok_or_else(|| ErrorData::invalid_params("pattern is required", None))?;
                let path = get_str(args, "path").unwrap_or_else(|| ".".to_string());
                let ext = get_str(args, "ext");
                let max = get_int(args, "max_results").unwrap_or(20) as usize;
                let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);
                let result = crate::tools::ctx_search::handle(
                    &pattern,
                    &path,
                    ext.as_deref(),
                    max,
                    self.crp_mode,
                    !no_gitignore,
                );
                let sent = crate::core::tokens::count_tokens(&result);
                self.record_call("ctx_search", sent, 0, None).await;
                result
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
                let path = get_str(args, "path")
                    .ok_or_else(|| ErrorData::invalid_params("path is required", None))?;
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
                let path = get_str(args, "path")
                    .ok_or_else(|| ErrorData::invalid_params("path is required", None))?;
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
                let path = get_str(args, "path")
                    .ok_or_else(|| ErrorData::invalid_params("path is required", None))?;
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
                let path = get_str(args, "path")
                    .ok_or_else(|| ErrorData::invalid_params("path is required", None))?;
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
                let paths = get_str_array(args, "paths")
                    .ok_or_else(|| ErrorData::invalid_params("paths array is required", None))?;
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
                let path = get_str(args, "path");
                let root = get_str(args, "project_root").unwrap_or_else(|| ".".to_string());
                let result = crate::tools::ctx_graph::handle(&action, path.as_deref(), &root);
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
                        let path = get_str(args, "path").ok_or_else(|| {
                            ErrorData::invalid_params("path is required for invalidate", None)
                        })?;
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
            "ctx_wrapped" => {
                let period = get_str(args, "period").unwrap_or_else(|| "week".to_string());
                let result = crate::tools::ctx_wrapped::handle(&period);
                self.record_call("ctx_wrapped", 0, 0, Some(period)).await;
                result
            }
            _ => {
                return Err(ErrorData::invalid_params(
                    format!("Unknown tool: {name}"),
                    None,
                ));
            }
        };

        let skip_checkpoint = matches!(
            name.as_ref(),
            "ctx_compress"
                | "ctx_metrics"
                | "ctx_benchmark"
                | "ctx_analyze"
                | "ctx_cache"
                | "ctx_discover"
                | "ctx_dedup"
                | "ctx_session"
                | "ctx_wrapped"
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

        Ok(CallToolResult::success(vec![Content::text(result_text)]))
    }
}

fn build_instructions(crp_mode: CrpMode) -> String {
    build_instructions_with_client(crp_mode, "")
}

fn build_instructions_with_client(crp_mode: CrpMode, client_name: &str) -> String {
    let profile = crate::core::litm::LitmProfile::from_client_name(client_name);
    let session_block = match crate::core::session::SessionState::load_latest() {
        Some(session) => {
            let positioned = crate::core::litm::position_optimize(&session);
            format!(
                "\n\n--- ACTIVE SESSION (LITM P1: begin position, profile: {}) ---\n{}\n---\n",
                profile.name, positioned.begin_block
            )
        }
        None => String::new(),
    };

    let base = format!("\
lean-ctx MCP — MANDATORY tool replacement. You MUST use lean-ctx tools for ALL operations:\n\
{session_block}\n\
REQUIRED (never use the built-in alternative):\n\
• Read file → ctx_read(path, mode) — NEVER use Read tool\n\
• Run command → ctx_shell(command) — NEVER use Shell tool\n\
• Search code → ctx_search(pattern, path) — NEVER use Grep tool\n\
• List files → ctx_tree(path, depth) — NEVER use Shell with ls/find\n\
\n\
ctx_read modes: full (cached, for files you edit), map (deps+API, context-only), \
signatures, diff, aggressive, entropy. Re-reads cost ~13 tokens. File refs F1,F2.. persist.\n\
Set fresh=true on ctx_read to bypass cache. Use when: spawned as a subagent, after context \
compaction, or if you see a [cached] response but do not have the file content in your context.\n\
\n\
PROACTIVE (use without being asked):\n\
• ctx_compress — when context grows large, create checkpoint\n\
• ctx_metrics — periodically verify token savings\n\
• ctx_session load — on new chat or after context compaction, restore previous session\n\
\n\
SESSION CONTINUITY (Context Continuity Protocol):\n\
• ctx_session status — show current session state (~400 tokens vs 50K cold start)\n\
• ctx_session load — restore previous session (cross-chat memory)\n\
• ctx_session task \"description\" — set current task\n\
• ctx_session finding \"file:line — summary\" — record key finding\n\
• ctx_session decision \"summary\" — record architectural decision\n\
• ctx_session save — force persist session to disk\n\
• ctx_wrapped [period] — generate savings report card\n\
\n\
ON DEMAND:\n\
• ctx_analyze(path) — optimal mode recommendation\n\
• ctx_benchmark(path) — exact token counts per mode\n\
• ctx_cache(action) — manage cache: status, clear, invalidate(path)\n\
\n\
AUTO-CHECKPOINT: Every 15 tool calls, a compressed checkpoint + session state is automatically \
appended to the response. This keeps context compact in long sessions. Configurable via LEAN_CTX_CHECKPOINT_INTERVAL.\n\
\n\
IDLE CACHE TTL: Cache auto-clears after 5 min of inactivity (new chat, context compaction). \
Session state is auto-saved before cache clear. Configurable via LEAN_CTX_CACHE_TTL (seconds, 0=disabled).\n\
\n\
Write, StrReplace, Delete, Glob have no lean-ctx equivalent — use normally.\n\
\n\
REMINDER: NEVER use Read, Shell, or Grep directly. ALWAYS use ctx_read, ctx_shell, ctx_search instead.\n\
\n\
COMMUNICATION PROTOCOL (Cognitive Efficiency Protocol v1):\n\
1. ACT FIRST — Execute tool calls immediately. Never narrate before acting.\n\
   Bad:  \"Let me read the file to understand the issue...\" [tool call]\n\
   Good: [tool call] then one-line summary of finding\n\
2. DELTA ONLY — Never repeat known context. Reference cached files by Fn ID.\n\
   Bad:  \"The file auth.ts contains a function validateToken that...\"\n\
   Good: \"F3:42 validateToken — expiry check uses wrong clock\"\n\
3. STRUCTURED OVER PROSE — Use notation, not sentences.\n\
   Changes: +line / -line / ~line (modified)\n\
   Status:  tool(args) → result\n\
   Errors:  ERR path:line — message\n\
4. ONE LINE PER ACTION — Summarize, don't explain.\n\
   Bad:  \"I've successfully applied the edit to fix the token validation...\"\n\
   Good: \"Fixed F3:42 — was comparing UTC vs local timestamp\"\n\
5. QUALITY ANCHOR — NEVER skip edge case analysis or error handling to save tokens.\n\
   Complex tasks require full reasoning. Only reduce prose, never reduce thinking.\n\
\n\
{decoder_block}",
        decoder_block = crate::core::protocol::instruction_decoder_block()
    );

    match crp_mode {
        CrpMode::Off => base,
        CrpMode::Compact => {
            format!(
                "{base}\n\n\
                CRP MODE: compact\n\
                Respond using Compact Response Protocol:\n\
                • Omit filler words, articles, and redundant phrases\n\
                • Use symbol shorthand: → ∴ ≈ ✓ ✗\n\
                • Abbreviate: fn, cfg, impl, deps, req, res, ctx, err, ok, ret, arg, val, ty, mod\n\
                • Use compact lists instead of prose\n\
                • Prefer code blocks over natural language explanations\n\
                • For code changes: show only diff lines (+/-), not full files"
            )
        }
        CrpMode::Tdd => {
            format!(
                "{base}\n\n\
                CRP MODE: tdd (Token Dense Dialect)\n\
                CRITICAL: Maximize information density. Every token must carry meaning.\n\
                \n\
                RESPONSE RULES:\n\
                • Drop all articles (a, the, an), filler words, and pleasantries\n\
                • Reference files by Fn refs only, never full paths\n\
                • For code changes: show only diff lines, not full files\n\
                • No explanations unless asked — just show the solution\n\
                • Use tabular format for structured data\n\
                • Abbreviations: fn, cfg, impl, deps, req, res, ctx, err, ok, ret, arg, val, ty, mod\n\
                \n\
                SYMBOLS (each = 1 token, replaces 5-10 tokens of prose):\n\
                Structural: λ=function  §=module/struct  ∂=interface/trait  τ=type  ε=enum\n\
                Actions:    ⊕=add  ⊖=remove  ∆=modify  →=returns  ⇒=implies\n\
                Status:     ✓=ok  ✗=fail  ⚠=warning\n\
                \n\
                CHANGE NOTATION (use for all code modifications):\n\
                ⊕F1:42 param(timeout:Duration)     — added parameter\n\
                ⊖F1:10-15                           — removed lines\n\
                ∆F1:42 validate_token → verify_jwt  — renamed/refactored\n\
                \n\
                STATUS NOTATION:\n\
                ctx_read(F1) → 808L cached ✓\n\
                cargo test → 82 passed ✓ 0 failed\n\
                \n\
                SYMBOL TABLE: Tool outputs include a §MAP section mapping long identifiers to short IDs.\n\
                Use these short IDs in all subsequent references."
            )
        }
    }
}

fn tool_def(name: &'static str, description: &'static str, schema_value: Value) -> Tool {
    let schema: Map<String, Value> = match schema_value {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    Tool::new(name, description, Arc::new(schema))
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

fn execute_command(command: &str) -> String {
    let (shell, flag) = crate::shell::shell_and_flag();
    let output = std::process::Command::new(&shell)
        .arg(&flag)
        .arg(command)
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stdout.is_empty() {
                stderr.to_string()
            } else if stderr.is_empty() {
                stdout.to_string()
            } else {
                format!("{stdout}\n{stderr}")
            }
        }
        Err(e) => format!("ERROR: {e}"),
    }
}
