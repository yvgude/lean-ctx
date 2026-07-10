use std::path::Path;

use crate::core::cache::SessionCache;
use crate::core::compressor;
use crate::core::deps;
use crate::core::entropy;
use crate::core::plugins::{PluginManager, executor::HookPoint};
use crate::core::protocol;
use crate::core::signatures;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;
// `pub(crate)`: the conformance suite renders modes directly for its
// accuracy invariants (GL#441).
pub(crate) mod render;
pub(crate) use render::*;
/// Type-safe read-mode vocabulary (#528): single source of truth for which
/// modes exist and how each is classified.
pub(crate) mod mode;
pub(crate) use mode::ReadMode;
#[cfg(test)]
mod tests;

/// Pre-counted read output carrying the output string, resolved mode,
/// and token count computed during mode processing.
pub struct ReadOutput {
    pub content: String,
    pub resolved_mode: String,
    /// Approximate output token count from mode processing.
    /// The dispatch layer recounts the final assembled string for accurate savings.
    pub output_tokens: usize,
}

/// SSOT via [`ReadMode`] (#528): the `map`/`signatures` summaries whose rendered
/// body is stored per-file in `compressed_outputs`. Unknown modes are not
/// cacheable, matching the prior `["map","signatures"].contains(mode)`.
fn is_cacheable_mode(mode: &str) -> bool {
    mode.parse::<ReadMode>()
        .is_ok_and(|m| m.is_compressed_cacheable())
}

/// `#361` anti-inflation capping applies to whole-file views (`full` and the
/// lossy summaries `map`/`signatures`/`aggressive`/`entropy`/`task`/…), where the
/// raw file is a strict superset of the information and is therefore never a
/// worse answer when the framing happens to inflate on a small file. `full` is
/// included: an `auto` read can resolve to `full` and reach this path, and its
/// header must not push the cost above raw. Selection and delta views have
/// view-specific semantics — `lines:` returns a window, `reference` a pointer,
/// `diff` a delta, `raw` the bytes — so replacing them with the whole file would
/// be wrong, not cheaper, and they are never capped.
fn mode_allows_raw_cap(mode: &str) -> bool {
    // SSOT via [`ReadMode`] (#528). Unknown modes keep the prior default of
    // `true` (only `lines:`/`reference`/`diff`/`raw` opt out of the #361 cap).
    mode.parse::<ReadMode>()
        .map_or(true, |m| m.allows_raw_cap())
}

fn compressed_cache_key(
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
    aggressiveness: Option<f64>,
    protect: &[String],
) -> String {
    // Bump when the rendered map/signatures body changes shape so stale
    // pre-line-range entries are not served from an older session cache.
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
    let keyed = match task.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            t.hash(&mut h);
            format!("{base}:t{:x}", h.finish())
        }
        None => base,
    };
    // Aggressiveness and the explicit protect list both change lossy output, so
    // both must change the key (#498). Empty fragments keep pre-feature keys
    // byte-identical, so unmodified reads still hit their existing cache entries.
    let mut key = keyed;
    let aggr_frag = crate::core::aggressiveness::cache_fragment(aggressiveness);
    if !aggr_frag.is_empty() {
        key = format!("{key}:{aggr_frag}");
    }
    let protect_frag = crate::core::protect::protect_fragment(protect);
    if !protect_frag.is_empty() {
        key = format!("{key}:{protect_frag}");
    }
    key
}

/// Appends the reactive recovery footer to a compressed view, leading with the
/// MCP-free "read the path directly" route. Tier (`off|minimal|full`) and wording
/// are resolved centrally in [`crate::core::recovery`] so `ctx_read`, the shell
/// tee and archive handles all speak the same grammar. Only lossy/compressed
/// modes reach this helper, so the footer is naturally absent from `full`/`raw`.
fn append_compressed_hint(output: &str, file_path: &str) -> String {
    match crate::core::recovery::read_footer(file_path) {
        Some(footer) => format!("{output}\n{footer}"),
        None => output.to_string(),
    }
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
                    .is_ok_and(|d| canonical.starts_with(d));
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
    let s = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    Ok(crate::core::io_boundary::strip_utf8_bom(s))
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
    if let (Some(parent), Some(filename)) = (p.parent(), p.file_name())
        && parent.exists()
    {
        let canonical_parent = crate::core::pathutil::safe_canonicalize_bounded(parent, 2000);
        let canonical_path = canonical_parent.join(filename);
        return std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&canonical_path);
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
    handle_with_options_resolved(
        cache,
        path,
        mode,
        false,
        crp_mode,
        task,
        ReadTuning::resolve(None, &[]),
    )
}

