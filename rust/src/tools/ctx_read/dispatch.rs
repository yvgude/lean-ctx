use super::{
    CrpMode, HookPoint, PluginManager, ReadMode, ReadOutput, ReadTuning, SessionCache,
    count_tokens, dedup_hook, handle_with_options_inner, kernel, protocol,
};

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
    let mut result = handle_with_options(cache, path, mode, false, crp_mode, task);
    kernel::enrich_with_kernel(&mut result, task);
    result
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
pub(crate) fn force_fresh_env() -> bool {
    static FORCE_FRESH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCE_FRESH.get_or_init(|| {
        std::env::var("LEAN_CTX_FORCE_FRESH").is_ok_and(|v| v == "1" || v == "true")
    })
}

/// Detects a subagent (forked agent) execution context.
///
/// A subagent must never be served a stub for content only the parent received.
/// That used to be enforced by force-freshing *every* subagent read; with
/// conversation scoping (#954/#955) the subagent instead runs under its own scope
/// (`conversation::current_conversation_id` → `task:{id}` or `proc:{id}`), so
/// the stub gate withholds cross-agent stubs precisely while restoring the
/// subagent's *own* cheap re-reads. The blanket force-fresh is therefore kept
/// only as the fallback when scoping is disabled (#956).
///
/// Checks `CURSOR_TASK_ID` (Cursor) and `CLAUDE_CODE_ENTRYPOINT=local-agent`
/// (future Claude Code subagent marker). Current Claude Code is handled by
/// per-process scoping in [`crate::core::conversation`] (#1292).
pub(crate) fn is_subagent_context() -> bool {
    static IS_SUBAGENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_SUBAGENT.get_or_init(|| {
        std::env::var("CURSOR_TASK_ID").is_ok_and(|v| !v.is_empty())
            || std::env::var("CLAUDE_CODE_ENTRYPOINT")
                .ok()
                .as_deref()
                .map(str::trim)
                == Some("local-agent")
    })
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
    // #1292: Sub-agents have separate context windows and never received
    // the parent's reads. Always force fresh regardless of scope state —
    // correctness over cache savings for short-lived sub-agent contexts.
    let effective_fresh = fresh || force_fresh_env() || is_subagent_context();

    if !effective_fresh && let Some(stub) = try_cross_agent_stub(path, mode) {
        return stub;
    }

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
        // #841: a partial/filtered read means the model's most recent view is NOT
        // the full content. Clear the delivery flag so a subsequent mode="full"
        // re-delivers real content instead of the [unchanged] stub. Without this,
        // a task→full sequence returns an empty stub because the flag was set by an
        // earlier full delivery and never cleared by the intervening non-full read.
        if !matches!(result.resolved_mode.as_str(), "full" | "full-compact") {
            entry.full_content_delivered = false;
        }
    }

    if !result.is_cache_hit {
        record_cross_agent_delivery(path, result.output_tokens);
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

    // R28: Kernel content dedup — detect re-reads of unchanged content.
    if let Some(stub) = dedup_hook::maybe_dedup(path, &result.content, mode) {
        let stub_tokens = count_tokens(&stub);
        if stub_tokens < result.output_tokens {
            result.content = stub;
            result.output_tokens = stub_tokens;
            result.is_cache_hit = true;
        }
    }

    // R30: Feed bounce-tracker signal into adaptive compression bridge.
    crate::core::context_kernel::adaptive_hook::update_from_bounce_tracker();
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
pub(crate) fn try_stub_hit_readonly_scoped(
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
        crate::core::telemetry::global_metrics().record_cache(true);
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
        is_cache_hit: true,
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

fn file_blake3_prefix(path: &str) -> Option<([u8; 12], u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let bytes = std::fs::read(path).ok()?;
    let hash = blake3::hash(&bytes);
    let full = hash.as_bytes();
    let mut prefix = [0u8; 12];
    prefix.copy_from_slice(&full[..12]);
    Some((prefix, mtime))
}

fn try_cross_agent_stub(path: &str, mode: &str) -> Option<ReadOutput> {
    if !crate::core::config::Config::load().ocla.delivery_enabled() {
        return None;
    }
    if matches!(mode, "full" | "raw" | "diff") {
        return None;
    }
    let (hash, mtime) = file_blake3_prefix(path)?;
    let current_agent = std::env::var("CURSOR_TASK_ID")
        .or_else(|_| std::env::var("CLAUDECODE"))
        .unwrap_or_else(|_| format!("proc:{}", std::process::id()));
    let current_conversation = current_agent.clone();
    let reg = crate::core::ocla::OclaRegistry::global();
    let record = reg.delivery_registry.check_delivery(
        &hash,
        mtime,
        Some(path),
        Some(&current_agent),
        Some(&current_conversation),
    )?;

    let short = protocol::shorten_path(path);
    let stub = format!(
        "{short} [cross-agent · {lines}L · read by {agent} · use fresh=true to force]",
        lines = record.line_count,
        agent = record.agent_id,
    );
    let tokens = count_tokens(&stub);
    reg.delivery_registry
        .record_stub_served(&record, tokens as u64);
    Some(ReadOutput {
        content: stub,
        resolved_mode: "cross-agent-stub".into(),
        output_tokens: tokens,
        is_cache_hit: true,
    })
}

fn record_cross_agent_delivery(path: &str, tokens: usize) {
    if !crate::core::config::Config::load().ocla.delivery_enabled() {
        return;
    }
    let Some((hash, mtime)) = file_blake3_prefix(path) else {
        return;
    };
    let line_count = std::fs::read_to_string(path).map_or(0, |c| c.lines().count() as u32);
    let agent_id = std::env::var("CURSOR_TASK_ID")
        .or_else(|_| std::env::var("CLAUDECODE"))
        .unwrap_or_else(|_| format!("proc:{}", std::process::id()));
    let conversation_id = agent_id.clone();
    let entry = crate::core::ocla::types::DeliveryEntry {
        blake3: hash,
        path: path.into(),
        line_count,
        token_count: tokens as u64,
        agent_id,
        conversation_id,
        mtime,
    };
    let reg = crate::core::ocla::OclaRegistry::global();
    reg.delivery_registry.record_delivery(entry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn cross_agent_stub_miss_returns_none() {
        let stub = try_cross_agent_stub("/nonexistent/file.rs", "auto");
        assert!(stub.is_none());
    }

    #[test]
    fn warm_stub_hit_records_central_telemetry() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("telemetry-hit.rs");
        std::fs::write(&file, "fn telemetry_hit() {}\n").unwrap();
        let path = file.to_string_lossy();
        let mut cache = SessionCache::new();
        cache.store(&path, "fn telemetry_hit() {}\n");
        cache.mark_full_delivered(&path);

        let metrics = crate::core::telemetry::global_metrics();
        let before = metrics.cache_hits.load(Ordering::Relaxed);
        let output = try_stub_hit_readonly_scoped(&cache, &path, None);
        let after = metrics.cache_hits.load(Ordering::Relaxed);

        assert!(output.is_some(), "warm re-read must use the stub cache");
        assert!(
            after > before,
            "stub cache hit must increment central telemetry"
        );
    }
}
