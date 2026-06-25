//! Quality loop v1 (#494): compression-caused edit failures feed back into
//! mode selection.
//!
//! `BounceTracker`/`path_mode_memory` close the loop for *re-read* bounces,
//! but an edit that fails because the file was last read in a compressed mode
//! (`old_string` not found — the body simply wasn't in context) taught the
//! system nothing. This module records edit outcomes correlated with the last
//! read mode and feeds two signals back into `auto_mode_resolver::resolve`:
//!
//! 1. **Per-path escalation** — after a compression-correlated edit failure
//!    the *next* auto read of that file resolves to `full` (one-shot, 1 h TTL).
//! 2. **Per-(extension × mode) penalty** — modes whose edit-failure rate for a
//!    file type crosses the risky threshold resolve to `full` until the rate
//!    recovers (hysteresis, see below).
//!
//! Risk formula (documented in `docs/contracts/quality-loop-v1.md`):
//! a (ext, mode) pair becomes risky when `fails >= 2 && fails / (fails +
//! successes) >= 0.25`, and stops being risky only when the rate drops below
//! `0.15` — two thresholds so one lucky edit doesn't flap the decision.
//!
//! Storage: `~/.lean-ctx/edit_quality.json`, atomic write (tmp+rename),
//! loaded once per process, flushed periodically like `path_mode_memory`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "edit_quality.json";
/// (ext, mode) pairs without a failure for this long are dropped on load.
const DECAY_SECS: u64 = 30 * 24 * 3600;
/// Pending per-path escalations expire after this long.
const ESCALATION_TTL_SECS: u64 = 3600;
/// Hard caps; oldest entries are evicted first.
const MAX_PAIRS: usize = 200;
const MAX_PENDING: usize = 100;
const FLUSH_EVERY: usize = 10;

/// Risky when the failure share reaches this rate (with >= 2 fails)…
const RISKY_ENTER_RATE: f64 = 0.25;
/// …and recovers only once the rate drops below this (hysteresis).
const RISKY_EXIT_RATE: f64 = 0.15;
const RISKY_MIN_FAILS: u32 = 2;

static STORE: OnceLock<Mutex<EditQualityStore>> = OnceLock::new();
static RECORD_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PairStats {
    pub fails: u32,
    pub successes: u32,
    pub risky: bool,
    pub last_fail_unix: u64,
}

impl PairStats {
    fn fail_rate(&self) -> f64 {
        let total = self.fails + self.successes;
        if total == 0 {
            return 0.0;
        }
        f64::from(self.fails) / f64::from(total)
    }

