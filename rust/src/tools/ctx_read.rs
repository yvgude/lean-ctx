use std::path::Path;

use crate::core::cache::SessionCache;
use crate::core::compressor;
use crate::core::deps;
use crate::core::entropy;
use crate::core::protocol;
use crate::core::signatures;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

/// Pre-counted read output carrying the output string, resolved mode,
/// and token count computed during mode processing.
pub struct ReadOutput {
    pub content: String,
    pub resolved_mode: String,
    /// Approximate output token count from mode processing.
    /// The dispatch layer recounts the final assembled string for accurate savings.
    pub output_tokens: usize,
}

const COMPRESSED_HINT: &str = "[compressed — use mode=\"full\" for complete source]";

const CACHEABLE_MODES: &[&str] = &["map", "signatures"];

fn is_cacheable_mode(mode: &str) -> bool {
    CACHEABLE_MODES.contains(&mode)
}

fn compressed_cache_key(mode: &str, crp_mode: CrpMode, task: Option<&str>) -> String {
    let versioned_mode = match mode {
        "map" => "map:v2",
        "signatures" => "signatures:v2",
        _ => mode,
    };
    let base = if crp_mode.is_tdd() {
        format!("{versioned_mode}:tdd")
    } else {
        versioned_mode.to_string()
    };
    // map/signatures output now embeds a task-relevant body, so task-aware and
    // task-free variants must cache under distinct keys.
    match task.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            t.hash(&mut h);
            format!("{base}:t{:x}", h.finish())
        }
        None => base,
    }
}

/// Extracts a short proof-line from file content to include in cache-hit stubs.
/// Returns the first non-empty line (truncated to 60 chars) as evidence the cache is valid.
/// Only shown after 2+ reads to avoid noise on early interactions.
fn cache_hit_proof_line(content: &str, read_count: u32) -> Option<String> {
    if read_count < 2 {
        return None;
    }
    let first_line = content.lines().find(|l| !l.trim().is_empty())?;
    let trimmed = first_line.trim();
    if trimmed.len() > 60 {
        let mut end = 57;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        Some(format!("{}...", &trimmed[..end]))
    } else {
        Some(trimmed.to_string())
    }
}

fn append_compressed_hint(output: &str, file_path: &str) -> String {
    if !crate::core::profiles::active_profile()
        .output_hints
        .compressed_hint()
    {
        return output.to_string();
    }
    format!(
        "{output}\n{COMPRESSED_HINT}\n  ctx_read(\"{file_path}\", mode=\"full\") | ctx_retrieve(\"{file_path}\")"
    )
}

/// Reads a file as UTF-8 with lossy fallback, enforcing binary detection and max read size limit.
/// Defense-in-depth: verifies that the canonical path stays within the process's project root
/// (if determinable) even though callers SHOULD have already jail-checked the path.
pub fn read_file_lossy(path: &str) -> Result<String, std::io::Error> {
    if crate::core::binary_detect::is_binary_file(path) {
        let msg = crate::core::binary_detect::binary_file_message(path);
        return Err(std::io::Error::other(msg));
    }

    {
        let canonical =
            crate::core::pathutil::safe_canonicalize_bounded(std::path::Path::new(path), 2000);
        if let Ok(cwd) = std::env::current_dir() {
            let root = crate::core::pathutil::safe_canonicalize_bounded(&cwd, 2000);
            if !canonical.starts_with(&root) {
                let allow = crate::core::pathjail::allow_paths_from_env_and_config();
                let data_dir_ok = crate::core::data_dir::lean_ctx_data_dir()
                    .ok()
                    .is_some_and(|d| canonical.starts_with(d));
                let tmp_ok = canonical.starts_with(std::env::temp_dir());
                if !allow.iter().any(|a| canonical.starts_with(a)) && !data_dir_ok && !tmp_ok {
                    tracing::warn!(
                        "defense-in-depth: path may escape project root: {}",
                        canonical.display()
                    );
                }
            }
        }
    }

    let cap = crate::core::limits::max_read_bytes();

    let file = open_with_retry(path)?;
    let meta = file
        .metadata()
        .map_err(|e| std::io::Error::other(format!("cannot stat open file descriptor: {e}")))?;
    if meta.len() > cap as u64 {
        return Err(std::io::Error::other(format!(
            "file too large ({} bytes, limit {} bytes via LCTX_MAX_READ_BYTES). \
             Increase the limit or use a line-range read: mode=\"lines:1-100\"",
            meta.len(),
            cap
        )));
    }

    use std::io::Read;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::io::BufReader::new(file).read_to_end(&mut bytes)?;
    match String::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(e) => Ok(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

/// Opens a file, retrying once after a brief pause on NotFound.
/// Works around overlay/FUSE stat-cache races in container runtimes (Docker, Codex).
/// Uses O_NOFOLLOW on Unix for TOCTOU symlink protection.
fn open_with_retry(path: &str) -> Result<std::fs::File, std::io::Error> {
    match open_nofollow(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            open_nofollow(path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    std::io::Error::other(format!(
                        "file not found: {path} — verify the path with ctx_tree or ctx_search"
                    ))
                } else {
                    e
                }
            })
        }
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn open_nofollow(path: &str) -> Result<std::fs::File, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    let p = Path::new(path);
    // Canonicalize the parent directory (resolving symlinks in the directory path)
    // but apply O_NOFOLLOW only to the final file component. This prevents
    // symlink-following attacks on the target file while allowing legitimate
    // directory symlinks (e.g., /tmp → /private/tmp on macOS).
    if let (Some(parent), Some(filename)) = (p.parent(), p.file_name()) {
        if parent.exists() {
            let canonical_parent = crate::core::pathutil::safe_canonicalize_bounded(parent, 2000);
            let canonical_path = canonical_parent.join(filename);
            return std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&canonical_path);
        }
    }

    // Fallback: direct open with O_NOFOLLOW
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_nofollow(path: &str) -> Result<std::fs::File, std::io::Error> {
    std::fs::File::open(path)
}

/// Reads a file through the cache and applies the requested compression mode.
pub fn handle(cache: &mut SessionCache, path: &str, mode: &str, crp_mode: CrpMode) -> String {
    handle_with_options(cache, path, mode, false, crp_mode, None)
}