/// Like [`handle_with_task_resolved`] but with an explicit per-call
/// aggressiveness (the `ctx_read` `aggressiveness` arg, #714). `None` falls back
/// to the `LEAN_CTX_AGGRESSIVENESS` env var / config field.
pub fn handle_with_task_resolved_tuned(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
    aggressiveness: Option<f64>,
    protect: &[String],
) -> ReadOutput {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        false,
        crp_mode,
        task,
        ReadTuning::resolve(aggressiveness, protect),
    )
}

/// Like [`handle_with_task_resolved_tuned`] but accepts pre-read file content,
/// avoiding disk I/O under the cache write-lock (Two-Phase Read pattern, #1098).
#[allow(clippy::too_many_arguments)]
pub fn handle_with_preread(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
    aggressiveness: Option<f64>,
    protect: &[String],
    preread: String,
) -> ReadOutput {
    handle_with_options_resolved_preread(
        cache,
        path,
        mode,
        fresh,
        crp_mode,
        task,
        ReadTuning::resolve(aggressiveness, protect),
        Some(preread),
    )
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
    handle_with_options_resolved(
        cache,
        path,
        mode,
        true,
        crp_mode,
        task,
        ReadTuning::resolve(None, &[]),
    )
}

/// Fresh-read variant of [`handle_with_task_resolved_tuned`] (#714).
pub fn handle_fresh_with_task_resolved_tuned(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
    aggressiveness: Option<f64>,
    protect: &[String],
) -> ReadOutput {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        true,
        crp_mode,
        task,
        ReadTuning::resolve(aggressiveness, protect),
    )
}

fn handle_with_options(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        fresh,
        crp_mode,
        task,
        ReadTuning::resolve(None, &[]),
    )
    .content
}

/// `LEAN_CTX_FORCE_FRESH=1` — an explicit operator override that always forces a
/// cold full read, independent of conversation scoping.
fn force_fresh_env() -> bool {
    static FORCE_FRESH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCE_FRESH.get_or_init(|| {
        std::env::var("LEAN_CTX_FORCE_FRESH").is_ok_and(|v| v == "1" || v == "true")
    })
}

/// Detects a subagent (forked agent) execution context via `CURSOR_TASK_ID`.
///
/// A subagent must never be served a stub for content only the parent received.
/// That used to be enforced by force-freshing *every* subagent read; with
/// conversation scoping (#954/#955) the subagent instead runs under its own scope
/// (`conversation::current_conversation_id` → `task:{id}`), so the stub gate
/// withholds cross-agent stubs precisely while restoring the subagent's *own*
/// cheap re-reads. The blanket force-fresh is therefore kept only as the fallback
/// when scoping is disabled (#956).
fn is_subagent_context() -> bool {
    static IS_SUBAGENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_SUBAGENT.get_or_init(|| std::env::var("CURSOR_TASK_ID").is_ok_and(|v| !v.is_empty()))
}

fn handle_with_options_resolved(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
    tuning: ReadTuning<'_>,
) -> ReadOutput {
    handle_with_options_resolved_preread(cache, path, mode, fresh, crp_mode, task, tuning, None)
}

