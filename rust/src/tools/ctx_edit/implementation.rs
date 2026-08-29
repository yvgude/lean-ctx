use std::path::{Path, PathBuf};

use crate::core::cache::SessionCache;
use crate::core::tokens::count_tokens;
// Shared TOCTOU-safe read→verify→atomic-write primitives (epic #1008): the same
// audited boundary `ctx_patch` (anchored editing) builds on, so a fix protects
// both tools. `verify_expected_preimage` stays here — it is `ctx_edit`-specific
// (keyed on `EditParams`).
use crate::tools::edit_io::{
    FileFingerprint, FilePreimage, default_backup_path, ensure_preimage_still_matches,
    read_preimage, system_time_to_millis, write_atomic_bytes_with_permissions,
};

/// Parameters for a file edit operation: path, old/new strings, and flags.
pub struct EditParams {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
    pub create: bool,
    /// Optional preimage guards. If provided, ctx_edit fails if the current file preimage differs.
    ///
    /// The content guard is BLAKE3, named for what it is (#1592); the wire
    /// parameter `expected_md5` stays accepted as an alias for existing callers.
    pub expected_blake3: Option<String>,
    pub expected_size: Option<u64>,
    pub expected_mtime_ms: Option<u64>,
    /// Optional backup before writing.
    pub backup: bool,
    pub backup_path: Option<String>,
    /// Emit bounded diff evidence (redacted) by default.
    pub evidence: bool,
    pub diff_max_lines: usize,
    /// Reject invalid UTF-8 by default; allow lossy reads only when explicitly enabled.
    pub allow_lossy_utf8: bool,
}

struct ReplaceArgs<'a> {
    content: &'a str,
    old_str: &'a str,
    new_str: &'a str,
    occurrences: usize,
    replace_all: bool,
    old_tokens: usize,
    new_tokens: usize,
}

fn verify_expected_preimage(pre: &FilePreimage, params: &EditParams) -> Result<(), String> {
    if let Some(expected) = params.expected_size
        && expected != pre.fp.size
    {
        return Err(format!(
            "ERROR: preimage mismatch for {}: expected_size={}, actual_size={}",
            params.path, expected, pre.fp.size
        ));
    }
    if let Some(expected) = params.expected_mtime_ms
        && expected != pre.fp.mtime_ms
    {
        return Err(format!(
            "ERROR: preimage mismatch for {}: expected_mtime_ms={}, actual_mtime_ms={}",
            params.path, expected, pre.fp.mtime_ms
        ));
    }
    if let Some(expected) = params.expected_blake3.as_deref()
        && expected != pre.fp.blake3
    {
        return Err(format!(
            "ERROR: preimage mismatch for {}: expected_blake3={}, actual_blake3={}",
            params.path, expected, pre.fp.blake3
        ));
    }
    Ok(())
}