/// Like `handle`, but invalidates the cache first to force a fresh disk read.
pub fn handle_fresh(cache: &mut SessionCache, path: &str, mode: &str, crp_mode: CrpMode) -> String {
    handle_with_options(cache, path, mode, true, crp_mode, None)
}

/// Reads a file with task-aware filtering to prioritize task-relevant content.
pub fn handle_with_task(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_options(cache, path, mode, false, crp_mode, task)
}

/// Like `handle_with_task`, also returns the resolved mode name and pre-counted tokens.
pub fn handle_with_task_resolved(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    handle_with_options_resolved(cache, path, mode, false, crp_mode, task)
}

/// Fresh read with task-aware filtering (invalidates cache first).
pub fn handle_fresh_with_task(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_options(cache, path, mode, true, crp_mode, task)
}

/// Fresh read with task-aware filtering, also returns the resolved mode name and pre-counted tokens.
pub fn handle_fresh_with_task_resolved(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    handle_with_options_resolved(cache, path, mode, true, crp_mode, task)
}

fn handle_with_options(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_options_resolved(cache, path, mode, fresh, crp_mode, task).content
}

/// Detects if the current execution context is a subagent (forked agent).
/// Subagents inherit stale parent caches, so force-fresh prevents VERIFY FAIL.
fn is_subagent_context() -> bool {
    static IS_SUBAGENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_SUBAGENT.get_or_init(|| {
        if std::env::var("LEAN_CTX_FORCE_FRESH").is_ok_and(|v| v == "1" || v == "true") {
            return true;
        }
        std::env::var("CURSOR_TASK_ID").is_ok_and(|v| !v.is_empty())
    })
}

fn handle_with_options_resolved(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    let effective_fresh = fresh || is_subagent_context();

    if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
        bt.next_seq();
    }
    let mut result = handle_with_options_inner(cache, path, mode, effective_fresh, crp_mode, task);

    if let Some(entry) = cache.get_mut(path) {
        entry.last_mode.clone_from(&result.resolved_mode);
    }

    let dedup_allowed = matches!(
        result.resolved_mode.as_str(),
        "map" | "signatures" | "aggressive" | "entropy" | "task"
    );
    if dedup_allowed {
        if let Some(deduped) = cache.apply_dedup(path, &result.content) {
            let new_tokens = count_tokens(&deduped);
            if new_tokens < result.output_tokens {
                result.content = deduped;
                result.output_tokens = new_tokens;
            }
        }
    }

    if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
        let original_tokens = cache.get(path).map_or(0, |e| e.original_tokens);
        bt.record_read(
            path,
            &result.resolved_mode,
            result.output_tokens,
            original_tokens,
        );
    }

    result
}

