use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::cache::SessionCache;
use crate::core::tokens::count_tokens;

/// Parameters for a file edit operation: path, old/new strings, and flags.
pub struct EditParams {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
    pub create: bool,
    /// Optional preimage guards. If provided, `ctx_edit` fails if the current file preimage differs.
    pub expected_md5: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileFingerprint {
    size: u64,
    mtime_ms: u64,
    md5: String,
}

#[derive(Clone, Debug)]
struct FilePreimage {
    fp: FileFingerprint,
    permissions: std::fs::Permissions,
    bytes: Vec<u8>,
    text: String,
    uses_crlf: bool,
}

fn system_time_to_millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Rejects symlinks at `path` (TOCTOU protection, same boundary as
/// `core::io_boundary::read_file_nofollow`): a symlink planted inside the jail
/// after the jail check could otherwise read or overwrite files outside it.
fn reject_symlink(path: &Path) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        // Windows: also covers NTFS junctions/reparse points (GL#442).
        if crate::core::pathutil::is_symlink_or_reparse(&meta) {
            return Err(format!(
                "ERROR: {} is a symlink — refusing to edit through it (TOCTOU protection). \
                 Edit the symlink target directly via its real path.",
                path.display()
            ));
        }
    }
    Ok(())
}

fn read_file_bytes_limited(
    path: &Path,
    cap: usize,
) -> Result<(Vec<u8>, std::fs::Metadata), String> {
    reject_symlink(path)?;

    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > cap as u64
    {
        return Err(format!(
            "ERROR: file too large ({} bytes, cap {} via LCTX_MAX_READ_BYTES): {}",
            meta.len(),
            cap,
            path.display()
        ));
    }

    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        // Defense in depth alongside `reject_symlink`: O_NOFOLLOW closes the
        // race between the lstat check and the open.
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts.open(path).map_err(|e| {
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::ELOOP) {
            return format!(
                "ERROR: {} is a symlink — refusing to edit through it (TOCTOU protection).",
                path.display()
            );
        }
        format!("ERROR: cannot open {}: {e}", path.display())
    })?;

    use std::io::Read;
    let mut raw: Vec<u8> = Vec::new();
    let mut limited = (&mut file).take((cap as u64).saturating_add(1));
    limited
        .read_to_end(&mut raw)
        .map_err(|e| format!("ERROR: cannot read {}: {e}", path.display()))?;
    if raw.len() > cap {
        return Err(format!(
            "ERROR: file too large (cap {} via LCTX_MAX_READ_BYTES): {}",
            cap,
            path.display()
        ));
    }

    let meta = file
        .metadata()
        .map_err(|e| format!("ERROR: cannot stat {}: {e}", path.display()))?;
    Ok((raw, meta))
}

fn fingerprint_from_bytes(bytes: &[u8], meta: &std::fs::Metadata) -> FileFingerprint {
    FileFingerprint {
        size: bytes.len() as u64,
        mtime_ms: meta.modified().map_or(0, system_time_to_millis),
        md5: crate::core::hasher::hash_hex(bytes),
    }
}

fn read_preimage(path: &Path, cap: usize, allow_lossy_utf8: bool) -> Result<FilePreimage, String> {
    let (bytes, meta) = read_file_bytes_limited(path, cap)?;
    let permissions = meta.permissions();
    let fp = fingerprint_from_bytes(&bytes, &meta);

    let text = if allow_lossy_utf8 {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::from_utf8(bytes.clone()).map_err(|_| {
            format!(
                "ERROR: file is not valid UTF-8 (binary/encoding). Refusing to edit: {}",
                path.display()
            )
        })?
    };
    let uses_crlf = text.contains("\r\n");

    Ok(FilePreimage {
        fp,
        permissions,
        bytes,
        text,
        uses_crlf,
    })
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
    if let Some(expected) = params.expected_md5.as_deref()
        && expected != pre.fp.md5
    {
        return Err(format!(
            "ERROR: preimage mismatch for {}: expected_md5={}, actual_md5={}",
            params.path, expected, pre.fp.md5
        ));
    }
    Ok(())
}