/// Bounded, secret-redacted unified diff for edit evidence. `pub(crate)` so the
/// anchored editor (`ctx_patch`) reuses the identical evidence format (#1008).
pub(crate) fn build_diff_evidence(old: &str, new: &str, label: &str, max_lines: usize) -> String {
    let diff = similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(label, label)
        .to_string();
    // Single source of truth for secret masking — `core::redaction` carries the
    // GH #430 non-secret-literal guard (type annotations, `undefined`, …) and
    // does not leak the value of generic long secrets like this duplicate once
    // did. Keeping a second regex set here only invites drift.
    let diff = crate::core::redaction::redact_text(&diff);

    let mut out = String::new();
    for (i, line) in diff.lines().enumerate() {
        if i >= max_lines {
            out.push_str(&format!("\n... diff truncated (max_lines={max_lines})"));
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// A cache mutation that an edit needs *after* its disk I/O completes.
///
/// Decoupling the cache mutation from the I/O lets the MCP layer perform the
/// (slow) file read/replace/write while holding only a cheap per-file lock, then
/// touch the shared cache for a sub-millisecond instant — instead of holding the
/// global cache write-lock across all disk I/O (the root cause of issue #320).
pub enum CacheEffect {
    /// No cache change required (e.g. the edit failed before writing).
    None,
    /// The file on disk changed; drop the stale cache entry.
    Invalidate,
    /// Auto-escalation re-read full content that should be stored and marked
    /// as fully delivered.
    StoreFull(String),
}

/// Performs a string replacement edit on a file with CRLF/LF and whitespace
/// tolerance. Thin wrapper that runs the I/O and applies the resulting cache
/// effect to `cache` in one shot (used by tests and any in-process caller that
/// already holds the cache exclusively).
pub fn handle(cache: &mut SessionCache, params: &EditParams) -> String {
    let last_mode = cache
        .get(&params.path)
        .map(|e| e.last_mode.clone())
        .unwrap_or_default();
    let (text, effect) = run_io(params, &last_mode);
    record_outcome(params, &last_mode, &text, &effect);
    apply_cache_effect(cache, &params.path, effect);
    text
}

/// Quality loop (#494): classify the edit result and feed it into
/// `crate::core::edit_quality`. Only two outcomes carry a compression
/// signal: a clean replacement (success) and an `old_string` miss
/// (failure — the body the agent quoted wasn't what's on disk). Parameter
/// mistakes (empty/identical strings, preimage mismatch, missing file) and
/// already-applied edits say nothing about the read mode and are skipped.
pub fn record_outcome(params: &EditParams, last_mode: &str, text: &str, effect: &CacheEffect) {
    if params.create {
        return;
    }
    let success = matches!(effect, CacheEffect::Invalidate);
    let not_found_failure = matches!(effect, CacheEffect::StoreFull(_))
        || (matches!(effect, CacheEffect::None)
            && text.starts_with("ERROR: old_string not found")
            && !text.contains("already"));
    if success {
        if let Some(project_root) = crate::server::derive_project_root_from_cwd() {
            crate::core::solution_auto_capture::capture_edit_decisions(
                &project_root,
                &params.path,
                &params.old_string,
                &params.new_string,
            );
        }
    }

    if success || not_found_failure {
        crate::core::edit_quality::record_edit_outcome(&params.path, last_mode, success);
    }
    // Edit-efficiency channel (#1008): the str_replace baseline — output tokens
    // actually paid reproducing `old_string`, and blind-retry round-trips.
    // Separate from the read-gain ledger, never printed in tool output (#498).
    if success {
        crate::core::edit_metering::record_str_replace_success(
            count_tokens(&params.old_string) as u64
        );
    } else if not_found_failure {
        crate::core::edit_metering::record_str_replace_miss();
    }
}

/// Applies a deferred [`CacheEffect`] to the session cache.
pub fn apply_cache_effect(cache: &mut SessionCache, path: &str, effect: CacheEffect) {
    match effect {
        CacheEffect::None => {}
        CacheEffect::Invalidate => {
            cache.invalidate(path);
        }
        CacheEffect::StoreFull(content) => {
            cache.store(path, &content);
            cache.mark_full_delivered(path);
        }
    }
}

/// Performs the full edit on disk **without** touching the session cache, and
/// reports back the [`CacheEffect`] the caller should apply afterwards.
///
/// `last_mode` is the cache's recorded read mode for the path (used only to
/// decide whether to auto-escalate on a not-found match); pass `""` when unknown.
pub fn run_io(params: &EditParams, last_mode: &str) -> (String, CacheEffect) {
    let file_path = &params.path;

    if params.create {
        return handle_create(file_path, &params.new_string, params);
    }

    let cap = crate::core::limits::max_read_bytes();
    let path = Path::new(file_path);
    let pre = match read_preimage(path, cap, params.allow_lossy_utf8) {
        Ok(p) => p,
        Err(e) => {
            // File missing? Tell the agent whether it moved or the path is
            // wrong, instead of a bare "cannot open" (#331 point 3).
            if !path.exists() {
                let hint = crate::tools::edit_recovery::moved_or_deleted_hint(path);
                return (format!("{e}{hint}"), CacheEffect::None);
            }
            return (e, CacheEffect::None);
        }
    };
    if let Err(e) = verify_expected_preimage(&pre, params) {
        return (e, CacheEffect::None);
    }
    let content = &pre.text;

    if params.old_string.is_empty() {
        return (
            "ERROR: old_string must not be empty (use create=true to create a new file)".into(),
            CacheEffect::None,
        );
    }

    if params.old_string == params.new_string {
        return (
            "ERROR: old_string and new_string are identical — nothing to change.".into(),
            CacheEffect::None,
        );
    }

    let uses_crlf = pre.uses_crlf;
    let old_str = &params.old_string;
    let new_str = &params.new_string;

    let occurrences = content.matches(old_str).count();

    if occurrences > 0 {
        let args = ReplaceArgs {
            content,
            old_str,
            new_str,
            occurrences,
            replace_all: params.replace_all,
            old_tokens: count_tokens(&params.old_string),
            new_tokens: count_tokens(&params.new_string),
        };
        return do_replace(path, &pre, params, cap, &args);
    }

    if uses_crlf && !old_str.contains('\r') {
        let old_crlf = old_str.replace('\n', "\r\n");
        let occ = content.matches(&old_crlf).count();
        if occ > 0 {
            let new_crlf = new_str.replace('\n', "\r\n");
            let args = ReplaceArgs {
                content,
                old_str: &old_crlf,
                new_str: &new_crlf,
                occurrences: occ,
                replace_all: params.replace_all,
                old_tokens: count_tokens(&params.old_string),
                new_tokens: count_tokens(&params.new_string),
            };
            return do_replace(path, &pre, params, cap, &args);
        }
    } else if !uses_crlf && old_str.contains("\r\n") {
        let old_lf = old_str.replace("\r\n", "\n");
        let occ = content.matches(&old_lf).count();
        if occ > 0 {
            let new_lf = new_str.replace("\r\n", "\n");
            let args = ReplaceArgs {
                content,
                old_str: &old_lf,
                new_str: &new_lf,
                occurrences: occ,
                replace_all: params.replace_all,
                old_tokens: count_tokens(&params.old_string),
                new_tokens: count_tokens(&params.new_string),
            };
            return do_replace(path, &pre, params, cap, &args);
        }
    }

    let normalized_content = trim_trailing_per_line(content);
    let normalized_old = trim_trailing_per_line(old_str);
    if !normalized_old.is_empty() && normalized_content.contains(&normalized_old) {
        let line_sep = if uses_crlf { "\r\n" } else { "\n" };
        let adapted_new = adapt_new_string_to_line_sep(new_str, line_sep);
        let adapted_old = find_original_span(content, &normalized_old);
        if let Some(original_match) = adapted_old {
            let occ = content.matches(&original_match).count();
            let args = ReplaceArgs {
                content,
                old_str: &original_match,
                new_str: &adapted_new,
                occurrences: occ,
                replace_all: params.replace_all,
                old_tokens: count_tokens(&params.old_string),
                new_tokens: count_tokens(&params.new_string),
            };
            return do_replace(path, &pre, params, cap, &args);
        }
    }

    if content.contains(new_str) {
        return (
            format!(
                "ERROR: old_string not found in {file_path}, but new_string already exists in the file. \
                 The edit was likely already applied (by a previous tool call or another agent)."
            ),
            CacheEffect::None,
        );
    }

    let preview = if old_str.len() > 80 {
        format!("{}...", &old_str[..old_str.floor_char_boundary(77)])
    } else {
        old_str.clone()
    };
    let hint = if uses_crlf {
        " (file uses CRLF line endings)"
    } else {
        ""
    };

    let closest_hint = find_closest_line_hint(content, old_str);
    let cross_file = crate::tools::edit_recovery::cross_file_hint(path, old_str);

    let (escalation, effect) = auto_escalate_reread(last_mode, file_path);

    (
        format!(
            "ERROR: old_string not found in {file_path}{hint}. \
             Make sure it matches exactly (including whitespace/indentation).\n\
             Searched for: {preview}{closest_hint}{cross_file}{escalation}"
        ),
        effect,
    )
}

/// Finds the closest matching line in the file content to help the agent
/// understand what went wrong. Returns a hint string or empty if no useful match.
fn find_closest_line_hint(content: &str, old_str: &str) -> String {
    let first_line = old_str.lines().next().unwrap_or("").trim();
    if first_line.len() < 4 {
        return String::new();
    }

    let mut best_line: Option<(usize, &str)> = None;

    for (i, line) in content.lines().enumerate() {
        if line.contains(first_line) {
            best_line = Some((i + 1, line));
            break;
        }
    }

    // Try matching with significant identifiers from old_string's first line
    if best_line.is_none() {
        let keyword = first_line
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .find(|w| w.len() >= 4);

        if let Some(keyword) = keyword {
            for (i, line) in content.lines().enumerate() {
                if line.contains(keyword) {
                    best_line = Some((i + 1, line));
                    break;
                }
            }
        }
    }

    match best_line {
        Some((line_num, line_content)) => {
            let trimmed = line_content.trim();
            let preview = if trimmed.len() > 100 {
                format!("{}...", &trimmed[..trimmed.floor_char_boundary(97)])
            } else {
                trimmed.to_string()
            };
            format!(
                "\nClosest match at line {line_num}: `{preview}`\n\
                 Hint: check indentation/whitespace differences."
            )
        }
        None => String::new(),
    }
}

/// Auto-escalation: when old_string is not found and the file was previously read
/// in a compressed mode, re-read in full and return the content so the agent
/// can immediately retry with the correct old_string. Returns the text to append
/// plus the [`CacheEffect`] the caller should apply (store full content).
fn auto_escalate_reread(last_mode: &str, path: &str) -> (String, CacheEffect) {
    if last_mode.is_empty() || last_mode == "full" {
        return (String::new(), CacheEffect::None);
    }

    let Ok(fresh_content) = std::fs::read_to_string(path) else {
        return (String::new(), CacheEffect::None);
    };

    let line_count = fresh_content.lines().count();
    const MAX_LINES: usize = 300;

    let content_preview = if line_count <= MAX_LINES {
        fresh_content.clone()
    } else {
        let lines: Vec<&str> = fresh_content.lines().collect();
        let head = &lines[..MAX_LINES / 2];
        let tail = &lines[line_count - MAX_LINES / 2..];
        let omitted = line_count - MAX_LINES;
        format!(
            "{}\n[... {omitted} lines omitted ...]\n{}",
            head.join("\n"),
            tail.join("\n")
        )
    };

    (
        format!(
            "\n\n[auto-escalation] Last read used mode=\"{last_mode}\". \
             Full content ({line_count}L) below — retry edit with exact text from here:\n\n{content_preview}"
        ),
        CacheEffect::StoreFull(fresh_content),
    )
}

fn do_replace(
    path: &Path,
    pre: &FilePreimage,
    params: &EditParams,
    cap: usize,
    args: &ReplaceArgs<'_>,
) -> (String, CacheEffect) {
    if args.occurrences > 1 && !args.replace_all {
        return (
            format!(
                "ERROR: old_string found {} times in {}. \
                 Use replace_all=true to replace all, or provide more context to make old_string unique.",
                args.occurrences,
                path.display()
            ),
            CacheEffect::None,
        );
    }

    let new_content = if args.replace_all {
        args.content.replace(args.old_str, args.new_str)
    } else {
        args.content.replacen(args.old_str, args.new_str, 1)
    };

    // Code-health gate: warn on (or block) cognitive-complexity drift before write.
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let health_notice =
        match crate::core::code_health::gate::evaluate(args.content, &new_content, ext) {
            crate::core::code_health::gate::GateOutcome::Block(reason) => {
                return (
                    format!("ERROR: code-health gate: {reason}"),
                    CacheEffect::None,
                );
            }
            crate::core::code_health::gate::GateOutcome::Allow(notice) => notice,
        };

    // #960: a point-in-time check, not a held lock — see
    // ensure_preimage_still_matches' doc for the residual window between
    // this check and the write below.
    if let Err(e) = ensure_preimage_still_matches(path, &pre.fp, cap) {
        return (e, CacheEffect::None);
    }

    let backup_path = if params.backup {
        let bp = params
            .backup_path
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| default_backup_path(path));
        let Some(bp) = bp else {
            return (
                format!("ERROR: cannot compute backup path for {}", path.display()),
                CacheEffect::None,
            );
        };
        if let Err(e) = write_atomic_bytes_with_permissions(&bp, &pre.bytes, Some(&pre.permissions))
        {
            return (
                format!("ERROR: cannot create backup {}: {e}", bp.display()),
                CacheEffect::None,
            );
        }
        Some(bp.to_string_lossy().to_string())
    } else {
        None
    };

    if let Err(e) =
        write_atomic_bytes_with_permissions(path, new_content.as_bytes(), Some(&pre.permissions))
    {
        return (e, CacheEffect::None);
    }

    let replacements = if args.replace_all {
        args.occurrences
    } else {
        1
    };
    let replacements = u64::try_from(replacements).unwrap_or(u64::MAX);
    let lines_added = u64::try_from(args.new_str.lines().count())
        .unwrap_or(u64::MAX)
        .saturating_mul(replacements);
    let lines_removed = u64::try_from(args.old_str.lines().count())
        .unwrap_or(u64::MAX)
        .saturating_mul(replacements);
    crate::core::edit_metering::record_loc_change(Some(&params.path), lines_added, lines_removed);

    if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
        bt.record_edit(&params.path);
    }

    let old_lines = args.content.lines().count();
    let new_lines = new_content.lines().count();
    let line_delta = new_lines as i64 - old_lines as i64;
    let delta_str = if line_delta > 0 {
        format!("+{line_delta}")
    } else {
        format!("{line_delta}")
    };

    let old_tokens = args.old_tokens;
    let new_tokens = args.new_tokens;

    let replaced_str = if args.replace_all && args.occurrences > 1 {
        format!("{} replacements", args.occurrences)
    } else {
        "1 replacement".into()
    };

    let short = path.file_name().map_or_else(
        || path.to_string_lossy().to_string(),
        |f| f.to_string_lossy().to_string(),
    );

    let post_mtime_ms = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map_or(0, system_time_to_millis);
    let post_fp = FileFingerprint {
        size: new_content.len() as u64,
        mtime_ms: post_mtime_ms,
        blake3: crate::core::hasher::hash_hex(new_content.as_bytes()),
    };

    let mut out = format!(
        "✓ {short}: {replaced_str}, {delta_str} lines ({old_tokens}→{new_tokens} tok)\n\
preimage: bytes={}, mtime_ms={}, blake3={}\n\
postimage: bytes={}, mtime_ms={}, blake3={}",
        pre.fp.size, pre.fp.mtime_ms, pre.fp.blake3, post_fp.size, post_fp.mtime_ms, post_fp.blake3
    );
    if let Some(bp) = backup_path {
        out.push_str(&format!("\nbackup: {bp}"));
    }
    if params.evidence {
        let diff = build_diff_evidence(args.content, &new_content, &short, params.diff_max_lines);
        out.push_str("\n\nevidence (diff, redacted, bounded):\n```diff\n");
        out.push_str(&diff);
        out.push_str("\n```");
    }
    if let Some(notice) = health_notice {
        out.push_str("\n\n");
        out.push_str(&notice);
    }
    (out, CacheEffect::Invalidate)
}

fn handle_create(file_path: &str, content: &str, params: &EditParams) -> (String, CacheEffect) {
    let path = Path::new(file_path);
    let cap = crate::core::limits::max_read_bytes();

    // Deny before the standalone create_dir_all below can materialise a
    // directory inside a read-only root (#475). The atomic writer guards the
    // file write too, but this stops an empty-dir side effect first.
    if let Err(e) = crate::core::pathjail::enforce_writable(path) {
        return (format!("ERROR: {e}"), CacheEffect::None);
    }

    let mut preimage: Option<FilePreimage> = None;
    if path.exists() {
        let pre = match read_preimage(path, cap, params.allow_lossy_utf8) {
            Ok(p) => p,
            Err(e) => return (e, CacheEffect::None),
        };
        if let Err(e) = verify_expected_preimage(&pre, params) {
            return (e, CacheEffect::None);
        }
        // #960: a point-in-time check, not a held lock — see
        // ensure_preimage_still_matches' doc for the residual window between
        // this check and the write below.
        if let Err(e) = ensure_preimage_still_matches(path, &pre.fp, cap) {
            return (e, CacheEffect::None);
        }
        preimage = Some(pre);
    }

    if let Some(parent) = path.parent()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return (
            format!("ERROR: cannot create directory {}: {e}", parent.display()),
            CacheEffect::None,
        );
    }

    let backup_path = if params.backup {
        if let Some(pre) = &preimage {
            let bp = params
                .backup_path
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| default_backup_path(path));
            let Some(bp) = bp else {
                return (
                    format!("ERROR: cannot compute backup path for {}", path.display()),
                    CacheEffect::None,
                );
            };
            if let Err(e) =
                write_atomic_bytes_with_permissions(&bp, &pre.bytes, Some(&pre.permissions))
            {
                return (
                    format!("ERROR: cannot create backup {}: {e}", bp.display()),
                    CacheEffect::None,
                );
            }
            Some(bp.to_string_lossy().to_string())
        } else {
            None
        }
    } else {
        None
    };

    let perms = preimage.as_ref().map(|p| &p.permissions);
    if let Err(e) = write_atomic_bytes_with_permissions(path, content.as_bytes(), perms) {
        return (e, CacheEffect::None);
    }

    let lines_added = u64::try_from(content.lines().count()).unwrap_or(u64::MAX);
    let lines_removed = preimage.as_ref().map_or(0, |pre| {
        u64::try_from(pre.text.lines().count()).unwrap_or(u64::MAX)
    });
    crate::core::edit_metering::record_loc_change(Some(file_path), lines_added, lines_removed);

    let lines = content.lines().count();
    let tokens = count_tokens(content);
    let short = path.file_name().map_or_else(
        || path.to_string_lossy().to_string(),
        |f| f.to_string_lossy().to_string(),
    );

    let mut out = format!("✓ created {short}: {lines} lines, {tokens} tok");
    if let Some(bp) = backup_path {
        out.push_str(&format!("\nbackup: {bp}"));
    }
    (out, CacheEffect::Invalidate)
}