fn handle_with_options_inner(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    let file_ref = cache.get_file_ref(path);
    let short = protocol::shorten_path(path);
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if fresh {
        if mode == "diff" {
            let warning = "[warning] fresh+diff is redundant — fresh invalidates cache, no diff possible. Use mode=full with fresh=true instead.";
            return ReadOutput {
                content: warning.to_string(),
                resolved_mode: "diff".into(),
                output_tokens: count_tokens(warning),
            };
        }
        cache.invalidate(path);
    }

    if mode == "diff" {
        let (out, _) = handle_diff(cache, path, &file_ref);
        let out = crate::core::redaction::redact_text_if_enabled(&out);
        let sent = count_tokens(&out);
        return ReadOutput {
            content: out,
            resolved_mode: "diff".into(),
            output_tokens: sent,
        };
    }

    if mode != "full" {
        if let Some(existing) = cache.get(path) {
            let stale = crate::core::cache::is_cache_entry_stale(path, existing.stored_mtime);
            if stale {
                cache.invalidate(path);
            }
        }
    }

    // Extract immutable data from cache entry, then drop the borrow before
    // any mutable operations (record_cache_hit, set_compressed, invalidate).
    let cache_snapshot = cache.get(path).map(|existing| {
        (
            existing.stored_mtime,
            existing.read_count,
            existing.line_count,
            existing.original_tokens,
            existing.content(),
        )
    });

    if let Some((cached_mtime, read_count, line_count, original_tokens, content_opt)) =
        cache_snapshot
    {
        if mode == "full" {
            let no_deg = crate::core::config::Config::load().no_degrade_effective();
            let prof = crate::core::profiles::active_profile();
            let force_full = no_deg
                || (prof.read.default_mode_effective() == "full"
                    && prof.compression.crp_mode_effective() == "off");
            let policy_allows_stub =
                crate::server::compaction_sync::effective_cache_policy() != "safe" && !force_full;
            if policy_allows_stub
                && !crate::core::cache::is_cache_entry_stale(path, cached_mtime)
                && cache.is_full_delivered(path)
            {
                cache.record_cache_hit(path);
                let out = if crate::core::protocol::meta_visible() {
                    format!(
                        "{file_ref}={short} [unchanged {line_count}L]\nUnchanged on disk. Use fresh=true to force re-read.",
                        )
                } else {
                    let proof = content_opt
                        .as_deref()
                        .and_then(|c| cache_hit_proof_line(c, read_count));
                    let reads_note = if read_count > 3 {
                        format!(" (read {}x)", read_count + 1)
                    } else {
                        String::new()
                    };
                    match proof {
                        Some(p) => format!(
                            "{file_ref}={short} [unchanged {line_count}L{reads_note} | \"{p}\"]"
                        ),
                        None => format!("{file_ref}={short} [unchanged {line_count}L{reads_note}]"),
                    }
                };
                let out = crate::core::redaction::redact_text_if_enabled(&out);
                let sent = count_tokens(&out);
                return ReadOutput {
                    content: out,
                    resolved_mode: "full".into(),
                    output_tokens: sent,
                };
            }
            let (out, _) = handle_full_with_auto_delta(cache, path, &file_ref, &short, ext, task);
            let out = crate::core::redaction::redact_text_if_enabled(&out);
            let sent = count_tokens(&out);
            return ReadOutput {
                content: out,
                resolved_mode: "full".into(),
                output_tokens: sent,
            };
        }

        // Resolve mode first so we can check compressed output cache BEFORE
        // decompressing the full content (avoids ~2-5ms zstd overhead on hits).
        let resolved_mode = if mode == "auto" {
            resolve_auto_mode(path, original_tokens, task)
        } else {
            mode.to_string()
        };

        if is_cacheable_mode(&resolved_mode) {
            let cache_key = compressed_cache_key(&resolved_mode, crp_mode, task);
            let compressed_hit = cache.get_compressed(path, &cache_key).cloned();
            if let Some(cached_output) = compressed_hit {
                cache.record_cache_hit(path);
                let out = crate::core::redaction::redact_text_if_enabled(&cached_output);
                let sent = count_tokens(&out);
                return ReadOutput {
                    content: out,
                    resolved_mode,
                    output_tokens: sent,
                };
            }
        }

        if let Some(content) = content_opt {
            let (out, _) = process_mode(
                &content,
                &resolved_mode,
                &file_ref,
                &short,
                ext,
                original_tokens,
                crp_mode,
                path,
                task,
            );
            if is_cacheable_mode(&resolved_mode) {
                let cache_key = compressed_cache_key(&resolved_mode, crp_mode, task);
                cache.set_compressed(path, &cache_key, out.clone());
            }
            let out = crate::core::redaction::redact_text_if_enabled(&out);
            let sent = count_tokens(&out);
            return ReadOutput {
                content: out,
                resolved_mode,
                output_tokens: sent,
            };
        }
        cache.invalidate(path);
    }

    let content = match read_file_lossy(path) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("ERROR: {e}");
            let tokens = count_tokens(&msg);
            return ReadOutput {
                content: msg,
                resolved_mode: "error".into(),
                output_tokens: tokens,
            };
        }
    };

    let store_result = cache.store(path, &content);

    // Skip expensive hint computation for line-range reads and first reads.
    // Hints are only useful from the 2nd read onwards when the file is contextually relevant.
    let is_line_range = mode.starts_with("lines:");
    let hints = crate::core::profiles::active_profile().output_hints;
    let is_repeat_read = store_result.read_count > 1;
    let similar_hint = if !is_line_range && is_repeat_read && hints.semantic_hint() {
        find_similar_and_update_semantic_index(path, &content)
    } else {
        None
    };
    let graph_hint = if !is_line_range && is_repeat_read && hints.related_hint() {
        build_graph_related_hint(path)
    } else {
        None
    };

    if mode == "full" {
        cache.mark_full_delivered(path);
        let (mut output, _) = format_full_output(
            &file_ref,
            &short,
            ext,
            &content,
            store_result.original_tokens,
            store_result.line_count,
            task,
        );
        if let Some(hint) = &graph_hint {
            output.push_str(&format!("\n{hint}"));
        }
        if let Some(hint) = similar_hint {
            output.push_str(&format!("\n{hint}"));
        }
        let output = crate::core::redaction::redact_text_if_enabled(&output);
        let sent = count_tokens(&output);
        return ReadOutput {
            content: output,
            resolved_mode: "full".into(),
            output_tokens: sent,
        };
    }

    let resolved_mode = if mode == "auto" {
        resolve_auto_mode(path, store_result.original_tokens, task)
    } else {
        mode.to_string()
    };

    let (mut output, _sent) = process_mode(
        &content,
        &resolved_mode,
        &file_ref,
        &short,
        ext,
        store_result.original_tokens,
        crp_mode,
        path,
        task,
    );
    if let Some(hint) = &graph_hint {
        output.push_str(&format!("\n{hint}"));
    }
    if let Some(hint) = similar_hint {
        output.push_str(&format!("\n{hint}"));
    }
    if is_cacheable_mode(&resolved_mode) {
        let cache_key = compressed_cache_key(&resolved_mode, crp_mode, task);
        cache.set_compressed(path, &cache_key, output.clone());
    }
    let output = crate::core::redaction::redact_text_if_enabled(&output);
    let final_tokens = count_tokens(&output);
    ReadOutput {
        content: output,
        resolved_mode,
        output_tokens: final_tokens,
    }
}

pub fn is_instruction_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    let filename = std::path::Path::new(&lower)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    matches!(
        filename,
        "skill.md"
            | "agents.md"
            | "rules.md"
            | ".cursorrules"
            | ".clinerules"
            | "lean-ctx.md"
            | "lean-ctx.mdc"
    ) || lower.contains("/skills/")
        || lower.contains("/.cursor/rules/")
        || lower.contains("/.claude/rules/")
        || lower.contains("/agents.md")
}

/// Delegates to the unified `auto_mode_resolver::resolve()`.
fn resolve_auto_mode(file_path: &str, original_tokens: usize, task: Option<&str>) -> String {
    let ctx = crate::core::auto_mode_resolver::AutoModeContext {
        path: file_path,
        token_count: original_tokens,
        task,
        cache: None,
    };
    crate::core::auto_mode_resolver::resolve(&ctx).mode
}

fn find_similar_and_update_semantic_index(path: &str, content: &str) -> Option<String> {
    const MAX_CONTENT_BYTES_FOR_SEMANTIC: usize = 32_768;

    if content.len() > MAX_CONTENT_BYTES_FOR_SEMANTIC {
        return None;
    }

    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    if !profile.semantic_cache_enabled() {
        return None;
    }

    let project_root = detect_project_root(path);
    let session_id = format!("{}", std::process::id());
    let mut index = crate::core::semantic_cache::SemanticCacheIndex::load_or_create(&project_root);

    let similar = index.find_similar(content, 0.7);
    let relevant: Vec<_> = similar
        .into_iter()
        .filter(|(p, _)| p != path)
        .take(3)
        .collect();

    index.add_file(path, content, &session_id);
    if let Err(e) = index.save(&project_root) {
        tracing::warn!("lean-ctx: failed to persist semantic index: {e}");
    }

    if relevant.is_empty() {
        return None;
    }

    let hints: Vec<String> = relevant
        .iter()
        .map(|(p, score)| format!("  {p} ({:.0}% similar)", score * 100.0))
        .collect();

    Some(format!(
        "[semantic: {} similar file(s) in cache]\n{}",
        relevant.len(),
        hints.join("\n")
    ))
}

fn detect_project_root(path: &str) -> String {
    crate::core::protocol::detect_project_root_or_cwd(path)
}

