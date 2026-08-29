use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};
use similar::{ChangeTag, TextDiff};
use std::path::Path;

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
            "Search-and-replace edit with race-condition guards — for simple text replacement in a single file.\n\
             For editing code you've read, prefer ctx_patch (hash-anchored): it never makes you reproduce old text byte-for-byte. Read with ctx_read(mode=\"anchored\") first.\n\
             old_string must be unique unless replace_all=true. create=true writes new files.\n\
             backup creates .bak. MD5/size/mtime pre-guards prevent race conditions.\n\
             ANTIPATTERN: Do NOT loop on failures — switch to ctx_patch (anchored), or verify file content and adjust old_string.\n\
             For LSP-aware refactoring (rename, move, inline), use ctx_refactor.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to edit" },
                    "old_string": { "type": "string", "description": "Text to replace (unique unless replace_all=true)" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)", "default": false },
                    "create": { "type": "boolean", "description": "Create file", "default": false }
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
        // #1592: the guard is a BLAKE3 digest; `expected_md5` was a misnomer and
        // stays accepted so existing callers keep working.
        let expected_blake3 =
            get_str(args, "expected_blake3").or_else(|| get_str(args, "expected_md5"));
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
            expected_blake3,
            expected_size,
            expected_mtime_ms,
            backup,
            backup_path,
            evidence,
            diff_max_lines,
            allow_lossy_utf8,
        };

        {
            let cache_lock = ctx
                .cache
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;

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
            let last_mode =
                match crate::server::bounded_lock::read(cache_lock, "ctx_edit cache read") {
                    Some(cache) => cache
                        .get(&path)
                        .map(|e| e.last_mode.clone())
                        .unwrap_or_default(),
                    None => String::new(),
                };

            // Heavy disk I/O — no global cache lock held here.
            let before = std::fs::read(&path).unwrap_or_default();
            let (output, effect) = crate::tools::ctx_edit::run_io(&edit_params, &last_mode);

            if matches!(effect, crate::tools::ctx_edit::CacheEffect::Invalidate) {
                let after = std::fs::read(&path).unwrap_or_default();
                observe_mcp_edit(ctx, &path, "ctx_edit", &before, &after);
            }

            // Quality loop (#494): feed success/old_string-miss back into
            // per-(ext × mode) stats and the one-shot read escalation.
            crate::tools::ctx_edit::record_outcome(&edit_params, &last_mode, &output, &effect);

            // Apply the deferred cache mutation under a brief exclusive lock.
            if !matches!(effect, crate::tools::ctx_edit::CacheEffect::None) {
                crate::tools::ctx_read::dedup_hook::on_write(&path);
                match crate::server::bounded_lock::write(cache_lock, "ctx_edit cache write") {
                    Some(mut cache) => {
                        crate::tools::ctx_edit::apply_cache_effect(&mut cache, &path, effect);
                    }
                    None => {
                        tracing::warn!(
                            "ctx_edit: cache write-lock timeout applying post-edit effect for {path}"
                        );
                    }
                }
            }

            if let Some(session_lock) = ctx.session.as_ref() {
                if let Some(mut session) =
                    crate::server::bounded_lock::write(session_lock, "ctx_edit session write")
                {
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
                content_blocks: None,
            })
        }
    }
}

pub(crate) fn observe_mcp_edit(
    ctx: &ToolContext,
    path: &str,
    tool: &str,
    before: &[u8],
    after: &[u8],
) {
    let config = crate::core::config::Config::load();
    if !config.provenance.enabled || !config.provenance.capture_mcp_edits || before == after {
        return;
    }

    let root = ctx.project_root.as_str();
    if root.is_empty() {
        return;
    }

    let before_text = String::from_utf8_lossy(before);
    let after_text = String::from_utf8_lossy(after);
    let (lines_added, lines_removed) = TextDiff::from_lines(&before_text, &after_text)
        .iter_all_changes()
        .fold((0_u64, 0_u64), |(added, removed), change| {
            match change.tag() {
                ChangeTag::Insert => (added.saturating_add(1), removed),
                ChangeTag::Delete => (added, removed.saturating_add(1)),
                ChangeTag::Equal => (added, removed),
            }
        });
    let tracked_path = Path::new(path)
        .strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned());
    let session_id = ctx
        .session
        .as_ref()
        .and_then(|session| {
            crate::server::bounded_lock::read(session, "provenance session read")
                .map(|session| session.id.clone())
        })
        .unwrap_or_else(|| "mcp".to_owned());
    let agent_id = ctx
        .agent_id
        .as_ref()
        .and_then(|agent_id| {
            crate::server::bounded_lock::read(agent_id, "provenance agent read")
                .and_then(|agent_id| (*agent_id).clone())
        })
        .unwrap_or_else(|| "mcp".to_owned());

    let _ = crate::core::provenance::ProvenanceTracker::new(root).and_then(|tracker| {
        tracker.observe_edit(
            tracked_path,
            tool,
            sha256_hex(before),
            sha256_hex(after),
            lines_added,
            lines_removed,
            session_id,
            agent_id,
        )
    });
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    let mut hex = String::with_capacity(64);
    for b in &hash {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}
