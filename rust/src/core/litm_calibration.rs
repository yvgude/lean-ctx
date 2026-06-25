//! Empirical LITM placement calibration (#539, EFF-2).
//!
//! The static `LitmProfile` weights encode the *assumed* positional attention
//! of each client (begin strong, middle weak, end strong). This module closes
//! the loop with observed evidence: every wakeup injection records a manifest
//! of placed items (begin block vs end block). If the agent later explicitly
//! recalls an item that was already placed, that placement failed to register
//! — a *miss* for its position. Items never re-recalled count as *hits* when
//! the manifest rotates on the next wakeup build.
//!
//! The calibrated output is `begin_share`: the fraction of session items that
//! should go to the begin block. A persistently missing begin position shifts
//! budget toward the end block (recency wins for that client), and vice versa.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Default share of items placed at the begin position (today's layout).
pub const DEFAULT_BEGIN_SHARE: f64 = 0.7;
/// Calibration only activates after this many total observations per profile.
const MIN_OBSERVATIONS: u32 = 20;
/// Calibrated share is clamped to this range — both positions always get data.
const SHARE_CLAMP: (f64, f64) = (0.4, 0.9);
const FLUSH_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Begin,
    End,
}

impl Position {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Position::Begin => "begin",
            Position::End => "end",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "begin" => Some(Position::Begin),
            "end" => Some(Position::End),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlacementStats {
    pub begin_hits: u32,
    pub begin_misses: u32,
    pub end_hits: u32,
    pub end_misses: u32,
}

impl PlacementStats {
    fn total(&self) -> u32 {
        self.begin_hits + self.begin_misses + self.end_hits + self.end_misses
    }

