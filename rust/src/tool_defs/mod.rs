use std::sync::Arc;

use rmcp::model::*;
use serde_json::{json, Map, Value};

pub fn tool_def(name: &'static str, description: &'static str, schema_value: Value) -> Tool {
    let schema: Map<String, Value> = match schema_value {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    Tool::new(name, description, Arc::new(schema))
}

mod granular;
pub use granular::granular_tool_defs;

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
task (A2A tasks), workflow (state machine).",
            json!({
                "type": "object",
                "properties": {
                    "tool": {
                        "type": "string",
                        "description": "compress|metrics|analyze|cache|discover|smart_read|delta|dedup|fill|intent|response|context|graph|session|knowledge|agent|overview|wrapped|benchmark|multi_read|semantic_search|cost|heatmap|impact|architecture|task|workflow"
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

const CORE_TOOL_NAMES: &[&str] = &[
    "ctx_read",
    "ctx_multi_read",
    "ctx_shell",
    "ctx_search",
    "ctx_tree",
    "ctx_edit",
    "ctx_session",
    "ctx_knowledge",
];

pub fn lazy_tool_defs() -> Vec<Tool> {
    let all = granular_tool_defs();
    let mut core: Vec<Tool> = all
        .into_iter()
        .filter(|t| CORE_TOOL_NAMES.contains(&t.name.as_ref()))
        .collect();

    core.push(tool_def(
        "ctx_discover_tools",
        "Search available lean-ctx tools by keyword. Returns matching tool names + descriptions for on-demand loading.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keyword (e.g. 'graph', 'cost', 'workflow', 'dedup')"
                }
            },
            "required": ["query"]
        }),
    ));

    core
}

pub fn discover_tools(query: &str) -> String {
    let all = list_all_tool_defs();
    let query_lower = query.to_lowercase();
    let matches: Vec<(&str, &str)> = all
        .iter()
        .filter(|(name, desc, _)| {
            name.to_lowercase().contains(&query_lower) || desc.to_lowercase().contains(&query_lower)
        })
        .map(|(name, desc, _)| (*name, *desc))
        .collect();

    if matches.is_empty() {
        return format!("No tools found matching '{query}'. Try broader terms like: graph, cost, session, search, compress, agent, workflow, gain.");
    }

    let mut out = format!("{} tools matching '{query}':\n", matches.len());
    for (name, desc) in &matches {
        let short = if desc.len() > 80 { &desc[..80] } else { desc };
        out.push_str(&format!("  {name} — {short}\n"));
    }
    out.push_str("\nCall the tool directly by name to use it.");
    out
}

pub fn is_lazy_mode() -> bool {
    std::env::var("LEAN_CTX_LAZY_TOOLS").is_ok()
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
    ]
}
