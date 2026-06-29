//! `ctx_patch` — hash-anchored editing (epic #1008).
//!
//! "Edit by reference, not by reproduction": the model edits lines by their
//! `(line, hash)` anchor (from `ctx_read(mode="anchored")`) instead of quoting
//! the old text byte-for-byte. Each anchor is verified against the *current*
//! file; on drift the edit is rejected with fresh anchors. Multiple edits in
//! one call are **batch-atomic** — all validated against the same preimage and
//! applied all-or-nothing, bottom-up.
//!
//! Reuses the exact `ctx_edit` I/O boundary (`crate::tools::edit_io`):
//! TOCTOU preimage guard, permission-preserving atomic write, read-only-roots
//! deny, symlink rejection. `ctx_edit` (str_replace) stays as the fallback.

mod anchors;
mod apply;
mod output;
mod symbol;
#[cfg(test)]
mod tests;

pub use anchors::AnchorOp;
pub(crate) use symbol::{build_refactor_args, is_replace_symbol};

use std::path::{Path, PathBuf};

use crate::core::cache::SessionCache;
use crate::core::tokens::count_tokens;
use crate::tools::ctx_edit::{CacheEffect, apply_cache_effect, build_diff_evidence};
use crate::tools::edit_io::{
    default_backup_path, ensure_preimage_still_matches, read_preimage,
    write_atomic_bytes_with_permissions,
};

/// Parameters for an anchored patch: the target file and one or more anchored
/// edit ops, plus optional guards/evidence (mirrors `EditParams` where it makes
/// sense so the registered wrapper stays uniform).
pub struct PatchParams {
    pub path: String,
    pub ops: Vec<AnchorOp>,
    /// Optional whole-file preimage guard (BLAKE3 hex, as printed by ctx_edit's
    /// `postimage:` line). When set, the edit fails if the file's hash differs.
    pub expected_md5: Option<String>,
    pub backup: bool,
    pub backup_path: Option<String>,
    pub evidence: bool,
    pub diff_max_lines: usize,
    pub allow_lossy_utf8: bool,
    /// Post-edit tree-sitter gate (#1008): reject a write that turns a cleanly
    /// parsing file into a broken one. Default `true`; set `false` to override
    /// (e.g. intentionally writing an incomplete snippet).
    pub validate_syntax: bool,
}

/// Parse the raw tool arguments into [`AnchorOp`]s (single op or `ops[]`).
pub fn parse_ops(
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<AnchorOp>, String> {
    anchors::parse_ops(args)
}

/// Apply an anchored patch and the resulting cache effect in one shot (tests and
/// in-process callers that hold the cache exclusively).
pub fn handle(cache: &mut SessionCache, params: &PatchParams) -> String {
    let last_mode = cache
        .get(&params.path)
        .map(|e| e.last_mode.clone())
        .unwrap_or_default();
    let (text, effect) = run_io(params, &last_mode);
    record_outcome(params, &last_mode, &text, &effect);
    apply_cache_effect(cache, &params.path, effect);
    text
}

/// Quality loop (#494/#1008): a clean anchored edit is a success signal for the
/// read mode that produced the anchors; a stale-anchor `CONFLICT` is a failure
/// signal (the view the model edited against had drifted) that arms a one-shot
/// escalation of the next auto read to `anchored` — fresh line anchors to retry
/// by reference. Structural errors say nothing about the read mode and are
/// skipped.
pub fn record_outcome(params: &PatchParams, last_mode: &str, text: &str, effect: &CacheEffect) {
    let success = matches!(effect, CacheEffect::Invalidate);
    let conflict = matches!(effect, CacheEffect::None) && text.starts_with("CONFLICT:");
    if success || conflict {
        crate::core::edit_quality::record_anchored_edit_outcome(&params.path, last_mode, success);
    }
}

/// Perform the anchored patch on disk **without** touching the cache; returns
/// the [`CacheEffect`] for the caller to apply. `last_mode` is currently only
/// used by [`record_outcome`]; pass `""` when unknown.
pub fn run_io(params: &PatchParams, _last_mode: &str) -> (String, CacheEffect) {
    let file_path = &params.path;
    let path = Path::new(file_path);
    let cap = crate::core::limits::max_read_bytes();

    let pre = match read_preimage(path, cap, params.allow_lossy_utf8) {
        Ok(p) => p,
        Err(e) => {
            if !path.exists() {
                let hint = crate::tools::edit_recovery::moved_or_deleted_hint(path);
                return (format!("{e}{hint}"), CacheEffect::None);
            }
            return (e, CacheEffect::None);
        }
    };

    if let Some(expected) = params.expected_md5.as_deref()
        && expected != pre.fp.md5
    {
        return (
            format!(
                "ERROR: preimage mismatch for {file_path}: expected_md5={expected}, actual_md5={}",
                pre.fp.md5
            ),
            CacheEffect::None,
        );
    }

    if params.ops.is_empty() {
        return (
            "ERROR: no edits provided (pass an op or ops:[…])".to_string(),
            CacheEffect::None,
        );
    }

    let (lines, sep, trailing) = apply::split_lines(&pre.text);

    let edits = match apply::resolve_ops(&lines, &params.ops) {
        Ok(e) => e,
        Err(apply::ResolveError::Conflict(misses)) => {
            return (
                output::render_conflict(file_path, &lines, &misses),
                CacheEffect::None,
            );
        }
        Err(apply::ResolveError::Invalid(msg)) => {
            return (format!("ERROR: {msg}"), CacheEffect::None);
        }
    };

    let n_edits = edits.len();
    let lines_before = lines.len();
    let new_lines = apply::apply_edits(lines.clone(), edits);
    let new_content = apply::join_lines(&new_lines, sep, trailing);

    if new_content == pre.text {
        return (
            "ERROR: edits produced no change to the file".to_string(),
            CacheEffect::None,
        );
    }

    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    // Post-edit syntax gate (#1008): block a clean → broken regression before any
    // write. Pure (no I/O), so it runs before the TOCTOU re-read.
    if params.validate_syntax
        && let Some(reason) = crate::core::syntax_validate::gate_edit(ext, &pre.text, &new_content)
    {
        return (reason, CacheEffect::None);
    }

    // Code-health gate: warn on (or block) cognitive-complexity drift before write.
    let health_notice = match crate::core::code_health::gate::evaluate(&pre.text, &new_content, ext)
    {
        crate::core::code_health::gate::GateOutcome::Block(reason) => {
            return (
                format!("ERROR: code-health gate: {reason}"),
                CacheEffect::None,
            );
        }
        crate::core::code_health::gate::GateOutcome::Allow(notice) => notice,
    };

    // TOCTOU guard: confirm the file did not change between read and write.
    if let Err(e) = ensure_preimage_still_matches(path, &pre.fp, cap) {
        return (e, CacheEffect::None);
    }

    let backup_path = match make_backup(params, path, &pre.bytes, &pre.permissions) {
        Ok(bp) => bp,
        Err(e) => return (e, CacheEffect::None),
    };

    if let Err(e) =
        write_atomic_bytes_with_permissions(path, new_content.as_bytes(), Some(&pre.permissions))
    {
        return (e, CacheEffect::None);
    }

    if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
        bt.record_edit(file_path);
    }

    let mut out = render_success(
        params,
        &pre.text,
        &new_content,
        pre.fp.size,
        pre.fp.mtime_ms,
        &pre.fp.md5,
        lines_before,
        new_lines.len(),
        n_edits,
        backup_path,
    );
    if let Some(notice) = health_notice {
        out.push_str("\n\n");
        out.push_str(&notice);
    }
    (out, CacheEffect::Invalidate)
}

