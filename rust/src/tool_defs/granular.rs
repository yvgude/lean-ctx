use rmcp::model::*;
use serde_json::{json, Value};

use super::tool_def;

pub fn granular_tool_defs() -> Vec<Tool> {
    vec![
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
                        "description": "Compression mode (default: full). Same modes as ctx_read (auto, full, map, signatures, diff, aggressive, entropy, task, reference, lines:N-M)."
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
            "Run shell command (compressed output, 90+ patterns). Use raw=true to skip compression. cwd sets working directory (persists across calls via cd tracking).",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "raw": { "type": "boolean", "description": "Skip compression, return full uncompressed output. Use for small outputs or when full detail is critical." },
                    "cwd": { "type": "string", "description": "Working directory for the command. If omitted, uses last cd target or project root." }
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
            "ctx_edit",
            "Edit a file via search-and-replace. Works without native Read/Edit tools. Use this when the IDE's Edit tool requires Read but Read is unavailable.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute file path" },
                    "old_string": { "type": "string", "description": "Exact text to find and replace (must be unique unless replace_all=true)" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences (default: false)", "default": false },
                    "create": { "type": "boolean", "description": "Create a new file with new_string as content (ignores old_string)", "default": false }
                },
                "required": ["path", "new_string"]
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
                    },
                    "task": {
                        "type": "string",
                        "description": "Optional task for POP intent-driven pruning"
                    }
                },
                "required": ["paths", "budget"]
            }),
        ),
        tool_def(
            "ctx_intent",
            "Structured intent input (optional) — submit compact JSON or short text; server also infers intents automatically from tool calls.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Compact JSON intent or short text" },
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
            "Persistent project knowledge (survives sessions). Actions: remember (store fact with temporal tracking + contradiction detection), \
recall (search), pattern (record convention), consolidate (extract session findings), \
gotcha (record a bug/mistake to never repeat — trigger+resolution required), \
timeline (view fact history for a category), rooms (list knowledge categories), \
search (cross-session search across ALL projects), wakeup (compact AAAK briefing), \
status (list all), remove, export, embeddings_status|embeddings_reset|embeddings_reindex (local semantic index management for recall).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["remember", "recall", "pattern", "consolidate", "gotcha", "status", "remove", "export", "timeline", "rooms", "search", "wakeup", "embeddings_status", "embeddings_reset", "embeddings_reindex"],
                        "description": "Knowledge operation. remember: auto-detects contradictions + tracks temporal validity. timeline: view version history. rooms: list categories. search: cross-project search. wakeup: compact AAAK briefing. embeddings_*: manage local semantic index for recall."
                    },
                    "trigger": {
                        "type": "string",
                        "description": "For gotcha action: what triggers the bug (e.g. 'cargo build fails with E0507 on match arms')"
                    },
                    "resolution": {
                        "type": "string",
                        "description": "For gotcha action: how to fix/avoid it (e.g. 'Use .clone() or ref pattern')"
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["critical", "warning", "info"],
                        "description": "For gotcha action: severity level (default: warning)"
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
            "Multi-agent coordination (shared message bus + persistent diaries). Actions: register (join with agent_type+role), \
post (broadcast or direct message with category), read (poll messages), status (update state: active|idle|finished), \
handoff (transfer task to another agent with summary), sync (overview of all agents + pending messages + shared contexts), \
diary (log discovery/decision/blocker/progress/insight — persisted across sessions), \
recall_diary (read agent diary), diaries (list all agent diaries), \
list, info.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["register", "list", "post", "read", "status", "info", "handoff", "sync", "diary", "recall_diary", "diaries", "share_knowledge", "receive_knowledge"],
                        "description": "Agent operation. diary: persistent log. share_knowledge: broadcast key=value facts (message: 'k1=v1;k2=v2'). receive_knowledge: poll shared facts from other agents."
                    },
                    "agent_type": {
                        "type": "string",
                        "description": "Agent type for register (cursor, claude, codex, gemini, crush, subagent)"
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
            "ctx_share",
            "Share cached file contexts between agents. Actions: push (share files from your cache to another agent), \
pull (receive files shared by other agents), list (show all shared contexts), clear (remove your shared contexts).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["push", "pull", "list", "clear"],
                        "description": "Share operation to perform"
                    },
                    "paths": {
                        "type": "string",
                        "description": "Comma-separated file paths to share (for push action)"
                    },
                    "to_agent": {
                        "type": "string",
                        "description": "Target agent ID (omit for broadcast to all agents)"
                    },
                    "message": {
                        "type": "string",
                        "description": "Optional context message explaining what was shared"
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
            "ctx_prefetch",
            "Predictive prefetch — prewarm cache for blast radius files (graph + task signals) within budgets.",
            json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string", "description": "Project root (default: .)" },
                    "task": { "type": "string", "description": "Optional task for relevance scoring" },
                    "changed_files": { "type": "array", "items": { "type": "string" }, "description": "Optional changed files (paths) to compute blast radius" },
                    "budget_tokens": { "type": "integer", "description": "Soft budget hint for mode selection (default: 3000)" },
                    "max_files": { "type": "integer", "description": "Max files to prefetch (default: 10)" }
                }
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
            "ctx_cost",
            "Cost attribution (local-first). Actions: report|agent|tools|json|reset.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["report", "agent", "tools", "json", "reset", "status"],
                        "description": "Operation to perform (default: report)"
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Agent ID for action=agent (optional)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max rows (default: 10)"
                    }
                }
            }),
        ),
        tool_def(
            "ctx_gain",
            "Gain report.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "report", "score", "cost", "tasks", "heatmap", "wrapped", "agents", "json"]
                    },
                    "period": {
                        "type": "string",
                        "enum": ["week", "month", "all"]
                    },
                    "model": {
                        "type": "string"
                    },
                    "limit": {
                        "type": "integer"
                    }
                }
            }),
        ),
        tool_def(
            "ctx_feedback",
            "Harness feedback for LLM output tokens/latency (local-first). Actions: record|report|json|reset|status.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["record", "report", "json", "reset", "status"],
                        "description": "Operation to perform (default: report)"
                    },
                    "agent_id": { "type": "string", "description": "Agent ID (optional; defaults to current agent when available)" },
                    "intent": { "type": "string", "description": "Intent/task string (optional)" },
                    "model": { "type": "string", "description": "Model identifier (optional)" },
                    "llm_input_tokens": { "type": "integer", "description": "Required for action=record" },
                    "llm_output_tokens": { "type": "integer", "description": "Required for action=record" },
                    "latency_ms": { "type": "integer", "description": "Optional for action=record" },
                    "note": { "type": "string", "description": "Optional note (no prompts/PII)" },
                    "limit": { "type": "integer", "description": "For report/json: how many recent events to consider (default: 500)" }
                }
            }),
        ),
        tool_def(
            "ctx_handoff",
            "Context Ledger Protocol (hashed, deterministic, local-first). Actions: create|show|list|pull|clear.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "show", "list", "pull", "clear"],
                        "description": "Operation to perform (default: list)"
                    },
                    "path": { "type": "string", "description": "Ledger file path (for show/pull)" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Optional file paths to include as signatures-only curated refs (for create)" },
                    "apply_workflow": { "type": "boolean", "description": "For pull: apply workflow state (default: true)" },
                    "apply_session": { "type": "boolean", "description": "For pull: apply session/task snapshot (default: true)" },
                    "apply_knowledge": { "type": "boolean", "description": "For pull: import knowledge facts (default: true)" }
                }
            }),
        ),
        tool_def(
            "ctx_heatmap",
            "File access heatmap (local-first). Actions: status|directory|cold|json.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "directory", "dirs", "cold", "json"],
                        "description": "Operation to perform (default: status)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Project root for cold scan (default: .)"
                    }
                }
            }),
        ),
        tool_def(
            "ctx_task",
            "Multi-agent task orchestration. Actions: create|update|list|get|cancel|message|info.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "update", "list", "get", "cancel", "message", "info"],
                        "description": "Task operation"
                    },
                    "task_id": { "type": "string", "description": "Task ID (required for update|get|cancel|message)" },
                    "to_agent": { "type": "string", "description": "Target agent ID (required for create)" },
                    "description": { "type": "string", "description": "Task description (for create)" },
                    "state": { "type": "string", "description": "New state for update (working|input-required|completed|failed|canceled)" },
                    "message": { "type": "string", "description": "Optional message / reason" }
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "ctx_impact",
            "Graph-based impact analysis. Actions: analyze|chain|build|status.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["analyze", "chain", "build", "status"],
                        "description": "Impact operation (default: analyze)"
                    },
                    "path": { "type": "string", "description": "Target file path (required for analyze). For chain: from->to spec." },
                    "root": { "type": "string", "description": "Project root (default: .)" },
                    "depth": { "type": "integer", "description": "Max traversal depth (default: 5)" }
                }
            }),
        ),
        tool_def(
            "ctx_architecture",
            "Graph-based architecture analysis. Actions: overview|clusters|layers|cycles|entrypoints|module.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["overview", "clusters", "layers", "cycles", "entrypoints", "module"],
                        "description": "Architecture operation (default: overview)"
                    },
                    "path": { "type": "string", "description": "Used for action=module (module/file path)" },
                    "root": { "type": "string", "description": "Project root (default: .)" }
                }
            }),
        ),
        tool_def(
            "ctx_workflow",
            "Workflow rails (state machine + evidence). Actions: start|status|transition|complete|evidence_add|evidence_list|stop.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "status", "transition", "complete", "evidence_add", "evidence_list", "stop"],
                        "description": "Workflow operation (default: status)"
                    },
                    "name": { "type": "string", "description": "Optional workflow name override (action=start)" },
                    "spec": { "type": "string", "description": "WorkflowSpec JSON (action=start). If omitted, uses builtin plan_code_test." },
                    "to": { "type": "string", "description": "Target state (action=transition)" },
                    "key": { "type": "string", "description": "Evidence key (action=evidence_add)" },
                    "value": { "type": "string", "description": "Optional evidence value / transition note" }
                }
            }),
        ),
        tool_def(
            "ctx_semantic_search",
            "Semantic code search (BM25 + optional embeddings/hybrid). action=reindex to rebuild.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language search query" },
                    "path": { "type": "string", "description": "Project root to search (default: .)" },
                    "top_k": { "type": "integer", "description": "Number of results (default: 10)" },
                    "action": { "type": "string", "description": "reindex to rebuild index" },
                    "mode": {
                        "type": "string",
                        "enum": ["bm25", "dense", "hybrid"],
                        "description": "Search mode (default: hybrid). bm25=lexical only, dense=embeddings only, hybrid=BM25+embeddings"
                    },
                    "languages": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional: restrict to languages/extensions (e.g. [\"rust\",\"ts\",\"py\"] or [\"rs\",\"tsx\"])"
                    },
                    "path_glob": {
                        "type": "string",
                        "description": "Optional: glob over relative file paths (e.g. \"rust/src/**\" or \"**/*.rs\")"
                    }
                },
                "required": ["query"]
            }),
        ),
        tool_def(
            "ctx_execute",
            "Run code in sandbox (11 languages). Only stdout enters context. Raw data never leaves subprocess. Languages: javascript, typescript, python, shell, ruby, go, rust, php, perl, r, elixir.",
            json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "Language: javascript|typescript|python|shell|ruby|go|rust|php|perl|r|elixir"
                    },
                    "code": {
                        "type": "string",
                        "description": "Code to execute in sandbox"
                    },
                    "intent": {
                        "type": "string",
                        "description": "What you want from the output (triggers intent-driven filtering for large results)"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 30)"
                    },
                    "action": {
                        "type": "string",
                        "description": "batch — execute multiple scripts. Provide items as JSON array [{language, code}]"
                    },
                    "items": {
                        "type": "string",
                        "description": "JSON array of [{\"language\": \"...\", \"code\": \"...\"}] for batch execution"
                    },
                    "path": {
                        "type": "string",
                        "description": "For action=file: process a file in sandbox (auto-detects language)"
                    }
                },
                "required": ["language", "code"]
            }),
        ),
        tool_def(
            "ctx_symbol",
            "Read a specific symbol (function, struct, class) by name. Returns only the symbol \
code block instead of the entire file. 90-97% fewer tokens than full file read.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Symbol name (function, struct, class, method)" },
                    "file": { "type": "string", "description": "Optional: file path to narrow search" },
                    "kind": { "type": "string", "description": "Optional: fn|struct|class|method|trait|enum" }
                },
                "required": ["name"]
            }),
        ),
        tool_def(
            "ctx_graph_diagram",
            "Generate a Mermaid diagram of the dependency or call graph. Useful for understanding architecture.",
            json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Optional: scope to dependencies of a specific file" },
                    "depth": { "type": "integer", "description": "Max depth (default: 2)" },
                    "kind": { "type": "string", "description": "deps (file dependencies) or calls (symbol call graph)" }
                }
            }),
        ),
        tool_def(
            "ctx_routes",
            "List HTTP routes/endpoints extracted from the project. Supports Express, Flask, FastAPI, Actix, Spring, Rails, Next.js.",
            json!({
                "type": "object",
                "properties": {
                    "method": { "type": "string", "description": "Optional: GET, POST, PUT, DELETE" },
                    "path": { "type": "string", "description": "Optional: path prefix filter, e.g. /api/users" }
                }
            }),
        ),
        tool_def(
            "ctx_compress_memory",
            "Compress a memory/config file (CLAUDE.md, .cursorrules, etc.) to save tokens on every session start. \
Preserves code blocks, URLs, paths, headings, tables. Creates .original.md backup.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to memory file" }
                },
                "required": ["path"]
            }),
        ),
        tool_def(
            "ctx_callers",
            "Find all symbols that call a given function/method. Returns caller file, symbol, and line.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name to find callers of" },
                    "file": { "type": "string", "description": "Optional: scope to a specific file" }
                },
                "required": ["symbol"]
            }),
        ),
        tool_def(
            "ctx_callees",
            "Find all functions/methods called by a given symbol. Returns callee name, file, and line.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name to find callees of" },
                    "file": { "type": "string", "description": "Optional: scope to a specific file" }
                },
                "required": ["symbol"]
            }),
        ),
        tool_def(
            "ctx_outline",
            "List all symbols in a file (functions, structs, classes, methods) with signatures. \
Much fewer tokens than reading the full file.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "kind": { "type": "string", "description": "Optional filter: fn|struct|class|all" }
                },
                "required": ["path"]
            }),
        ),
        tool_def(
            "ctx_expand",
            "Retrieve archived tool output (zero-loss). Large outputs are auto-archived; use this to retrieve full details. Actions: retrieve (default), list.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Archive ID from the [Archived: ...] hint" },
                    "action": { "type": "string", "description": "retrieve (default) or list" },
                    "start_line": { "type": "integer", "description": "Start line for range retrieval" },
                    "end_line": { "type": "integer", "description": "End line for range retrieval" },
                    "search": { "type": "string", "description": "Search pattern to filter archived output" },
                    "session_id": { "type": "string", "description": "Filter list by session ID" }
                }
            }),
        ),
    ]
}