fn ensure_preimage_still_matches(
    path: &Path,
    expected: &FileFingerprint,
    cap: usize,
) -> Result<(), String> {
    let (bytes, meta) = read_file_bytes_limited(path, cap)?;
    let now = fingerprint_from_bytes(&bytes, &meta);
    if &now != expected {
        return Err(format!(
            "ERROR: file changed since read (TOCTOU guard). Re-read and retry: {}\nexpected: size={}, mtime_ms={}, md5={}\nactual:   size={}, mtime_ms={}, md5={}",
            path.display(),
            expected.size,
            expected.mtime_ms,
            expected.md5,
            now.size,
            now.mtime_ms,
            now.md5
        ));
    }
    Ok(())
}

fn default_backup_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let filename = path.file_name()?.to_string_lossy();
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    Some(parent.join(format!("{filename}.lean-ctx.bak.{pid}.{nanos}")))
}

fn write_atomic_bytes_with_permissions(
    path: &Path,
    bytes: &[u8],
    permissions: Option<&std::fs::Permissions>,
) -> Result<(), String> {
    // Read-only-roots choke point (#475). Every ctx_edit write — edit, create,
    // and the pre-edit backup — funnels here, including the backup whose raw
    // `backup_path` bypasses the dispatch jail, so this single guard makes the
    // whole tool default-deny inside a read-only root before any byte is written
    // or temp/dir created.
    crate::core::pathjail::enforce_writable(path)?;

    // The rename below would *replace* a symlink at `path` (safe), but the edit
    // pipeline read through this path moments ago — a symlink here means the
    // read/write pair straddles two different files. Reject for consistency
    // with the read-side O_NOFOLLOW boundary.
    reject_symlink(path)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    match try_atomic_write(path, bytes, permissions) {
        Ok(()) => Ok(()),
        // Read-only *directory* holding a writable *file* inode — common in
        // sandboxes/containers that bind-mount individual files rw on top of a
        // read-only directory subvolume (e.g. ~/.config/opencode ro,
        // opencode.jsonc rw). The same-dir tempfile + rename dance needs
        // directory write permission, which the ro mount denies, but the
        // existing inode is writable, so overwrite it in place. Not
        // crash-atomic, yet ctx_edit's preimage guard (size/mtime/hash) already
        // gates the write, so a torn write is caught on the next read instead of
        // being silently accepted. (GH #459)
        Err(e) if is_readonly_dir_error(&e) && path.is_file() => {
            in_place_overwrite(path, bytes, permissions).map_err(|fallback_err| {
                format!(
                    "ERROR: atomic write failed ({e}); in-place fallback also failed: {fallback_err} ({})",
                    path.display()
                )
            })
        }
        Err(e) => Err(format!("ERROR: atomic write failed: {e} ({})", path.display())),
    }
}

/// Durable, crash-atomic write: a temp file in the **same directory** followed by
/// `rename` over the target. Requires write permission on the *parent
/// directory*; the caller handles the read-only-directory fallback.
fn try_atomic_write(
    path: &Path,
    bytes: &[u8],
    permissions: Option<&std::fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid path (no parent directory)",
        )
    })?;
    let filename = path
        .file_name()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid path (no filename)",
            )
        })?
        .to_string_lossy();

    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = parent.join(format!(".{filename}.lean-ctx.tmp.{pid}.{nanos}"));

    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        let _ = f.flush();
        let _ = f.sync_all();
    }

    if let Some(perms) = permissions {
        let _ = std::fs::set_permissions(&tmp, perms.clone());
    }

    #[cfg(windows)]
    {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        // Don't leave a half-written temp behind before the caller decides
        // whether to fall back.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// In-place overwrite of an existing file inode (`O_WRONLY|O_TRUNC`). Works when
/// the parent directory is read-only but the file itself is writable. Not
/// crash-atomic — used only as a fallback when the atomic path is impossible.
fn in_place_overwrite(
    path: &Path,
    bytes: &[u8],
    permissions: Option<&std::fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW: a symlink swapped in after reject_symlink must never be
        // followed (mirrors the read-side O_NOFOLLOW boundary).
        opts.custom_flags(libc::O_NOFOLLOW);
    }

    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    let _ = f.flush();
    let _ = f.sync_all();

    if let Some(perms) = permissions {
        let _ = std::fs::set_permissions(path, perms.clone());
    }
    Ok(())
}

