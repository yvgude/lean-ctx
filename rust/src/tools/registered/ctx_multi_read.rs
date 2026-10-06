// noqa: SIZE_OK — single-responsibility tool handler, 255 pure LOC (5 over).
// Inline tests (~80 lines) are conventional in Rust. Self-contained MCP tool wrapper.
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
            "DEPRECATED → use ctx_read with paths=['a.rs','b.rs']. Folded into ctx_read\n\
             (#509); hidden from tools/list, still callable for one release.",
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
                        "description": "auto|full|raw|signatures|map (same as ctx_read)"
                    },
                    "fresh": {
                        "type": "boolean",
                        "description": "Bypass cache, full re-read"
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
        batch_read(args, ctx)
    }
}

/// Batch-read multiple files in one call. The single implementation shared by
/// the (deprecated) `ctx_multi_read` tool and by `ctx_read` when it is called
/// with a `paths` array (#509) — no duplicated batch logic across the two.
///
/// Panic guard (mirrors ctx_read): a panic in tree-sitter / compression must
/// never unwind through the dispatch `block_in_place` and kill the MCP server.
pub(crate) fn batch_read(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<ToolOutput, ErrorData> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle_inner(args, ctx))) {
        Ok(result) => result,
        Err(_) => Err(ErrorData::internal_error(
            "ctx_multi_read panicked while processing the batch. This is a bug — please report it.",
            None,
        )),
    }
}

