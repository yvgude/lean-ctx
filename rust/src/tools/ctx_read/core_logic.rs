use super::{
    CrpMode, Path, ReadOutput, ReadTuning, SessionCache, compressed_cache_key, compressor,
    count_tokens, find_similar_and_update_semantic_index, format_full_compact_output,
    format_full_output, is_cacheable_mode, mode_allows_raw_cap, process_mode_tuned, protocol,
    read_file_lossy, try_disk_anchored_window, try_stub_hit_readonly,
};

pub(super) fn handle_with_options_inner(
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
                is_cache_hit: false,
            };
        }
        cache.invalidate(path);
    }

    // #811: a fresh, explicitly windowed `anchored:N-M` read never needs the
    // cache (fresh always bypasses it) or the whole file in memory — try the
    // disk-streaming short-circuit first.
    if let Some(out) =
        try_disk_anchored_window(path, mode, fresh, preread.is_none(), &file_ref, &short)
    {
        return out;
    }

    if mode == "diff" {
        let (out, _) = handle_diff(cache, path, &file_ref);
        let out = crate::core::redaction::redact_text_if_enabled(&out);
        let sent = count_tokens(&out);
        return ReadOutput {
            content: out,
            resolved_mode: "diff".into(),
            output_tokens: sent,
            is_cache_hit: false,
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
            tuning.auto_density_mode().unwrap_or_else(|| {
                resolve_auto_mode(Some(cache), path, original_tokens, None, task)
            })
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
                            is_cache_hit: false,
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
                    is_cache_hit: false,
                };
            }
            let (out, _) = handle_full_with_auto_delta(cache, path, &file_ref, &short, ext, task);
            let out = crate::core::redaction::redact_text_if_enabled(&out);
            let sent = count_tokens(&out);
            return ReadOutput {
                content: out,
                resolved_mode: "full".into(),
                output_tokens: sent,
                is_cache_hit: false,
            };
        }

        let compressed_hit = if is_cacheable_mode(&resolved_mode) {
            let cache_key = compressed_cache_key(
                &resolved_mode,
                crp_mode,
                task,
                tuning.aggressiveness,
                tuning.protect,
            );
            // #1287: a same-conversation re-read of the SAME variant of an
            // unchanged file collapses to the ~15-token variant stub instead
            // of re-emitting the identical payload. Staleness was verified
            // above (a stale entry is invalidated before this point), and the
            // conversation gate mirrors the full-content stub — stubs must
            // never leak across chats (#1042). Fresh reads bypass the cache
            // entirely, so `fresh=true` remains the escape hatch.
            if super::dispatch::stub_policy_allows()
                && let crate::core::cache::VariantDelivery::Conversation(delivered) =
                    cache.compressed_delivered_conversation(path, &cache_key)
                && crate::core::conversation::conversation_allows_stub(
                    crate::core::conversation::current_conversation_id_fresh().as_deref(),
                    Some(&delivered),
                )
            {
                crate::core::telemetry::global_metrics().record_cache(true);
                crate::core::auto_mode_resolver::count_source("compressed_cache_stub");
                let stub =
                    super::dispatch::render_unchanged_variant_stub(&file_ref, path, &resolved_mode);
                crate::core::stats::record_reread(
                    original_tokens.saturating_sub(stub.output_tokens),
                );
                return stub;
            }
            let hit = cache.get_compressed(path, &cache_key).cloned();
            if let Some(cached_output) = &hit {
                // get_compressed() already recorded the cache hit (stats + event)
                crate::core::auto_mode_resolver::count_source("compressed_cache_hit");
                let out = crate::core::redaction::redact_text_if_enabled(cached_output);
                let sent = count_tokens(&out);
                crate::core::stats::record_reread(original_tokens.saturating_sub(sent));
                return ReadOutput {
                    content: out,
                    resolved_mode,
                    output_tokens: sent,
                    is_cache_hit: true,
                };
            }
            hit
        } else {
            None
        };

        if compressed_hit.is_none() && is_cacheable_mode(&resolved_mode) {
            crate::core::auto_mode_resolver::count_source("compressed_cache_miss");
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
                cap_to_raw(
                    out,
                    framed_tokens,
                    &content,
                    original_tokens,
                    &resolved_mode,
                )
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
                is_cache_hit: false,
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
                    is_cache_hit: false,
                };
            }
        }
    };

    let store_result = cache.store(path, &content);
    crate::core::telemetry::global_metrics().record_cache(store_result.was_hit);

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
                is_cache_hit: false,
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
        // Verbatim `full` — the cap only strips framing, never a summary, so no
        // no-compression banner is warranted here.
        let output = cap_to_raw(
            output,
            framed_tokens,
            &content,
            store_result.original_tokens,
            "full",
        );
        let output = crate::core::redaction::redact_text_if_enabled(&output);
        let sent = count_tokens(&output);
        return ReadOutput {
            content: output,
            resolved_mode: "full".into(),
            output_tokens: sent,
            is_cache_hit: false,
        };
    }

    let resolved_mode = if mode == "auto" {
        tuning.auto_density_mode().unwrap_or_else(|| {
            resolve_auto_mode(
                None,
                path,
                store_result.original_tokens,
                Some(store_result.line_count),
                task,
            )
        })
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
            &resolved_mode,
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
        is_cache_hit: false,
    }
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
/// #361 anti-inflation cap: when framing a whole-file view costs more tokens
/// than the file itself, return the bare file instead.
///
/// `requested_mode` names the view the caller asked for. If that view was a
/// *compressed* one, the bare file is prefixed with a one-line banner: the
/// caller ordered a summary and is getting the whole file, and without the
/// banner that failure is invisible — indistinguishable from a summary that
/// happened to need every line, at full-file cost. Verbatim views (`full`,
/// `anchored`, …) are returned byte-identical, so callers that demand exact
/// bytes (compress-protected paths, #1150) are never given a prefix.
pub(crate) fn cap_to_raw(
    framed: String,
    framed_tokens: usize,
    raw_content: &str,
    raw_tokens: usize,
    requested_mode: &str,
) -> String {
    if raw_tokens > 0 && framed_tokens > raw_tokens {
        let prevented = (framed_tokens - raw_tokens) as u64;
        crate::core::cache_telemetry::record_raw_cap(prevented);
        // #1587: a caller who asked for a *summary* and silently receives the
        // whole file pays full-file tokens believing they compressed. Say so —
        // but only above the banner threshold, so the cap keeps its #361
        // guarantee (a read never costs more than the raw file) on the small
        // files where framing alone was the inflation.
        let compressed_request = requested_mode
            .parse::<crate::tools::ctx_read::ReadMode>()
            .is_ok_and(|m| m.counts_as_compressed());
        if compressed_request
            && let Some(banner) =
                crate::tools::ctx_read::render::no_compression_banner(requested_mode, raw_tokens)
        {
            return format!("{banner}\n{raw_content}");
        }
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
pub(crate) fn resolve_auto_mode(
    cache: Option<&SessionCache>,
    file_path: &str,
    original_tokens: usize,
    line_count: Option<usize>,
    task: Option<&str>,
) -> String {
    let ctx = crate::core::auto_mode_resolver::AutoModeContext {
        path: file_path,
        token_count: original_tokens,
        line_count,
        task,
        cache,
    };
    crate::core::auto_mode_resolver::resolve(&ctx).mode
}

const AUTO_DELTA_THRESHOLD: f64 = 0.6;

/// Re-reads from disk; if content changed and delta is compact, sends auto-delta.
pub(super) fn handle_full_with_auto_delta(
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
        crate::core::telemetry::global_metrics().record_cache(true);
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
    crate::core::telemetry::global_metrics().record_cache(store_result.was_hit);

    if store_result.was_hit {
        // #1128: no stub here. Whether an unchanged file may collapse to
        // `[unchanged …]` is decided once, by `try_stub_hit_readonly`, which the
        // caller already consulted before routing here — and only that gate knows
        // whether THIS conversation received the content (#954/#955). A second
        // decision built from `StoreResult` cannot: `full_content_delivered` is
        // carried over from the cache entry, so it answers "some conversation got
        // this", which is the question the gate exists to stop trusting.
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

/// Render the delta between the cached baseline of `path` and its current disk
/// content, refreshing the baseline. Used by both read paths: the CLI/in-process
/// path (`handle_with_options_inner`) and the MCP tool handler — `diff` is a
/// delta view with no whole-file renderer, so it must never reach
/// `process_mode_tuned` (which would report it as an unknown mode and fall back
/// to dumping the entire file, #1584).
pub(crate) fn handle_diff(cache: &mut SessionCache, path: &str, file_ref: &str) -> (String, usize) {
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
        let store_result = cache.store(path, &new_content);
        crate::core::telemetry::global_metrics().record_cache(store_result.was_hit);
        let msg = format!(
            "{file_ref}={short} [no cached version for diff — use mode=full first, then diff on re-read]"
        );
        let sent = count_tokens(&msg);
        return (msg, sent);
    };

    let store_result = cache.store(path, &new_content);
    crate::core::telemetry::global_metrics().record_cache(store_result.was_hit);

    let sent = count_tokens(&diff_output);
    let savings = protocol::format_savings(original_tokens, sent);
    let head = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short}")
    } else {
        short
    };
    (format!("{head} [diff]\n{diff_output}\n{savings}"), sent)
}

#[cfg(test)]
mod tests {
    use super::handle_full_with_auto_delta;
    use crate::core::cache::SessionCache;
    use std::sync::atomic::Ordering;

    #[test]
    fn fresh_full_read_records_central_cache_miss() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("telemetry-miss.rs");
        std::fs::write(&file, "fn telemetry_miss() {}\n").unwrap();
        let path = file.to_string_lossy();
        let mut cache = SessionCache::new();
        let metrics = crate::core::telemetry::global_metrics();
        let before = metrics.cache_misses.load(Ordering::Relaxed);

        let (output, _) = handle_full_with_auto_delta(&mut cache, &path, "F1", &path, "rs", None);
        let after = metrics.cache_misses.load(Ordering::Relaxed);

        assert!(output.contains("telemetry_miss"));
        assert!(
            after > before,
            "fresh cache store must increment central miss telemetry"
        );
    }

    #[test]
    fn count_source_is_accessible() {
        crate::core::auto_mode_resolver::count_source("test_compressed_cache_hit");
        let counts = crate::core::auto_mode_resolver::source_counts();
        assert!(
            counts
                .iter()
                .any(|(key, _)| *key == "test_compressed_cache_hit")
        );
    }

    #[test]
    fn compressed_cache_hit_fires_on_second_map_read() {
        use super::*;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cachehit.rs");
        std::fs::write(
            &file,
            "pub struct Foo {\n    bar: u32,\n    baz: String,\n}\nimpl Foo {\n    pub fn new() -> Self { Self { bar: 0, baz: String::new() } }\n}\n",
        )
        .unwrap();
        let path = file.to_string_lossy();
        let mut cache = SessionCache::new();
        let tuning = ReadTuning::default();

        let r1 = handle_with_options_inner(
            &mut cache,
            &path,
            "map",
            false,
            CrpMode::Off,
            Some("test task"),
            tuning,
            None,
        );
        assert!(!r1.is_cache_hit, "first map read must be a miss");
        assert_eq!(r1.resolved_mode, "map");

        let r2 = handle_with_options_inner(
            &mut cache,
            &path,
            "map",
            false,
            CrpMode::Off,
            Some("test task"),
            tuning,
            None,
        );
        assert!(r2.is_cache_hit, "second map read must hit compressed cache");
        // #1287: with a resolvable conversation scope the hit collapses to the
        // tiny variant stub; without one it re-serves the byte-identical
        // payload. Both are cache hits — assert the matching invariant.
        if r2.content.contains("unchanged map view") {
            assert!(r2.content.contains("fresh=true"), "stub carries the escape");
            assert!(
                r2.output_tokens < r1.output_tokens,
                "stub must be smaller than the payload"
            );
        } else {
            assert_eq!(
                r2.content, r1.content,
                "cached output must be byte-identical"
            );
        }

        let counts = crate::core::auto_mode_resolver::source_counts();
        assert!(
            counts
                .iter()
                .any(|(k, _)| *k == "compressed_cache_hit" || *k == "compressed_cache_stub"),
            "compressed cache counter must fire; got: {counts:?}"
        );
    }
}
