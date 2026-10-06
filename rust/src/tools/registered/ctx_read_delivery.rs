// SPDX-License-Identifier: Apache-2.0
//! Delivery effects are owned by a prepared read and discarded unless emitted.
use crate::core::cache::ReuseOutcome;
use crate::server::tool_trait::{ToolContext, ToolOutput};
use std::time::SystemTime;
#[derive(Clone)]
pub(super) struct CacheSourceIdentity {
    pub(super) hash: String,
    pub(super) stored_mtime: Option<SystemTime>,
}

impl CacheSourceIdentity {
    pub(super) fn from_entry(entry: &crate::core::cache::CacheEntry) -> Self {
        Self {
            hash: entry.hash.clone(),
            stored_mtime: entry.stored_mtime,
        }
    }

    pub(super) fn matches(&self, entry: &crate::core::cache::CacheEntry) -> bool {
        self.hash == entry.hash && self.stored_mtime == entry.stored_mtime
    }
}

#[derive(Default)]
pub(super) struct PendingCacheDelivery {
    pub(super) identity: Option<CacheSourceIdentity>,
    pub(super) compressed_variant: Option<(String, String)>,
    pub(super) full_content: bool,
    pub(super) source_fingerprint: Option<[u8; 12]>,
    pub(super) last_mode: Option<String>,
    pub(super) record_bounce_read: bool,
    pub(super) diff_baseline: Option<String>,
}

pub(super) struct PendingCrossAgentDelivery {
    pub(super) hash: [u8; 12],
    pub(super) mtime: u64,
    /// Real line count of the delivered source (#1909: was a hardcoded `0L`).
    pub(super) line_count: u32,
    pub(super) output_tokens: usize,
    pub(super) output: String,
    pub(super) relay_key: Option<String>,
}

pub(super) struct PendingReadDelivery {
    pub(super) path: String,
    pub(super) cache_delivery: PendingCacheDelivery,
    pub(super) cross_agent: Option<PendingCrossAgentDelivery>,
    pub(super) file_ref: Option<String>,
    pub(super) file_summary: String,
    pub(super) resolved_mode: String,
    pub(super) original_tokens: usize,
    pub(super) reuse_outcome: ReuseOutcome,
    pub(super) is_cache_hit: bool,
    pub(super) agent_id: Option<String>,
}

impl PendingCacheDelivery {
    fn commit(&self, cache: &mut crate::core::cache::SessionCache, path: &str) -> bool {
        if let Some(content) = &self.diff_baseline {
            let baseline_matches = match (&self.identity, cache.get(path)) {
                (Some(identity), Some(entry)) => identity.matches(entry),
                (None, None) => true,
                _ => false,
            };
            // A diff intentionally starts from an older source. Preserve any
            // concurrent replacement, and never attach a newer mtime to old bytes.
            if !baseline_matches
                || !crate::tools::ctx_read::read_file_lossy(path)
                    .is_ok_and(|current| current == *content)
            {
                return false;
            }
            let result = cache.store(path, content);
            crate::core::telemetry::global_metrics().record_cache(result.was_hit);
            if let Some(mode) = &self.last_mode
                && let Some(entry) = cache.get_mut(path)
            {
                entry.last_mode.clone_from(mode);
            }
            return true;
        }
        let Some(identity) = &self.identity else {
            return false;
        };
        // Check identity under the same write guard as every delivery mark.
        // Never mark newer cache bytes as seen by an older prepared read.
        if !cache.get(path).is_some_and(|entry| identity.matches(entry))
            || crate::core::cache::is_cache_entry_stale_verified(
                path,
                identity.stored_mtime,
                &identity.hash,
            )
        {
            return false;
        }
        if let Some((key, text)) = &self.compressed_variant {
            cache.set_compressed(path, key, text.clone());
        }
        if self.full_content {
            cache.mark_full_delivered(path);
        }
        if let Some(mode) = &self.last_mode
            && let Some(entry) = cache.get_mut(path)
        {
            entry.last_mode.clone_from(mode);
        }
        true
    }
}