fn build_graph_related_hint(path: &str) -> Option<String> {
    let project_root = detect_project_root(path);
    crate::core::graph_context::build_related_hint(path, &project_root, 5)
}

const AUTO_DELTA_THRESHOLD: f64 = 0.6;

/// Re-reads from disk; if content changed and delta is compact, sends auto-delta.
fn handle_full_with_auto_delta(
    cache: &mut SessionCache,
    path: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    task: Option<&str>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("full");
    let Ok(disk_content) = read_file_lossy(path) else {
        cache.record_cache_hit(path);
        if let Some(existing) = cache.get(path) {
            if !crate::core::protocol::meta_visible() {
                if let Some(cached) = existing.content() {
                    return format_full_output(
                        file_ref,
                        short,
                        ext,
                        &cached,
                        existing.original_tokens,
                        existing.line_count,
                        task,
                    );
                }
            }
            let out = format!(
                "[using cached version — file read failed]\n{file_ref}={short} cached {}t {}L",
                existing.read_count, existing.line_count
            );
            let sent = count_tokens(&out);
            return (out, sent);
        }
        let out = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
            format!("[file read failed and no cached version available] {file_ref}={short}")
        } else {
            format!("[file read failed and no cached version available] {short}")
        };
        let sent = count_tokens(&out);
        return (out, sent);
    };

    let no_deg = crate::core::config::Config::load().no_degrade_effective();
    let prof = crate::core::profiles::active_profile();
    let force_full = no_deg
        || (prof.read.default_mode_effective() == "full"
            && prof.compression.crp_mode_effective() == "off");

    let old_content = cache
        .get(path)
        .and_then(crate::core::cache::CacheEntry::content)
        .unwrap_or_default();
    let store_result = cache.store(path, &disk_content);

    if store_result.was_hit {
        let policy_allows_stub =
            crate::server::compaction_sync::effective_cache_policy() != "safe" && !force_full;
        if policy_allows_stub && store_result.full_content_delivered {
            let out = if crate::core::protocol::meta_visible() {
                format!(
                    "{file_ref}={short} [unchanged {}L]\nUnchanged on disk. Use fresh=true to force re-read.",
                    store_result.line_count
                )
            } else {
                let proof = cache_hit_proof_line(&disk_content, store_result.read_count);
                let reads_note = if store_result.read_count > 3 {
                    format!(" (read {}x)", store_result.read_count)
                } else {
                    String::new()
                };
                match proof {
                    Some(p) => format!(
                        "{file_ref}={short} [unchanged {}L{reads_note} | \"{p}\"]",
                        store_result.line_count
                    ),
                    None => format!(
                        "{file_ref}={short} [unchanged {}L{reads_note}]",
                        store_result.line_count
                    ),
                }
            };
            let sent = count_tokens(&out);
            return (out, sent);
        }
        cache.mark_full_delivered(path);
        return format_full_output(
            file_ref,
            short,
            ext,
            &disk_content,
            store_result.original_tokens,
            store_result.line_count,
            task,
        );
    }

    let diff = compressor::diff_content(&old_content, &disk_content);
    let diff_tokens = count_tokens(&diff);
    let full_tokens = store_result.original_tokens;

    if !force_full
        && full_tokens > 0
        && (diff_tokens as f64) < (full_tokens as f64 * AUTO_DELTA_THRESHOLD)
    {
        let savings = protocol::format_savings(full_tokens, diff_tokens);
        let head = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
            format!("{file_ref}={short}")
        } else {
            short.to_string()
        };
        let out = format!(
            "{head} [auto-delta] ∆{}L\n{diff}\n{savings}",
            disk_content.lines().count()
        );
        return (out, diff_tokens);
    }

    format_full_output(
        file_ref,
        short,
        ext,
        &disk_content,
        store_result.original_tokens,
        store_result.line_count,
        task,
    )
}

fn format_full_output(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    original_tokens: usize,
    line_count: usize,
    _task: Option<&str>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("full");
    let tokens = original_tokens;
    let metadata = build_header(file_ref, short, ext, content, line_count, true);

    let output = format!("{metadata}\n{content}");
    let sent = count_tokens(&output);
    (protocol::append_savings(&output, tokens, sent), sent)
}

fn build_header(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    line_count: usize,
    include_deps: bool,
) -> String {
    let mut header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L")
    } else {
        format!("{short} {line_count}L")
    };

    if include_deps {
        let dep_info = deps::extract_deps(content, ext);
        if !dep_info.imports.is_empty() {
            let imports_str: Vec<&str> = dep_info
                .imports
                .iter()
                .take(8)
                .map(std::string::String::as_str)
                .collect();
            header.push_str(&format!("\n deps {}", imports_str.join(",")));
        }
        if !dep_info.exports.is_empty() {
            let exports_str: Vec<&str> = dep_info
                .exports
                .iter()
                .take(8)
                .map(std::string::String::as_str)
                .collect();
            header.push_str(&format!("\n exports {}", exports_str.join(",")));
        }
    }

    header
}