fn handle_with_options_resolved_preread(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
    tuning: ReadTuning<'_>,
    preread: Option<String>,
) -> ReadOutput {
    let effective_fresh = fresh
        || force_fresh_env()
        || (is_subagent_context() && !crate::core::conversation::scope_enabled());

    if PluginManager::has_listener("pre_read") {
        PluginManager::fire_hook_background(HookPoint::PreRead {
            path: path.to_string(),
        });
    }

    if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
        bt.next_seq();
    }
    let mut result = handle_with_options_inner(
        cache,
        path,
        mode,
        effective_fresh,
        crp_mode,
        task,
        tuning,
        preread,
    );

    if let Some(entry) = cache.get_mut(path) {
        entry.last_mode.clone_from(&result.resolved_mode);
    }

    // SSOT via [`ReadMode`] (#528): lossy summaries may elide shared blocks.
    let dedup_allowed = result
        .resolved_mode
        .parse::<ReadMode>()
        .is_ok_and(|m| m.is_lossy_summary());
    if dedup_allowed && let Some(deduped) = cache.apply_dedup(path, &result.content) {
        let new_tokens = count_tokens(&deduped);
        if new_tokens < result.output_tokens {
            result.content = deduped;
            result.output_tokens = new_tokens;
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

        // Quality signals (#538): compressed reads count as clean until a
        // bounce proves otherwise (the bounce signal outweighs 6:1); large
        // full reads of never-bouncing extensions are wasted compression
        // opportunities and push the learned threshold up.
        // SSOT via [`ReadMode`] (#528): only verbatim `full` and the `diff`
        // delta are uncompressed. A resolved window is always the canonical
        // `lines:N-M` (parses to `Lines` ⇒ compressed); the default of `true`
        // for the unreachable bare `"lines"` keeps prior behaviour everywhere a
        // real resolved mode can occur.
        let compressed = result
            .resolved_mode
            .parse::<ReadMode>()
            .map_or(true, |m| m.counts_as_compressed());
        if compressed {
            crate::core::adaptive_thresholds::record_quality_signal(
                path,
                crate::core::threshold_learning::QualitySignal::CleanCompressed,
            );
        } else if result.resolved_mode == "full"
            && result.output_tokens > 2000
            && bt.bounce_rate_for_extension(path).unwrap_or(0.0) < 0.05
        {
            crate::core::adaptive_thresholds::record_quality_signal(
                path,
                crate::core::threshold_learning::QualitySignal::WastedFull,
            );
        }
    }

    // Plugin seam: emit the realized compression stats. Same zero-cost guard.
    if PluginManager::has_listener("post_compress") {
        let original_tokens = cache.get(path).map_or(0, |e| e.original_tokens);
        PluginManager::fire_hook_background(HookPoint::PostCompress {
            path: path.to_string(),
            original_tokens,
            compressed_tokens: result.output_tokens,
        });
    }

    // Stigmergy (#540): deposit a Hot scent for this read in the background
    // (the field file lock may briefly block; never stall the read path). The
    // foreign-claim hint is intentionally NOT appended to the body: it carries a
    // relative timestamp ("claimed Nm ago"), which would make the output a
    // non-pure function of wall-clock time and defeat provider prompt caching
    // (#498). The deposit remains so the field still reflects active work.
    {
        let self_agent = crate::core::scent_field::scent_agent_id();
        let scent_path = crate::core::pathutil::normalize_tool_path(path);
        std::thread::spawn(move || {
            crate::core::scent_field::deposit(
                self_agent,
                crate::core::scent_field::ScentKind::Hot,
                &scent_path,
                0.3,
            );
        });
    }

    result
}

/// Attempt to serve a `mode="full"` cache hit (`[unchanged …]`) using only a
/// shared borrow of the cache.
///
/// Returns `None` when the file is not cached, was modified on disk, full
/// content was never delivered, or the cache policy forbids stubbing — in those
/// cases the caller must fall back to the write path.
///
/// This is the read-locked fast path: it needs no `&mut SessionCache`, so the
/// dominant "re-read an unchanged file" case proceeds under a shared lock and
/// parallel reads of distinct files no longer serialize on a global write lock.
pub fn try_stub_hit_readonly(cache: &SessionCache, path: &str) -> Option<ReadOutput> {
    // Resolve the caller *fresh* (TTL-bypassed): the stub gate's concurrency
    // detection must see a just-appeared second chat with zero lag, else a stub
    // could leak across chats in the pre-detection window (#1042).
    let current_conversation = crate::core::conversation::current_conversation_id_fresh();
    try_stub_hit_readonly_scoped(cache, path, current_conversation.as_deref())
}

