//! Edit-efficiency metering (#1008, honest-metering philosophy #361).
//!
//! Anchored editing (`ctx_patch`) saves *output* tokens: the model references a
//! `(line, hash)` anchor instead of reproducing the replaced span byte-for-byte
//! the way a str_replace `old_string` requires. This module measures that claim
//! with real per-edit numbers instead of a marketing multiplier:
//!
//! * **avoided output tokens** — per successful anchored op:
//!   `tokens(replaced span) − tokens(anchor args)`, floored at 0. The replaced
//!   span is exactly what a str_replace edit would have re-emitted as
//!   `old_string`; the anchor args are what the model actually sent instead.
//! * **conflict round-trips** — stale-anchor `CONFLICT` responses (each one is
//!   an extra turn the anchored loop needed).
//! * **str_replace baseline** — successful `ctx_edit` calls with the
//!   `old_string` tokens they really paid, plus `old_string`-miss round-trips.
//!
//! This is a **separate metric channel**: values are never folded into the
//! read-gain ledger and never appear in tool output bodies (#498 determinism).
//! Consumers are `ctx_metrics`, the dashboard (`/api/stats` →
//! `edit_efficiency`) and the A/B eval harness.
//!
//! Storage: `~/.lean-ctx/edit_metering.json`, atomic write (tmp+rename),
//! loaded once per process, flushed every few records like `edit_quality`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[allow(dead_code)]
pub struct EditMetrics {
    pub lines_added: u64,
    pub lines_removed: u64,
    pub net_loc_delta: i64,
    pub edits_count: u32,
    pub files_touched: u32,
}

const STORE_FILE: &str = "edit_metering.json";
const FLUSH_EVERY: usize = 5;

static STORE: OnceLock<Mutex<EditMeteringStore>> = OnceLock::new();
static RECORD_CALLS: AtomicUsize = AtomicUsize::new(0);

/// All-time counters for both edit paths. Small, append-only aggregates —
/// per-file/per-op detail intentionally lives in `edit_quality`, not here.
/// `serde(default)` keeps older/partial store files loadable field-by-field
/// instead of silently resetting all counters via `unwrap_or_default`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(crate) struct EditMeteringStore {
    /// Successful `ctx_patch` calls (a batch counts once).
    pub anchored_calls: u64,
    /// Anchored ops applied across those calls.
    pub anchored_ops: u64,
    /// Σ `max(0, tokens(replaced span) − tokens(anchor args))` over applied ops.
    pub anchored_avoided_output_tokens: u64,
    /// Stale-anchor `CONFLICT` responses (self-heal retry round-trips).
    pub anchored_conflicts: u64,
    /// Successful `ctx_edit` (str_replace) calls.
    pub str_replace_calls: u64,
    /// Σ `tokens(old_string)` those calls actually paid in output.
    pub str_replace_old_string_tokens: u64,
    /// `old_string`-not-found responses (blind retry round-trips).
    pub str_replace_misses: u64,
    /// Lines added across all edit operations.
    pub loc_added: u64,
    /// Lines removed across all edit operations.
    pub loc_removed: u64,
    #[serde(skip)]
    dirty: bool,
}

impl EditMeteringStore {
    fn load_from_disk() -> Self {
        let Ok(raw) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub(crate) fn save(&self) -> std::io::Result<()> {
        let path = store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(self)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }
}

fn store_path() -> std::path::PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(STORE_FILE)
}

fn global() -> &'static Mutex<EditMeteringStore> {
    STORE.get_or_init(|| Mutex::new(EditMeteringStore::load_from_disk()))
}

fn with_store(f: impl FnOnce(&mut EditMeteringStore)) {
    let Ok(mut store) = global().lock() else {
        return;
    };
    f(&mut store);
    store.dirty = true;
    let n = RECORD_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(FLUSH_EVERY) && store.save().is_ok() {
        store.dirty = false;
    }
}