    /// Laplace-smoothed hit rate for a position.
    fn hit_rate(&self, pos: Position) -> f64 {
        let (hits, misses) = match pos {
            Position::Begin => (self.begin_hits, self.begin_misses),
            Position::End => (self.end_hits, self.end_misses),
        };
        (f64::from(hits) + 1.0) / (f64::from(hits + misses) + 2.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LitmCalibration {
    /// Keyed by LITM profile name ("claude" | "gpt" | "gemini").
    pub per_profile: HashMap<String, PlacementStats>,
    pub schema_version: u32,
}

static BUFFER: Mutex<Option<(LitmCalibration, Instant)>> = Mutex::new(None);

fn store_path() -> std::path::PathBuf {
    crate::core::paths::cache_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("litm_calibration.json")
}

impl LitmCalibration {
    fn load_from_disk() -> Self {
        if let Ok(content) = std::fs::read_to_string(store_path())
            && let Ok(c) = serde_json::from_str::<LitmCalibration>(&content)
        {
            return c;
        }
        LitmCalibration {
            schema_version: 1,
            ..Default::default()
        }
    }

    fn save_to_disk(&self) {
        let path = store_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Team-merge (#550): element-wise maximum per counter. Cumulative
    /// counters only ever grow, so `max` is idempotent on re-import and never
    /// double-counts; divergent machines converge to the strongest evidence.
    pub fn merge_from(&mut self, other: &Self) {
        for (profile, theirs) in &other.per_profile {
            let ours = self.per_profile.entry(profile.clone()).or_default();
            ours.begin_hits = ours.begin_hits.max(theirs.begin_hits);
            ours.begin_misses = ours.begin_misses.max(theirs.begin_misses);
            ours.end_hits = ours.end_hits.max(theirs.end_hits);
            ours.end_misses = ours.end_misses.max(theirs.end_misses);
        }
    }

    pub fn record(&mut self, profile: &str, pos: Position, hit: bool) {
        let stats = self.per_profile.entry(profile.to_string()).or_default();
        match (pos, hit) {
            (Position::Begin, true) => stats.begin_hits += 1,
            (Position::Begin, false) => stats.begin_misses += 1,
            (Position::End, true) => stats.end_hits += 1,
            (Position::End, false) => stats.end_misses += 1,
        }
    }

    /// Calibrated begin-share for a profile. Returns the default until enough
    /// observations exist; afterwards shifts budget toward the position that
    /// empirically holds information for this client.
    #[must_use]
    pub fn begin_share(&self, profile: &str) -> f64 {
        let Some(stats) = self.per_profile.get(profile) else {
            return DEFAULT_BEGIN_SHARE;
        };
        if stats.total() < MIN_OBSERVATIONS {
            return DEFAULT_BEGIN_SHARE;
        }
        let hb = stats.hit_rate(Position::Begin);
        let he = stats.hit_rate(Position::End);
        let raw = hb / (hb + he);
        // Re-center: equal hit rates map to the default layout, not to 0.5.
        let share = DEFAULT_BEGIN_SHARE + (raw - 0.5) * 2.0 * (1.0 - DEFAULT_BEGIN_SHARE);
        share.clamp(SHARE_CLAMP.0, SHARE_CLAMP.1)
    }

    /// Aggregate raw counters across profiles (#549 efficacy snapshots):
    /// `(begin_hits, begin_misses, end_hits, end_misses)`.
    #[must_use]
    pub fn totals(&self) -> (u32, u32, u32, u32) {
        self.per_profile.values().fold((0, 0, 0, 0), |acc, s| {
            (
                acc.0 + s.begin_hits,
                acc.1 + s.begin_misses,
                acc.2 + s.end_hits,
                acc.3 + s.end_misses,
            )
        })
    }

    #[must_use]
    pub fn report_lines(&self) -> Vec<String> {
        let mut profiles: Vec<_> = self.per_profile.iter().collect();
        profiles.sort_by(|a, b| a.0.cmp(b.0));
        profiles
            .iter()
            .map(|(name, s)| {
                format!(
                    "  {name}: begin {}/{} hit, end {}/{} hit -> share {:.2}",
                    s.begin_hits,
                    s.begin_hits + s.begin_misses,
                    s.end_hits,
                    s.end_hits + s.end_misses,
                    self.begin_share(name)
                )
            })
            .collect()
    }
}

fn with_buffer<R>(f: impl FnOnce(&mut LitmCalibration) -> R) -> R {
    let mut guard = BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some((LitmCalibration::load_from_disk(), Instant::now()));
    }
    let (cal, last_flush) = guard.as_mut().expect("buffer initialized above");
    let result = f(cal);
    if last_flush.elapsed().as_secs() >= FLUSH_SECS {
        cal.save_to_disk();
        *last_flush = Instant::now();
    }
    result
}

/// Process-global: record a placement outcome.
pub fn record_outcome(profile: &str, pos: Position, hit: bool) {
    if profile.is_empty() {
        return;
    }
    with_buffer(|c| c.record(profile, pos, hit));
}

/// Process-global: calibrated begin-share for a profile.
#[must_use]
pub fn begin_share(profile: &str) -> f64 {
    with_buffer(|c| c.begin_share(profile))
}

/// Process-global: flush to disk (shutdown paths).
pub fn flush() {
    let guard = BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((ref cal, _)) = *guard {
        cal.save_to_disk();
    }
}

/// Process-global: report lines for `ctx_metrics`.
#[must_use]
pub fn report() -> Vec<String> {
    with_buffer(|c| c.report_lines())
}

/// Process-global aggregate counters (#549).
#[must_use]
pub fn totals() -> (u32, u32, u32, u32) {
    with_buffer(|c| c.totals())
}

/// Process-global: machine-readable snapshot for the dashboard (#548):
/// `(profile, stats, calibrated begin_share)`, sorted by profile.
#[must_use]
pub fn snapshot() -> Vec<(String, PlacementStats, f64)> {
    with_buffer(|c| {
        let mut v: Vec<_> = c
            .per_profile
            .iter()
            .map(|(p, s)| (p.clone(), s.clone(), c.begin_share(p)))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    })
}

/// Process-global: clone of the full calibration state for export (#550).
#[must_use]
pub fn export_state() -> LitmCalibration {
    with_buffer(|c| c.clone())
}

/// Process-global: merge a foreign calibration in and persist (#550).
pub fn merge_state(other: &LitmCalibration) {
    with_buffer(|c| c.merge_from(other));
    flush();
}

/// Loose match between a recall query and a manifest key: lowercase
/// containment either way, or token-Jaccard >= 0.5.
#[must_use]
pub fn key_matches(manifest_key: &str, query: &str) -> bool {
    let k = manifest_key.to_lowercase();
    let q = query.to_lowercase();
    if k.len() >= 6 && q.len() >= 6 && (k.contains(&q) || q.contains(&k)) {
        return true;
    }
    let ks: std::collections::HashSet<&str> = k.split_whitespace().collect();
    let qs: std::collections::HashSet<&str> = q.split_whitespace().collect();
    if ks.is_empty() || qs.is_empty() {
        return false;
    }
    let inter = ks.intersection(&qs).count() as f64;
    let union = ks.union(&qs).count() as f64;
    inter / union >= 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_share_before_min_observations() {
        let mut c = LitmCalibration::default();
        for _ in 0..MIN_OBSERVATIONS - 1 {
            c.record("claude", Position::Begin, false);
        }
        assert!((c.begin_share("claude") - DEFAULT_BEGIN_SHARE).abs() < f64::EPSILON);
    }

    #[test]
    fn begin_miss_series_lowers_share() {
        let mut c = LitmCalibration::default();
        for _ in 0..30 {
            c.record("claude", Position::Begin, false);
            c.record("claude", Position::End, true);
        }
        let share = c.begin_share("claude");
        assert!(
            share < DEFAULT_BEGIN_SHARE,
            "begin misses should lower share, got {share}"
        );
        assert!(share >= SHARE_CLAMP.0);
    }

    #[test]
    fn end_miss_series_raises_share() {
        let mut c = LitmCalibration::default();
        for _ in 0..30 {
            c.record("gpt", Position::Begin, true);
            c.record("gpt", Position::End, false);
        }
        let share = c.begin_share("gpt");
        assert!(share > DEFAULT_BEGIN_SHARE);
        assert!(share <= SHARE_CLAMP.1);
    }

    #[test]
    fn balanced_hits_keep_default_layout() {
        let mut c = LitmCalibration::default();
        for _ in 0..50 {
            c.record("gemini", Position::Begin, true);
            c.record("gemini", Position::End, true);
        }
        assert!((c.begin_share("gemini") - DEFAULT_BEGIN_SHARE).abs() < 0.01);
    }

    #[test]
    fn unknown_profile_uses_default() {
        let c = LitmCalibration::default();
        assert!((c.begin_share("nope") - DEFAULT_BEGIN_SHARE).abs() < f64::EPSILON);
    }

    #[test]
    fn key_matching_containment_and_jaccard() {
        assert!(key_matches("billing webhook fix", "webhook fix"));
        assert!(key_matches(
            "stripe cancel_at parsing",
            "parsing stripe cancel_at"
        ));
        assert!(!key_matches("frontend css", "database migration"));
        assert!(key_matches("ab", "ab")); // identical tokens match via Jaccard
    }
}