/// Conversation-scoped core of [`try_stub_hit_readonly`]. The current
/// conversation id is injected (not read from the global resolver) so the
/// conversation gate can be tested deterministically without global state.
fn try_stub_hit_readonly_scoped(
    cache: &SessionCache,
    path: &str,
    current_conversation: Option<&str>,
) -> Option<ReadOutput> {
    let no_deg = crate::core::config::Config::load().no_degrade_effective();
    let prof = crate::core::profiles::active_profile();
    let force_full = no_deg
        || (prof.read.default_mode_effective() == "full"
            && prof.compression.crp_mode_effective() == "off");
    let policy_allows_stub =
        crate::server::compaction_sync::effective_cache_policy() != "safe" && !force_full;
    if !policy_allows_stub {
        return None;
    }

    // Warm path: a live in-memory entry is the freshest source of truth.
    if let Some(file_ref) = cache.get_file_ref_readonly(path) {
        let (cached_mtime, cached_hash, line_count, delivered_conv) = {
            let entry = cache.get(path)?;
            (
                entry.stored_mtime,
                entry.hash.clone(),
                entry.line_count,
                entry.delivered_conversation.clone(),
            )
        };
        if crate::core::cache::is_cache_entry_stale_verified(path, cached_mtime, &cached_hash)
            || !cache.is_full_delivered(path)
        {
            return None;
        }
        // Conversation scoping (#954): only stub when THIS conversation received
        // the content. A different (or unknown) conversation re-delivers in full
        // rather than emit a misleading stub. `current == None` (hooks absent)
        // preserves legacy process-scoped behavior, so single-chat hit rates are
        // unchanged.
        if !crate::core::conversation::conversation_allows_stub(
            current_conversation,
            delivered_conv.as_deref(),
        ) {
            crate::core::cache_telemetry::record_conversation_mismatch();
            return None;
        }
        cache.record_cache_hit(path);
        return Some(render_unchanged_stub(&file_ref, path, line_count));
    }

    // Cold fallback (#955): no live entry (e.g. after a daemon restart or idle
    // clear). Serve the stub from the persisted index iff the file is unchanged
    // AND the *same known* conversation is asking — a stricter gate than the warm
    // path, because a cold stub crosses a process boundary (no "no context →
    // legacy" escape; see `conversation_allows_cold_stub`).
    let rec = crate::core::read_stub_index::lookup(path)?;
    if crate::core::cache::is_cache_entry_stale_verified(path, rec.stored_mtime(), &rec.hash) {
        return None;
    }
    if !crate::core::conversation::conversation_allows_cold_stub(
        current_conversation,
        rec.delivered_conversation.as_deref(),
    ) {
        crate::core::cache_telemetry::record_conversation_mismatch();
        return None;
    }
    Some(render_unchanged_stub(&rec.file_ref, path, rec.line_count))
}

/// Renders the `[unchanged …]` stub body shared by the warm and cold stub paths.
///
/// #498 determinism: the stub is a pure function of (file_ref, path, line_count),
/// so identical re-reads stay byte-stable and provider prompt caching applies.
/// The `fresh=true` escape is a *static* suffix (no rotating proof lines or
/// read-count notes), so a re-reader in non-meta mode still sees how to force the
/// content (#513) without breaking byte-stability.
fn render_unchanged_stub(file_ref: &str, path: &str, line_count: usize) -> ReadOutput {
    let short = protocol::shorten_path(path);
    let out = if crate::core::protocol::meta_visible() {
        format!(
            "{file_ref}={short} [unchanged {line_count}L]\nUnchanged on disk. Use fresh=true to force re-read.",
        )
    } else {
        format!("{file_ref}={short} [unchanged {line_count}L · fresh=true to re-read]")
    };
    let out = crate::core::redaction::redact_text_if_enabled(&out);
    let sent = count_tokens(&out);
    ReadOutput {
        content: out,
        resolved_mode: "full".into(),
        output_tokens: sent,
    }
}

/// Outcome of [`resolve_explicit_delta_mode`]: the (possibly rewritten) read
/// mode plus an optional advisory note to surface to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaExplicitDecision {
    /// The mode the read should proceed with (rewritten only when the feature
    /// fires; otherwise the caller's mode, unchanged).
    pub mode: String,
    /// A byte-stable advisory appended to the read body when the mode was
    /// rewritten to `diff`. `None` when nothing was rewritten or the collapse
    /// was a silent `lines:`→`full` stub.
    pub note: Option<String>,
}