/// A successful anchored patch: `ops` applied, `avoided_tokens` output tokens
/// the model did not have to reproduce (already anchor-overhead-adjusted).
pub fn record_anchored_success(ops: u64, avoided_tokens: u64) {
    with_store(|s| {
        s.anchored_calls = s.anchored_calls.saturating_add(1);
        s.anchored_ops = s.anchored_ops.saturating_add(ops);
        s.anchored_avoided_output_tokens = s
            .anchored_avoided_output_tokens
            .saturating_add(avoided_tokens);
    });
}

/// A stale-anchor `CONFLICT` response (one extra self-heal round-trip).
pub fn record_anchored_conflict() {
    with_store(|s| s.anchored_conflicts = s.anchored_conflicts.saturating_add(1));
}

/// A successful str_replace edit and the `old_string` tokens it paid.
pub fn record_str_replace_success(old_string_tokens: u64) {
    with_store(|s| {
        s.str_replace_calls = s.str_replace_calls.saturating_add(1);
        s.str_replace_old_string_tokens = s
            .str_replace_old_string_tokens
            .saturating_add(old_string_tokens);
    });
}

/// An `old_string`-not-found miss (one blind retry round-trip).
pub fn record_str_replace_miss() {
    with_store(|s| s.str_replace_misses = s.str_replace_misses.saturating_add(1));
}

#[allow(dead_code)]
pub fn record_loc_change(path: Option<&str>, added: u64, removed: u64) {
    with_store(|s| {
        s.loc_added = s.loc_added.saturating_add(added);
        s.loc_removed = s.loc_removed.saturating_add(removed);
    });
    crate::core::solution_tracker::record_loc_change(added, removed);
    if let Some(path) = path {
        crate::core::savings_ledger::record_edit_event(path, added, removed);
    }
}

/// Snapshot for `ctx_metrics` and the dashboard `/api/stats` payload.
pub fn metrics_snapshot() -> serde_json::Value {
    let Ok(store) = global().lock() else {
        return serde_json::json!({});
    };
    serde_json::json!({
        "anchored_calls": store.anchored_calls,
        "anchored_ops": store.anchored_ops,
        "anchored_avoided_output_tokens": store.anchored_avoided_output_tokens,
        "anchored_conflicts": store.anchored_conflicts,
        "str_replace_calls": store.str_replace_calls,
        "str_replace_old_string_tokens": store.str_replace_old_string_tokens,
        "str_replace_misses": store.str_replace_misses,
        "loc_added": store.loc_added,
        "loc_removed": store.loc_removed,
    })
}

pub(crate) fn flush() {
    // Clears `dirty` once saved: flush runs after every tool call, and a
    // store left dirty was rewritten on each one.
    if let Ok(mut store) = global().lock()
        && store.dirty
        && store.save().is_ok()
    {
        store.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_and_saturate() {
        let mut s = EditMeteringStore::default();
        s.anchored_calls = u64::MAX;
        s.anchored_calls = s.anchored_calls.saturating_add(1);
        assert_eq!(s.anchored_calls, u64::MAX, "saturating, never wraps");
    }

    #[test]
    fn roundtrip_serialization() {
        let s = EditMeteringStore {
            anchored_calls: 3,
            anchored_ops: 7,
            anchored_avoided_output_tokens: 412,
            anchored_conflicts: 1,
            str_replace_calls: 2,
            str_replace_old_string_tokens: 260,
            str_replace_misses: 4,
            loc_added: 15,
            loc_removed: 9,
            dirty: false,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: EditMeteringStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back.anchored_ops, 7);
        assert_eq!(back.anchored_avoided_output_tokens, 412);
        assert_eq!(back.str_replace_misses, 4);
        assert_eq!(back.loc_added, 15);
        assert_eq!(back.loc_removed, 9);
    }

    #[test]
    fn legacy_or_empty_store_deserializes() {
        let s: EditMeteringStore = serde_json::from_str("{}").unwrap();
        assert_eq!(s.anchored_calls, 0);
        assert_eq!(s.str_replace_old_string_tokens, 0);
        assert_eq!(s.loc_added, 0);
        assert_eq!(s.loc_removed, 0);
    }
}