#[allow(clippy::too_many_arguments)]
fn process_mode(
    content: &str,
    mode: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
    file_path: &str,
    task: Option<&str>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new(mode);
    let line_count = content.lines().count();

    match mode {
        "auto" => {
            let chosen = resolve_auto_mode(file_path, original_tokens, task);
            process_mode(
                content,
                &chosen,
                file_ref,
                short,
                ext,
                original_tokens,
                crp_mode,
                file_path,
                task,
            )
        }
        "full" => format_full_output(
            file_ref,
            short,
            ext,
            content,
            original_tokens,
            line_count,
            task,
        ),
        "signatures" => {
            let sigs = signatures::extract_signatures(content, ext);
            let dep_info = deps::extract_deps(content, ext);

            let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short} {line_count}L")
            } else {
                format!("{short} {line_count}L")
            };
            if !dep_info.imports.is_empty() {
                let imports_str: Vec<&str> = dep_info
                    .imports
                    .iter()
                    .take(8)
                    .map(std::string::String::as_str)
                    .collect();
                output.push_str(&format!("\n deps {}", imports_str.join(",")));
            }
            for sig in &sigs {
                output.push('\n');
                if crp_mode.is_tdd() {
                    output.push_str(&sig.to_tdd());
                } else {
                    output.push_str(&sig.to_compact());
                }
            }
            if let Some(body) = task_relevant_body(content, file_path, ext, task) {
                output.push('\n');
                output.push_str(&body);
            }
            let sent = count_tokens(&output);
            (
                append_compressed_hint(
                    &protocol::append_savings(&output, original_tokens, sent),
                    file_path,
                ),
                sent,
            )
        }
        "map" => {
            if ext == "php" {
                if let Some(php_map) = crate::core::patterns::php::compress_php_map(content, short)
                {
                    let output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                        format!("{file_ref}={short} {line_count}L\n{php_map}")
                    } else {
                        format!("{short} {line_count}L\n{php_map}")
                    };
                    let sent = count_tokens(&output);
                    let output = protocol::append_savings(&output, original_tokens, sent);
                    return (append_compressed_hint(&output, file_path), sent);
                }
            }

            let structured = match ext {
                "md" | "mdx" | "rst" => {
                    crate::core::structured_read::extract_markdown_outline(content)
                }
                "json" => crate::core::structured_read::extract_json_structure(content),
                "yaml" | "yml" => crate::core::structured_read::extract_yaml_structure(content),
                "toml" => crate::core::structured_read::extract_toml_structure(content),
                _ if file_path.to_lowercase().ends_with(".lock")
                    || file_path.to_lowercase().ends_with("go.sum") =>
                {
                    crate::core::structured_read::extract_lock_summary(content, file_path)
                }
                _ => String::new(),
            };

            if !structured.is_empty() {
                let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                    format!("{file_ref}={short} {line_count}L\n{structured}")
                } else {
                    format!("{short} {line_count}L\n{structured}")
                };
                let sent = count_tokens(&output);
                output = protocol::append_savings(&output, original_tokens, sent);
                return (append_compressed_hint(&output, file_path), sent);
            }

            let sigs = signatures::extract_signatures(content, ext);
            let dep_info = deps::extract_deps(content, ext);

            let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short} {line_count}L")
            } else {
                format!("{short} {line_count}L")
            };

            if !dep_info.imports.is_empty() {
                output.push_str("\n  deps: ");
                output.push_str(&dep_info.imports.join(", "));
            }

            if !dep_info.exports.is_empty() {
                output.push_str("\n  exports: ");
                output.push_str(&dep_info.exports.join(", "));
            }

            let key_sigs: Vec<&signatures::Signature> = sigs
                .iter()
                .filter(|s| s.is_exported || s.indent == 0)
                .collect();

            if !key_sigs.is_empty() {
                output.push_str("\n  API:");
                for sig in &key_sigs {
                    output.push_str("\n    ");
                    if crp_mode.is_tdd() {
                        output.push_str(&sig.to_tdd());
                    } else {
                        output.push_str(&sig.to_compact());
                    }
                }
            }

            if let Some(body) = task_relevant_body(content, file_path, ext, task) {
                output.push('\n');
                output.push_str(&body);
            }

            let sent = count_tokens(&output);
            (
                append_compressed_hint(
                    &protocol::append_savings(&output, original_tokens, sent),
                    file_path,
                ),
                sent,
            )
        }
        "aggressive" => {
            #[cfg(feature = "tree-sitter")]
            let ast_pruned = crate::core::signatures_ts::ast_prune(content, ext);
            #[cfg(not(feature = "tree-sitter"))]
            let ast_pruned: Option<String> = None;

            let base = ast_pruned.as_deref().unwrap_or(content);

            let session_intent = crate::core::session::SessionState::load_latest()
                .and_then(|s| s.active_structured_intent);
            let raw = if let Some(ref intent) = session_intent {
                compressor::task_aware_compress(base, Some(ext), intent)
            } else {
                compressor::aggressive_compress(base, Some(ext))
            };
            let compressed = compressor::safeguard_ratio(content, &raw);
            let header = build_header(file_ref, short, ext, content, line_count, true);

            let mut sym = SymbolMap::new();
            let idents = symbol_map::extract_identifiers(&compressed, ext);
            for ident in &idents {
                sym.register(ident);
            }

            if symbol_map::substitution_enabled() && sym.len() >= 3 {
                let sym_table = sym.format_table();
                let sym_applied = sym.apply(&compressed);
                let orig_tok = count_tokens(&compressed);
                let comp_tok = count_tokens(&sym_applied) + count_tokens(&sym_table);
                let net = orig_tok.saturating_sub(comp_tok);
                if orig_tok > 0 && net * 100 / orig_tok >= 5 {
                    let savings = protocol::format_savings(original_tokens, comp_tok);
                    return (
                        append_compressed_hint(
                            &format!("{header}\n{sym_applied}{sym_table}\n{savings}"),
                            file_path,
                        ),
                        comp_tok,
                    );
                }
                let savings = protocol::format_savings(original_tokens, orig_tok);
                return (
                    append_compressed_hint(
                        &format!("{header}\n{compressed}\n{savings}"),
                        file_path,
                    ),
                    orig_tok,
                );
            }

            let sent = count_tokens(&compressed);
            let savings = protocol::format_savings(original_tokens, sent);
            (
                append_compressed_hint(&format!("{header}\n{compressed}\n{savings}"), file_path),
                sent,
            )
        }
        "entropy" => {
            let result = entropy::entropy_compress_adaptive(content, file_path);
            let avg_h = entropy::analyze_entropy(content).avg_entropy;
            let header = build_header(file_ref, short, ext, content, line_count, false);
            let techs = result.techniques.join(", ");
            let output = format!("{header} H̄={avg_h:.1} [{techs}]\n{}", result.output);
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            let compression_ratio = if original_tokens > 0 {
                1.0 - (sent as f64 / original_tokens as f64)
            } else {
                0.0
            };
            crate::core::adaptive_thresholds::report_bandit_outcome(compression_ratio > 0.15);
            (
                append_compressed_hint(&format!("{output}\n{savings}"), file_path),
                sent,
            )
        }
        "task" => {
            let task_str = task.unwrap_or("");
            if task_str.is_empty() {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                let out = format!("{header}\n{content}\n[task mode: no task set — returned full]");
                let sent = count_tokens(&out);
                return (out, sent);
            }
            let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task_str);
            if keywords.is_empty() {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                let out = format!(
                    "{header}\n{content}\n[task mode: no keywords extracted — returned full]"
                );
                let sent = count_tokens(&out);
                return (out, sent);
            }
            let filtered =
                crate::core::task_relevance::information_bottleneck_filter(content, &keywords, 0.3);
            let filtered_lines = filtered.lines().count();
            let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short} {line_count}L [task-filtered: {line_count}→{filtered_lines}]")
            } else {
                format!("{short} {line_count}L [task-filtered: {line_count}→{filtered_lines}]")
            };
            let graph_ctx = if crate::core::profiles::active_profile()
                .output_hints
                .graph_context_block()
            {
                let project_root = detect_project_root(file_path);
                crate::core::graph_context::build_graph_context(
                    file_path,
                    &project_root,
                    Some(crate::core::graph_context::GraphContextOptions::default()),
                )
                .map(|c| crate::core::graph_context::format_graph_context(&c))
                .unwrap_or_default()
            } else {
                String::new()
            };

            let sent = count_tokens(&filtered) + count_tokens(&header) + count_tokens(&graph_ctx);
            let savings = protocol::format_savings(original_tokens, sent);
            (
                append_compressed_hint(
                    &format!("{header}\n{filtered}{graph_ctx}\n{savings}"),
                    file_path,
                ),
                sent,
            )
        }
        "reference" => {
            let tok = count_tokens(content);
            let output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short}: {line_count} lines, {tok} tok ({ext})")
            } else {
                format!("{short}: {line_count} lines, {tok} tok ({ext})")
            };
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            (format!("{output}\n{savings}"), sent)
        }
        mode if mode.starts_with("lines:") => {
            let range_str = &mode[6..];
            let extracted = extract_line_range(content, range_str);
            let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short} {line_count}L lines:{range_str}")
            } else {
                format!("{short} {line_count}L lines:{range_str}")
            };
            let sent = count_tokens(&extracted);
            let savings = protocol::format_savings(original_tokens, sent);
            (format!("{header}\n{extracted}\n{savings}"), sent)
        }
        unknown => {
            let header = build_header(file_ref, short, ext, content, line_count, true);
            let out = format!(
                "[WARNING: unknown mode '{unknown}', falling back to full]\n{header}\n{content}"
            );
            let sent = count_tokens(&out);
            (out, sent)
        }
    }
}