fn handle_inner(args: &Map<String, Value>, ctx: &ToolContext) -> Result<ToolOutput, ErrorData> {
    let raw_paths = get_str_array(args, "paths")
        .ok_or_else(|| ErrorData::invalid_params("paths array is required", None))?;

    let session_lock = ctx
        .session
        .as_ref()
        .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
    let cap = crate::core::limits::max_read_bytes() as u64;

    // Resolve and filter paths under one short session read lock.
    // `bounded_lock` uses `Handle::block_on` directly — NOT a nested
    // `block_in_place` — because the dispatch layer already wraps this handler in
    // `block_in_place`. The previous nested `block_in_place` calls could exhaust the
    // 32-thread blocking pool under concurrent reads and freeze the server (#271).
    let (paths, task) = {
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
        // #1590: an inferred task never steers a read or its supplement.
        let task = session
            .task
            .as_ref()
            .filter(|t| super::ctx_read::task_intent_steers_read(t.intent.as_deref()))
            .map(|t| t.description.clone());
        (paths, task)
    };

    if paths.is_empty() {
        return Err(ErrorData::invalid_params(
            "all paths are binary or exceed the size limit",
            None,
        ));
    }

    // Share single-read precedence: explicit > config > learned > default.
    // `auto` delegates the learned per-file decision to ctx_read below.
    // #1993: `raw=true` means exact bytes here too, as it does for a single read.
    let explicit_mode = super::ctx_read::resolve_explicit_mode(
        get_bool(args, "raw").unwrap_or(false),
        get_str(args, "mode"),
    );
    let configured_mode = explicit_mode
        .is_none()
        .then(crate::core::auto_mode_resolver::configured_default_mode)
        .flatten();
    let mode = crate::core::auto_mode_resolver::resolve_mode_precedence(
        explicit_mode,
        configured_mode,
        Some("auto".to_string()),
        "full",
    );
    let max_bytes = crate::tools::ctx_multi_read::max_multi_read_bytes();
    let mut single_args = args.clone();
    single_args.remove("paths");
    single_args.insert("mode".into(), Value::String(mode.clone()));
    let mut sections = Vec::with_capacity(paths.len());
    let mut deliveries = Vec::with_capacity(paths.len());
    let mut bytes = 0usize;
    let mut total_original = 0usize;
    // Each file uses the same admitted source, policy refresh, rendering and
    // optional private hints as a single read. Never hold the batch cache lock
    // across that path: it owns its short cache locks and bounded worker itself.
    for path in &paths {
        single_args.insert("path".into(), Value::String(path.clone()));
        let prepared =
            super::ctx_read::CtxReadTool.handle_inner_prepared(&single_args, ctx, path, false)?;
        let output = &prepared.output;
        if !sections.is_empty() && bytes.saturating_add(output.text.len()) > max_bytes {
            break;
        }
        bytes = bytes.saturating_add(output.text.len());
        total_original = total_original.saturating_add(output.original_tokens);
        sections.push(output.text.clone());
        deliveries.push(prepared);
    }
    let count = sections.len();
    let mut text = sections.join("\n---\n");
    if count < paths.len() {
        text.push_str(&format!(
            "\n---\nRead {count}/{} files\nOutput capped at {max_bytes} bytes (LCTX_MAX_MULTI_READ_BYTES). {} file(s) skipped. Use individual ctx_read calls for remaining files.",
            paths.len(), paths.len() - count,
        ));
    } else {
        text.push_str(&format!("\n---\nRead {count} files"));
    }
    // One kernel supplement per batch, never per file; raw stays verbatim.
    if mode != "raw" {
        if let Some(trailer) = crate::tools::ctx_read::kernel_trailer(task.as_deref()) {
            text.push('\n');
            text.push_str(&trailer);
        }
    }
    // Count the actual combined application output, including per-file hints
    // and framing. Inner source metrics are not a second savings claim.
    let tokens = crate::core::tokens::count_tokens(&text);
    let agent_id = ctx
        .agent_id
        .as_ref()
        .and_then(|lock| lock.try_read().ok().and_then(|id| id.clone()));
    if let Some(id) = agent_id.as_deref()
        && let crate::core::agent_budget::BudgetCheckResult::Exceeded { limit, consumed } =
            crate::core::agent_budget::check_budget(id, tokens)
    {
        return Err(ErrorData::invalid_params(
            format!(
                "Agent budget exceeded: {consumed}/{limit} tokens consumed; batch requires {tokens} tokens."
            ),
            None,
        ));
    }
    // Only the complete, admitted application response counts as delivered.
    // A capped item or an error later in the batch drops its pending effects.
    for prepared in deliveries {
        prepared.commit(ctx, false);
    }
    if let Some(id) = agent_id.as_deref() {
        crate::core::agent_budget::record_consumption(id, tokens);
    }

    Ok(ToolOutput {
        text,
        original_tokens: total_original,
        saved_tokens: total_original.saturating_sub(tokens),
        mode: Some(mode),
        path: None,
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    })
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
            client_role: None,
            shell_access: None,
            pipeline_stats: None,
            call_count: None,
            autonomy: None,
            pressure_snapshot: None,
            path_errors: std::collections::HashMap::new(),
            bm25_cache: None,
            progress_sender: None,
            cancel: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_prepared_read_does_not_publish_a_compressed_variant_or_session_read() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending.rs");
        std::fs::write(&path, "fn pending() { let x = 42; }\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let mut state = SessionState::new();
        state.project_root = Some(root.to_owned());
        let cache = Arc::new(RwLock::new(SessionCache::new()));
        let session = Arc::new(RwLock::new(state));
        let ctx = ctx_with(cache.clone(), session.clone(), root);
        let args = json!({"mode":"map"}).as_object().unwrap().clone();
        let prepared = tokio::task::block_in_place(|| {
            super::super::ctx_read::CtxReadTool.handle_inner_prepared(
                &args,
                &ctx,
                path.to_str().unwrap(),
                false,
            )
        })
        .unwrap();
        assert!(prepared.output.text.contains("pending"));
        drop(prepared);
        let key =
            crate::tools::ctx_read::compressed_cache_key("map", CrpMode::Off, None, None, &[]);
        assert!(
            cache
                .read()
                .await
                .get_compressed(path.to_str().unwrap(), &key)
                .is_none()
        );
        assert_eq!(session.read().await.stats.files_read, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batch_budget_checks_combined_output_before_delivery() {
        use std::fmt::Write as _;

        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for name in ["one.rs", "two.rs"] {
            let path = dir.path().join(name);
            let mut content = String::new();
            for i in 0..100 {
                writeln!(content, "let variable_{i} = {i};").unwrap();
            }
            std::fs::write(&path, content).unwrap();
            paths.push(path);
        }
        let root = dir.path().to_str().unwrap();
        let mut state = SessionState::new();
        state.project_root = Some(root.to_owned());
        let cache = Arc::new(RwLock::new(SessionCache::new()));
        let session = Arc::new(RwLock::new(state));
        let mut ctx = ctx_with(cache.clone(), session.clone(), root);
        let id = format!("batch-budget-{root}");
        ctx.agent_id = Some(Arc::new(RwLock::new(Some(id.clone()))));
        crate::core::agent_budget::set_limit(&id, 1200);
        let args = json!({"paths":paths, "mode":"full"})
            .as_object()
            .unwrap()
            .clone();
        let result = tokio::task::block_in_place(|| CtxMultiReadTool.handle(&args, &ctx));
        let status = crate::core::agent_budget::get_status(&id);
        crate::core::agent_budget::remove(&id);
        assert!(
            result
                .err()
                .expect("aggregate budget must refuse the batch")
                .message
                .contains("batch requires")
        );
        assert_eq!(status.tokens_consumed, 0);
        assert_eq!(session.read().await.stats.files_read, 0);
        let guard = cache.read().await;
        for path in &paths {
            assert!(
                !guard
                    .get(path.to_str().unwrap())
                    .unwrap()
                    .full_content_delivered
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batch_shares_raw_alias_and_line_window_semantics() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.py");
        std::fs::write(
            &path,
            "def sample():\n    return 'batch_exact_value'\n# last_window_marker\n",
        )
        .unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let mut state = SessionState::new();
        state.project_root = Some(root.clone());
        let ctx = ctx_with(
            Arc::new(RwLock::new(SessionCache::new())),
            Arc::new(RwLock::new(state)),
            &root,
        );
        let raw = json!({"paths":[path], "mode":"map", "raw":true})
            .as_object()
            .unwrap()
            .clone();
        let output = tokio::task::block_in_place(|| CtxMultiReadTool.handle(&raw, &ctx)).unwrap();
        assert!(output.text.contains("return 'batch_exact_value'"));
        assert_eq!(output.mode.as_deref(), Some("raw"));
        let window = json!({"paths":[path], "mode":"full", "start_line":2, "limit":1})
            .as_object()
            .unwrap()
            .clone();
        let output =
            tokio::task::block_in_place(|| CtxMultiReadTool.handle(&window, &ctx)).unwrap();
        assert!(output.text.contains("batch_exact_value"));
        assert!(!output.text.contains("last_window_marker"));
        assert_eq!(
            output.saved_tokens,
            output
                .original_tokens
                .saturating_sub(crate::core::tokens::count_tokens(&output.text))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn changed_policy_rechecks_batch_source_before_reusing_compressed_view() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restricted.py");
        std::fs::write(
            &path,
            "def restricted():\n    # CONFIDENTIAL\n    return 'batch_sensitive_value'\n",
        )
        .unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let mut state = SessionState::new();
        state.project_root = Some(root.clone());
        let ctx = ctx_with(
            Arc::new(RwLock::new(SessionCache::new())),
            Arc::new(RwLock::new(state)),
            &root,
        );
        let args = json!({"paths":[path], "mode":"map"})
            .as_object()
            .unwrap()
            .clone();
        {
            let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let output =
                tokio::task::block_in_place(|| CtxMultiReadTool.handle(&args, &ctx)).unwrap();
            assert!(output.text.contains("restricted"));
        }
        let policy = "name='batch-block'\nversion='1.0.0'\ndescription='test'\n[filters]\nclassification='block'\n";
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
            crate::core::policy::resolve(&crate::core::policy::parse(policy).unwrap()).unwrap(),
        ));
        // Dispatch normally binds the request project before invoking a tool;
        // reproduce that scope so detached single-read workers inherit it.
        let result = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(dir.path().to_path_buf())),
            || tokio::task::block_in_place(|| CtxMultiReadTool.handle(&args, &ctx)),
        );
        let withheld = match result {
            Ok(output) => output.text,
            Err(error) => error.message.into_owned(),
        };
        assert!(withheld.contains("withheld by policy"), "{withheld}");
        assert!(!withheld.contains("batch_sensitive_value"));
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

    /// #509: `ctx_read` with a `paths` array must route to the shared
    /// `batch_read` (folding `ctx_multi_read` into `ctx_read`), producing the
    /// same multi-file batch output as calling `ctx_multi_read` directly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ctx_read_with_paths_delegates_to_batch_read() {
        use crate::tools::registered::ctx_read::CtxReadTool;

        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..3 {
            let p = dir.path().join(format!("f{i}.rs"));
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
        let ctx = ctx_with(cache, session, &root);
        let args = json!({ "paths": paths, "mode": "full" })
            .as_object()
            .unwrap()
            .clone();

        let out = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx))
            .expect("ctx_read(paths) returned an error");
        assert!(
            out.text.contains("Read 3 files"),
            "ctx_read(paths) must batch-read like ctx_multi_read, got: {}",
            out.text
        );
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
