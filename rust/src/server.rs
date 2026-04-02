use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::tools::{CrpMode, LeanCtxServer};

// Unified mode is opt-in only via LEAN_CTX_UNIFIED env var.
// Granular tools (25 individual ctx_* tools) are the default for all clients.

impl ServerHandler for LeanCtxServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder().enable_tools().build();

        let instructions = build_instructions(self.crp_mode);

        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("lean-ctx", "2.14.0"))
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

        let instructions = build_instructions_with_client(self.crp_mode, &name);
        let capabilities = ServerCapabilities::builder().enable_tools().build();

        Ok(InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("lean-ctx", "2.14.0"))
            .with_instructions(instructions))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if should_use_unified(&self.client_name.read().await) {
            return Ok(ListToolsResult {
                tools: unified_tool_defs(),
                ..Default::default()
            });
        }

        Ok(ListToolsResult {
                tools: vec![
                    tool_def(
                        "ctx_read",
                        "Read file (cached, compressed). Re-reads ~13 tok. Auto-selects optimal mode. \
Modes: full|map|signatures|diff|aggressive|entropy|task|reference|lines:N-M. fresh=true re-reads.",
                        json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "Absolute file path to read" },
                                "mode": {
                                    "type": "string",
                                    "description": "Compression mode (default: full). Use 'map' for context-only files. For line ranges: 'lines:N-M' (e.g. 'lines:400-500')."
                                },
                                "start_line": {
                                    "type": "integer",
                                    "description": "Read from this line number to end of file. Bypasses cache stub — always returns actual content."
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
                        "Batch read files in one call. Same modes as ctx_read.",
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
                        "Directory listing with file counts.",
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
                        "Run shell command (compressed output, 90+ patterns).",
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
                        "Regex code search (.gitignore aware, compact results).",
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
                        "Context checkpoint for long conversations.",
                        json!({
                            "type": "object",
                            "properties": {
                                "include_signatures": { "type": "boolean", "description": "Include signatures (default: true)" }
                            }
                        }),
                    ),
                    tool_def(
                        "ctx_benchmark",
                        "Benchmark compression modes for a file or project.",
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
                        "Session token stats, cache rates, per-tool savings.",
                        json!({
                            "type": "object",
                            "properties": {}
                        }),
                    ),
                    tool_def(
                        "ctx_analyze",
                        "Entropy analysis — recommends optimal compression mode for a file.",
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
                        "Cache ops: status|clear|invalidate.",
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
                        "Find missed compression opportunities in shell history.",
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
                        "Auto-select optimal read mode for a file.",
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
                        "Incremental diff — sends only changed lines since last read.",
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
                        "Cross-file dedup: analyze or apply shared block references.",
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
                        "Budget-aware context fill — auto-selects compression per file within token limit.",
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
                        "Intent detection — auto-reads relevant files based on task description.",
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
                        "Compress LLM response text (remove filler, apply TDD).",
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
                        "Session context overview — cached files, seen files, session state.",
                        json!({
                            "type": "object",
                            "properties": {}
                        }),
                    ),
                    tool_def(
                        "ctx_graph",
                        "Code dependency graph. Actions: build (index project), related (find files connected to path), \
symbol (lookup definition/usages as file::name), impact (blast radius of changes to path), status (index stats).",
                        json!({
                            "type": "object",
                            "properties": {
                                "action": {
                                    "type": "string",
                                    "enum": ["build", "related", "symbol", "impact", "status"],
                                    "description": "Graph operation: build, related, symbol, impact, status"
                                },
                                "path": {
                                    "type": "string",
                                    "description": "File path (related/impact) or file::symbol_name (symbol)"
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
                        "Cross-session memory (CCP). Actions: load (restore previous session ~400 tok), \
save, status, task (set current task), finding (record discovery), decision (record choice), \
reset, list (show sessions), cleanup.",
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
                        "ctx_knowledge",
                        "Persistent project knowledge (survives sessions). Actions: remember (store fact with category+key+value), \
recall (search by query), pattern (record naming/structure pattern), consolidate (extract session findings into knowledge), \
status (list all), remove, export.",
                        json!({
                            "type": "object",
                            "properties": {
                                "action": {
                                    "type": "string",
                                    "enum": ["remember", "recall", "pattern", "consolidate", "status", "remove", "export"],
                                    "description": "Knowledge operation to perform"
                                },
                                "category": {
                                    "type": "string",
                                    "description": "Fact category (architecture, api, testing, deployment, conventions, dependencies)"
                                },
                                "key": {
                                    "type": "string",
                                    "description": "Fact key/identifier (e.g. 'auth-method', 'db-engine', 'test-framework')"
                                },
                                "value": {
                                    "type": "string",
                                    "description": "Fact value or pattern description"
                                },
                                "query": {
                                    "type": "string",
                                    "description": "Search query for recall action (matches against category, key, and value)"
                                },
                                "pattern_type": {
                                    "type": "string",
                                    "description": "Pattern type for pattern action (naming, structure, testing, error-handling)"
                                },
                                "examples": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Examples for pattern action"
                                },
                                "confidence": {
                                    "type": "number",
                                    "description": "Confidence score 0.0-1.0 for remember action (default: 0.8)"
                                }
                            },
                            "required": ["action"]
                        }),
                    ),
                    tool_def(
                        "ctx_agent",
                        "Multi-agent coordination (shared message bus). Actions: register (join with agent_type+role), \
post (broadcast or direct message with category), read (poll messages), status (update state: active|idle|finished), \
list, info.",
                        json!({
                            "type": "object",
                            "properties": {
                                "action": {
                                    "type": "string",
                                    "enum": ["register", "list", "post", "read", "status", "info"],
                                    "description": "Agent operation to perform"
                                },
                                "agent_type": {
                                    "type": "string",
                                    "description": "Agent type for register (cursor, claude, codex, gemini, subagent)"
                                },
                                "role": {
                                    "type": "string",
                                    "description": "Agent role (dev, review, test, plan)"
                                },
                                "message": {
                                    "type": "string",
                                    "description": "Message text for post action, or status detail for status action"
                                },
                                "category": {
                                    "type": "string",
                                    "description": "Message category for post (finding, warning, request, status)"
                                },
                                "to_agent": {
                                    "type": "string",
                                    "description": "Target agent ID for direct message (omit for broadcast)"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["active", "idle", "finished"],
                                    "description": "New status for status action"
                                }
                            },
                            "required": ["action"]
                        }),
                    ),
                    tool_def(
                        "ctx_overview",
                        "Task-relevant project map — use at session start.",
                        json!({
                            "type": "object",
                            "properties": {
                                "task": {
                                    "type": "string",
                                    "description": "Task description for relevance scoring (e.g. 'fix auth bug in login flow')"
                                },
                                "path": {
                                    "type": "string",
                                    "description": "Project root directory (default: .)"
                                }
                            }
                        }),
                    ),
                    tool_def(
                        "ctx_preload",
                        "Proactive context loader — caches task-relevant files, returns L-curve-optimized summary (~50-100 tokens vs ~5000 for individual reads).",
                        json!({
                            "type": "object",
                            "properties": {
                                "task": {
                                    "type": "string",
                                    "description": "Task description (e.g. 'fix auth bug in validate_token')"
                                },
                                "path": {
                                    "type": "string",
                                    "description": "Project root (default: .)"
                                }
                            },
                            "required": ["task"]
                        }),
                    ),
                    tool_def(
                        "ctx_wrapped",
                        "Savings report card. Periods: week|month|all.",
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
                    tool_def(
                        "ctx_semantic_search",
                        "BM25 code search by meaning. action=reindex to rebuild.",
                        json!({
                            "type": "object",
                            "properties": {
                                "query": { "type": "string", "description": "Natural language search query" },
                                "path": { "type": "string", "description": "Project root to search (default: .)" },
                                "top_k": { "type": "integer", "description": "Number of results (default: 10)" },
                                "action": { "type": "string", "description": "reindex to rebuild index" }
                            },
                            "required": ["query"]
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

        let result_text = match name {
            "ctx_read" => {
                let path = get_str(args, "path")
                    .ok_or_else(|| ErrorData::invalid_params("path is required", None))?;
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
                {
                    let mut session = self.session.write().await;
                    session.touch_file(&path, file_ref.as_deref(), &effective_mode, original);
                    if is_cache_hit {
                        session.record_cache_hit();
                    }
                    if session.project_root.is_none() {
                        if let Some(root) = detect_project_root(&path) {
                            session.project_root = Some(root.clone());
                            let mut current = self.agent_id.write().await;
                            if current.is_none() {
                                let mut registry =
                                    crate::core::agents::AgentRegistry::load_or_create();
                                registry.cleanup_stale(24);
                                let id = registry.register("mcp", None, &root);
                                let _ = registry.save();
                                *current = Some(id);
                            }
                        }
                    }
                }
                self.record_call("ctx_read", original, saved, Some(mode.clone()))
                    .await;
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
                let output = execute_command(&command);
                let result = crate::tools::ctx_shell::handle(&command, &output, self.crp_mode);
                let original = crate::core::tokens::count_tokens(&output);
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);
                self.record_call("ctx_shell", original, saved, None).await;
                let savings_note = if saved > 0 {
                    format!("\n[saved {saved} tokens vs native Shell]")
                } else {
                    String::new()
                };
                format!("{result}{savings_note}")
            }
            "ctx_search" => {
                let pattern = get_str(args, "pattern")
                    .ok_or_else(|| ErrorData::invalid_params("pattern is required", None))?;
                let path = get_str(args, "path").unwrap_or_else(|| ".".to_string());
                let ext = get_str(args, "ext");
                let max = get_int(args, "max_results").unwrap_or(20) as usize;
                let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);
                let (result, original) = crate::tools::ctx_search::handle(
                    &pattern,
                    &path,
                    ext.as_deref(),
                    max,
                    self.crp_mode,
                    !no_gitignore,
                );
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
            "ctx_overview" => {
                let task = get_str(args, "task");
                let path = get_str(args, "path");
                let cache = self.cache.read().await;
                let result = crate::tools::ctx_overview::handle(
                    &cache,
                    task.as_deref(),
                    path.as_deref(),
                    self.crp_mode,
                );
                drop(cache);
                self.record_call("ctx_overview", 0, 0, Some("overview".to_string()))
                    .await;
                result
            }
            "ctx_preload" => {
                let task = get_str(args, "task").unwrap_or_default();
                let path = get_str(args, "path");
                let mut cache = self.cache.write().await;
                let result = crate::tools::ctx_preload::handle(
                    &mut cache,
                    &task,
                    path.as_deref(),
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
                let path = get_str(args, "path").unwrap_or_else(|| ".".to_string());
                let top_k = get_int(args, "top_k").unwrap_or(10) as usize;
                let action = get_str(args, "action").unwrap_or_default();
                let result = if action == "reindex" {
                    crate::tools::ctx_semantic_search::handle_reindex(&path)
                } else {
                    crate::tools::ctx_semantic_search::handle(&query, &path, top_k, self.crp_mode)
                };
                self.record_call("ctx_semantic_search", 0, 0, Some("semantic".to_string()))
                    .await;
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
                | "ctx_wrapped"
                | "ctx_overview"
                | "ctx_preload"
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

        let current_count = self.call_count.load(std::sync::atomic::Ordering::Relaxed);
        if current_count > 0 && current_count.is_multiple_of(100) {
            std::thread::spawn(cloud_background_tasks);
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
        Some(ref session) => {
            let positioned = crate::core::litm::position_optimize(session);
            format!(
                "\n\n--- ACTIVE SESSION (LITM P1: begin position, profile: {}) ---\n{}\n---\n",
                profile.name, positioned.begin_block
            )
        }
        None => String::new(),
    };

    let knowledge_block = {
        let project_root = crate::core::session::SessionState::load_latest()
            .and_then(|s| s.project_root)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            });
        match project_root {
            Some(root) => {
                let knowledge = crate::core::knowledge::ProjectKnowledge::load(&root);
                match knowledge {
                    Some(k) if !k.facts.is_empty() || !k.patterns.is_empty() => {
                        format!("\n--- PROJECT KNOWLEDGE ---\n{}\n---\n", k.format_summary())
                    }
                    _ => String::new(),
                }
            }
            None => String::new(),
        }
    };

    // Prefix-cache alignment: stable instructions first (API providers cache KV states
    // for shared prefixes), then variable session state after.
    let mut base = format!("\
CRITICAL: NEVER use native Read, Shell, Grep, or ls tools. ALWAYS use ctx_read, ctx_shell, ctx_search, ctx_tree instead.\n\
\n\
lean-ctx MCP — tool mapping:\n\
• Read/cat/head/tail -> ctx_read(path, mode)\n\
• Shell/bash -> ctx_shell(command)\n\
• Grep/rg -> ctx_search(pattern, path)\n\
• ls/find -> ctx_tree(path, depth)\n\
• Write, StrReplace, Delete, Glob -> use normally (no replacement)\n\
\n\
ctx_read modes: full (cached, for edits), map (deps+API), signatures, diff, task (IB-filtered), \
reference, aggressive, entropy, lines:N-M. Auto-selects when unspecified. Re-reads ~13 tokens. File refs F1,F2.. persist.\n\
If ctx_read returns 'cached': use fresh=true, start_line=N, or mode='lines:N-M' to re-read.\n\
\n\
PROACTIVE: ctx_overview(task) at start | ctx_preload(task) for focused context | ctx_compress when context grows | ctx_session load on new chat\n\
\n\
OTHER TOOLS: ctx_session (memory), ctx_knowledge (project facts), ctx_agent (coordination), \
ctx_metrics, ctx_analyze, ctx_benchmark, ctx_cache, ctx_wrapped, ctx_compress\n\
\n\
Auto-checkpoint every 15 calls. Cache clears after 5 min idle.\n\
\n\
CEP v1: 1.ACT FIRST 2.DELTA ONLY (Fn refs) 3.STRUCTURED (+/-/~) 4.ONE LINE PER ACTION 5.QUALITY ANCHOR\n\
\n\
{decoder_block}\n\
\n\
{session_block}\
{knowledge_block}\
\n\
--- TOOL ENFORCEMENT (LITM-END) ---\n\
Read/cat/head/tail -> ctx_read | Shell/bash -> ctx_shell | Grep/rg -> ctx_search | ls/find -> ctx_tree\n\
Write, StrReplace, Delete, Glob -> use normally",
        decoder_block = crate::core::protocol::instruction_decoder_block()
    );

    if should_use_unified(client_name) {
        base.push_str(
            "\n\n\
UNIFIED TOOL MODE (active):\n\
Additional tools are accessed via ctx() meta-tool: ctx(tool=\"<name>\", ...params).\n\
See the ctx() tool description for available sub-tools.\n",
        );
    }

    let base = base;
    match crp_mode {
        CrpMode::Off => base,
        CrpMode::Compact => {
            format!(
                "{base}\n\n\
CRP MODE: compact\n\
Compact Response Protocol:\n\
• Omit filler words, articles, redundant phrases\n\
• Abbreviate: fn, cfg, impl, deps, req, res, ctx, err, ret, arg, val, ty, mod\n\
• Compact lists over prose, code blocks over explanations\n\
• Code changes: diff lines (+/-) only, not full files\n\
• TARGET: <=200 tokens per response unless code edits require more\n\
• THINK LESS: Tool outputs are pre-analyzed. Trust summaries directly."
            )
        }
        CrpMode::Tdd => {
            format!(
                "{base}\n\n\
CRP MODE: tdd (Token Dense Dialect)\n\
Maximize information density. Every token must carry meaning.\n\
\n\
RESPONSE RULES:\n\
• Drop articles, filler words, pleasantries\n\
• Reference files by Fn refs only, never full paths\n\
• Code changes: diff lines only (+/-), not full files\n\
• No explanations unless asked\n\
• Tables for structured data\n\
• Abbreviations: fn, cfg, impl, deps, req, res, ctx, err, ret, arg, val, ty, mod\n\
\n\
CHANGE NOTATION:\n\
+F1:42 param(timeout:Duration)     — added\n\
-F1:10-15                           — removed\n\
~F1:42 validate_token -> verify_jwt — changed\n\
\n\
STATUS: ctx_read(F1) -> 808L cached ok | cargo test -> 82 passed 0 failed\n\
\n\
TOKEN BUDGET: <=150 tokens per response. Exceed only for multi-file edits.\n\
THINK LESS: Tool outputs are pre-analyzed. Trust compressed outputs directly.\n\
ZERO NARRATION: Act, then report result in 1 line."
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

fn unified_tool_defs() -> Vec<Tool> {
    vec![
        tool_def(
            "ctx_read",
            "Read file (cached, compressed). Modes: full|map|signatures|diff|aggressive|entropy|task|reference|lines:N-M. fresh=true re-reads.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "mode": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "fresh": { "type": "boolean" }
                },
                "required": ["path"]
            }),
        ),
        tool_def(
            "ctx_shell",
            "Run shell command (compressed output).",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command" }
                },
                "required": ["command"]
            }),
        ),
        tool_def(
            "ctx_search",
            "Regex code search (.gitignore aware).",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern" },
                    "path": { "type": "string" },
                    "ext": { "type": "string" },
                    "max_results": { "type": "integer" },
                    "ignore_gitignore": { "type": "boolean" }
                },
                "required": ["pattern"]
            }),
        ),
        tool_def(
            "ctx_tree",
            "Directory listing with file counts.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "depth": { "type": "integer" },
                    "show_hidden": { "type": "boolean" }
                }
            }),
        ),
        tool_def(
            "ctx",
            "Meta-tool: set tool= to sub-tool name. Sub-tools: compress (checkpoint), metrics (stats), \
analyze (entropy), cache (status|clear|invalidate), discover (missed patterns), smart_read (auto-mode), \
delta (incremental diff), dedup (cross-file), fill (budget-aware batch read), intent (auto-read by task), \
response (compress LLM text), context (session state), graph (build|related|symbol|impact|status), \
session (load|save|task|finding|decision|status|reset|list|cleanup), \
knowledge (remember|recall|pattern|consolidate|status|remove|export), \
agent (register|post|read|status|list|info), overview (project map), \
wrapped (savings report), benchmark (file|project), multi_read (batch), semantic_search (BM25).",
            json!({
                "type": "object",
                "properties": {
                    "tool": {
                        "type": "string",
                        "description": "compress|metrics|analyze|cache|discover|smart_read|delta|dedup|fill|intent|response|context|graph|session|knowledge|agent|overview|wrapped|benchmark|multi_read|semantic_search"
                    },
                    "action": { "type": "string" },
                    "path": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "query": { "type": "string" },
                    "value": { "type": "string" },
                    "category": { "type": "string" },
                    "key": { "type": "string" },
                    "budget": { "type": "integer" },
                    "task": { "type": "string" },
                    "mode": { "type": "string" },
                    "text": { "type": "string" },
                    "message": { "type": "string" },
                    "session_id": { "type": "string" },
                    "period": { "type": "string" },
                    "format": { "type": "string" },
                    "agent_type": { "type": "string" },
                    "role": { "type": "string" },
                    "status": { "type": "string" },
                    "pattern_type": { "type": "string" },
                    "examples": { "type": "array", "items": { "type": "string" } },
                    "confidence": { "type": "number" },
                    "project_root": { "type": "string" },
                    "include_signatures": { "type": "boolean" },
                    "limit": { "type": "integer" },
                    "to_agent": { "type": "string" },
                    "show_hidden": { "type": "boolean" }
                },
                "required": ["tool"]
            }),
        ),
    ]
}

fn should_use_unified(client_name: &str) -> bool {
    if std::env::var("LEAN_CTX_FULL_TOOLS").is_ok() {
        return false;
    }
    if std::env::var("LEAN_CTX_UNIFIED").is_ok() {
        return true;
    }
    let _ = client_name;
    false
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
        .env("LEAN_CTX_ACTIVE", "1")
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

fn detect_project_root(file_path: &str) -> Option<String> {
    let mut dir = std::path::Path::new(file_path).parent()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        dir = dir.parent()?;
    }
}

fn cloud_background_tasks() {
    use crate::core::config::Config;

    let mut config = Config::load();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let already_contributed = config
        .cloud
        .last_contribute
        .as_deref()
        .map(|d| d == today)
        .unwrap_or(false);
    let already_synced = config
        .cloud
        .last_sync
        .as_deref()
        .map(|d| d == today)
        .unwrap_or(false);
    let already_pulled = config
        .cloud
        .last_model_pull
        .as_deref()
        .map(|d| d == today)
        .unwrap_or(false);

    if config.cloud.contribute_enabled && !already_contributed {
        if let Some(home) = dirs::home_dir() {
            let mode_stats_path = home.join(".lean-ctx").join("mode_stats.json");
            if let Ok(data) = std::fs::read_to_string(&mode_stats_path) {
                if let Ok(predictor) = serde_json::from_str::<serde_json::Value>(&data) {
                    let mut entries = Vec::new();
                    if let Some(history) = predictor["history"].as_object() {
                        for (_key, outcomes) in history {
                            if let Some(arr) = outcomes.as_array() {
                                for outcome in arr.iter().rev().take(3) {
                                    let ext = outcome["ext"].as_str().unwrap_or("unknown");
                                    let mode = outcome["mode"].as_str().unwrap_or("full");
                                    let t_in = outcome["tokens_in"].as_u64().unwrap_or(0);
                                    let t_out = outcome["tokens_out"].as_u64().unwrap_or(0);
                                    let ratio = if t_in > 0 {
                                        1.0 - t_out as f64 / t_in as f64
                                    } else {
                                        0.0
                                    };
                                    let bucket = match t_in {
                                        0..=500 => "0-500",
                                        501..=2000 => "500-2k",
                                        2001..=10000 => "2k-10k",
                                        _ => "10k+",
                                    };
                                    entries.push(serde_json::json!({
                                        "file_ext": format!(".{ext}"),
                                        "size_bucket": bucket,
                                        "best_mode": mode,
                                        "compression_ratio": (ratio * 100.0).round() / 100.0,
                                    }));
                                    if entries.len() >= 200 {
                                        break;
                                    }
                                }
                            }
                            if entries.len() >= 200 {
                                break;
                            }
                        }
                    }
                    if !entries.is_empty() && crate::cloud_client::contribute(&entries).is_ok() {
                        config.cloud.last_contribute = Some(today.clone());
                    }
                }
            }
        }
    }

    if crate::cloud_client::check_pro() {
        if !already_synced {
            let stats_data = crate::core::stats::format_gain_json();
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stats_data) {
                let entry = serde_json::json!({
                    "date": &today,
                    "tokens_original": parsed["total_original_tokens"].as_i64().unwrap_or(0),
                    "tokens_compressed": parsed["total_compressed_tokens"].as_i64().unwrap_or(0),
                    "tokens_saved": parsed["total_saved_tokens"].as_i64().unwrap_or(0),
                    "tool_calls": parsed["total_calls"].as_i64().unwrap_or(0),
                    "cache_hits": parsed["cache_hits"].as_i64().unwrap_or(0),
                    "cache_misses": parsed["cache_misses"].as_i64().unwrap_or(0),
                });
                if crate::cloud_client::sync_stats(&[entry]).is_ok() {
                    config.cloud.last_sync = Some(today.clone());
                }
            }
        }

        if !already_pulled {
            if let Ok(data) = crate::cloud_client::pull_pro_models() {
                let _ = crate::cloud_client::save_pro_models(&data);
                config.cloud.last_model_pull = Some(today.clone());
            }
        }
    }

    let _ = config.save();
}