/// When a task is active, find the symbol whose name best matches a task
/// keyword and return its body as numbered source lines (capped).
///
/// `map`/`signatures` stay compact but include the one symbol body the agent is
/// most likely about to read, avoiding a follow-up full read. Uses the
/// tree-sitter chunk extractor (which carries spans + body across languages); a
/// no-op when tree-sitter is unavailable.
fn task_relevant_body(
    content: &str,
    file_path: &str,
    ext: &str,
    task: Option<&str>,
) -> Option<String> {
    const MAX_BODY_LINES: usize = 80;

    let task = task.map(str::trim).filter(|t| !t.is_empty())?;
    let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task);
    if keywords.is_empty() {
        return None;
    }
    let kw_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    let chunks = crate::core::chunks_ts::extract_chunks_ts(file_path, content, ext)?;

    // Score: exact name match (2) beats substring overlap (1).
    let mut best_idx: Option<usize> = None;
    let mut best_score = 0u8;
    for (i, ch) in chunks.iter().enumerate() {
        if ch.symbol_name.is_empty() {
            continue;
        }
        let name_l = ch.symbol_name.to_lowercase();
        let substr = kw_lower
            .iter()
            .any(|k| k.len() >= 3 && (name_l.contains(k.as_str()) || k.contains(name_l.as_str())));
        let score = if kw_lower.contains(&name_l) {
            2
        } else {
            u8::from(substr)
        };
        if score > best_score {
            best_score = score;
            best_idx = Some(i);
        }
    }

    let ch = &chunks[best_idx?];
    let body_lines: Vec<&str> = ch.content.lines().collect();
    let total = body_lines.len();
    let shown = total.min(MAX_BODY_LINES);
    let body: String = body_lines[..shown]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>4}|{l}", ch.start_line + i))
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = if shown < total {
        format!(
            "\n  … +{} lines — ctx_read(mode=\"lines:{}-{}\")",
            total - shown,
            ch.start_line + shown,
            ch.end_line
        )
    } else {
        String::new()
    };
    Some(format!(
        "  ▸ body {} L{}-{}:\n{body}{truncated}",
        ch.symbol_name, ch.start_line, ch.end_line
    ))
}

fn extract_line_range(content: &str, range_str: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let mut selected = Vec::new();

    for part in range_str.split(',') {
        let part = part.trim();
        if let Some((start_s, end_s)) = part.split_once('-') {
            let start = start_s.trim().parse::<usize>().unwrap_or(1).max(1);
            let end = end_s.trim().parse::<usize>().unwrap_or(total).min(total);
            for i in start..=end {
                if i >= 1 && i <= total {
                    selected.push(format!("{i:>4}| {}", lines[i - 1]));
                }
            }
        } else if let Ok(n) = part.parse::<usize>() {
            if n >= 1 && n <= total {
                selected.push(format!("{n:>4}| {}", lines[n - 1]));
            }
        }
    }

    if selected.is_empty() {
        "No lines matched the range.".to_string()
    } else {
        selected.join("\n")
    }
}