fn trim_trailing_per_line(s: &str) -> String {
    s.lines().map(str::trim_end).collect::<Vec<_>>().join("\n")
}

fn adapt_new_string_to_line_sep(s: &str, sep: &str) -> String {
    let normalized = s.replace("\r\n", "\n");
    if sep == "\r\n" {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    }
}

/// Find the original (un-trimmed) span in `content` that matches `normalized_needle`
/// after trailing-whitespace trimming per line.
fn find_original_span(content: &str, normalized_needle: &str) -> Option<String> {
    let needle_lines: Vec<&str> = normalized_needle.lines().collect();
    if needle_lines.is_empty() {
        return None;
    }

    let content_lines: Vec<&str> = content.lines().collect();

    'outer: for start in 0..content_lines.len() {
        if start + needle_lines.len() > content_lines.len() {
            break;
        }
        for (i, nl) in needle_lines.iter().enumerate() {
            if content_lines[start + i].trim_end() != *nl {
                continue 'outer;
            }
        }
        let sep = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        return Some(content_lines[start..start + needle_lines.len()].join(sep));
    }
    None
}

pub fn captured_solution_decision_kinds(old_content: &str, new_content: &str) -> Vec<&'static str> {
    let mut kinds = Vec::new();

    if new_content.contains("use std::") && !old_content.contains("use std::") {
        kinds.push("stdlib");
    }

    if new_content.contains("// lean-ctx:") && !old_content.contains("// lean-ctx:") {
        kinds.push("debt");
    }

    let old_deps = old_content
        .lines()
        .filter(|line| line.contains("[dependencies]") || line.contains("= \""))
        .count();
    let new_deps = new_content
        .lines()
        .filter(|line| line.contains("[dependencies]") || line.contains("= \""))
        .count();
    if new_deps < old_deps {
        kinds.push("native");
    }

    let old_lines = old_content.lines().count();
    let new_lines = new_content.lines().count();
    if old_lines > 5 && new_lines < old_lines / 2 {
        kinds.push("oneline");
    }

    kinds
}

#[cfg(test)]
mod solution_auto_capture_tests {
    use super::captured_solution_decision_kinds;

    #[test]
    fn classifies_decisions_from_completed_edit_content() {
        let old = "use crate::core::Config;\n[dependencies]\nlegacy = \"1\"\nfn verbose() {\n    first();\n    second();\n    third();\n    fourth();\n    fifth();\n    sixth();\n}\n";
        let new = "use crate::core::Config;\nuse std::collections::HashMap;\n[dependencies]\n// lean-ctx: replace legacy dependency\nfn concise() {}\n";

        assert_eq!(
            captured_solution_decision_kinds(old, new),
            ["stdlib", "debt", "native"]
        );
    }
}
