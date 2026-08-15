use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rmcp::ErrorData;
use rmcp::model::{ContentBlock, Tool};
use serde_json::{Map, Value, json};

use crate::core::cache::ReuseOutcome;
use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_f64, get_int, get_str, get_str_array,
    require_resolved_path,
};
use crate::tool_defs::tool_def;

/// Per-file lock that serializes concurrent reads of the same path.
///
/// When multiple subagents read sequentially through a shared set of files,
/// they tend to hit the same path at the same time. Without per-file locking
/// they all contend on the global cache write lock while doing redundant I/O.
/// This lock ensures only one thread reads a given file from disk; the others
/// wait cheaply on the per-file mutex, then hit the warm cache.
///
/// Backed by the shared `core::path_locks` registry so reads and edits of the
/// same path coordinate through a single mutex (see issue #320).
fn per_file_lock(path: &str) -> Arc<Mutex<()>> {
    crate::core::path_locks::per_file_lock(path)
}

pub struct CtxReadTool;

impl McpTool for CtxReadTool {
    fn name(&self) -> &'static str {
        "ctx_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_read",
            "Read source files. mode recommended — choose by intent (see `mode` below); defaults to auto when omitted.\n\
             To UNDERSTAND code run ctx_compose FIRST; ctx_read after it identified files.\n\
             anchored → edit by reference via ctx_patch (no exact-recall).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Batch read" },
                    "mode": {
                        "type": "string",
                        "description": "Recommended (defaults to auto). full=verbatim(edit-ready) anchored=full+N:hh|anchors(edit via ctx_patch) raw=exact-bytes signatures=API map=structure auto=smart diff=git-delta lines:N-M=window (comma multi-selects: lines:5,10-20) reference=quotes task=focus"
                    },
                    "raw": { "type": "boolean", "description": "Verbatim (= mode=raw + fresh)" },
                    "start_line": { "type": "integer", "description": "1-based" },
                    "offset": { "type": "integer", "description": "start_line alias" },
                    "limit": { "type": "integer", "description": "Max lines" },
                    "fresh": { "type": "boolean", "description": "Bypass cache" },
                    "aggressiveness": { "type": "number", "description": "0.0–1.0 density (entropy/task)" },
                    "protect": { "type": "array", "items": { "type": "string" }, "description": "Symbols kept verbatim" }
                },
                "required": []
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        // #509: ctx_read absorbs multi-file batch reads (supersedes ctx_multi_read).
        // A non-empty `paths` array routes to the one shared batch implementation.
        if args
            .get("paths")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
        {
            let result = super::ctx_multi_read::batch_read(args, ctx);
            if let Ok(output) = &result {
                record_attribution_result(ctx, "ctx_read batch".to_string(), output);
            }
            return result;
        }

        let path = if let Some(repo) = get_str(args, "repo") {
            let root = crate::core::multi_repo::resolve_repo_root(&repo).ok_or_else(|| {
                let known = crate::core::multi_repo::known_aliases().join(", ");
                let known = if known.is_empty() {
                    "none registered — use ctx_multi_repo add_root".to_string()
                } else {
                    known
                };
                ErrorData::invalid_params(
                    format!("unknown repo alias: {repo} (known: {known})"),
                    None,
                )
            })?;
            let rel = get_str(args, "path").unwrap_or_else(|| ".".to_string());
            crate::core::path_resolve::resolve_tool_path(Some(&root), None, &rel)
                .map_err(|e| ErrorData::invalid_params(e, None))?
        } else {
            require_resolved_path(ctx, args, "path")?
        };

        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle_inner(args, ctx, &path)
        })) {
            Ok(result) => result,
            Err(_) => Err(ErrorData::internal_error(
                format!(
                    "ctx_read panicked while processing '{path}'. This is a bug — please report it."
                ),
                None,
            )),
        };
        if let Ok(output) = &result {
            record_attribution_result(ctx, format!("ctx_read {path}"), output);
        }
        result
    }
}

fn record_attribution_result(ctx: &ToolContext, source: String, output: &ToolOutput) {
    let session_id = crate::core::task_spine::TaskSpine::task_id()
        .or_else(|| {
            ctx.session
                .as_ref()
                .and_then(|session| session.try_read().ok().map(|state| state.id.clone()))
        })
        .unwrap_or_else(|| "mcp-session".to_string());
    let turn_provided = ctx
        .call_count
        .as_ref()
        .map_or(0, |count| count.load(Ordering::Relaxed) as u64);
    let token_cost = crate::core::tokens::count_tokens(&output.text);
    let chunk = crate::core::causal_attribution::ContextChunkRecord::new(
        &output.text,
        source,
        token_cost,
        turn_provided,
    );
    if let Err(error) = crate::core::causal_attribution::record_chunk(&session_id, chunk) {
        tracing::debug!(%error, "causal attribution ctx_read recording failed");
    }
}

