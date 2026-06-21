use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_str, get_str_array,
};
use crate::tool_defs::tool_def;

pub struct CtxMultiReadTool;

impl McpTool for CtxMultiReadTool {
    fn name(&self) -> &'static str {
        "ctx_multi_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_multi_read",
            "Batch-read multiple files in one call — more token-efficient than N sequential\n\
             ctx_read calls. paths=['a.rs','b.rs'] reads them all at once.\n\
             mode=full for files you edit; mode=auto for general reading (compressed).\n\
             Use when you need the content of several files. For understanding code logic,\n\
             use ctx_compose FIRST — it returns relevant symbol source grouped by file.",
            json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Paths to batch-read, in order"
                    },
                    "mode": {
                        "type": "string",
                        "default": "auto",
                        "description": "Same as ctx_read modes (default auto). full→edit; raw→zero-overhead. Omit for optimal per-file"
                    },
                    "fresh": {
                        "type": "boolean",
                        "description": "Bypass cache, full re-read all. Use in subagents with stale parent cache"
                    }
                },
                "required": ["paths"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        // Panic guard (mirrors ctx_read): a panic in tree-sitter / compression must
        // never unwind through the dispatch `block_in_place` and kill the MCP server.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle_inner(args, ctx)
        })) {
            Ok(result) => result,
            Err(_) => Err(ErrorData::internal_error(
                "ctx_multi_read panicked while processing the batch. This is a bug — please report it.",
                None,
            )),
        }
    }
}