fn handle_diff(cache: &mut SessionCache, path: &str, file_ref: &str) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("diff");
    let short = protocol::shorten_path(path);
    let old_content = cache
        .get(path)
        .and_then(crate::core::cache::CacheEntry::content);

    let new_content = match read_file_lossy(path) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("ERROR: {e}");
            let tokens = count_tokens(&msg);
            return (msg, tokens);
        }
    };

    let original_tokens = count_tokens(&new_content);

    let diff_output = if let Some(old) = &old_content {
        compressor::diff_content(old, &new_content)
    } else {
        // No previous version cached — store content for future diffs but
        // return a short guidance message instead of dumping the full file.
        cache.store(path, &new_content);
        let msg = format!(
            "{file_ref}={short} [no cached version for diff — use mode=full first, then diff on re-read]"
        );
        let sent = count_tokens(&msg);
        return (msg, sent);
    };

    cache.store(path, &new_content);

    let sent = count_tokens(&diff_output);
    let savings = protocol::format_savings(original_tokens, sent);
    let head = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short}")
    } else {
        short.clone()
    };
    (format!("{head} [diff]\n{diff_output}\n{savings}"), sent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_header_toon_format_no_brackets() {
        let _lock = crate::core::data_dir::test_env_lock();
        std::env::set_var("LEAN_CTX_META", "1");
        let content = "use std::io;\nfn main() {}\n";
        let header = build_header("F1", "main.rs", "rs", content, 2, false);
        assert!(!header.contains('['));
        assert!(!header.contains(']'));
        assert!(header.contains("F1=main.rs 2L"));
        std::env::remove_var("LEAN_CTX_META");
    }

    #[test]
    fn test_header_toon_deps_indented() {
        let _lock = crate::core::data_dir::test_env_lock();
        std::env::set_var("LEAN_CTX_META", "1");
        let content = "use crate::core::cache;\nuse crate::tools;\npub fn main() {}\n";
        let header = build_header("F1", "main.rs", "rs", content, 3, true);
        if header.contains("deps") {
            assert!(
                header.contains("\n deps "),
                "deps should use indented TOON format"
            );
            assert!(
                !header.contains("deps:["),
                "deps should not use bracket format"
            );
        }
        std::env::remove_var("LEAN_CTX_META");
    }

    #[test]
    fn test_header_toon_saves_tokens() {
        let _lock = crate::core::data_dir::test_env_lock();
        std::env::set_var("LEAN_CTX_META", "1");
        let content = "use crate::foo;\nuse crate::bar;\npub fn baz() {}\npub fn qux() {}\n";
        let old_header = "F1=main.rs [4L +] deps:[foo,bar] exports:[baz,qux]".to_string();
        let new_header = build_header("F1", "main.rs", "rs", content, 4, true);
        let old_tokens = count_tokens(&old_header);
        let new_tokens = count_tokens(&new_header);
        assert!(
            new_tokens <= old_tokens,
            "TOON header ({new_tokens} tok) should be <= old format ({old_tokens} tok)"
        );
        std::env::remove_var("LEAN_CTX_META");
    }

    #[test]
    fn test_tdd_symbols_are_compact() {
        let symbols = [
            "⊕", "⊖", "∆", "→", "⇒", "✓", "✗", "⚠", "λ", "§", "∂", "τ", "ε",
        ];
        for sym in &symbols {
            let tok = count_tokens(sym);
            assert!(tok <= 2, "Symbol {sym} should be 1-2 tokens, got {tok}");
        }
    }

    #[test]
    fn test_task_mode_filters_content() {
        let content = (0..200)
            .map(|i| {
                if i % 20 == 0 {
                    format!("fn validate_token(token: &str) -> bool {{ /* line {i} */ }}")
                } else {
                    format!("fn unrelated_helper_{i}(x: i32) -> i32 {{ x + {i} }}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let full_tokens = count_tokens(&content);
        let task = Some("fix bug in validate_token");
        let (result, result_tokens) = process_mode(
            &content,
            "task",
            "F1",
            "test.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "test.rs",
            task,
        );
        assert!(
            result_tokens < full_tokens,
            "task mode ({result_tokens} tok) should be less than full ({full_tokens} tok)"
        );
        assert!(
            result.contains("task-filtered"),
            "output should contain task-filtered marker"
        );
    }

    #[test]
    fn test_task_mode_without_task_returns_full() {
        let content = "fn main() {}\nfn helper() {}\n";
        let tokens = count_tokens(content);
        let (result, _sent) = process_mode(
            content,
            "task",
            "F1",
            "test.rs",
            "rs",
            tokens,
            CrpMode::Off,
            "test.rs",
            None,
        );
        assert!(
            result.contains("no task set"),
            "should indicate no task: {result}"
        );
    }

    #[test]
    fn test_reference_mode_one_line() {
        let content = "fn main() {}\nfn helper() {}\nfn other() {}\n";
        let tokens = count_tokens(content);
        let (result, _sent) = process_mode(
            content,
            "reference",
            "F1",
            "test.rs",
            "rs",
            tokens,
            CrpMode::Off,
            "test.rs",
            None,
        );
        let lines: Vec<&str> = result.lines().collect();
        assert!(
            lines.len() <= 3,
            "reference mode should be very compact, got {} lines",
            lines.len()
        );
        assert!(result.contains("lines"), "should contain line count");
        assert!(result.contains("tok"), "should contain token count");
    }

    #[test]
    fn map_mode_includes_signature_line_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        let p = path.to_string_lossy().to_string();
        std::fs::write(
            &path,
            "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n",
        )
        .unwrap();

        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &p, "map", CrpMode::Off);

        assert!(
            result.contains("API:"),
            "map output should include API: {result}"
        );
        assert!(
            result.contains("cl ⊛ Config @L1"),
            "struct signature should include line suffix: {result}"
        );
        assert!(
            result.contains("fn ⊛ build() → Config @L3"),
            "function signature should include line suffix: {result}"
        );
    }

    #[test]
    fn cached_lines_mode_invalidates_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let p = path.to_string_lossy().to_string();

        std::fs::write(&path, "one\nsecond\n").unwrap();
        let mut cache = SessionCache::new();

        let r1 = handle_with_task_resolved(&mut cache, &p, "lines:1-1", CrpMode::Off, None);
        let l1: Vec<&str> = r1.content.lines().collect();
        let got1 = l1.get(1).copied().unwrap_or_default().trim();
        let got1 = got1.split_once('|').map_or(got1, |(_, s)| s.trim());
        assert_eq!(got1, "one");

        std::thread::sleep(Duration::from_secs(1));
        std::fs::write(&path, "two\nsecond\n").unwrap();

        let r2 = handle_with_task_resolved(&mut cache, &p, "lines:1-1", CrpMode::Off, None);
        let l2: Vec<&str> = r2.content.lines().collect();
        let got2 = l2.get(1).copied().unwrap_or_default().trim();
        let got2 = got2.split_once('|').map_or(got2, |(_, s)| s.trim());
        assert_eq!(got2, "two");
    }

    #[test]
    #[cfg_attr(tarpaulin, ignore)]
    fn benchmark_task_conditioned_compression() {
        // Keep this reasonably small so CI coverage instrumentation stays fast.
        let content = generate_benchmark_code(200);
        let full_tokens = count_tokens(&content);
        let task = Some("fix authentication in validate_token");

        let (_full_output, full_tok) = process_mode(
            &content,
            "full",
            "F1",
            "server.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "server.rs",
            task,
        );
        let (_task_output, task_tok) = process_mode(
            &content,
            "task",
            "F1",
            "server.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "server.rs",
            task,
        );
        let (_sig_output, sig_tok) = process_mode(
            &content,
            "signatures",
            "F1",
            "server.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "server.rs",
            task,
        );
        let (_ref_output, ref_tok) = process_mode(
            &content,
            "reference",
            "F1",
            "server.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "server.rs",
            task,
        );

        eprintln!("\n=== Task-Conditioned Compression Benchmark ===");
        eprintln!("Source: 200-line Rust file, task='fix authentication in validate_token'");
        eprintln!("  full:       {full_tok:>6} tokens (baseline)");
        eprintln!(
            "  task:       {task_tok:>6} tokens ({:.0}% savings)",
            (1.0 - task_tok as f64 / full_tok as f64) * 100.0
        );
        eprintln!(
            "  signatures: {sig_tok:>6} tokens ({:.0}% savings)",
            (1.0 - sig_tok as f64 / full_tok as f64) * 100.0
        );
        eprintln!(
            "  reference:  {ref_tok:>6} tokens ({:.0}% savings)",
            (1.0 - ref_tok as f64 / full_tok as f64) * 100.0
        );
        eprintln!("================================================\n");

        assert!(task_tok < full_tok, "task mode should save tokens");
        assert!(sig_tok < full_tok, "signatures should save tokens");
        assert!(ref_tok < sig_tok, "reference should be most compact");
    }

    fn generate_benchmark_code(lines: usize) -> String {
        let mut code = Vec::with_capacity(lines);
        code.push("use std::collections::HashMap;".to_string());
        code.push("use crate::core::auth;".to_string());
        code.push(String::new());
        code.push("pub struct Server {".to_string());
        code.push("    config: Config,".to_string());
        code.push("    cache: HashMap<String, String>,".to_string());
        code.push("}".to_string());
        code.push(String::new());
        code.push("impl Server {".to_string());
        code.push(
            "    pub fn validate_token(&self, token: &str) -> Result<Claims, AuthError> {"
                .to_string(),
        );
        code.push("        let decoded = auth::decode_jwt(token)?;".to_string());
        code.push("        if decoded.exp < chrono::Utc::now().timestamp() {".to_string());
        code.push("            return Err(AuthError::Expired);".to_string());
        code.push("        }".to_string());
        code.push("        Ok(decoded.claims)".to_string());
        code.push("    }".to_string());
        code.push(String::new());

        let remaining = lines.saturating_sub(code.len());
        for i in 0..remaining {
            if i % 30 == 0 {
                code.push(format!(
                    "    pub fn handler_{i}(&self, req: Request) -> Response {{"
                ));
            } else if i % 30 == 29 {
                code.push("    }".to_string());
            } else {
                code.push(format!("        let val_{i} = self.cache.get(\"key_{i}\").unwrap_or(&\"default\".to_string());"));
            }
        }
        code.push("}".to_string());
        code.join("\n")
    }

    #[test]
    fn map_mode_inlines_task_relevant_body() {
        let content = "pub fn alpha() {\n    let a = 1;\n}\n\npub fn validate_token(t: &str) -> bool {\n    let ok = check(t);\n    ok\n}\n";
        let tokens = count_tokens(content);
        let (with_task, _) = process_mode(
            content,
            "map",
            "F1",
            "test.rs",
            "rs",
            tokens,
            CrpMode::Off,
            "test.rs",
            Some("fix bug in validate_token"),
        );
        assert!(
            with_task.contains("▸ body") && with_task.contains("validate_token"),
            "map with task should inline the matching body: {with_task}"
        );
        let (no_task, _) = process_mode(
            content,
            "map",
            "F1",
            "test.rs",
            "rs",
            tokens,
            CrpMode::Off,
            "test.rs",
            None,
        );
        assert!(
            !no_task.contains("▸ body"),
            "map without a task must not inline a body: {no_task}"
        );
    }

    #[test]
    fn compressed_cache_key_distinguishes_task() {
        let no_task = compressed_cache_key("map", CrpMode::Off, None);
        let tdd_no_task = compressed_cache_key("map", CrpMode::Tdd, None);
        let with_task = compressed_cache_key("map", CrpMode::Off, Some("fix login"));
        let other_task = compressed_cache_key("map", CrpMode::Off, Some("refactor db"));
        assert_eq!(no_task, "map:v2");
        assert_eq!(tdd_no_task, "map:v2:tdd");
        assert_ne!(with_task, no_task);
        assert_ne!(with_task, other_task);
    }

    #[test]
    fn instruction_file_detection() {
        assert!(is_instruction_file(
            "/home/user/.pi/agent/skills/committing-changes/SKILL.md"
        ));
        assert!(is_instruction_file("/workspace/.cursor/rules/lean-ctx.mdc"));
        assert!(is_instruction_file("/project/AGENTS.md"));
        assert!(is_instruction_file("/project/.cursorrules"));
        assert!(is_instruction_file("/home/user/.claude/rules/my-rule.md"));
        assert!(is_instruction_file("/skills/some-skill/README.md"));

        assert!(!is_instruction_file("/project/src/main.rs"));
        assert!(!is_instruction_file("/project/config.json"));
        assert!(!is_instruction_file("/project/data/report.csv"));
    }

    #[test]
    fn resolve_auto_mode_returns_full_for_instruction_files() {
        let mode = resolve_auto_mode(
            "/home/user/.pi/agent/skills/committing-changes/SKILL.md",
            5000,
            Some("read"),
        );
        assert_eq!(mode, "full", "SKILL.md must always be read in full");

        let mode = resolve_auto_mode("/workspace/AGENTS.md", 3000, Some("read"));
        assert_eq!(mode, "full", "AGENTS.md must always be read in full");

        let mode = resolve_auto_mode("/workspace/.cursorrules", 2000, None);
        assert_eq!(mode, "full", ".cursorrules must always be read in full");
    }
}