/// Decide whether an **explicit** `full`/`lines:N-M` re-read of a session-cached
/// file should be served as a delta instead of re-emitting content the model
/// already holds (the `delta_explicit` opt-in; env `LCTX_DELTA_EXPLICIT`).
///
/// Returns the mode the read should proceed with:
/// - **Changed on disk** (verified mtime+md5 stale) and full content is cached →
///   `diff`, plus an advisory note. The diff carries exactly the new
///   information in a fraction of the tokens.
/// - **Unchanged** and the request is `lines:` of an already-fully-delivered
///   file → `full`, so the read collapses to the ~15-token `[unchanged]` stub
///   instead of re-extracting a window the model has seen.
/// - Otherwise the caller's `mode` is returned untouched.
///
/// First reads (nothing cached) and `fresh=true` are never affected — the
/// caller gates those before calling. Staleness uses the **verified** variant
/// ([`crate::core::cache::is_cache_entry_stale_verified`]) so a same-second
/// write on a coarse-granularity filesystem cannot be mistaken for "unchanged"
/// and yield a misleading empty diff (#498 determinism).
///
/// Pure w.r.t. (cache, path, mode, enabled): no wall-clock, counters, or
/// randomness enter the result, so identical inputs stay byte-stable.
pub fn resolve_explicit_delta_mode(
    cache: &SessionCache,
    path: &str,
    mode: &str,
    explicit_mode: bool,
    fresh: bool,
    enabled: bool,
) -> DeltaExplicitDecision {
    let unchanged = DeltaExplicitDecision {
        mode: mode.to_string(),
        note: None,
    };
    if fresh
        || !enabled
        || !explicit_mode
        || !(mode == "full" || mode == "full-compact" || mode.starts_with("lines:"))
    {
        return unchanged;
    }
    let Some(entry) = cache.get(path) else {
        // First read this session — nothing to diff against.
        return unchanged;
    };
    let stale =
        crate::core::cache::is_cache_entry_stale_verified(path, entry.stored_mtime, &entry.hash);
    if stale {
        // Only divert to a diff when full content is actually cached: the diff
        // base is that full content (see `handle_diff`), never a compressed
        // view. Without it, `handle_diff` would have nothing to compare.
        if entry.content().is_some() {
            return DeltaExplicitDecision {
                mode: "diff".to_string(),
                note: Some(format!(
                    "[delta-explicit] requested mode={mode} served as a diff: the file \
                     changed since your last read and the diff is the new information. \
                     Pass fresh=true if you need the full content re-emitted."
                )),
            };
        }
        return unchanged;
    }
    // Unchanged on disk: a `lines:` window of a file already delivered in full
    // re-emits text the model holds — collapse to the full-mode stub
    // (~15 tokens). A plain `full` re-read already hits that stub downstream.
    if mode.starts_with("lines:") && cache.is_full_delivered(path) {
        return DeltaExplicitDecision {
            mode: "full".to_string(),
            note: None,
        };
    }
    unchanged
}

