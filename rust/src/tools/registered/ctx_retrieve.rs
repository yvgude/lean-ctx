use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxRetrieveTool;

impl McpTool for CtxRetrieveTool {
    fn name(&self) -> &'static str {
        "ctx_retrieve"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_retrieve",
            "Retrieve original uncompressed content from the session cache (CCR) —\n\
             restores full verbatim source when compressed ctx_read output is insufficient.\n\
             WORKFLOW: call ctx_read FIRST to populate cache, then ctx_retrieve for verbatim.\n\
             query='text' to find matching lines within cached content.\n\
             ANTIPATTERN: not for reading files directly — use ctx_read.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path previously read via ctx_read"
                    },
                    "query": {
                        "type": "string",
                        "description": "Search within cached content for matching lines"
                    }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path_raw = get_str(args, "path")
            .ok_or_else(|| ErrorData::invalid_params("path is required", None))?;
        let resolved = if let Some(p) = ctx.resolved_path("path") {
            p.to_string()
        } else if let Some(err) = ctx.path_error("path") {
            return Err(ErrorData::invalid_params(format!("path: {err}"), None));
        } else {
            path_raw.clone()
        };
        let query = get_str(args, "query");

        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(guard) = crate::server::bounded_lock::read(cache, "ctx_retrieve") else {
            return Ok(ToolOutput::simple(
                "[retrieve unavailable — cache busy, retry]".to_string(),
            ));
        };
        // `current_full_content` revalidates against disk and re-reads when the
        // cached copy is stale, so CCR can never hand back a version that no
        // longer matches the file (e.g. a handover file edited between agents).
        let result = match guard.current_full_content(&resolved) {
            Some((full, _tokens)) => {
                if let Some(ref q) = query {
                    ccr_search_within(&full, q)
                } else {
                    full
                }
            }
            None => {
                format!("No cached content for \"{path_raw}\". Use ctx_read(\"{path_raw}\") first.")
            }
        };

        Ok(ToolOutput::simple(result))
    }
}

fn ccr_search_within(content: &str, query: &str) -> String {
    let query_lower = query.to_lowercase();
    let terms: Vec<&str> = query_lower.split_whitespace().collect();
    if terms.is_empty() {
        return content.to_string();
    }

    let mut matches: Vec<(usize, &str)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let lower = line.to_lowercase();
        if terms.iter().any(|t| lower.contains(t)) {
            matches.push((i + 1, line));
        }
    }

    if matches.is_empty() {
        return format!("No lines matching \"{query}\" in cached content.");
    }

    let total = content.lines().count();
    let mut out = format!("# {}/{total} lines match \"{query}\"\n", matches.len());
    for (lineno, line) in matches.iter().take(200) {
        out.push_str(&format!("{lineno:>6}| {line}\n"));
    }
    if matches.len() > 200 {
        out.push_str(&format!("... and {} more matches\n", matches.len() - 200));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cache::SessionCache;
    use crate::server::tool_trait::ToolContext;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn ctx_with_cache(cache: Arc<RwLock<SessionCache>>, path: &str) -> ToolContext {
        ToolContext {
            cache: Some(cache),
            resolved_paths: HashMap::from([("path".to_string(), path.to_string())]),
            ..Default::default()
        }
    }

    fn args(path: &str, query: Option<&str>) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("path".to_string(), Value::String(path.to_string()));
        if let Some(q) = query {
            m.insert("query".to_string(), Value::String(q.to_string()));
        }
        m
    }

    /// Run the real handler the way dispatch does: synchronously on a blocking
    /// thread that still has a runtime handle (so `bounded_lock` can block_on).
    async fn run(args: Map<String, Value>, ctx: ToolContext) -> String {
        tokio::task::spawn_blocking(move || CtxRetrieveTool.handle(&args, &ctx))
            .await
            .unwrap()
            .unwrap()
            .text
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retrieve_serves_cached_when_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("h.md");
        std::fs::write(&file, "FRESH marker-AAA\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let cache = Arc::new(RwLock::new(SessionCache::new()));
        cache.write().await.store(&path, "FRESH marker-AAA\n");

        let out = run(args(&path, None), ctx_with_cache(cache, &path)).await;
        assert!(out.contains("marker-AAA"), "got: {out}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retrieve_rereads_changed_file_not_stale() {
        // CCR retrieve after the file was edited must return the NEW content.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("h.md");
        std::fs::write(&file, "V1 marker-AAA\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let cache = Arc::new(RwLock::new(SessionCache::new()));
        cache.write().await.store(&path, "V1 marker-AAA\n");

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&file, "V2 marker-BBB\n").unwrap();

        let out = run(args(&path, None), ctx_with_cache(cache, &path)).await;
        assert!(out.contains("marker-BBB"), "fresh content missing: {out}");
        assert!(!out.contains("marker-AAA"), "stale content served: {out}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retrieve_query_runs_on_fresh_content() {
        // A query must search the CURRENT file, not the stale cached copy.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("h.md");
        std::fs::write(&file, "old keep\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let cache = Arc::new(RwLock::new(SessionCache::new()));
        cache.write().await.store(&path, "old keep\n");

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&file, "alpha\nNEEDLE here\nbeta\n").unwrap();

        let out = run(args(&path, Some("NEEDLE")), ctx_with_cache(cache, &path)).await;
        assert!(
            out.contains("NEEDLE"),
            "query must match fresh content: {out}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retrieve_without_cache_entry_directs_to_ctx_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("h.md");
        std::fs::write(&file, "x\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let cache = Arc::new(RwLock::new(SessionCache::new())); // empty
        let out = run(args(&path, None), ctx_with_cache(cache, &path)).await;
        assert!(out.contains("No cached content"), "got: {out}");
    }
}