impl PendingReadDelivery {
    pub(super) fn commit(self, ctx: &ToolContext, result: &ToolOutput, record_agent_budget: bool) {
        let (Some(cache_lock), Some(session_lock)) = (ctx.cache.as_ref(), ctx.session.as_ref())
        else {
            return;
        };
        let path = self.path.as_str();
        let resolved_mode = self.resolved_mode;
        let original = self.original_tokens;
        let is_cache_hit = self.is_cache_hit;
        let file_ref = self.file_ref;
        let resolved_agent_id = self.agent_id;
        let output = result.text.clone();
        let output_tokens = crate::core::tokens::count_tokens(&output);
        let saved = original.saturating_sub(output_tokens);
        let cache_current = crate::server::bounded_lock::write(cache_lock, "ctx_read:delivery")
            .is_some_and(|mut cache| self.cache_delivery.commit(&mut cache, path));
        if cache_current {
            if let Ok(mut tracker) = crate::core::bounce_tracker::global().lock() {
                tracker.next_seq();
                if self.cache_delivery.record_bounce_read {
                    tracker.record_read(path, &resolved_mode, output_tokens, original);
                }
            }
            if let Some(relay) = self.cross_agent {
                crate::tools::ctx_read::record_cross_agent_delivery(
                    path,
                    relay.hash,
                    relay.mtime,
                    relay.line_count,
                    relay.output_tokens,
                    relay.relay_key.as_ref().map(|_| relay.output.as_str()),
                    relay.relay_key.as_deref(),
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
                let file_summary = self.file_summary;
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
        // response, yet they did synchronous disk I/O on every read
        // (ModePredictor load+save). Push them off
        // the hot path so reads — especially cache-hit stubs — return without
        // waiting on disk (#149).
        {
            let path_bg = path.to_string();
            let resolved_mode_bg = resolved_mode.clone();
            let project_root_bg = project_root_snapshot.clone();
            // #685: model-correct verified-ledger inputs, computed off the hot path.
            // The default O200kBase model reuses the o200k `original`/`saved` below
            // (byte-identical, no clone). Only a resolved Claude/Gemini/Llama model
            // carries the cache handle + output so the bg thread can re-tokenize the
            // raw source and the sent output in the family the provider actually bills.
            let ledger_cache = (crate::core::savings_ledger::ledger_family()
                != crate::core::tokens::TokenizerFamily::O200kBase)
                .then(|| cache_lock.clone());
            let ledger_output = ledger_cache.as_ref().map(|_| output.clone());
            let ledger_source = self.cache_delivery.identity.clone();
            crate::core::execution_lifecycle::record_heatmap_access(&path_bg, original, saved);
            crate::core::task_spine::TaskSpine::spawn_thread(move || {
                // A panic in telemetry must not poison locks or leave a zombie thread;
                // it never affects the already-returned read response.
                let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    // #685: verified savings ledger, decoupled from the heatmap so it
                    // can denominate in the active model's tokenizer family. O200kBase
                    // reuses the o200k counts; other families re-tokenize raw (cache)
                    // + output. A cache miss falls back to o200k (conservative).
                    {
                        use crate::core::savings_ledger as ledger;
                        let (lbase, lsaved) = match (&ledger_cache, &ledger_output) {
                            (Some(cl), Some(out)) => match cl.try_read().ok().and_then(|c| {
                                c.get(&path_bg)
                                    .filter(|entry| {
                                        ledger_source
                                            .as_ref()
                                            .is_some_and(|identity| identity.matches(entry))
                                    })
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

                    // A read supplies observed metrics, not task acceptance.
                    // Outcome-based learning requires the canonical execution
                    // protocol; do not mutate legacy feedback from bounce rates.
                }));
                if completed.is_ok() {
                    // Opt-in local diagnostics mark completion, not successful
                    // persistence or task acceptance; no paths/content are logged.
                    tracing::debug!(
                        target: "lean_ctx::read_observation",
                        "read_observation_worker_completed"
                    );
                }
            });
        }
        if record_agent_budget && let Some(aid) = resolved_agent_id.as_deref() {
            crate::core::agent_budget::record_consumption(aid, output_tokens);
        }

        crate::core::cache::record_ctx_read_outcome(self.reuse_outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cache::SessionCache;

    #[test]
    fn discarded_diff_preserves_baseline_until_delivery() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("delta.rs");
        let old = "fn old() {}\n";
        let new = "fn changed() {}\n";
        std::fs::write(&file, old).unwrap();
        let path = file.to_str().unwrap();
        let mut cache = SessionCache::new();
        cache.store(path, old);
        let identity = CacheSourceIdentity::from_entry(cache.get(path).unwrap());
        std::fs::write(&file, new).unwrap();
        let (first, _, _) = crate::tools::ctx_read::prepare_diff(&cache, path, "F1");
        let (retry, _, baseline) = crate::tools::ctx_read::prepare_diff(&cache, path, "F1");
        assert!(first.contains("changed"));
        assert_eq!(first, retry);
        assert_eq!(cache.get(path).unwrap().content().as_deref(), Some(old));
        let pending = PendingCacheDelivery {
            identity: Some(identity),
            diff_baseline: baseline,
            last_mode: Some("diff".into()),
            ..Default::default()
        };
        assert!(pending.commit(&mut cache, path));
        assert_eq!(cache.get(path).unwrap().content().as_deref(), Some(new));
    }

    #[test]
    fn delayed_delivery_does_not_mark_replaced_cache_bytes_with_same_mtime() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("version.rs");
        std::fs::write(&file, "fn old() {}\n").unwrap();
        let path = file.to_str().unwrap();
        let mut cache = SessionCache::new();
        cache.store(path, "fn old() {}\n");
        let identity = CacheSourceIdentity::from_entry(cache.get(path).unwrap());
        let pending = PendingCacheDelivery {
            identity: Some(identity.clone()),
            full_content: true,
            compressed_variant: Some(("map".into(), "old map".into())),
            last_mode: Some("full".into()),
            ..Default::default()
        };
        std::fs::write(&file, "fn new() {}\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(identity.stored_mtime.unwrap()))
            .unwrap();
        cache.store(path, "fn new() {}\n");
        assert_eq!(cache.get(path).unwrap().stored_mtime, identity.stored_mtime);
        assert!(!pending.commit(&mut cache, path));
        assert!(!cache.get(path).unwrap().full_content_delivered);
        assert!(cache.get_compressed(path, "map").is_none());
    }

    #[test]
    fn delayed_delivery_marks_unchanged_source_and_variant_together() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("stable.rs");
        std::fs::write(&file, "fn stable() {}\n").unwrap();
        let path = file.to_str().unwrap();
        let mut cache = SessionCache::new();
        cache.store(path, "fn stable() {}\n");
        let pending = PendingCacheDelivery {
            identity: Some(CacheSourceIdentity::from_entry(cache.get(path).unwrap())),
            full_content: true,
            compressed_variant: Some(("map".into(), "stable map".into())),
            last_mode: Some("full".into()),
            ..Default::default()
        };
        assert!(!cache.get(path).unwrap().full_content_delivered);
        assert!(pending.commit(&mut cache, path));
        assert!(cache.get(path).unwrap().full_content_delivered);
        assert_eq!(
            cache.get_compressed(path, "map").map(String::as_str),
            Some("stable map")
        );
    }
}