impl CtxReadTool {
    #[allow(clippy::unused_self)]
    fn handle_inner(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
        path: &str,
    ) -> Result<ToolOutput, ErrorData> {
        let session_lock = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let cache_lock = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;

        let current_task = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            let mut attempt = 0u32;
            loop {
                // #1018: use try_read_owned instead of Handle::block_on to avoid
                // the async-runtime-saturation anti-pattern on Windows.
                if let Ok(guard) = session_lock.clone().try_read_owned() {
                    break guard.task.as_ref().map(|t| t.description.clone());
                }
                attempt += 1;
                if std::time::Instant::now() >= deadline {
                    tracing::warn!(
                        "session read-lock timeout after {attempt} attempts in ctx_read for {path}"
                    );
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        };
        let task_ref = current_task.as_deref();
        // #513: `raw=true` is the intuitive "give me the exact bytes" escape an
        // agent reaches for. Alias it to mode="raw" (verbatim, unframed) and
        // force a fresh disk read below so a re-read never collapses to an
        // `[unchanged]`/auto-delta stub. An explicit raw flag wins over `mode`.
        let arg_raw = get_bool(args, "raw").unwrap_or(false);
        let explicit_mode_arg = resolve_raw_alias(arg_raw, get_str(args, "mode"));
        let explicit_mode = explicit_mode_arg.is_some();
        // #1209: a malformed line-range payload (e.g. `lines:44:48` with a colon
        // instead of a dash) must fail loudly, not silently return an empty
        // window on small files or bounce to a full-file dump on large ones.
        // `lines:` and `anchored:` share `parse_line_range`, so validating both
        // line-range modes at the boundary catches the typo for either. Other
        // modes are left untouched: bare `density:` is a valid fallback and
        // unknown keywords keep the historical warn-and-fall-back behaviour.
        if let Some(ref requested) = explicit_mode_arg
            && (requested.starts_with("lines:") || requested.starts_with("anchored:"))
            && let Err(e) = requested.parse::<crate::tools::ctx_read::ReadMode>()
        {
            return Err(ErrorData::invalid_params(e.user_message(), None));
        }
        let configured_mode = (!explicit_mode)
            .then(crate::core::auto_mode_resolver::configured_default_mode)
            .flatten();
        let learned_mode = if !explicit_mode && configured_mode.is_none() {
            if let Ok(cache) = cache_lock.try_read() {
                Some(crate::tools::ctx_smart_read::select_mode_with_task(
                    &cache, path, task_ref,
                ))
            } else {
                tracing::debug!(
                    "cache lock contested during auto-mode selection for {path}; \
                     falling back to full"
                );
                None
            }
        } else {
            None
        };
        let mut mode = crate::core::auto_mode_resolver::resolve_mode_precedence(
            explicit_mode_arg,
            configured_mode,
            learned_mode,
            "full",
        );
        let mut fresh = get_bool(args, "fresh").unwrap_or(false);
        // #513: a raw/verbatim request always reads from disk — the whole point
        // is exact current bytes, never a cached stub or delta.
        if arg_raw {
            fresh = true;
        }
        let cache_policy = crate::server::compaction_sync::effective_cache_policy();
        let cache_policy_bypassed = cache_policy == "off";
        if cache_policy_bypassed {
            fresh = true;
        }
        let aggressiveness =
            crate::core::aggressiveness::effective(get_f64(args, "aggressiveness"));
        let protect = get_str_array(args, "protect").unwrap_or_default();
        // One-knob UX: when the caller sets aggressiveness without pinning a mode,
        // route through the proven density path at the mapped target. An explicit
        // mode (incl. entropy/task) instead has the knob tune it via ReadTuning.
        if !explicit_mode && let Some(a) = aggressiveness {
            // SSOT mode construction via the typed `ReadMode` (#528): the typed
            // `Density` Display emits the same `density:0.NN` the pipeline parses.
            mode = crate::tools::ctx_read::ReadMode::Density(
                crate::core::aggressiveness::AggressivenessProfile::from_level(a).density_target,
            )
            .to_string();
        }
        // `start_line` (and its `offset`/`limit` aliases) can pin a line window.
        // The resolution lives in `apply_line_window`/`resolve_line_window` so
        // the runtime path and the unit tests share one implementation and can
        // never drift (GitHub #432 aliases, #259 explicit-mode, #253 line-1).
        apply_line_window(
            &mut mode,
            &mut fresh,
            explicit_mode,
            get_int(args, "start_line"),
            get_int(args, "offset"),
            get_int(args, "limit"),
        );

        let pressure_action = ctx.pressure_snapshot.as_ref().map(|p| &p.recommendation);
        let resolved_agent_id = ctx.agent_id.as_ref().and_then(|a| match a.try_read() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        });
        let gate_result = crate::server::context_gate::pre_dispatch_read_for_agent(
            path,
            &mode,
            task_ref,
            Some(&ctx.project_root),
            pressure_action,
            resolved_agent_id.as_deref(),
        );
        if gate_result.budget_blocked {
            let msg = gate_result
                .budget_warning
                .unwrap_or_else(|| "Agent token budget exceeded".to_string());
            return Err(ErrorData::invalid_params(msg, None));
        }
        let budget_warning = gate_result.budget_warning.clone();
        // #513: an explicit raw/verbatim request is never silently downgraded by
        // the budget gate — the caller asked for exact bytes.
        let mut mode_override_note: Option<String> = None;
        if mode != "raw"
            && let Some(overridden) = gate_result.overridden_mode
        {
            if explicit_mode {
                let reason = gate_result.reason.unwrap_or("context-gate");
                mode_override_note = Some(format!(
                    "[mode overridden: {mode} -> {overridden}, reason={reason}]"
                ));
            }
            mode = overridden;
        }

        let (instruction_mode, instruction_mode_note) = resolve_instruction_file_mode(path, &mode);
        let (mut mode, degrade_warning) =
            if instruction_mode_note.is_some() || instruction_mode != mode {
                (instruction_mode, None)
            } else if mode == "raw" || mode.starts_with("anchored") || mode.starts_with("lines:") {
                // #513: raw bypasses context-pressure degradation (which would
                // otherwise downgrade to signatures under Block), exactly like
                // instruction files — explicit lossless modes mean lossless.
                (mode, None)
            } else {
                auto_degrade_read_mode(&mode)
            };

        // Delta-aware explicit re-reads (opt-in: config `delta_explicit`, env
        // LCTX_DELTA_EXPLICIT). Re-requesting full/lines:N-M content for a file
        // this session already read re-emits content the model already holds;
        // when the file changed on disk, a diff carries the same information in
        // a fraction of the tokens, and an unchanged lines: request of a
        // fully-delivered file collapses to the full-mode stub. The decision is
        // a pure function of (cache, path, mode) — see
        // `ctx_read::resolve_explicit_delta_mode`. First reads are unaffected;
        // fresh=true always bypasses. Runs BEFORE the lines:→fresh guard below
        // so a changed-file lines: re-read can still be diverted to a diff.
        let mut delta_explicit_note: Option<String> = None;
        if !fresh
            && explicit_mode
            && (mode == "full" || mode == "full-compact" || mode.starts_with("lines:"))
            && crate::core::config::Config::load().delta_explicit_effective()
            && let Ok(cache) = cache_lock.try_read()
        {
            let decision = crate::tools::ctx_read::resolve_explicit_delta_mode(
                &cache,
                path,
                &mode,
                explicit_mode,
                fresh,
                true,
            );
            mode = decision.mode;
            delta_explicit_note = decision.note;
        }

        if mode.starts_with("lines:") {
            fresh = true;
        }

        if crate::core::binary_detect::is_llm_viewable_image(path) {
            return read_image_file(path);
        }
        if crate::core::binary_detect::is_binary_file(path) {
            let msg = crate::core::binary_detect::binary_file_message(path);
            return Err(ErrorData::invalid_params(msg, None));
        }
        {
            let cap = crate::core::limits::max_read_bytes() as u64;
            if let Ok(meta) = std::fs::metadata(path)
                && meta.len() > cap
            {
                let msg = format!(
                    "File too large ({} bytes, limit {} bytes via LCTX_MAX_READ_BYTES). \
                     Use mode=\"lines:1-100\" or start_line+limit for partial reads, \
                     mode=\"anchored\" with start_line+limit for edit-ready windows, \
                     or increase the limit.",
                    meta.len(),
                    cap
                );
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

        // Compaction-aware: if host compacted since last check, reset delivery flags
        // so post-compaction reads deliver full content instead of stubs.
        if !fresh
            && let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir()
            && let Ok(mut cache) = cache_lock.try_write()
        {
            crate::server::compaction_sync::sync_if_compacted(&mut cache, &data_dir);
        }

        // Fast path: if both per-file lock and cache write-lock are immediately
        // available, execute inline without spawning a thread. This avoids thread +
        // channel overhead for the ~90% of calls that are cache hits.
        let read_timeout = std::time::Duration::from_secs(30);
        let cancelled = Arc::new(AtomicBool::new(false));
        // Hash once for cross-agent delivery (avoids re-reading on record).
        let delivery_metadata = crate::core::config::Config::load()
            .ocla
            .delivery_enabled()
            .then(|| crate::tools::ctx_read::file_blake3_prefix(path))
            .flatten();
        let (output, resolved_mode, original, is_cache_hit, file_ref, cache_stats, reuse_outcome) = {
            let crp_mode = ctx.crp_mode;
            let fast_result = 'fast: {
                let file_lock = per_file_lock(path);
                let Some(_file_guard) = file_lock.try_lock().ok() else {
                    break 'fast None;
                };

                // Phase 1 (shared lock): the dominant case is re-reading an
                // unchanged file. Serve the `[unchanged]` stub under a *read* lock
                // so parallel reads of distinct files run concurrently instead of
                // serializing on the global write lock. `auto` is included because
                // a warm `auto` re-read of a fully-delivered file resolves to a
                // full cache-hit; `try_stub_hit_readonly` self-guards (returns None
                // unless full content was delivered and the file is unchanged), so a
                // first or compressed-only `auto` read still falls through to
                // Phase 2. When aggressiveness is set `mode` was already rewritten
                // to `density:` upstream, so it never reaches this `auto` branch.
                if !fresh
                    && (mode == "full" || mode == "full-compact" || mode == "auto")
                    && let Ok(cache) = cache_lock.try_read()
                    && let Some(read_output) =
                        crate::tools::ctx_read::try_stub_hit_readonly(&cache, path)
                {
                    let hit = read_output.is_cache_hit;
                    let content = read_output.content;
                    let rmode = read_output.resolved_mode;
                    let orig = cache.get(path).map_or(0, |e| e.original_tokens);
                    let fref = cache.file_ref_map().get(path).cloned();
                    let stats = cache.get_stats();
                    let stats_snapshot = (stats.total_reads(), stats.cache_hits());
                    break 'fast Some((
                        content,
                        rmode,
                        orig,
                        hit,
                        fref,
                        stats_snapshot,
                        ReuseOutcome::UnchangedStub,
                    ));
                }

                // Misses use the slow path's double-check sequence: it does a
                // second cache check, computes without the global lock, then
                // briefly takes a write lock to store the result.
                None
            };

            if let Some(result) = fast_result {
                result
            } else {
                let cache_lock = cache_lock.clone();
                let mode = mode.clone();
                let task_owned = current_task.clone();
                let protect_owned = protect.clone();
                let path_owned = path.to_string();
                let cancel_flag = cancelled.clone();
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                std::thread::spawn(move || {
                    let file_lock = per_file_lock(&path_owned);

                    let _file_guard = {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(25);
                        loop {
                            if cancel_flag.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Ok(guard) = file_lock.try_lock() {
                                break guard;
                            }
                            if std::time::Instant::now() >= deadline {
                                tracing::error!(
                                    "ctx_read: per-file lock timeout after 25s for {path_owned}"
                                );
                                let _ = tx.send((
                                    format!("per-file lock contention for {path_owned} — retry in a moment"),
                                    "error".to_string(), 0, false, None, (0, 0), ReuseOutcome::Cold,
                                ));
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    };

                    if cancel_flag.load(Ordering::Relaxed) {
                        return;
                    }

                    // ── Two-Phase Read (#1098) ──────────────────────────
                    //
                    // Phase 1 (read lock): try the [unchanged] stub — this is the
                    // ~70% case (repeated reads of unchanged files). Previously
                    // missing in the slow path, forcing every slow-path call into
                    // the expensive write-lock branch.
                    if !fresh
                        && (mode == "full" || mode == "full-compact" || mode == "auto")
                        && let Ok(cache) = cache_lock.try_read()
                        && let Some(read_output) =
                            crate::tools::ctx_read::try_stub_hit_readonly(&cache, &path_owned)
                    {
                        let content = read_output.content;
                        let rmode = read_output.resolved_mode;
                        let orig = cache.get(&path_owned).map_or(0, |e| e.original_tokens);
                        let hit = true;
                        let fref = cache.file_ref_map().get(path_owned.as_str()).cloned();
                        let stats = cache.get_stats();
                        let stats_snapshot = (stats.total_reads(), stats.cache_hits());
                        let _ = tx.send((
                            content,
                            rmode,
                            orig,
                            hit,
                            fref,
                            stats_snapshot,
                            ReuseOutcome::UnchangedStub,
                        ));
                        return;
                    }

                    // The session-local stub is checked first so the current
                    // agent's own delivery always wins. On a miss, a verified
                    // cross-agent delivery can avoid the disk read below.
                    if !crate::tools::ctx_read::effective_fresh_for_delivery(fresh)
                        && let Some((hash, mtime)) = delivery_metadata
                        && let Some(read_output) = crate::tools::ctx_read::try_cross_agent_stub(
                            &path_owned,
                            &mode,
                            hash,
                            mtime,
                        )
                    {
                        let _ = tx.send((
                            read_output.content,
                            read_output.resolved_mode,
                            0,
                            read_output.is_cache_hit,
                            None,
                            (0, 0),
                            ReuseOutcome::CrossFileRef,
                        ));
                        return;
                    }

                    // Phase 2a: disk I/O under per-file lock but WITHOUT cache lock.
                    let preread = match crate::tools::ctx_read::read_file_lossy(&path_owned) {
                        Ok(c) => Some(c),
                        Err(e) => {
                            tracing::warn!("ctx_read: cannot read {path_owned}: {e}");
                            None
                        }
                    };

                    if cancel_flag.load(Ordering::Relaxed) {
                        return;
                    }

                    // ── Phase 2b: Three-sub-phase read (#807) ──────────
                    //
                    // Previously held the global cache write-lock for the
                    // entire computation (tree-sitter, entropy compression).
                    // For large files this caused 30+ second lock holds and
                    // cascading timeouts for all concurrent tool calls.
                    //
                    // New: prepare (brief lock) → compute (no lock) → store (brief lock).

                    let task_ref = task_owned.as_deref();
                    let tuning =
                        crate::tools::ctx_read::ReadTuning::resolve(aggressiveness, &protect_owned);

                    // Helper: acquire write lock with deadline.
                    macro_rules! acquire_write {
                        ($deadline_secs:expr, $label:expr) => {{
                            let deadline = std::time::Instant::now()
                                + std::time::Duration::from_secs($deadline_secs);
                            loop {
                                if cancel_flag.load(Ordering::Relaxed) {
                                    return;
                                }
                                if let Ok(guard) = cache_lock.try_write() {
                                    break guard;
                                }
                                if std::time::Instant::now() >= deadline {
                                    tracing::error!(
                                        "ctx_read: cache write-lock timeout ({}) for {path_owned}",
                                        $label,
                                    );
                                    let _ = tx.send((
                                        format!(
                                            "cache lock contention for {path_owned} — retry in a moment"
                                        ),
                                        "error".into(),
                                        0,
                                        false,
                                        None,
                                        (0, 0),
                                        ReuseOutcome::Cold,
                                    ));
                                    return;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                        }};
                    }

                    // 2b-i: Brief write lock — prepare cache state, resolve
                    // mode, check for hits. Sub-millisecond: HashMap lookups,
                    // staleness checks, raw-content storage for new files.
                    #[allow(clippy::large_enum_variant)]
                    enum PrepareOutcome {
                        Hit(
                            String,
                            String,
                            usize,
                            bool,
                            Option<String>,
                            (u64, u64),
                            ReuseOutcome,
                        ),
                        Compute {
                            file_ref: String,
                            resolved_mode: String,
                            content: String,
                            original_tokens: usize,
                            reuse_outcome: ReuseOutcome,
                        },
                    }

                    let outcome = {
                        let mut cache = acquire_write!(10, "prepare 10s");
                        let mut reuse_outcome = if cache_policy_bypassed {
                            ReuseOutcome::PolicyBypass
                        } else if fresh {
                            ReuseOutcome::FreshBypass
                        } else {
                            ReuseOutcome::Cold
                        };

                        if crate::core::plugins::PluginManager::has_listener("pre_read") {
                            crate::core::plugins::PluginManager::fire_hook_background(
                                crate::core::plugins::executor::HookPoint::PreRead {
                                    path: path_owned.clone(),
                                },
                            );
                        }
                        if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
                            bt.next_seq();
                        }

                        let file_ref = cache.get_file_ref(&path_owned);

                        let effective_fresh = fresh
                            || crate::tools::ctx_read::force_fresh_env()
                            || (crate::tools::ctx_read::is_subagent_context()
                                && !crate::core::conversation::scope_enabled());

                        let mode_eff = if mode != "raw"
                            && !mode.starts_with("lines:")
                            && crate::core::config::Config::load()
                                .proxy
                                .is_path_compress_protected(&path_owned)
                        {
                            "full".to_string()
                        } else {
                            mode.clone()
                        };

                        if effective_fresh {
                            cache.invalidate(&path_owned);
                        }

                        if !effective_fresh {
                            let stale = cache.get(&path_owned).is_some_and(|e| {
                                crate::core::cache::is_cache_entry_stale_verified(
                                    &path_owned,
                                    e.stored_mtime,
                                    &e.hash,
                                )
                            });
                            if stale {
                                cache.invalidate(&path_owned);
                                reuse_outcome = ReuseOutcome::Stale;
                            }
                        }

                        let snap = cache
                            .get(&path_owned)
                            .map(|e| (e.original_tokens, e.content()));

                        if let Some((orig_tok, content_opt)) = snap {
                            let resolved = if mode_eff == "auto" {
                                tuning.auto_density_mode().unwrap_or_else(|| {
                                    crate::tools::ctx_read::resolve_auto_mode(
                                        Some(&cache),
                                        &path_owned,
                                        orig_tok,
                                        None,
                                        task_ref,
                                    )
                                })
                            } else {
                                mode_eff
                            };

                            if (resolved == "full" || resolved == "full-compact")
                                && let Some(out) = crate::tools::ctx_read::try_stub_hit_readonly(
                                    &cache,
                                    &path_owned,
                                )
                            {
                                let orig = cache.get(&path_owned).map_or(0, |e| e.original_tokens);
                                let fref = cache.file_ref_map().get(path_owned.as_str()).cloned();
                                let s = cache.get_stats();
                                PrepareOutcome::Hit(
                                    out.content,
                                    out.resolved_mode,
                                    orig,
                                    true,
                                    fref,
                                    (s.total_reads(), s.cache_hits()),
                                    ReuseOutcome::UnchangedStub,
                                )
                            } else if crate::tools::ctx_read::is_cacheable_mode(&resolved) {
                                let ck = crate::tools::ctx_read::compressed_cache_key(
                                    &resolved,
                                    crp_mode,
                                    task_ref,
                                    tuning.aggressiveness,
                                    tuning.protect,
                                );
                                if let Some(hit) = cache.get_compressed(&path_owned, &ck).cloned() {
                                    crate::core::auto_mode_resolver::count_source(
                                        "compressed_cache_hit",
                                    );
                                    let hit = crate::core::redaction::redact_text_if_enabled(&hit);
                                    let orig =
                                        cache.get(&path_owned).map_or(0, |e| e.original_tokens);
                                    let fref =
                                        cache.file_ref_map().get(path_owned.as_str()).cloned();
                                    let s = cache.get_stats();
                                    PrepareOutcome::Hit(
                                        hit,
                                        resolved,
                                        orig,
                                        true,
                                        fref,
                                        (s.total_reads(), s.cache_hits()),
                                        ReuseOutcome::RenderCacheHit,
                                    )
                                } else {
                                    if reuse_outcome == ReuseOutcome::Cold
                                        && cache.was_compressed_variant_evicted(&path_owned, &ck)
                                    {
                                        reuse_outcome = ReuseOutcome::VariantEvicted;
                                    }
                                    let c = content_opt
                                        .or_else(|| preread.as_deref().map(String::from));
                                    PrepareOutcome::Compute {
                                        file_ref,
                                        resolved_mode: resolved,
                                        content: c.unwrap_or_default(),
                                        original_tokens: orig_tok,
                                        reuse_outcome,
                                    }
                                }
                            } else {
                                let c =
                                    content_opt.or_else(|| preread.as_deref().map(String::from));
                                PrepareOutcome::Compute {
                                    file_ref,
                                    resolved_mode: resolved,
                                    content: c.unwrap_or_default(),
                                    original_tokens: orig_tok,
                                    reuse_outcome,
                                }
                            }
                        } else {
                            let raw = match preread {
                                Some(c) if !c.is_empty() => c,
                                _ => match crate::tools::ctx_read::read_file_lossy(&path_owned) {
                                    Ok(c) if !c.is_empty() => c,
                                    Ok(_) => {
                                        tracing::debug!(
                                            "ctx_read: skipping cache for empty content: {path_owned}"
                                        );
                                        let _ = tx.send((
                                            format!("File is empty: {path_owned}"),
                                            "error".into(),
                                            0,
                                            false,
                                            None,
                                            (0, 0),
                                            ReuseOutcome::Cold,
                                        ));
                                        return;
                                    }
                                    Err(e) => {
                                        tracing::debug!(
                                            "ctx_read: skipping cache for empty content: {path_owned}"
                                        );
                                        let _ = tx.send((
                                            format!("Cannot read file: {path_owned}: {e}"),
                                            "error".into(),
                                            0,
                                            false,
                                            None,
                                            (0, 0),
                                            ReuseOutcome::Cold,
                                        ));
                                        return;
                                    }
                                },
                            };
                            let sr = cache.store(&path_owned, &raw);
                            let resolved = if mode_eff == "auto" {
                                tuning.auto_density_mode().unwrap_or_else(|| {
                                    crate::tools::ctx_read::resolve_auto_mode(
                                        None,
                                        &path_owned,
                                        sr.original_tokens,
                                        Some(sr.line_count),
                                        task_ref,
                                    )
                                })
                            } else {
                                mode_eff
                            };
                            PrepareOutcome::Compute {
                                file_ref,
                                resolved_mode: resolved,
                                content: raw,
                                original_tokens: sr.original_tokens,
                                reuse_outcome,
                            }
                        }
                    }; // write lock released

                    if let PrepareOutcome::Hit(c, rm, orig, hit, fref, ss, reuse_outcome) = outcome
                    {
                        // Update last_mode for compressed-cache hits so the auto-mode
                        // resolver can reuse this mode on future re-reads (#E26).
                        let mut cache = acquire_write!(10, "hit last_mode 10s");
                        if let Some(entry) = cache.get_mut(&path_owned) {
                            entry.last_mode.clone_from(&rm);
                        }
                        let _ = tx.send((c, rm, orig, hit, fref, ss, reuse_outcome));
                        return;
                    }
                    let PrepareOutcome::Compute {
                        file_ref,
                        resolved_mode,
                        content: compute_content,
                        original_tokens,
                        reuse_outcome,
                    } = outcome
                    else {
                        unreachable!()
                    };

                    if cancel_flag.load(Ordering::Relaxed) {
                        return;
                    }

                    // 2b-ii: Heavy computation WITHOUT cache lock.
                    // Tree-sitter, entropy compression, mode rendering all
                    // run under the per-file mutex only (serializes same-file
                    // reads, but does not block other files or tool calls).
                    let short = crate::core::protocol::shorten_path(&path_owned);
                    let ext_s = std::path::Path::new(&*path_owned)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");

                    let (mut computed, rmode) = if resolved_mode == "full"
                        || resolved_mode == "full-compact"
                    {
                        if resolved_mode == "full-compact" {
                            let (out, _) = crate::tools::ctx_read::format_full_compact_output(
                                &compute_content,
                            );
                            (out, "full-compact".to_string())
                        } else {
                            let lc = compute_content.lines().count();
                            let (out, _) = crate::tools::ctx_read::format_full_output(
                                &file_ref,
                                &short,
                                ext_s,
                                &compute_content,
                                original_tokens,
                                lc,
                                task_ref,
                            );
                            let ft = crate::core::tokens::count_tokens(&out);
                            let out = crate::tools::ctx_read::cap_to_raw(
                                out,
                                ft,
                                &compute_content,
                                original_tokens,
                            );
                            (out, "full".to_string())
                        }
                    } else {
                        let (out, _) = crate::tools::ctx_read::process_mode_tuned(
                            &compute_content,
                            &resolved_mode,
                            &file_ref,
                            &short,
                            ext_s,
                            original_tokens,
                            crp_mode,
                            &path_owned,
                            task_ref,
                            tuning,
                        );
                        let out = if crate::tools::ctx_read::mode_allows_raw_cap(&resolved_mode) {
                            let ft = crate::core::tokens::count_tokens(&out);
                            crate::tools::ctx_read::cap_to_raw(
                                out,
                                ft,
                                &compute_content,
                                original_tokens,
                            )
                        } else {
                            out
                        };
                        (out, resolved_mode)
                    };

                    computed = crate::core::redaction::redact_text_if_enabled(&computed);

                    if cancel_flag.load(Ordering::Relaxed) {
                        return;
                    }

                    // 2b-iii: Brief write lock — store result + metadata.
                    // Sub-millisecond: HashMap insert + stats snapshot.
                    // Graceful degradation: if the lock cannot be acquired
                    // within 5s, return the result without caching it.
                    {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(5);
                        let cache_guard = loop {
                            if cancel_flag.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Ok(g) = cache_lock.try_write() {
                                break Some(g);
                            }
                            if std::time::Instant::now() >= deadline {
                                tracing::warn!(
                                    "ctx_read: store-lock timeout (5s) for {path_owned},                                      returning without caching"
                                );
                                break None;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        };

                        if let Some(mut cache) = cache_guard {
                            if crate::tools::ctx_read::is_cacheable_mode(&rmode) {
                                let ck = crate::tools::ctx_read::compressed_cache_key(
                                    &rmode,
                                    crp_mode,
                                    task_ref,
                                    tuning.aggressiveness,
                                    tuning.protect,
                                );
                                cache.set_compressed(&path_owned, &ck, computed.clone());
                            }
                            if rmode == "full" || rmode == "full-compact" {
                                cache.mark_full_delivered(&path_owned);
                            }
                            if let Some(entry) = cache.get_mut(&path_owned) {
                                entry.last_mode.clone_from(&rmode);
                            }
                            if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
                                bt.record_read(
                                    &path_owned,
                                    &rmode,
                                    crate::core::tokens::count_tokens(&computed),
                                    original_tokens,
                                );
                            }
                            let orig = cache.get(&path_owned).map_or(0, |e| e.original_tokens);
                            let fref = cache.file_ref_map().get(path_owned.as_str()).cloned();
                            let s = cache.get_stats();
                            let _ = tx.send((
                                computed,
                                rmode,
                                orig,
                                false,
                                fref,
                                (s.total_reads(), s.cache_hits()),
                                reuse_outcome,
                            ));
                        } else {
                            let _ = tx.send((
                                computed,
                                rmode,
                                original_tokens,
                                false,
                                None,
                                (0, 0),
                                reuse_outcome,
                            ));
                        }
                    }
                });
                if let Ok(result) = rx.recv_timeout(read_timeout) {
                    result
                } else {
                    cancelled.store(true, Ordering::Relaxed);
                    tracing::error!("ctx_read timed out after {read_timeout:?} for {path}");
                    let msg = format!(
                        "ERROR: ctx_read timed out after {}s reading {path}. \
                     The file may be very large or a blocking I/O issue occurred. \
                     Try mode=\"lines:1-100\" for a partial read.",
                        read_timeout.as_secs()
                    );
                    return Err(ErrorData::internal_error(msg, None));
                }
            } // end else (slow path)
        };

        if resolved_mode == "error" {
            return Err(ErrorData::invalid_params(output, None));
        }

        let output_tokens = crate::core::tokens::count_tokens(&output);
        let saved = original.saturating_sub(output_tokens);

        if !is_cache_hit {
            if let Some((hash, mtime)) = delivery_metadata {
                crate::tools::ctx_read::record_cross_agent_delivery(
                    path,
                    hash,
                    mtime,
                    0,
                    output_tokens,
                    None,
                    None,
                );
            }
        }

        // Session updates (bounded lock — 10s timeout, read already succeeded)
        let mut ensured_root: Option<String> = None;
        let mut traversal_working_set: Vec<String> = Vec::new();
        let mut prefetch_paths: Vec<String> = Vec::new();
        let project_root_snapshot;
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let session_guard = loop {
                if let Ok(g) = session_lock.clone().try_write_owned() {
                    break Some(g);
                }
                if std::time::Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            };
            if let Some(mut session) = session_guard {
                session.touch_file(path, file_ref.as_deref(), &resolved_mode, original);
                prefetch_paths = session.prefetch_predictions(3);
                // Capture the recent working set (under the lock) so the
                // background thread can record a traversal/co-access edge (#289).
                traversal_working_set =
                    crate::core::tool_lifecycle::recent_working_set(&session, path);
                let file_summary = extract_file_summary(&output, path);
                if !file_summary.is_empty() {
                    session.set_file_summary(path, &file_summary);
                }
                if is_cache_hit {
                    session.record_cache_hit();
                }
                if session.active_structured_intent.is_none() && session.files_touched.len() >= 2 {
                    let touched: Vec<String> = session
                        .files_touched
                        .iter()
                        .map(|f| f.path.clone())
                        .collect();
                    let inferred =
                        crate::core::intent_engine::StructuredIntent::from_file_patterns(&touched);
                    if inferred.confidence >= 0.4 {
                        session.active_structured_intent = Some(inferred);
                    }
                }
                if session.task.is_none() && session.stats.files_read % 5 == 0 {
                    session.auto_infer_task();
                }
                let root_missing = session
                    .project_root
                    .as_deref()
                    .is_none_or(|r| r.trim().is_empty());
                if root_missing && let Some(root) = crate::core::protocol::detect_project_root(path)
                {
                    session.project_root = Some(root.clone());
                    ensured_root = Some(root);
                }
                project_root_snapshot = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
            } else {
                tracing::warn!(
                    "session write-lock timeout (5s) in ctx_read post-update for {path}"
                );
                project_root_snapshot = ctx.project_root.clone();
            }
        }
        if let Some(root) = ensured_root.as_deref() {
            crate::core::index_orchestrator::ensure_all_background(root);
        }

        if !prefetch_paths.is_empty() {
            crate::core::context_prefetch::warm_predictions(&prefetch_paths, Some(&cache_lock));
        }

        // Telemetry + learning are pure side-effects that never influence this
        // response, yet they did synchronous disk I/O on every read (heatmap
        // append, ModePredictor load+save, FeedbackStore load). Push them off
        // the hot path so reads — especially cache-hit stubs — return without
        // waiting on disk (#149).
        {
            let path_bg = path.to_string();
            let resolved_mode_bg = resolved_mode.clone();
            let project_root_bg = project_root_snapshot.clone();
            let (turns, hits) = cache_stats;
            // #685: model-correct verified-ledger inputs, computed off the hot path.
            // The default O200kBase model reuses the o200k `original`/`saved` below
            // (byte-identical, no clone). Only a resolved Claude/Gemini/Llama model
            // carries the cache handle + output so the bg thread can re-tokenize the
            // raw source and the sent output in the family the provider actually bills.
            let ledger_cache = (crate::core::savings_ledger::ledger_family()
                != crate::core::tokens::TokenizerFamily::O200kBase)
                .then(|| cache_lock.clone());
            let ledger_output = ledger_cache.as_ref().map(|_| output.clone());
            std::thread::spawn(move || {
                // A panic in telemetry must not poison locks or leave a zombie thread;
                // it never affects the already-returned read response.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    crate::core::heatmap::record_file_access(&path_bg, original, saved);

                    // #685: verified savings ledger, decoupled from the heatmap so it
                    // can denominate in the active model's tokenizer family. O200kBase
                    // reuses the o200k counts; other families re-tokenize raw (cache)
                    // + output. A cache miss falls back to o200k (conservative).
                    {
                        use crate::core::savings_ledger as ledger;
                        let (lbase, lsaved) = match (&ledger_cache, &ledger_output) {
                            (Some(cl), Some(out)) => match cl.try_read().ok().and_then(|c| {
                                c.get(&path_bg)
                                    .and_then(crate::core::cache::CacheEntry::content)
                            }) {
                                Some(raw) => {
                                    let lo = ledger::count_for_ledger(&raw);
                                    (lo, lo.saturating_sub(ledger::count_for_ledger(out)))
                                }
                                None => (original, saved),
                            },
                            _ => (original, saved),
                        };
                        ledger::record_read_event(lbase, lsaved, None, None);
                    }

                    // Traversal/co-access edge: this read fired together with the
                    // recent working set captured under the session lock (#289).
                    if let Some(root) =
                        crate::core::tool_lifecycle::usable_root(Some(project_root_bg.as_str()))
                    {
                        crate::core::cooccurrence::record_focus_access(
                            root,
                            &path_bg,
                            &traversal_working_set,
                        );
                    }
                    let sig =
                        crate::core::mode_predictor::FileSignature::from_path(&path_bg, original);
                    let density = if output_tokens > 0 {
                        original as f64 / output_tokens as f64
                    } else {
                        1.0
                    };
                    let outcome = crate::core::mode_predictor::ModeOutcome {
                        mode: resolved_mode_bg,
                        tokens_in: original,
                        tokens_out: output_tokens,
                        density: density.min(1.0),
                    };
                    let mut predictor = crate::core::mode_predictor::ModePredictor::new();
                    predictor.set_project_root(&project_root_bg);
                    predictor.record(sig, outcome);
                    predictor.save();

                    let ext = std::path::Path::new(&path_bg)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_string();
                    let thresholds =
                        crate::core::adaptive_thresholds::thresholds_for_path(&path_bg);
                    let feedback_outcome = crate::core::feedback::CompressionOutcome {
                        session_id: format!("{}", std::process::id()),
                        language: ext,
                        entropy_threshold: thresholds.bpe_entropy,
                        jaccard_threshold: thresholds.jaccard,
                        total_turns: turns as u32,
                        tokens_saved: saved as u64,
                        tokens_original: original as u64,
                        cache_hits: hits as u32,
                        total_reads: turns as u32,
                        // Real behavioral signal instead of a hardcoded success
                        // (#593): a compressed read only counts as task-completing
                        // when this extension is not in a high-bounce state —
                        // compression that keeps forcing full re-reads is not
                        // "completing" anything. Unknown (too few reads) stays
                        // optimistic so the cold start is unchanged. 0.30 mirrors
                        // bounce_tracker::BOUNCE_RATE_THRESHOLD.
                        task_completed: crate::core::bounce_tracker::global()
                            .lock()
                            .ok()
                            .and_then(|bt| bt.bounce_rate_for_extension(&path_bg))
                            .is_none_or(|rate| rate < 0.30),
                        timestamp: chrono::Local::now().to_rfc3339(),
                    };
                    let mut store = crate::core::feedback::FeedbackStore::load();
                    store.project_root = Some(project_root_bg);
                    store.record_outcome(feedback_outcome);
                }));
            });
        }
        if let Some(aid) = resolved_agent_id.as_deref() {
            crate::core::agent_budget::record_consumption(aid, output_tokens);
        }

        // #1098: graph-related hints (callers/callees) are now computed AFTER the
        // cache lock is released. They involve SQLite queries (~50-200ms) that
        // previously blocked all parallel reads while holding the write lock.
        let graph_hint = if !is_cache_hit
            && !resolved_mode.starts_with("lines:")
            && crate::core::profiles::active_profile()
                .output_hints
                .related_hint()
        {
            crate::tools::ctx_read::graph_related_hint(path)
        } else {
            None
        };

        // Cross-source hints: gated by profile `cross_source_hint` (default off).
        // When enabled, appends issue/PR/schema references from the property
        // graph. Skipped when graph.db doesn't exist (#682).
        let hints_suffix = if crate::core::profiles::active_profile()
            .output_hints
            .cross_source_hint()
        {
            let graph_db =
                crate::core::property_graph::graph_dir(&ctx.project_root).join("graph.db");
            let graph = graph_db
                .exists()
                .then(|| crate::core::property_graph::CodeGraph::open(&ctx.project_root))
                .transpose()
                .ok()
                .flatten();
            graph.map_or_else(String::new, |graph| {
                let edges = graph.all_cross_source_edges();
                if edges.is_empty() {
                    String::new()
                } else {
                    let ranges = scoped_read_ranges(&resolved_mode);
                    let relative_path =
                        crate::core::graph_index::graph_relative_key(path, &ctx.project_root);
                    let hints = crate::core::cross_source_hints::hints_for_file_matching(
                        path,
                        &edges,
                        &ctx.project_root,
                        |hint| {
                            ranges.as_ref().is_none_or(|ranges| {
                                hint_intersects_ranges(hint, ranges, &graph, &relative_path)
                            })
                        },
                    );
                    crate::core::cross_source_hints::format_hints(&hints)
                }
            })
        } else {
            String::new()
        };

        // Rule injection (#1325): discover and append rules scoped to this file
        // path (CLAUDE.md, AGENTS.md, .cursor/rules, .claude/rules) so the agent
        // receives the same context it would get from the native Read tool.
        let rules_suffix = {
            let client_id = ctx
                .client_name
                .as_ref()
                .map(|c| c.blocking_read().clone())
                .unwrap_or_default();
            crate::core::rule_discovery::rules_suffix_for_read(path, &ctx.project_root, &client_id)
        };
        let mut warnings = Vec::new();
        if let Some(ref w) = budget_warning {
            warnings.push(w.as_str());
        }
        if let Some(ref w) = degrade_warning {
            warnings.push(w.as_str());
        }
        if let Some(ref w) = delta_explicit_note {
            warnings.push(w.as_str());
        }
        if let Some(ref w) = mode_override_note {
            warnings.push(w.as_str());
        }
        if let Some(ref w) = instruction_mode_note {
            warnings.push(w.as_str());
        }
        let graph_suffix = graph_hint.map(|h| format!("\n{h}")).unwrap_or_default();
        // #977: notices (mode override, budget, degradation, delta) go BEFORE the
        // payload so client-side truncation of large outputs cannot hide them.
        let final_output = if !warnings.is_empty() {
            format!(
                "{}\n\n{output}{hints_suffix}{graph_suffix}{rules_suffix}",
                warnings.join("\n")
            )
        } else if hints_suffix.is_empty() && graph_suffix.is_empty() && rules_suffix.is_empty() {
            output
        } else {
            format!("{output}{hints_suffix}{graph_suffix}{rules_suffix}")
        };
        // Proactive context: gated by profile `proactive_context` (default off).
        // When enabled, auto-expands previously compressed content that is
        // keyword-relevant to the current read (up to 2000 tokens).
        let final_output = if crate::core::profiles::active_profile()
            .output_hints
            .proactive_context()
        {
            let proactive_query = format!(
                "ctx_read path={path} mode={resolved_mode} task={}",
                task_ref.unwrap_or_default()
            );
            if let Some(block) =
                crate::core::relevance_tracker::proactive_context_for_path(&proactive_query, path)
            {
                format!("{final_output}{block}")
            } else {
                final_output
            }
        } else {
            final_output
        };

        // Monotonic guard (#1326): re-count tokens on the fully assembled output
        // (including hints, warnings, proactive context) and verify the compressed
        // result is actually smaller than the original. If annotations inflated the
        // output beyond the raw baseline, report zero savings so the ledger stays
        // honest. We keep the compressed form (it may still be more useful than raw)
        // but correct the accounting.
        let final_tokens = crate::core::tokens::count_tokens(&final_output);
        let verified_saved = original.saturating_sub(final_tokens);
        crate::core::cache::record_ctx_read_outcome(reuse_outcome);

        Ok(ToolOutput {
            text: final_output,
            original_tokens: original,
            saved_tokens: verified_saved,
            mode: Some(resolved_mode),
            path: Some(path.to_string()),
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}

#[path = "ctx_read_window.rs"]
mod window;
#[allow(unused_imports)]
// lines_mode + resolve_line_window used in #[cfg(test)] ctx_read_inline_tests
use window::{
    apply_line_window, hint_intersects_ranges, lines_mode, resolve_instruction_file_mode,
    resolve_line_window, resolve_raw_alias, scoped_read_ranges,
};

fn apply_verdict(
    mode: &str,
    verdict: crate::core::degradation_policy::DegradationVerdictV1,
) -> (String, bool) {
    use crate::core::degradation_policy::DegradationVerdictV1;
    match verdict {
        DegradationVerdictV1::Ok => (mode.to_string(), false),
        DegradationVerdictV1::Warn => match mode {
            "full" => ("map".to_string(), true),
            other => (other.to_string(), false),
        },
        DegradationVerdictV1::Throttle => match mode {
            "full" | "map" => ("signatures".to_string(), true),
            other => (other.to_string(), false),
        },
        DegradationVerdictV1::Block => {
            if mode == "signatures" {
                ("signatures".to_string(), false)
            } else {
                ("signatures".to_string(), true)
            }
        }
    }
}

fn auto_degrade_read_mode(mode: &str) -> (String, Option<String>) {
    if crate::core::config::Config::load().no_degrade_effective() {
        return (mode.to_string(), None);
    }
    let profile = crate::core::profiles::active_profile();
    if !profile.degradation.enforce_effective() {
        return (mode.to_string(), None);
    }
    let policy = crate::core::degradation_policy::evaluate_v1_for_tool("ctx_read", None);
    let (new_mode, degraded) = apply_verdict(mode, policy.decision.verdict);
    let warning = if degraded {
        Some(format!(
            "⚠ Context pressure: mode={mode} was downgraded to mode={new_mode} \
             (verdict: {:?}). Use start_line=1 to bypass, or run ctx_compress to free budget.",
            policy.decision.verdict
        ))
    } else {
        None
    };
    (new_mode, warning)
}

fn extract_file_summary(output: &str, path: &str) -> String {
    let hint = crate::core::auto_findings::extract_content_hint(output);
    if !hint.is_empty() {
        return hint;
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let line_count = output.lines().count();
    if line_count > 5 {
        format!("{ext} file, {line_count} lines")
    } else {
        String::new()
    }
}

// #660 LOC gate: inline tests split out to keep this file under the line cap.
#[cfg(test)]
#[path = "ctx_read_inline_tests.rs"]
mod tests;

// #660 LOC gate: repo-param tests split out to keep this file under the line
// cap — see `ctx_read_repo_param_tests.rs`.

/// Read an image file and return it as MCP ContentBlock::Image for visual LLM processing.
fn read_image_file(path: &str) -> Result<ToolOutput, ErrorData> {
    use crate::core::binary_detect::{IMAGE_MAX_BYTES, image_mime_type};
    use base64::Engine;

    let metadata = std::fs::metadata(path)
        .map_err(|e| ErrorData::invalid_params(format!("Cannot read image: {e}"), None))?;

    if metadata.len() > IMAGE_MAX_BYTES {
        return Err(ErrorData::invalid_params(
            format!(
                "Image too large ({:.1} MB, limit {:.0} MB). Resize or use a smaller image.",
                metadata.len() as f64 / 1024.0 / 1024.0,
                IMAGE_MAX_BYTES as f64 / 1024.0 / 1024.0,
            ),
            None,
        ));
    }

    let mime_type = image_mime_type(path)
        .ok_or_else(|| ErrorData::invalid_params("Unsupported image format".to_string(), None))?;

    let bytes = std::fs::read(path)
        .map_err(|e| ErrorData::invalid_params(format!("Cannot read image: {e}"), None))?;

    let base64_data = base64::prelude::BASE64_STANDARD.encode(&bytes);
    let short_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);

    let text_block = ContentBlock::text(format!(
        "[Image: {} ({} KB, {})]",
        short_name,
        bytes.len() / 1024,
        mime_type
    ));
    let image_block = ContentBlock::image(base64_data, mime_type);

    Ok(ToolOutput::image(
        vec![text_block, image_block],
        path.to_string(),
    ))
}

#[cfg(test)]
#[path = "ctx_read_repo_param_tests.rs"]
mod repo_param_tests;