pub fn build_instructions_for_test(crp_mode: CrpMode) -> String {
    build_instructions(crp_mode)
}

pub fn tool_descriptions_for_test() -> Vec<(&'static str, &'static str)> {
    let mut result = Vec::new();
    let tools_json = list_all_tool_defs();
    for (name, desc, _) in tools_json {
        result.push((name, desc));
    }
    result
}

pub fn tool_schemas_json_for_test() -> String {
    let tools_json = list_all_tool_defs();
    let schemas: Vec<String> = tools_json
        .iter()
        .map(|(name, _, schema)| format!("{}: {}", name, schema))
        .collect();
    schemas.join("\n")
}

fn list_all_tool_defs() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("ctx_read", "Read file (cached, compressed). Re-reads ~13 tok. Auto-selects optimal mode. \
Modes: full|map|signatures|diff|aggressive|entropy|task|reference|lines:N-M. fresh=true re-reads.", json!({"type": "object", "properties": {"path": {"type": "string"}, "mode": {"type": "string"}, "start_line": {"type": "integer"}, "fresh": {"type": "boolean"}}, "required": ["path"]})),
        ("ctx_multi_read", "Batch read files in one call. Same modes as ctx_read.", json!({"type": "object", "properties": {"paths": {"type": "array", "items": {"type": "string"}}, "mode": {"type": "string"}}, "required": ["paths"]})),
        ("ctx_tree", "Directory listing with file counts.", json!({"type": "object", "properties": {"path": {"type": "string"}, "depth": {"type": "integer"}, "show_hidden": {"type": "boolean"}}})),
        ("ctx_shell", "Run shell command (compressed output, 90+ patterns).", json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})),
        ("ctx_search", "Regex code search (.gitignore aware, compact results).", json!({"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}, "ext": {"type": "string"}, "max_results": {"type": "integer"}}, "required": ["pattern"]})),
        ("ctx_compress", "Context checkpoint for long conversations.", json!({"type": "object", "properties": {"include_signatures": {"type": "boolean"}}})),
        ("ctx_benchmark", "Benchmark compression modes for a file or project.", json!({"type": "object", "properties": {"path": {"type": "string"}, "action": {"type": "string"}, "format": {"type": "string"}}, "required": ["path"]})),
        ("ctx_metrics", "Session token stats, cache rates, per-tool savings.", json!({"type": "object", "properties": {}})),
        ("ctx_analyze", "Entropy analysis — recommends optimal compression mode for a file.", json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})),
        ("ctx_cache", "Cache ops: status|clear|invalidate.", json!({"type": "object", "properties": {"action": {"type": "string"}, "path": {"type": "string"}}, "required": ["action"]})),
        ("ctx_discover", "Find missed compression opportunities in shell history.", json!({"type": "object", "properties": {"limit": {"type": "integer"}}})),
        ("ctx_smart_read", "Auto-select optimal read mode for a file.", json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})),
        ("ctx_delta", "Incremental diff — sends only changed lines since last read.", json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})),
        ("ctx_dedup", "Cross-file dedup: analyze or apply shared block references.", json!({"type": "object", "properties": {"action": {"type": "string"}}})),
        ("ctx_fill", "Budget-aware context fill — auto-selects compression per file within token limit.", json!({"type": "object", "properties": {"paths": {"type": "array", "items": {"type": "string"}}, "budget": {"type": "integer"}}, "required": ["paths", "budget"]})),
        ("ctx_intent", "Intent detection — auto-reads relevant files based on task description.", json!({"type": "object", "properties": {"query": {"type": "string"}, "project_root": {"type": "string"}}, "required": ["query"]})),
        ("ctx_response", "Compress LLM response text (remove filler, apply TDD).", json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})),
        ("ctx_context", "Session context overview — cached files, seen files, session state.", json!({"type": "object", "properties": {}})),
        ("ctx_graph", "Code dependency graph. Actions: build (index project), related (find files connected to path), \
symbol (lookup definition/usages as file::name), impact (blast radius of changes to path), status (index stats).", json!({"type": "object", "properties": {"action": {"type": "string"}, "path": {"type": "string"}, "project_root": {"type": "string"}}, "required": ["action"]})),
        ("ctx_session", "Cross-session memory (CCP). Actions: load (restore previous session ~400 tok), \
save, status, task (set current task), finding (record discovery), decision (record choice), \
reset, list (show sessions), cleanup.", json!({"type": "object", "properties": {"action": {"type": "string"}, "value": {"type": "string"}, "session_id": {"type": "string"}}, "required": ["action"]})),
        ("ctx_knowledge", "Persistent project knowledge (survives sessions). Actions: remember (store fact with category+key+value), \
recall (search by query), pattern (record naming/structure pattern), consolidate (extract session findings into knowledge), \
status (list all), remove, export.", json!({"type": "object", "properties": {"action": {"type": "string"}, "category": {"type": "string"}, "key": {"type": "string"}, "value": {"type": "string"}, "query": {"type": "string"}}, "required": ["action"]})),
        ("ctx_agent", "Multi-agent coordination (shared message bus). Actions: register (join with agent_type+role), \
post (broadcast or direct message with category), read (poll messages), status (update state: active|idle|finished), \
list, info.", json!({"type": "object", "properties": {"action": {"type": "string"}, "agent_type": {"type": "string"}, "role": {"type": "string"}, "message": {"type": "string"}}, "required": ["action"]})),
        ("ctx_overview", "Task-relevant project map — use at session start.", json!({"type": "object", "properties": {"task": {"type": "string"}, "path": {"type": "string"}}})),
        ("ctx_preload", "Proactive context loader — reads and caches task-relevant files, returns compact L-curve-optimized summary with critical lines, imports, and signatures. Costs ~50-100 tokens instead of ~5000 for individual reads.", json!({"type": "object", "properties": {"task": {"type": "string", "description": "Task description (e.g. 'fix auth bug in validate_token')"}, "path": {"type": "string", "description": "Project root (default: .)"}}, "required": ["task"]})),
        ("ctx_wrapped", "Savings report card. Periods: week|month|all.", json!({"type": "object", "properties": {"period": {"type": "string"}}})),
        ("ctx_semantic_search", "BM25 code search by meaning. action=reindex to rebuild.", json!({"type": "object", "properties": {"query": {"type": "string"}, "path": {"type": "string"}, "top_k": {"type": "integer"}, "action": {"type": "string"}}, "required": ["query"]})),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_use_unified_defaults_to_false() {
        assert!(!should_use_unified("cursor"));
        assert!(!should_use_unified("claude-code"));
        assert!(!should_use_unified("windsurf"));
        assert!(!should_use_unified(""));
        assert!(!should_use_unified("some-unknown-client"));
    }

    #[test]
    fn test_unified_tool_count() {
        let tools = unified_tool_defs();
        assert_eq!(tools.len(), 5, "Expected 5 unified tools");
    }
}
