use rmcp::model::Tool;
use serde_json::json;

use super::tool_def;

/// Full tool definitions for the MCP `tools/list` fallback and the lazy core
/// set.
///
/// Single source of truth: derived directly from the canonical tool registry
/// (`server::registry::build_registry`) so this list can never drift from the
/// trait-based `McpTool::tool_def()` schemas. `build_registry()` is a pure,
/// cheap-to-build function (registers unit structs only), so calling it here is
/// inexpensive and keeps schemas in lock-step (#141).
#[must_use]
pub fn granular_tool_defs() -> Vec<Tool> {
    crate::server::registry::build_registry().tool_defs()
}

/// Consolidated tool surface for `LEAN_CTX_UNIFIED` clients: the four native
/// replacements plus a single `ctx` meta-tool that fans out to every sub-tool
/// via an `action` argument. This is an intentionally distinct surface (the
/// `ctx` meta-tool does not exist as a standalone registry entry), so it is
/// maintained here rather than derived from the registry.
#[must_use]
pub fn unified_tool_defs() -> Vec<Tool> {
    vec![
        tool_def(
            "ctx_read",
            "Read file (replaces native Read). Cached, re-reads ~13 tok. Omit mode to auto-select; full only right before editing. Modes: auto|full|map|signatures|diff|aggressive|entropy|task|reference|lines:N-M. fresh=true re-reads.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "mode": { "type": "string", "default": "auto" },
                    "start_line": { "type": "integer", "description": "Read from this 1-based line on (alias: offset)" },
                    "offset": { "type": "integer", "description": "Alias for start_line (1-based first line)" },
                    "limit": { "type": "integer", "description": "Max number of lines to read from start_line/offset" },
                    "fresh": { "type": "boolean" }
                },
                "required": ["path"]
            }),
        ),
        tool_def(
            "ctx_shell",
            "Run shell (replaces native Shell). Compressed output (~95 patterns). raw=true for verbatim. cwd sets working dir (default: project root).",
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
            "Search code (replaces native Grep/rg) — use when you know the exact pattern. Regex, .gitignore aware, token-efficient. For understanding code, use ctx_compose FIRST.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern" },
                    "path": { "type": "string" },
                    "include": { "type": "string", "description": "File filter glob (e.g. *.ts, *.{rs,ts}, src/**/*.tsx)" },
                    "ext": { "type": "string", "description": "Deprecated alias for `include` (bare extension → *.ext)" },
                    "max_results": { "type": "integer" },
                    "ignore_gitignore": { "type": "boolean" }
                },
                "required": ["pattern"]
            }),
        ),
        tool_def(
            "ctx_tree",
            "Directory tree (replaces ls/find) — compact maps with file counts per directory. depth=N (default 3); paths for multi-root. Use for orientation before ctx_repomap or ctx_compose.",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #141: the fallback / lazy tool list must stay in lock-step with the
    /// canonical registry so schemas can never drift. `granular_tool_defs`
    /// delegates to the registry; this guards against anyone reintroducing a
    /// hand-maintained copy with divergent schemas (the original bug: ctx_read
    /// default mode `full` in granular vs `auto` in the registry).
    #[test]
    fn granular_defs_match_registry() {
        let granular = granular_tool_defs();
        let registry = crate::server::registry::build_registry().tool_defs();
        assert_eq!(
            granular.len(),
            registry.len(),
            "granular must mirror the registry tool set"
        );
        for (g, r) in granular.iter().zip(registry.iter()) {
            assert_eq!(g.name, r.name, "tool name drift");
            assert_eq!(
                g.description, r.description,
                "description drift for {}",
                g.name
            );
            assert_eq!(
                g.input_schema, r.input_schema,
                "schema drift for {}",
                g.name
            );
        }
    }

    /// Every curated core tool must exist in the registry, otherwise lazy mode
    /// would silently drop it (the #141 drift symptom).
    #[test]
    fn core_tool_names_exist_in_registry() {
        let registry = crate::server::registry::build_registry();
        for &name in crate::tool_defs::CORE_TOOL_NAMES {
            assert!(
                registry.contains(name),
                "CORE_TOOL_NAMES references '{name}' which is not registered"
            );
        }
    }

    /// The unified surface may only advertise registry tools plus the single
    /// `ctx` meta-tool (which fans out via its `action` argument).
    #[test]
    fn unified_names_are_registry_tools_or_meta() {
        let registry = crate::server::registry::build_registry();
        for t in unified_tool_defs() {
            let name = t.name.as_ref();
            assert!(
                name == "ctx" || registry.contains(name),
                "unified tool '{name}' is neither the ctx meta-tool nor a registry tool"
            );
        }
    }
}