/// True for errors that mean "this directory won't accept create/rename" even
/// though the target file may be writable: `EROFS` (read-only fs) plus
/// `EACCES`/`EPERM` (directory write denied).
fn is_readonly_dir_error(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            e.raw_os_error(),
            Some(libc::EROFS | libc::EACCES | libc::EPERM)
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn build_diff_evidence(old: &str, new: &str, label: &str, max_lines: usize) -> String {
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
/// [`crate::core::edit_quality`]. Only two outcomes carry a compression
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
    if success || not_found_failure {
        crate::core::edit_quality::record_edit_outcome(&params.path, last_mode, success);
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
#[must_use]
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

/// Auto-escalation: when `old_string` is not found and the file was previously read
/// in a compressed mode, re-read in full and return the content so the agent
/// can immediately retry with the correct `old_string`. Returns the text to append
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
        md5: crate::core::hasher::hash_hex(new_content.as_bytes()),
    };

    let mut out = format!(
        "✓ {short}: {replaced_str}, {delta_str} lines ({old_tokens}→{new_tokens} tok)\n\
preimage: bytes={}, mtime_ms={}, md5={}\n\
postimage: bytes={}, mtime_ms={}, md5={}",
        pre.fp.size, pre.fp.mtime_ms, pre.fp.md5, post_fp.size, post_fp.mtime_ms, post_fp.md5
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn mk_params(path: &Path, old: &str, new: &str, replace_all: bool, create: bool) -> EditParams {
        EditParams {
            path: path.to_string_lossy().to_string(),
            old_string: old.to_string(),
            new_string: new.to_string(),
            replace_all,
            create,
            expected_md5: None,
            expected_size: None,
            expected_mtime_ms: None,
            backup: false,
            backup_path: None,
            evidence: false,
            diff_max_lines: 200,
            allow_lossy_utf8: false,
        }
    }

    #[test]
    fn replace_single_occurrence() {
        let f = make_temp("fn hello() {\n    println!(\"hello\");\n}\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "hello", "world", false, false),
        );
        assert!(result.contains("ERROR"), "should fail: 'hello' appears 2x");
    }

    #[test]
    fn replace_all() {
        let f = make_temp("aaa bbb aaa\n");
        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &mk_params(f.path(), "aaa", "ccc", true, false));
        assert!(result.contains("2 replacements"));
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "ccc bbb ccc\n");
    }

    #[test]
    fn not_found_error() {
        let f = make_temp("some content\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "nonexistent", "x", false, false),
        );
        assert!(result.contains("ERROR: old_string not found"));
    }

    #[test]
    fn create_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/new_file.txt");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(&path, "", "line1\nline2\nline3\n", false, true),
        );
        assert!(result.contains("created new_file.txt"));
        assert!(result.contains("3 lines"));
        assert!(path.exists());
    }

    /// #475: creating a file inside a read-only root is refused before the
    /// directory is even materialised (guard in `handle_create`).
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn create_denied_in_read_only_root() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        std::fs::create_dir_all(&ro).unwrap();
        let path = ro.join("sub/new_file.txt");

        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &mk_params(&path, "", "x\n", false, true));
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            result.contains("read-only"),
            "create in a read-only root must be refused: {result}"
        );
        assert!(!path.exists(), "no file may be created in a read-only root");
        assert!(
            !ro.join("sub").exists(),
            "no directory may be created in a read-only root"
        );
    }

    /// #475: editing an existing file inside a read-only root is refused at the
    /// atomic-write choke point (`write_atomic_bytes_with_permissions`), leaving
    /// the original bytes intact.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn edit_denied_in_read_only_root() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        std::fs::create_dir_all(&ro).unwrap();
        let path = ro.join("a.txt");
        std::fs::write(&path, "alpha beta\n").unwrap();

        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(&path, "alpha", "OMEGA", false, false),
        );
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            result.contains("read-only"),
            "edit in a read-only root must be refused: {result}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha beta\n",
            "the file must be left untouched"
        );
    }

    /// #475 (the exact #464 regression): a caller-supplied `backup_path` must
    /// not be a side door into a read-only root. Even when the *target* file is
    /// writable, redirecting the pre-edit backup into a read-only root is denied
    /// — and because the backup is written first, the denial is fail-closed: the
    /// target keeps its original bytes and no backup is dropped in the root.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn backup_path_cannot_smuggle_writes_into_read_only_root() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("a.txt"); // writable target, outside the RO root
        std::fs::write(&target, "alpha beta\n").unwrap();
        let smuggled = ro.join("leak.bak"); // attacker-chosen backup inside RO root

        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let mut params = mk_params(&target, "alpha", "OMEGA", false, false);
        params.backup = true;
        params.backup_path = Some(smuggled.to_string_lossy().to_string());
        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &params);
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            result.contains("read-only"),
            "a backup_path into a read-only root must be refused: {result}"
        );
        assert!(
            !smuggled.exists(),
            "no backup may be smuggled into a read-only root"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "alpha beta\n",
            "fail-closed: the writable target must be untouched when the backup is denied"
        );
    }

    /// #475 end-to-end via the *real* config mechanism a user would use:
    /// `read_only_roots` declared in `config.toml` (not the env var) must make
    /// `ctx_edit` refuse the write. Exercises the `Config::load()` → predicate →
    /// tool-denial chain.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn edit_denied_via_config_read_only_roots() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        std::fs::create_dir_all(&ro).unwrap();
        let path = ro.join("a.txt");
        std::fs::write(&path, "alpha beta\n").unwrap();

        // Write the user-facing config.toml into the isolated config dir.
        let cfg_path = crate::core::config::Config::path().unwrap();
        if let Some(parent) = cfg_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // TOML literal string ('...') — no escaping of the temp path needed.
        std::fs::write(
            &cfg_path,
            format!("read_only_roots = ['{}']\n", ro.to_string_lossy()),
        )
        .unwrap();

        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(&path, "alpha", "OMEGA", false, false),
        );

        assert!(
            result.contains("read-only"),
            "config-declared read_only_roots must deny the edit: {result}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha beta\n",
            "the file must be left untouched"
        );
    }

    #[test]
    fn readonly_dir_error_classification() {
        assert!(is_readonly_dir_error(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
        assert!(!is_readonly_dir_error(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        #[cfg(unix)]
        {
            assert!(is_readonly_dir_error(&std::io::Error::from_raw_os_error(
                libc::EROFS
            )));
            assert!(is_readonly_dir_error(&std::io::Error::from_raw_os_error(
                libc::EACCES
            )));
            assert!(is_readonly_dir_error(&std::io::Error::from_raw_os_error(
                libc::EPERM
            )));
        }
    }

    #[cfg(unix)]
    #[test]
    fn in_place_overwrite_truncates_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.jsonc");
        std::fs::write(&path, b"longer original content").unwrap();
        in_place_overwrite(&path, b"short", None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
    }

    // GH #459: parent dir read-only, file inode writable (the bind-mount
    // sandbox shape). The atomic tempfile + rename needs *directory* write
    // permission and fails; the in-place fallback overwrites the existing inode
    // and succeeds. Skipped under root, which bypasses the directory permission
    // check (the atomic path would then succeed and the fallback never runs —
    // the write still lands correctly either way).
    #[cfg(unix)]
    #[test]
    fn write_falls_back_on_readonly_parent_dir() {
        use std::os::unix::fs::PermissionsExt;

        // SAFETY: geteuid() takes no arguments and only reads the caller's uid.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(&path, b"hello").unwrap();

        // r-x parent: create_new tempfile + rename fail with EACCES, but the
        // existing file mode (0o644) still allows O_WRONLY|O_TRUNC.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let res = write_atomic_bytes_with_permissions(&path, b"world", None);

        // Restore so tempdir cleanup can remove the directory.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(res.is_ok(), "in-place fallback should succeed: {res:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"world");
    }

    // GH #459 end-to-end: the full ctx_edit flow (read -> preimage -> write)
    // must succeed when the parent dir is read-only but the file is writable.
    #[cfg(unix)]
    #[test]
    fn handle_edit_succeeds_on_readonly_parent_dir() {
        use std::os::unix::fs::PermissionsExt;

        // SAFETY: geteuid() takes no arguments and only reads the caller's uid.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(&path, "hello world\n").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(&path, "hello", "goodbye", false, false),
        );

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.contains('✓'),
            "edit should succeed via in-place fallback: {result}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "goodbye world\n");
    }

    #[test]
    fn unique_match_succeeds() {
        let f = make_temp("fn main() {\n    let x = 42;\n}\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "let x = 42", "let x = 99", false, false),
        );
        assert!(result.contains("✓"));
        assert!(result.contains("1 replacement"));
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("let x = 99"));
    }

    #[test]
    fn crlf_file_with_lf_search() {
        let f = make_temp("line1\r\nline2\r\nline3\r\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "line1\nline2", "changed1\nchanged2", false, false),
        );
        assert!(result.contains("✓"), "CRLF fallback should work: {result}");
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            content.contains("changed1\r\nchanged2"),
            "new_string should be adapted to CRLF: {content:?}"
        );
        assert!(
            content.contains("\r\nline3\r\n"),
            "rest of file should keep CRLF: {content:?}"
        );
    }

    #[test]
    fn lf_file_with_crlf_search() {
        let f = make_temp("line1\nline2\nline3\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "line1\r\nline2", "a\r\nb", false, false),
        );
        assert!(result.contains("✓"), "LF fallback should work: {result}");
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            content.contains("a\nb"),
            "new_string should be adapted to LF: {content:?}"
        );
    }

    #[test]
    fn trailing_whitespace_tolerance() {
        let f = make_temp("  let x = 1;  \n  let y = 2;\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(
                f.path(),
                "  let x = 1;\n  let y = 2;",
                "  let x = 10;\n  let y = 20;",
                false,
                false,
            ),
        );
        assert!(
            result.contains("✓"),
            "trailing whitespace tolerance should work: {result}"
        );
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("let x = 10;"));
        assert!(content.contains("let y = 20;"));
    }

    #[test]
    fn crlf_with_trailing_whitespace() {
        let f = make_temp("  const a = 1;  \r\n  const b = 2;\r\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(
                f.path(),
                "  const a = 1;\n  const b = 2;",
                "  const a = 10;\n  const b = 20;",
                false,
                false,
            ),
        );
        assert!(
            result.contains("✓"),
            "CRLF + trailing whitespace should work: {result}"
        );
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("const a = 10;"));
        assert!(content.contains("const b = 20;"));
    }

    #[test]
    fn rejects_invalid_utf8_by_default() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&[0xff, 0xfe, 0xfd]).unwrap();
        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &mk_params(f.path(), "a", "b", false, false));
        assert!(
            result.contains("not valid UTF-8"),
            "expected utf8 rejection, got: {result}"
        );
    }

    #[test]
    fn allows_lossy_utf8_only_when_enabled() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&[0xff, 0xfe, 0xfd]).unwrap();
        let mut cache = SessionCache::new();
        let mut p = mk_params(f.path(), "a", "b", false, false);
        p.allow_lossy_utf8 = true;
        let result = handle(&mut cache, &p);
        assert!(
            !result.contains("not valid UTF-8"),
            "lossy mode should avoid utf8 hard error, got: {result}"
        );
    }

    #[test]
    fn expected_md5_mismatch_fails_without_writing() {
        let f = make_temp("aaa\n");
        let mut cache = SessionCache::new();
        let mut p = mk_params(f.path(), "aaa", "bbb", false, false);
        p.expected_md5 = Some("deadbeef".to_string());
        let result = handle(&mut cache, &p);
        assert!(
            result.contains("preimage mismatch"),
            "expected preimage mismatch, got: {result}"
        );
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "aaa\n");
    }

    #[test]
    fn backup_is_created_when_enabled() {
        let f = make_temp("aaa\n");
        let mut cache = SessionCache::new();
        let mut p = mk_params(f.path(), "aaa", "bbb", false, false);
        p.backup = true;
        let out = handle(&mut cache, &p);
        assert!(out.contains("backup:"), "expected backup path, got: {out}");
        let bp = out
            .lines()
            .find_map(|l| l.strip_prefix("backup: "))
            .expect("backup line");
        let backup_content = std::fs::read_to_string(bp).unwrap();
        assert_eq!(backup_content, "aaa\n");
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "bbb\n");
    }

    #[test]
    fn evidence_diff_is_emitted_when_enabled() {
        let f = make_temp("line1\nline2\n");
        let mut cache = SessionCache::new();
        let mut p = mk_params(f.path(), "line2", "changed2", false, false);
        p.evidence = true;
        p.diff_max_lines = 50;
        let out = handle(&mut cache, &p);
        assert!(out.contains("```diff"), "expected diff fence, got: {out}");
        assert!(
            out.contains("preimage:"),
            "expected preimage metadata, got: {out}"
        );
        assert!(
            out.contains("postimage:"),
            "expected postimage metadata, got: {out}"
        );
    }

    #[test]
    fn detects_toctou_via_preimage_guard() {
        let f = make_temp("aaa\n");
        let cap = crate::core::limits::max_read_bytes();
        let pre = read_preimage(f.path(), cap, false).unwrap();
        std::fs::write(f.path(), "bbb\n").unwrap();
        let err = ensure_preimage_still_matches(f.path(), &pre.fp, cap).unwrap_err();
        assert!(err.contains("TOCTOU guard"), "unexpected error: {err}");
    }

    /// Issue #320: run_io performs the full edit without any cache handle, so the
    /// MCP layer can avoid holding the global cache write-lock across disk I/O.
    /// A successful edit reports an Invalidate effect.
    #[test]
    fn run_io_success_reports_invalidate_effect() {
        let f = make_temp("fn main() {\n    let x = 42;\n}\n");
        let (text, effect) = run_io(
            &mk_params(f.path(), "let x = 42", "let x = 99", false, false),
            "",
        );
        assert!(text.contains("✓"), "expected success: {text}");
        assert!(
            matches!(effect, CacheEffect::Invalidate),
            "successful edit must invalidate the cache entry"
        );
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("let x = 99"));
    }

    #[test]
    fn run_io_failure_reports_no_cache_effect() {
        let f = make_temp("some content\n");
        let (text, effect) = run_io(&mk_params(f.path(), "nonexistent", "x", false, false), "");
        assert!(text.contains("ERROR: old_string not found"));
        assert!(
            matches!(effect, CacheEffect::None),
            "a failed edit must not mutate the cache"
        );
    }

    /// Issue #320: concurrent edits to *different* files must all succeed without
    /// serializing on any shared lock — run_io takes no cache, so there is nothing
    /// global to contend on.
    #[test]
    fn run_io_concurrent_edits_to_different_files_all_succeed() {
        use std::sync::Arc;
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let n = 16;
        let mut paths = Vec::new();
        for i in 0..n {
            let p = dir.path().join(format!("file_{i}.txt"));
            std::fs::write(&p, format!("value = {i}\n")).unwrap();
            paths.push(p);
        }
        let barrier = Arc::new(std::sync::Barrier::new(n));
        let mut handles = Vec::new();
        for (i, p) in paths.into_iter().enumerate() {
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let (text, effect) = run_io(
                    &mk_params(
                        &p,
                        &format!("value = {i}"),
                        &format!("value = {}", i + 1000),
                        false,
                        false,
                    ),
                    "",
                );
                assert!(text.contains("✓"), "edit {i} failed: {text}");
                assert!(matches!(effect, CacheEffect::Invalidate));
                (p, i)
            }));
        }
        for h in handles {
            let (p, i) = h.join().unwrap();
            let content = std::fs::read_to_string(&p).unwrap();
            assert_eq!(content, format!("value = {}\n", i + 1000));
        }
    }

    #[test]
    fn run_io_escalation_reports_store_full_effect() {
        // A file previously read in a compressed mode ("signatures") triggers
        // auto-escalation when old_string is not found: the full content is
        // returned for re-store.
        let f = make_temp("line a\nline b\nline c\n");
        let (text, effect) = run_io(
            &mk_params(f.path(), "definitely-not-present", "x", false, false),
            "signatures",
        );
        assert!(
            text.contains("[auto-escalation]"),
            "expected escalation: {text}"
        );
        match effect {
            CacheEffect::StoreFull(content) => {
                assert!(content.contains("line a") && content.contains("line c"));
            }
            _ => panic!("escalation must report a StoreFull cache effect"),
        }
    }

    #[test]
    fn apply_cache_effect_invalidate_and_store() {
        let f = make_temp("hello\n");
        let mut cache = SessionCache::new();
        cache.store(&f.path().to_string_lossy(), "hello\n");
        apply_cache_effect(
            &mut cache,
            &f.path().to_string_lossy(),
            CacheEffect::Invalidate,
        );
        assert!(
            cache.get(&f.path().to_string_lossy()).is_none(),
            "Invalidate must drop the entry"
        );
        apply_cache_effect(
            &mut cache,
            &f.path().to_string_lossy(),
            CacheEffect::StoreFull("fresh\n".to_string()),
        );
        assert!(
            cache.get(&f.path().to_string_lossy()).is_some(),
            "StoreFull must re-populate the entry"
        );
    }

    #[test]
    fn identical_old_new_rejected() {
        let f = make_temp("fn main() {}\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "fn main() {}", "fn main() {}", false, false),
        );
        assert!(result.contains("identical"));
    }

    #[test]
    fn edit_already_applied_detected() {
        let f = make_temp("fn updated() {}\n");
        let (text, effect) = run_io(
            &mk_params(
                f.path(),
                "fn original() {}",
                "fn updated() {}",
                false,
                false,
            ),
            "",
        );
        assert!(text.contains("already exists"));
        assert!(text.contains("already applied"));
        assert!(matches!(effect, CacheEffect::None));
    }

    #[test]
    fn closest_line_hint_shown() {
        let f = make_temp("  fn hello() {\n    println!(\"hi\");\n  }\n");
        let (text, _) = run_io(
            &mk_params(f.path(), "fn hello(){", "fn hello_world(){", false, false),
            "",
        );
        assert!(text.contains("Closest match at line"));
    }

    #[test]
    fn missing_file_suggests_relocated_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/new")).unwrap();
        std::fs::write(dir.path().join("src/new/gizmo.rs"), "fn gizmo() {}\n").unwrap();

        let (text, effect) = run_io(
            &mk_params(
                &dir.path().join("src/old/gizmo.rs"),
                "fn gizmo() {}",
                "fn gizmo2() {}",
                false,
                false,
            ),
            "",
        );
        assert!(text.contains("same-named file was found"), "got: {text}");
        assert!(text.contains("gizmo.rs"), "got: {text}");
        assert!(matches!(effect, CacheEffect::None));
    }

    #[test]
    fn old_string_in_other_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let target = dir.path().join("a.rs");
        std::fs::write(&target, "fn unrelated_a() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn the_target_symbol() {}\n").unwrap();

        let (text, _) = run_io(
            &mk_params(
                &target,
                "fn the_target_symbol() {}",
                "fn renamed() {}",
                false,
                false,
            ),
            "",
        );
        assert!(text.contains("matching line exists in"), "got: {text}");
        assert!(text.contains("b.rs"), "got: {text}");
    }

    // P0-6 (#418): a symlink at the edit path must be rejected on the read side —
    // a link planted inside the jail could otherwise read/overwrite outside it.
    #[cfg(unix)]
    #[test]
    fn editing_through_a_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.rs");
        std::fs::write(&real, "fn old() {}\n").unwrap();
        let link = dir.path().join("link.rs");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let (text, effect) = run_io(
            &mk_params(&link, "fn old() {}", "fn new() {}", false, false),
            "",
        );
        assert!(text.contains("symlink"), "got: {text}");
        assert!(matches!(effect, CacheEffect::None));
        // Target untouched.
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "fn old() {}\n");
    }

    // P0-6 (#418): the write side must also reject a symlink destination
    // (defense in depth for create-mode and backup paths).
    #[cfg(unix)]
    #[test]
    fn creating_over_a_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("victim.txt");
        std::fs::write(&real, "precious").unwrap();
        let link = dir.path().join("innocent.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let (text, _) = run_io(&mk_params(&link, "", "overwritten", false, true), "");
        assert!(
            text.contains("symlink") || text.contains("ERROR"),
            "got: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "precious",
            "symlink target must not be modified"
        );
    }

    #[test]
    fn regular_file_edit_still_works_after_symlink_guard() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("normal.rs");
        std::fs::write(&file, "fn old() {}\n").unwrap();

        let (text, _) = run_io(
            &mk_params(&file, "fn old() {}", "fn new() {}", false, false),
            "",
        );
        assert!(
            text.contains("Edit applied") || !text.starts_with("ERROR"),
            "got: {text}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn new() {}\n");
    }
}