/// Write a pre-edit backup when requested; returns the backup path (if any).
fn make_backup(
    params: &PatchParams,
    path: &Path,
    bytes: &[u8],
    permissions: &std::fs::Permissions,
) -> Result<Option<String>, String> {
    if !params.backup {
        return Ok(None);
    }
    let bp = params
        .backup_path
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| default_backup_path(path))
        .ok_or_else(|| format!("ERROR: cannot compute backup path for {}", path.display()))?;
    write_atomic_bytes_with_permissions(&bp, bytes, Some(permissions))
        .map_err(|e| format!("ERROR: cannot create backup {}: {e}", bp.display()))?;
    Ok(Some(bp.to_string_lossy().to_string()))
}

#[allow(clippy::too_many_arguments)]
fn render_success(
    params: &PatchParams,
    old_content: &str,
    new_content: &str,
    pre_size: u64,
    pre_mtime_ms: u64,
    pre_md5: &str,
    lines_before: usize,
    lines_after: usize,
    n_edits: usize,
    backup_path: Option<String>,
) -> String {
    let short = output::short_name(&params.path);
    let line_delta = lines_after as i64 - lines_before as i64;
    let delta_str = if line_delta >= 0 {
        format!("+{line_delta}")
    } else {
        format!("{line_delta}")
    };
    let old_tokens = count_tokens(old_content);
    let new_tokens = count_tokens(new_content);

    let post_mtime_ms = std::fs::metadata(&params.path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map_or(0, crate::tools::edit_io::system_time_to_millis);
    let post_md5 = crate::core::hasher::hash_hex(new_content.as_bytes());

    let edit_word = if n_edits == 1 { "edit" } else { "edits" };
    let mut out = format!(
        "✓ {short}: {n_edits} anchored {edit_word}, {delta_str} lines ({old_tokens}→{new_tokens} tok)\n\
preimage: bytes={pre_size}, mtime_ms={pre_mtime_ms}, md5={pre_md5}\n\
postimage: bytes={}, mtime_ms={post_mtime_ms}, md5={post_md5}",
        new_content.len()
    );
    if let Some(bp) = backup_path {
        out.push_str(&format!("\nbackup: {bp}"));
    }
    if params.evidence {
        let diff = build_diff_evidence(old_content, new_content, &short, params.diff_max_lines);
        out.push_str("\n\nevidence (diff, redacted, bounded):\n```diff\n");
        out.push_str(&diff);
        out.push_str("\n```");
    }
    out
}