    /// Applies the documented enter/exit thresholds after every outcome.
    fn update_risky(&mut self) {
        if self.risky {
            if self.fail_rate() < RISKY_EXIT_RATE {
                self.risky = false;
            }
        } else if self.fails >= RISKY_MIN_FAILS && self.fail_rate() >= RISKY_ENTER_RATE {
            self.risky = true;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EditQualityStore {
    /// Key: `"{ext}|{mode}"` (e.g. `"rs|map"`).
    pub pairs: HashMap<String, PairStats>,
    /// Normalized path -> unix time of the compression-correlated edit fail.
    pub pending_escalations: HashMap<String, u64>,
    /// All-time counter of consumed escalations (observability).
    #[serde(default)]
    pub escalations_served: u64,
    #[serde(skip)]
    dirty: bool,
}

fn pair_key(ext: &str, mode: &str) -> String {
    format!("{ext}|{mode}")
}

impl EditQualityStore {
    fn load_from_disk() -> Self {
        let Ok(raw) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        let mut store: Self = serde_json::from_str(&raw).unwrap_or_default();
        store.decay(now_unix());
        store
    }

    fn decay(&mut self, now: u64) {
        let before = self.pairs.len() + self.pending_escalations.len();
        self.pairs
            .retain(|_, s| now.saturating_sub(s.last_fail_unix) <= DECAY_SECS);
        self.pending_escalations
            .retain(|_, ts| now.saturating_sub(*ts) <= ESCALATION_TTL_SECS);
        if self.pairs.len() + self.pending_escalations.len() != before {
            self.dirty = true;
        }
    }

    fn evict_to_caps(&mut self) {
        if self.pairs.len() > MAX_PAIRS {
            let mut items: Vec<(String, u64)> = self
                .pairs
                .iter()
                .map(|(k, s)| (k.clone(), s.last_fail_unix))
                .collect();
            items.sort_by_key(|(_, ts)| *ts);
            let drop_n = self.pairs.len() - MAX_PAIRS;
            for (key, _) in items.into_iter().take(drop_n) {
                self.pairs.remove(&key);
            }
            self.dirty = true;
        }
        if self.pending_escalations.len() > MAX_PENDING {
            let mut items: Vec<(String, u64)> = self
                .pending_escalations
                .iter()
                .map(|(k, ts)| (k.clone(), *ts))
                .collect();
            items.sort_by_key(|(_, ts)| *ts);
            let drop_n = self.pending_escalations.len() - MAX_PENDING;
            for (key, _) in items.into_iter().take(drop_n) {
                self.pending_escalations.remove(&key);
            }
            self.dirty = true;
        }
    }

    pub fn record_failure(&mut self, ext: &str, mode: &str, now: u64) {
        let entry = self.pairs.entry(pair_key(ext, mode)).or_default();
        entry.fails = entry.fails.saturating_add(1);
        entry.last_fail_unix = now;
        entry.update_risky();
        self.dirty = true;
        self.evict_to_caps();
    }

    pub fn record_success(&mut self, ext: &str, mode: &str) {
        let entry = self.pairs.entry(pair_key(ext, mode)).or_default();
        entry.successes = entry.successes.saturating_add(1);
        entry.update_risky();
        self.dirty = true;
    }

    pub fn set_pending_escalation(&mut self, norm_path: &str, now: u64) {
        self.pending_escalations.insert(norm_path.to_string(), now);
        self.dirty = true;
        self.evict_to_caps();
    }

    /// Consumes the escalation for this path if present and not expired.
    pub fn take_pending_escalation(&mut self, norm_path: &str, now: u64) -> bool {
        match self.pending_escalations.remove(norm_path) {
            Some(ts) if now.saturating_sub(ts) <= ESCALATION_TTL_SECS => {
                self.escalations_served += 1;
                self.dirty = true;
                true
            }
            Some(_) => {
                self.dirty = true;
                false
            }
            None => false,
        }
    }

    #[must_use]
    pub fn is_risky(&self, ext: &str, mode: &str) -> bool {
        self.pairs
            .get(&pair_key(ext, mode))
            .is_some_and(|s| s.risky)
    }

    pub fn save(&self) -> std::io::Result<()> {
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

fn store_path() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(STORE_FILE)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn global() -> &'static Mutex<EditQualityStore> {
    STORE.get_or_init(|| Mutex::new(EditQualityStore::load_from_disk()))
}

fn ext_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// Process-global: record the outcome of an edit, correlated with the mode of
/// the last read of that file. `last_mode` must be the recorded read mode
/// (empty = file was never read through lean-ctx → no signal, skipped).
/// Compression-correlated failures additionally arm the one-shot per-path
/// escalation so the next auto read of `path` resolves to `full`.
pub fn record_edit_outcome(path: &str, last_mode: &str, success: bool) {
    if last_mode.is_empty() {
        return;
    }
    let ext = ext_of(path);
    let Ok(mut store) = global().lock() else {
        return;
    };
    if success {
        store.record_success(&ext, last_mode);
    } else {
        let now = now_unix();
        store.record_failure(&ext, last_mode, now);
        if last_mode != "full" {
            let norm = crate::core::pathutil::normalize_tool_path(path);
            store.set_pending_escalation(&norm, now);
            // Quality signal (#538): edit failures after compressed reads are
            // the strongest "compressed too much" evidence we have — they also
            // penalize the bandit arm that produced the read (#593).
            crate::core::adaptive_thresholds::record_quality_signal(
                path,
                crate::core::threshold_learning::QualitySignal::EditFail,
            );
            // Stigmergy (#540): edit failures mark the path as Stuck.
            let scent_path = norm.clone();
            std::thread::spawn(move || {
                crate::core::scent_field::deposit(
                    crate::core::scent_field::scent_agent_id(),
                    crate::core::scent_field::ScentKind::Stuck,
                    &scent_path,
                    1.0,
                );
            });
        }
    }
    maybe_flush(&mut store);
}

/// Process-global: one-shot check-and-consume of the per-path escalation.
#[must_use]
pub fn take_pending_escalation(path: &str) -> bool {
    let norm = crate::core::pathutil::normalize_tool_path(path);
    let Ok(mut store) = global().lock() else {
        return false;
    };
    let hit = store.take_pending_escalation(&norm, now_unix());
    if hit {
        maybe_flush(&mut store);
    }
    hit
}

/// Process-global: is `mode` currently risky for files with this extension?
#[must_use]
pub fn is_risky_mode(path: &str, mode: &str) -> bool {
    let ext = ext_of(path);
    global().lock().is_ok_and(|s| s.is_risky(&ext, mode))
}

/// Snapshot for `ctx_metrics`: (risky pairs, per-pair stats, escalations served).
#[must_use]
pub fn metrics_snapshot() -> serde_json::Value {
    let Ok(store) = global().lock() else {
        return serde_json::json!({});
    };
    let mut pairs: Vec<serde_json::Value> = store
        .pairs
        .iter()
        .map(|(key, s)| {
            serde_json::json!({
                "pair": key,
                "fails": s.fails,
                "successes": s.successes,
                "fail_rate": (s.fail_rate() * 1000.0).round() / 1000.0,
                "risky": s.risky,
            })
        })
        .collect();
    pairs.sort_by(|a, b| {
        let fa = a["fail_rate"].as_f64().unwrap_or(0.0);
        let fb = b["fail_rate"].as_f64().unwrap_or(0.0);
        fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
    });
    serde_json::json!({
        "pairs": pairs,
        "pending_escalations": store.pending_escalations.len(),
        "escalations_served": store.escalations_served,
    })
}

pub fn flush() {
    if let Ok(store) = global().lock()
        && store.dirty
    {
        let _ = store.save();
    }
}

fn maybe_flush(store: &mut EditQualityStore) {
    let n = RECORD_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(FLUSH_EVERY) && store.dirty && store.save().is_ok() {
        store.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risky_after_two_majority_fails_with_hysteresis() {
        let mut s = EditQualityStore::default();
        s.record_failure("rs", "map", 1000);
        assert!(!s.is_risky("rs", "map"), "one fail is not a pattern");
        s.record_failure("rs", "map", 1001);
        assert!(s.is_risky("rs", "map"), "2 fails, rate 1.0 >= 0.25");

        // Rate must drop below 0.15 to recover: 2 fails need > 11 successes.
        for _ in 0..11 {
            s.record_success("rs", "map");
        }
        assert!(s.is_risky("rs", "map"), "2/13 ≈ 0.154 still risky");
        s.record_success("rs", "map");
        assert!(!s.is_risky("rs", "map"), "2/14 ≈ 0.143 < 0.15 recovers");
    }

    #[test]
    fn entering_risky_needs_quarter_rate_not_just_two_fails() {
        let mut s = EditQualityStore::default();
        for _ in 0..7 {
            s.record_success("ts", "signatures");
        }
        s.record_failure("ts", "signatures", 1000);
        s.record_failure("ts", "signatures", 1001);
        // 2 fails / 9 total ≈ 0.22 < 0.25 — healthy mode stays usable.
        assert!(!s.is_risky("ts", "signatures"));
        s.record_failure("ts", "signatures", 1002);
        // 3/10 = 0.30 — now risky.
        assert!(s.is_risky("ts", "signatures"));
    }

    #[test]
    fn penalty_is_per_extension_not_global() {
        let mut s = EditQualityStore::default();
        s.record_failure("rs", "map", 1000);
        s.record_failure("rs", "map", 1001);
        assert!(s.is_risky("rs", "map"));
        assert!(!s.is_risky("py", "map"), "py|map untouched");
        assert!(!s.is_risky("rs", "signatures"), "rs|signatures untouched");
    }

    #[test]
    fn escalation_is_one_shot_and_expires() {
        let mut s = EditQualityStore::default();
        s.set_pending_escalation("src/a.rs", 1000);
        assert!(s.take_pending_escalation("src/a.rs", 1100));
        assert!(
            !s.take_pending_escalation("src/a.rs", 1101),
            "consumed — second read is normal again"
        );
        assert_eq!(s.escalations_served, 1);

        s.set_pending_escalation("src/b.rs", 1000);
        assert!(
            !s.take_pending_escalation("src/b.rs", 1000 + ESCALATION_TTL_SECS + 1),
            "expired escalations are dropped, not served"
        );
        assert_eq!(s.escalations_served, 1);
    }

    #[test]
    fn decay_drops_stale_pairs_and_pendings() {
        let mut s = EditQualityStore::default();
        s.record_failure("rs", "map", 1000);
        s.record_failure("go", "map", 5000);
        s.set_pending_escalation("old.rs", 1000);
        s.set_pending_escalation("fresh.rs", 5000);
        s.decay(5000 + DECAY_SECS - 10);
        assert!(!s.pairs.contains_key("rs|map"));
        assert!(s.pairs.contains_key("go|map"));
        // Pendings use the much shorter escalation TTL.
        assert!(s.pending_escalations.is_empty());
    }

    #[test]
    fn eviction_keeps_newest() {
        let mut s = EditQualityStore::default();
        for i in 0..(MAX_PAIRS + 10) {
            s.record_failure(&format!("e{i}"), "map", 1000 + i as u64);
        }
        assert_eq!(s.pairs.len(), MAX_PAIRS);
        assert!(!s.pairs.contains_key("e0|map"));
        for i in 0..(MAX_PENDING + 5) {
            s.set_pending_escalation(&format!("f{i}.rs"), 1000 + i as u64);
        }
        assert_eq!(s.pending_escalations.len(), MAX_PENDING);
        assert!(!s.pending_escalations.contains_key("f0.rs"));
    }

    #[test]
    fn roundtrip_serialization() {
        let mut s = EditQualityStore::default();
        s.record_failure("rs", "map", 42);
        s.set_pending_escalation("x.rs", 42);
        let json = serde_json::to_string(&s).unwrap();
        let back: EditQualityStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pairs.get("rs|map").unwrap().fails, 1);
        assert!(back.pending_escalations.contains_key("x.rs"));
    }
}