impl CtxMultiReadTool {
    #[allow(clippy::unused_self)]
    fn handle_inner(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let raw_paths = get_str_array(args, "paths")
            .ok_or_else(|| ErrorData::invalid_params("paths array is required", None))?;

        let session_lock = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let cache_lock = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;

        let cap = crate::core::limits::max_read_bytes() as u64;

        // Resolve + filter paths and capture the active task under one short read lock.
        // `bounded_lock` uses `Handle::block_on` directly — NOT a nested
        // `block_in_place` — because the dispatch layer already wraps this handler in
        // `block_in_place`. The previous nested `block_in_place` calls could exhaust the
        // 32-thread blocking pool under concurrent reads and freeze the server (#271).
        let (paths, current_task) = {
            let Some(session) =
                crate::server::bounded_lock::read(session_lock, "ctx_multi_read:session")
            else {
                return Err(ErrorData::internal_error(
                    "session read-lock timeout in ctx_multi_read — another tool may be holding it. Retry in a moment.",
                    None,
                ));
            };
            let mut paths = Vec::with_capacity(raw_paths.len());
            for p in &raw_paths {
                let resolved = super::resolve_path_sync(&session, p)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                if crate::core::binary_detect::is_binary_file(&resolved) {
                    continue;
                }
                if let Ok(meta) = std::fs::metadata(&resolved)
                    && meta.len() > cap
                {
                    continue;
                }
                paths.push(resolved);
            }
            let current_task = session.task.as_ref().map(|t| t.description.clone());
            (paths, current_task)
        };

        if paths.is_empty() {
            return Err(ErrorData::invalid_params(
                "all paths are binary or exceed the size limit",
                None,
            ));
        }

        // Default to the profile's read mode (auto) and let ctx_read resolve the
        // optimal mode per file. Previously this forced auto→full, which is exactly
        // the "everything comes back as full" complaint (#421): batch reads must
        // honour auto like single ctx_read does.
        let mode = get_str(args, "mode").unwrap_or_else(|| {
            crate::core::profiles::active_profile()
                .read
                .default_mode_effective()
                .to_string()
        });
        let fresh = get_bool(args, "fresh").unwrap_or(false);

        // Batch read under one bounded write lock. `bounded_lock` guarantees we never
        // block the runtime indefinitely and degrade gracefully on contention instead
        // of hanging; ctx_read's own fast/slow path tolerates this lock being held.
        let Some(mut cache) =
            crate::server::bounded_lock::write(cache_lock, "ctx_multi_read:cache")
        else {
            return Err(ErrorData::internal_error(
                "cache write-lock timeout in ctx_multi_read — another tool may be holding it. Retry in a moment.",
                None,
            ));
        };
        let output = crate::tools::ctx_multi_read::handle_with_task_fresh(
            &mut cache,
            &paths,
            &mode,
            fresh,
            ctx.crp_mode,
            current_task.as_deref(),
        );
        let mut total_original: usize = 0;
        for path in &paths {
            total_original =
                total_original.saturating_add(cache.get(path).map_or(0, |e| e.original_tokens));
        }
        let tokens = crate::core::tokens::count_tokens(&output);
        drop(cache);

        Ok(ToolOutput {
            text: output,
            original_tokens: total_original,
            saved_tokens: total_original.saturating_sub(tokens),
            mode: Some(mode),
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;

    use crate::core::cache::SessionCache;
    use crate::core::session::SessionState;
    use crate::tools::CrpMode;

    fn ctx_with(
        cache: Arc<RwLock<SessionCache>>,
        session: Arc<RwLock<SessionState>>,
        project_root: &str,
    ) -> ToolContext {
        ToolContext {
            project_root: project_root.to_string(),
            extra_roots: Vec::new(),
            minimal: false,
            resolved_paths: std::collections::HashMap::new(),
            crp_mode: CrpMode::Off,
            cache: Some(cache),
            session: Some(session),
            tool_calls: None,
            agent_id: None,
            workflow: None,
            ledger: None,
            client_name: None,
            pipeline_stats: None,
            call_count: None,
            autonomy: None,
            pressure_snapshot: None,
            path_errors: std::collections::HashMap::new(),
            bm25_cache: None,
            progress_sender: None,
        }
    }

    /// Regression for #271 (crash vector 11): under concurrent load,
    /// `ctx_multi_read` must not hang. The handler runs inside the dispatch
    /// layer's `block_in_place`, so it must acquire its session/cache locks
    /// via `Handle::block_on` WITHOUT nesting another `block_in_place` —
    /// nesting consumes extra blocking-pool threads and, under load, exhausts
    /// the pool, hanging the call (no JSON-RPC response → client "invoke"
    /// error).
    ///
    /// With only 2 worker threads and 8 concurrent batch reads, a nested
    /// `block_in_place` regression would deadlock the pool and trip the 20s
    /// timeout below.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_multi_read_does_not_hang() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..6 {
            let p = dir.path().join(format!("file_{i}.rs"));
            std::fs::write(&p, format!("fn f{i}() {{ let _ = {i}; }}\n")).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
        let root = dir.path().to_string_lossy().to_string();

        let cache: Arc<RwLock<SessionCache>> = Arc::new(RwLock::new(SessionCache::new()));
        let session = {
            let mut s = SessionState::new();
            s.project_root = Some(root.clone());
            Arc::new(RwLock::new(s))
        };

        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let session = session.clone();
            let paths = paths.clone();
            let root = root.clone();
            handles.push(tokio::spawn(async move {
                let ctx = ctx_with(cache, session, &root);
                let args = json!({ "paths": paths, "mode": "full" })
                    .as_object()
                    .unwrap()
                    .clone();
                tokio::task::block_in_place(|| CtxMultiReadTool.handle(&args, &ctx))
            }));
        }

        for h in handles {
            let joined = tokio::time::timeout(Duration::from_secs(20), h)
                .await
                .expect("ctx_multi_read hung (>20s) — nested block_in_place regression?")
                .expect("spawned task panicked");
            let out = joined.expect("ctx_multi_read returned an error");
            assert!(
                out.text.contains("Read 6 files"),
                "unexpected output: {}",
                out.text
            );
        }
    }

    /// #421: `ctx_multi_read` used to force `auto`→`full`, so omitting `mode`
    /// over-expanded every file regardless of the active profile. With no `mode`
    /// arg the handler must fall back to the profile's effective read mode
    /// (`auto` by default) and pass it through to `ctx_read` — never silently
    /// rewrite it to `full`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn omitting_mode_uses_profile_default_not_forced_full() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lib.rs");
        std::fs::write(&p, "fn a() {}\nfn b() {}\n").unwrap();
        let root = dir.path().to_string_lossy().to_string();

        let cache: Arc<RwLock<SessionCache>> = Arc::new(RwLock::new(SessionCache::new()));
        let session = {
            let mut s = SessionState::new();
            s.project_root = Some(root.clone());
            Arc::new(RwLock::new(s))
        };
        let ctx = ctx_with(cache, session, &root);
        let args = json!({ "paths": [p.to_string_lossy()] })
            .as_object()
            .unwrap()
            .clone();

        let out = tokio::task::block_in_place(|| CtxMultiReadTool.handle(&args, &ctx))
            .expect("ctx_multi_read returned an error");

        let expected = crate::core::profiles::active_profile()
            .read
            .default_mode_effective()
            .to_string();
        assert_eq!(
            out.mode,
            Some(expected),
            "omitting mode must use the profile default, not a forced override (#421)"
        );
    }
}