pub fn unified_tool_defs() -> Vec<Tool> {
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
            "Run shell command (compressed output). raw=true skips compression. cwd sets working directory.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command" },
                    "raw": { "type": "boolean", "description": "Skip compression for full output" },
                    "cwd": { "type": "string", "description": "Working directory (defaults to last cd or project root)" }
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
knowledge (remember|recall|pattern|consolidate|timeline|rooms|search|wakeup|status|remove|export|embeddings_status|embeddings_reset|embeddings_reindex), \
agent (register|post|read|status|list|info|diary|recall_diary|diaries), overview (project map), \
wrapped (savings report), benchmark (file|project), multi_read (batch), semantic_search (BM25), \
cost (attribution), heatmap (file access), impact (graph impact), architecture (graph structure), \
task (A2A tasks), workflow (state machine), expand (retrieve archived output).",
            json!({
                "type": "object",
                "properties": {
                    "tool": {
                        "type": "string",
                        "description": "compress|metrics|analyze|cache|discover|smart_read|delta|dedup|fill|intent|response|context|graph|session|knowledge|agent|overview|wrapped|benchmark|multi_read|semantic_search|cost|heatmap|impact|architecture|task|workflow|expand"
                    },
                    "action": { "type": "string" },
                    "path": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "query": { "type": "string" },
                    "value": { "type": "string" },
                    "category": { "type": "string" },
                    "key": { "type": "string" },
                    "to": { "type": "string" },
                    "spec": { "type": "string" },
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
                    "task_id": { "type": "string" },
                    "agent_id": { "type": "string" },
                    "description": { "type": "string" },
                    "state": { "type": "string" },
                    "root": { "type": "string" },
                    "depth": { "type": "integer" },
                    "show_hidden": { "type": "boolean" }
                },
                "required": ["tool"]
            }),
        ),
    ]
}

