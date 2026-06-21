use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str, require_resolved_path,
};
use crate::tool_defs::tool_def;

pub struct CtxEditTool;

impl McpTool for CtxEditTool {
    fn name(&self) -> &'static str {
        "ctx_edit"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_edit",
            "Search-and-replace edit: old_string must be unique unless replace_all=true\n\
             create=true writes new files from new_string. TOCTOU-guarded with preimage hash verification.\n\
             backup creates .bak before modifying. Supports MD5/size/mtime pre-guards for race-free edits.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path" },
                    "old_string": { "type": "string", "description": "Text to replace (unique unless replace_all)" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "default": false },
                    "create": { "type": "boolean", "description": "Create file from new_string", "default": false }
                },
                "required": ["path", "new_string"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = require_resolved_path(ctx, args, "path")?;

        let old_string = get_str(args, "old_string").unwrap_or_default();
        let new_string = get_str(args, "new_string")
            .ok_or_else(|| ErrorData::invalid_params("new_string is required", None))?;
        let replace_all = get_bool(args, "replace_all").unwrap_or(false);
        let create = get_bool(args, "create").unwrap_or(false);
        let expected_md5 = get_str(args, "expected_md5");
        let expected_size = get_int(args, "expected_size").and_then(|v| u64::try_from(v).ok());
        let expected_mtime_ms =
            get_int(args, "expected_mtime_ms").and_then(|v| u64::try_from(v).ok());
        let backup = get_bool(args, "backup").unwrap_or(false);
        let backup_path = get_str(args, "backup_path")
            .map(|p| ctx.resolved_paths.get("backup_path").cloned().unwrap_or(p));
        let evidence = get_bool(args, "evidence").unwrap_or(true);
        let diff_max_lines = get_int(args, "diff_max_lines")
            .and_then(|v| usize::try_from(v.max(0)).ok())
            .unwrap_or(200);
        let allow_lossy_utf8 = get_bool(args, "allow_lossy_utf8").unwrap_or(false);

        let edit_params = crate::tools::ctx_edit::EditParams {
            path: path.clone(),
            old_string,
            new_string,
            replace_all,
            create,
            expected_md5,
            expected_size,
            expected_mtime_ms,
            backup,
            backup_path,
            evidence,
            diff_max_lines,
            allow_lossy_utf8,
        };

        tokio::task::block_in_place(|| {
            let cache_lock = ctx
                .cache
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
            let rt = tokio::runtime::Handle::current();

            // Serialize edits to the SAME file via a cheap per-file lock. This
            // lets the (slow) disk read/replace/write run WITHOUT holding the
            // global cache write-lock, so concurrent agents editing different
            // files never block each other (issue #320). Correctness for same-file
            // edits is still guaranteed by the TOCTOU preimage guard + atomic
            // rename inside run_io.
            let file_lock = crate::core::path_locks::per_file_lock(&path);
            let _file_guard = {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    if let Ok(guard) = file_lock.try_lock() {
                        break guard;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(ErrorData::internal_error(
                            format!(
                                "per-file edit lock contention for {path} — another edit to the same file is in progress, retry in a moment"
                            ),
                            None,
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            };

            // Brief shared lock: read the recorded read-mode for auto-escalation.
            // On contention we simply skip escalation rather than blocking I/O.
            let last_mode = match rt.block_on(tokio::time::timeout(
                std::time::Duration::from_secs(5),
                cache_lock.read(),
            )) {
                Ok(cache) => cache
                    .get(&path)
                    .map(|e| e.last_mode.clone())
                    .unwrap_or_default(),
                Err(_) => String::new(),
            };

            // Heavy disk I/O — no global cache lock held here.
            let (output, effect) = crate::tools::ctx_edit::run_io(&edit_params, &last_mode);

            // Quality loop (#494): feed success/old_string-miss back into
            // per-(ext × mode) stats and the one-shot read escalation.
            crate::tools::ctx_edit::record_outcome(&edit_params, &last_mode, &output, &effect);

            // Apply the deferred cache mutation under a brief exclusive lock.
            if !matches!(effect, crate::tools::ctx_edit::CacheEffect::None) {
                match rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    cache_lock.write(),
                )) {
                    Ok(mut cache) => {
                        crate::tools::ctx_edit::apply_cache_effect(&mut cache, &path, effect);
                    }
                    Err(_) => {
                        tracing::warn!(
                            "ctx_edit: cache write-lock timeout (5s) applying post-edit cache effect for {path}"
                        );
                    }
                }
            }

            if let Some(session_lock) = ctx.session.as_ref() {
                let guard = rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    session_lock.write(),
                ));
                if let Ok(mut session) = guard {
                    session.mark_modified(&path);
                }
            }

            Ok(ToolOutput {
                text: output,
                original_tokens: 0,
                saved_tokens: 0,
                mode: None,
                path: Some(path),
                changed: false,
                shell_outcome: None,
            })
        })
    }
}