fn handle_with_options_inner(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
    tuning: ReadTuning<'_>,
    preread: Option<String>,
) -> ReadOutput {
    let file_ref = cache.get_file_ref(path);
    let short = protocol::shorten_path(path);
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    // #1150: a path the operator marked "never compress" is always returned in
    // full — exact bytes matter more than token savings for these files (golden
    // snapshots, byte-asserted fixtures, security-sensitive configs). Every lossy
    // mode (auto, aggressive, signatures, density, diff, …) collapses to the
    // verbatim full read; `raw` (already verbatim) and explicit `lines:` slices
    // are left as the user asked. The default config protects nothing, so this is
    // a fast no-op for everyone who hasn't opted in.
    let mode = if mode != "raw"
        && !mode.starts_with("lines:")
        && crate::core::config::Config::load()
            .proxy
            .is_path_compress_protected(path)
    {
        "full"
    } else {
        mode
    };

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

    if mode != "full"
        && let Some(existing) = cache.get(path)
    {
        let stale = crate::core::cache::is_cache_entry_stale_verified(
            path,
            existing.stored_mtime,
            &existing.hash,
        );
        if stale {
            cache.invalidate(path);
        }
    }

    // Snapshot the minimal immutable data the miss paths need, then drop the
    // borrow before any mutable operations (set_compressed, invalidate, store).
    let cache_snapshot = cache
        .get(path)
        .map(|existing| (existing.original_tokens, existing.content()));

    if let Some((original_tokens, content_opt)) = cache_snapshot {
        // Resolve the read mode first — and *cache-aware* for `auto`. Handing the
        // live cache to the resolver is what lets an `auto` re-read of an
        // unchanged, already-fully-delivered file short-circuit to
        // ("full", "cache_hit") and collapse to the cheap ~13-token `[unchanged]`
        // stub, exactly like an explicit `full` re-read. The previous call passed
        // no cache, so that branch was dead code and every `auto` re-read
        // re-delivered the whole file ("re-reads aren't cached"). Resolving
        // up-front also lets us hit the compressed-output cache BEFORE
        // decompressing the full body (avoids ~2-5ms zstd on hits). The
        // aggressiveness knob (#714) still routes `auto` through the density path.
        let resolved_mode = if mode == "auto" {
            tuning
                .auto_density_mode()
                .unwrap_or_else(|| resolve_auto_mode(Some(cache), path, original_tokens, task))
        } else {
            mode.to_string()
        };

        if resolved_mode == "full" || resolved_mode == "full-compact" {
            if let Some(out) = try_stub_hit_readonly(cache, path) {
                return out;
            }
            if resolved_mode == "full-compact" {
                let content = match read_file_lossy(path) {
                    Ok(c) => c,
                    Err(e) => {
                        let msg = format!("ERROR: {e}");
                        return ReadOutput {
                            content: msg,
                            resolved_mode: "error".into(),
                            output_tokens: 0,
                        };
                    }
                };
                let (out, _) = format_full_compact_output(&content);
                let out = crate::core::redaction::redact_text_if_enabled(&out);
                let sent = count_tokens(&out);
                return ReadOutput {
                    content: out,
                    resolved_mode: "full-compact".into(),
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

        if is_cacheable_mode(&resolved_mode) {
            let cache_key = compressed_cache_key(
                &resolved_mode,
                crp_mode,
                task,
                tuning.aggressiveness,
                tuning.protect,
            );
            let compressed_hit = cache.get_compressed(path, &cache_key).cloned();
            if let Some(cached_output) = compressed_hit {
                // get_compressed() already recorded the cache hit (stats + event)
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
            let (out, _) = process_mode_tuned(
                &content,
                &resolved_mode,
                &file_ref,
                &short,
                ext,
                original_tokens,
                crp_mode,
                path,
                task,
                tuning,
            );
            // #361 anti-inflation for lossy whole-file summaries (auto OR
            // explicit): map/signatures/… must never cost more than the raw file.
            // Selection/delta views keep their exact shape (see
            // mode_allows_raw_cap). Cap before caching so re-read hits serve the
            // same capped, byte-stable body.
            let out = if mode_allows_raw_cap(&resolved_mode) {
                let framed_tokens = count_tokens(&out);
                cap_to_raw(out, framed_tokens, &content, original_tokens)
            } else {
                out
            };
            if is_cacheable_mode(&resolved_mode) {
                let cache_key = compressed_cache_key(
                    &resolved_mode,
                    crp_mode,
                    task,
                    tuning.aggressiveness,
                    tuning.protect,
                );
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

    // Two-Phase Read (#1098): when pre-read content was provided (disk I/O
    // already happened outside the cache lock), use it directly. Otherwise
    // fall back to reading from disk (legacy path, still used by fast-path
    // inline calls where the write lock was immediately available).
    let content = if let Some(pr) = preread {
        pr
    } else {
        match read_file_lossy(path) {
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
    // #1098: graph hints moved to background — `graph_related_hint()` does a
    // SQLite query that can block for 50-200ms on Windows, which is unacceptable
    // while holding the global cache write-lock. The registered handler calls it
    // after releasing the lock and appends it to the response.
    let graph_hint: Option<String> = None;

    if mode == "full" || mode == "full-compact" {
        cache.mark_full_delivered(path);

        if mode == "full-compact" {
            let (output, _) = format_full_compact_output(&content);
            let output = crate::core::redaction::redact_text_if_enabled(&output);
            let sent = count_tokens(&output);
            return ReadOutput {
                content: output,
                resolved_mode: "full-compact".into(),
                output_tokens: sent,
            };
        }

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
        let framed_tokens = count_tokens(&output);
        let output = cap_to_raw(
            output,
            framed_tokens,
            &content,
            store_result.original_tokens,
        );
        let output = crate::core::redaction::redact_text_if_enabled(&output);
        let sent = count_tokens(&output);
        return ReadOutput {
            content: output,
            resolved_mode: "full".into(),
            output_tokens: sent,
        };
    }

    let resolved_mode = if mode == "auto" {
        tuning
            .auto_density_mode()
            .unwrap_or_else(|| resolve_auto_mode(None, path, store_result.original_tokens, task))
    } else {
        mode.to_string()
    };

    let (output, _sent) = process_mode_tuned(
        &content,
        &resolved_mode,
        &file_ref,
        &short,
        ext,
        store_result.original_tokens,
        crp_mode,
        path,
        task,
        tuning,
    );
    // #361 anti-inflation for lossy whole-file summaries (auto OR explicit);
    // selection/delta views keep their exact shape (see mode_allows_raw_cap).
    // Cap first, then cache the pure capped body so re-reads stay byte-stable
    // (#498) — the optional, read-state-dependent navigation hints below are
    // appended to the returned value only, never to the cached body.
    let mut output = if mode_allows_raw_cap(&resolved_mode) {
        let framed_tokens = count_tokens(&output);
        cap_to_raw(
            output,
            framed_tokens,
            &content,
            store_result.original_tokens,
        )
    } else {
        output
    };
    if is_cacheable_mode(&resolved_mode) {
        let cache_key = compressed_cache_key(
            &resolved_mode,
            crp_mode,
            task,
            tuning.aggressiveness,
            tuning.protect,
        );
        cache.set_compressed(path, &cache_key, output.clone());
    }
    if let Some(hint) = &graph_hint {
        output.push_str(&format!("\n{hint}"));
    }
    if let Some(hint) = similar_hint {
        output.push_str(&format!("\n{hint}"));
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

/// #361 anti-inflation invariant: a `ctx_read` must never cost more tokens than
/// reading the raw file would. Framing (file-ref header, deps/exports summary,
/// savings footer, navigation hints) only earns its keep on large files and
/// repeated reads — on a cold read of a small file it is pure overhead, the
/// exact inflation an independent benchmark measured (#361). When the framed
/// payload exceeds the bare content we ship the content verbatim, so a read is
/// break-even at worst and a win whenever a compressed mode or a cached re-read
/// applies. Re-reads are unaffected: the cache keys on path and re-derives the
/// file ref, so dropping the cold header here costs nothing on the next read.
///
/// `framed_tokens` and `raw_tokens` are both measured pre-redaction (redaction
/// is roughly token-neutral and applied to whichever string wins), so the
/// comparison is apples-to-apples with `original_tokens`. Empty files
/// (`raw_tokens == 0`) keep their framing so the reader still gets a signal.
fn cap_to_raw(
    framed: String,
    framed_tokens: usize,
    raw_content: &str,
    raw_tokens: usize,
) -> String {
    if raw_tokens > 0 && framed_tokens > raw_tokens {
        let prevented = (framed_tokens - raw_tokens) as u64;
        crate::core::cache_telemetry::record_raw_cap(prevented);
        raw_content.to_string()
    } else {
        framed
    }
}

/// Delegates to the unified `auto_mode_resolver::resolve()`.
/// Resolve `auto` to a concrete mode.
///
/// Pass `Some(cache)` on the warm read path: the resolver then short-circuits an
/// unchanged, already-fully-delivered file to `("full", "cache_hit")` so the
/// caller can collapse the re-read to the cheap `[unchanged]` stub instead of
/// re-delivering the whole body. Pass `None` only where no session cache exists
/// (the CLI cold path), which forces a stateless cold resolution.
fn resolve_auto_mode(
    cache: Option<&SessionCache>,
    file_path: &str,
    original_tokens: usize,
    task: Option<&str>,
) -> String {
    let ctx = crate::core::auto_mode_resolver::AutoModeContext {
        path: file_path,
        token_count: original_tokens,
        task,
        cache,
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

/// Build graph-related hints (callers/callees) — exported for the registered
/// handler to call in a background thread after releasing the cache lock (#1098).
pub fn graph_related_hint(path: &str) -> Option<String> {
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
            if !crate::core::protocol::meta_visible()
                && let Some(cached) = existing.content()
            {
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
            let out = format!(
                "[using cached version — file read failed]\n{file_ref}={short} cached {}t {}L",
                existing.read_count(),
                existing.line_count
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
                // #498 determinism: byte-stable cache-hit stub (see
                // try_stub_hit_readonly). The `fresh=true` escape is a static
                // suffix, so non-meta re-readers still see how to force content (#513).
                format!(
                    "{file_ref}={short} [unchanged {}L · fresh=true to re-read]",
                    store_result.line_count
                )
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
        short
    };
    (format!("{head} [diff]\n{diff_output}\n{savings}"), sent)
}