pub fn list_all_tool_defs() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("ctx_read", "Read file (cached, compressed). Re-reads ~13 tok. Auto-selects optimal mode. \
Modes: full|map|signatures|diff|aggressive|entropy|task|reference|lines:N-M. fresh=true re-reads.", json!({"type": "object", "properties": {"path": {"type": "string"}, "mode": {"type": "string"}, "start_line": {"type": "integer"}, "fresh": {"type": "boolean"}}, "required": ["path"]})),
        ("ctx_multi_read", "Batch read files in one call. Same modes as ctx_read.", json!({"type": "object", "properties": {"paths": {"type": "array", "items": {"type": "string"}}, "mode": {"type": "string"}}, "required": ["paths"]})),
        ("ctx_tree", "Directory listing with file counts.", json!({"type": "object", "properties": {"path": {"type": "string"}, "depth": {"type": "integer"}, "show_hidden": {"type": "boolean"}}})),
        ("ctx_shell", "Run shell command (compressed output, 90+ patterns). cwd sets working directory.", json!({"type": "object", "properties": {"command": {"type": "string"}, "cwd": {"type": "string", "description": "Working directory"}}, "required": ["command"]})),
        ("ctx_search", "Regex code search (.gitignore aware, compact results).", json!({"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}, "ext": {"type": "string"}, "max_results": {"type": "integer"}}, "required": ["pattern"]})),
        ("ctx_compress", "Context checkpoint for long conversations.", json!({"type": "object", "properties": {"include_signatures": {"type": "boolean"}}})),
        ("ctx_benchmark", "Benchmark compression modes for a file or project.", json!({"type": "object", "properties": {"path": {"type": "string"}, "action": {"type": "string"}, "format": {"type": "string"}}, "required": ["path"]})),
        ("ctx_metrics", "Session token stats, cache rates, per-tool savings.", json!({"type": "object", "properties": {}})),
        ("ctx_analyze", "Entropy analysis — recommends optimal compression mode for a file.", json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})),
        ("ctx_cache", "Cache ops: status|clear|invalidate.", json!({"type": "object", "properties": {"action": {"type": "string"}, "path": {"type": "string"}}, "required": ["action"]})),
        ("ctx_discover", "Find missed compression opportunities in shell history.", json!({"type": "object", "properties": {"limit": {"type": "integer"}}})),
        ("ctx_smart_read", "Auto-select optimal read mode for a file.", json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})),
        ("ctx_delta", "Incremental diff — sends only changed lines since last read.", json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})),
        ("ctx_edit", "Edit a file via search-and-replace. Works without native Read/Edit tools. Use when Edit requires Read but Read is unavailable.", json!({"type": "object", "properties": {"path": {"type": "string"}, "old_string": {"type": "string"}, "new_string": {"type": "string"}, "replace_all": {"type": "boolean"}, "create": {"type": "boolean"}}, "required": ["path", "new_string"]})),
        ("ctx_dedup", "Cross-file dedup: analyze or apply shared block references.", json!({"type": "object", "properties": {"action": {"type": "string"}}})),
        ("ctx_fill", "Budget-aware context fill — auto-selects compression per file within token limit.", json!({"type": "object", "properties": {"paths": {"type": "array", "items": {"type": "string"}}, "budget": {"type": "integer"}, "task": {"type": "string"}}, "required": ["paths", "budget"]})),
        ("ctx_intent", "Structured intent input (optional) — submit compact JSON or short text; server also infers intents automatically from tool calls.", json!({"type": "object", "properties": {"query": {"type": "string"}, "project_root": {"type": "string"}}, "required": ["query"]})),
        ("ctx_response", "Compress LLM response text (remove filler, apply TDD).", json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})),
        ("ctx_context", "Session context overview — cached files, seen files, session state.", json!({"type": "object", "properties": {}})),
        ("ctx_graph", "Code dependency graph. Actions: build (index project), related (find files connected to path), \
symbol (lookup definition/usages as file::name), impact (blast radius of changes to path), status (index stats).", json!({"type": "object", "properties": {"action": {"type": "string"}, "path": {"type": "string"}, "project_root": {"type": "string"}}, "required": ["action"]})),
        ("ctx_session", "Cross-session memory (CCP). Actions: load (restore previous session ~400 tok), \
save, status, task (set current task), finding (record discovery), decision (record choice), \
reset, list (show sessions), cleanup, snapshot (build compaction snapshot ~2KB), \
restore (rebuild state from snapshot after context compaction).", json!({"type": "object", "properties": {"action": {"type": "string"}, "value": {"type": "string"}, "session_id": {"type": "string"}}, "required": ["action"]})),
        ("ctx_knowledge", "Persistent project knowledge with temporal facts + contradiction detection. Actions: remember (auto-tracks validity + detects contradictions), recall, pattern, consolidate, \
gotcha (record a bug to never repeat — trigger+resolution), timeline (fact version history), rooms (list knowledge categories), \
search (cross-session/cross-project), wakeup (compact AAAK briefing), status, remove, export, embeddings_status|embeddings_reset|embeddings_reindex.", json!({"type": "object", "properties": {"action": {"type": "string"}, "category": {"type": "string"}, "key": {"type": "string"}, "value": {"type": "string"}, "query": {"type": "string"}, "trigger": {"type": "string"}, "resolution": {"type": "string"}, "severity": {"type": "string"}}, "required": ["action"]})),
        ("ctx_agent", "Multi-agent coordination with persistent diaries. Actions: register, \
post, read, status, handoff, sync, diary (log discovery/decision/blocker/progress/insight — persisted), \
recall_diary (read diary), diaries (list all), list, info.", json!({"type": "object", "properties": {"action": {"type": "string"}, "agent_type": {"type": "string"}, "role": {"type": "string"}, "message": {"type": "string"}, "to_agent": {"type": "string"}, "status": {"type": "string"}}, "required": ["action"]})),
        ("ctx_share", "Share cached file contexts between agents. Actions: push (share files from cache), \
pull (receive shared files), list (show all shared contexts), clear (remove your shared contexts).", json!({"type": "object", "properties": {"action": {"type": "string"}, "paths": {"type": "string"}, "to_agent": {"type": "string"}, "message": {"type": "string"}}, "required": ["action"]})),
        ("ctx_overview", "Task-relevant project map — use at session start.", json!({"type": "object", "properties": {"task": {"type": "string"}, "path": {"type": "string"}}})),
        ("ctx_preload", "Proactive context loader — reads and caches task-relevant files, returns compact L-curve-optimized summary with critical lines, imports, and signatures. Costs ~50-100 tokens instead of ~5000 for individual reads.", json!({"type": "object", "properties": {"task": {"type": "string", "description": "Task description (e.g. 'fix auth bug in validate_token')"}, "path": {"type": "string", "description": "Project root (default: .)"}}, "required": ["task"]})),
        ("ctx_prefetch", "Predictive prefetch — prewarm cache for blast radius files (graph + task signals) within budgets.", json!({"type": "object", "properties": {"root": {"type": "string"}, "task": {"type": "string"}, "changed_files": {"type": "array", "items": {"type": "string"}}, "budget_tokens": {"type": "integer"}, "max_files": {"type": "integer"}}})),
        ("ctx_wrapped", "Savings report card. Periods: week|month|all.", json!({"type": "object", "properties": {"period": {"type": "string"}}})),
        ("ctx_cost", "Cost attribution (local-first). Actions: report|agent|tools|json|reset.", json!({"type": "object", "properties": {"action": {"type": "string"}, "agent_id": {"type": "string"}, "limit": {"type": "integer"}}})),
        ("ctx_gain", "Gain report.", json!({"type": "object", "properties": {"action": {"type": "string"}, "period": {"type": "string"}, "model": {"type": "string"}, "limit": {"type": "integer"}}})),
        ("ctx_feedback", "Harness feedback for LLM output tokens/latency (local-first). Actions: record|report|json|reset|status.", json!({"type": "object", "properties": {"action": {"type": "string"}, "agent_id": {"type": "string"}, "intent": {"type": "string"}, "model": {"type": "string"}, "llm_input_tokens": {"type": "integer"}, "llm_output_tokens": {"type": "integer"}, "latency_ms": {"type": "integer"}, "note": {"type": "string"}, "limit": {"type": "integer"}}})),
        ("ctx_handoff", "Context Ledger Protocol (hashed, deterministic, local-first). Actions: create|show|list|pull|clear.", json!({"type": "object", "properties": {"action": {"type": "string"}, "path": {"type": "string"}, "paths": {"type": "array", "items": {"type": "string"}}, "apply_workflow": {"type": "boolean"}, "apply_session": {"type": "boolean"}, "apply_knowledge": {"type": "boolean"}}})),
        ("ctx_heatmap", "File access heatmap (local-first). Actions: status|directory|cold|json.", json!({"type": "object", "properties": {"action": {"type": "string"}, "path": {"type": "string"}}})),
        ("ctx_task", "Multi-agent task orchestration. Actions: create|update|list|get|cancel|message|info.", json!({"type": "object", "properties": {"action": {"type": "string"}, "task_id": {"type": "string"}, "to_agent": {"type": "string"}, "description": {"type": "string"}, "state": {"type": "string"}, "message": {"type": "string"}}, "required": ["action"]})),
        ("ctx_impact", "Graph-based impact analysis. Actions: analyze|chain|build|status.", json!({"type": "object", "properties": {"action": {"type": "string"}, "path": {"type": "string"}, "root": {"type": "string"}, "depth": {"type": "integer"}}})),
        ("ctx_architecture", "Graph-based architecture analysis. Actions: overview|clusters|layers|cycles|entrypoints|module.", json!({"type": "object", "properties": {"action": {"type": "string"}, "path": {"type": "string"}, "root": {"type": "string"}}})),
        ("ctx_workflow", "Workflow rails (state machine + evidence). Actions: start|status|transition|complete|evidence_add|evidence_list|stop.", json!({"type": "object", "properties": {"action": {"type": "string"}, "name": {"type": "string"}, "spec": {"type": "string"}, "to": {"type": "string"}, "key": {"type": "string"}, "value": {"type": "string"}}})),
        ("ctx_semantic_search", "Semantic code search (BM25 + optional embeddings/hybrid). action=reindex to rebuild.", json!({"type": "object", "properties": {"query": {"type": "string"}, "path": {"type": "string"}, "top_k": {"type": "integer"}, "action": {"type": "string"}, "mode": {"type": "string", "enum": ["bm25","dense","hybrid"]}, "languages": {"type": "array", "items": {"type": "string"}}, "path_glob": {"type": "string"}}, "required": ["query"]})),
        ("ctx_execute", "Run code in sandbox (11 languages). Only stdout enters context. Languages: javascript, typescript, python, shell, ruby, go, rust, php, perl, r, elixir. Actions: batch (multiple scripts), file (process file in sandbox).", json!({"type": "object", "properties": {"language": {"type": "string"}, "code": {"type": "string"}, "intent": {"type": "string"}, "timeout": {"type": "integer"}, "action": {"type": "string"}, "items": {"type": "string"}, "path": {"type": "string"}}, "required": ["language", "code"]})),
        ("ctx_symbol", "Read a specific symbol (function, struct, class) by name. Returns only the symbol code block instead of the entire file. 90-97% fewer tokens than full file read.", json!({"type": "object", "properties": {"name": {"type": "string"}, "file": {"type": "string"}, "kind": {"type": "string"}}, "required": ["name"]})),
        ("ctx_outline", "List all symbols in a file with signatures. Much fewer tokens than reading the full file.", json!({"type": "object", "properties": {"path": {"type": "string"}, "kind": {"type": "string"}}, "required": ["path"]})),
        ("ctx_compress_memory", "Compress a memory/config file (CLAUDE.md, .cursorrules) preserving code, URLs, paths. Creates .original.md backup.", json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})),
        ("ctx_callers", "Find all symbols that call a given function/method.", json!({"type": "object", "properties": {"symbol": {"type": "string"}, "file": {"type": "string"}}, "required": ["symbol"]})),
        ("ctx_callees", "Find all functions/methods called by a given symbol.", json!({"type": "object", "properties": {"symbol": {"type": "string"}, "file": {"type": "string"}}, "required": ["symbol"]})),
        ("ctx_routes", "List HTTP routes/endpoints extracted from the project. Supports Express, Flask, FastAPI, Actix, Spring, Rails, Next.js.", json!({"type": "object", "properties": {"method": {"type": "string"}, "path": {"type": "string"}}})),
        ("ctx_graph_diagram", "Generate a Mermaid diagram of the dependency or call graph.", json!({"type": "object", "properties": {"file": {"type": "string"}, "depth": {"type": "integer"}, "kind": {"type": "string"}}})),
        ("ctx_expand", "Retrieve archived tool output (zero-loss). Large outputs are auto-archived; use this to retrieve full details. Actions: retrieve (default), list.", json!({"type": "object", "properties": {"id": {"type": "string", "description": "Archive ID from the [Archived: ...] hint"}, "action": {"type": "string", "description": "retrieve (default) or list"}, "start_line": {"type": "integer", "description": "Start line for range retrieval"}, "end_line": {"type": "integer", "description": "End line for range retrieval"}, "search": {"type": "string", "description": "Search pattern to filter archived output"}, "session_id": {"type": "string", "description": "Filter list by session ID"}}})),
    ]
}
