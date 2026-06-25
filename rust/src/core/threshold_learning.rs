//! Online-learned per-extension compression-threshold deltas (#538, EFF-1).
//!
//! Closes the quality feedback loop that the static `LANGUAGE_THRESHOLDS`
//! table and the savings-only bandit cannot: bounces (compressed read followed
//! by a full re-read) and edit failures after compressed reads push the
//! entropy threshold DOWN (compress less), while clean compressed reads and
//! wasted full reads push it UP (compress more). The learned delta is additive
//! on top of the static base table, hard-clamped so the base stays the safety
//! anchor, and decays toward zero daily so stale lessons fade.
//!
//! Neuroscience analogue: dopaminergic active forgetting — eviction policy is
//! learned from outcome signals, not hardcoded (`SleepGate` 2603.14517).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Learning rate per signal; signal weights below multiply this.
const LR: f64 = 0.02;
/// Learned delta never exceeds ±CLAMP — static table stays the anchor.
const CLAMP: f64 = 0.15;
/// Deltas only apply once an extension has this many observations.
const MIN_SAMPLES: u32 = 10;
/// Daily multiplicative decay toward 0 (drift back to the base table).
const DAILY_DECAY: f64 = 0.98;
/// Flush to disk at most this often.
const FLUSH_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualitySignal {
    /// Compressed read was followed by a full re-read within the bounce window.
    Bounce,
    /// An edit failed after the file was last read in a compressed mode.
    EditFail,
    /// A compressed read that (so far) was not bounced.
    CleanCompressed,
    /// A large full read of an extension that never bounces — compression
    /// would almost certainly have been safe.
    WastedFull,
}

impl QualitySignal {
    /// Signed weight: negative lowers the entropy threshold (compress less),
    /// positive raises it (compress more aggressively).
    fn weight(self) -> f64 {
        match self {
            QualitySignal::Bounce => -3.0,
            QualitySignal::EditFail => -6.0,
            QualitySignal::CleanCompressed => 0.5,
            QualitySignal::WastedFull => 2.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LearnedDelta {
    pub delta_entropy: f64,
    pub samples: u32,
    /// Unix epoch day of the last decay application.
    pub last_decay_day: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThresholdLearner {
    /// Keyed by lowercase extension without dot (e.g. "rs").
    pub per_ext: HashMap<String, LearnedDelta>,
    pub schema_version: u32,
}

static BUFFER: Mutex<Option<(ThresholdLearner, Instant)>> = Mutex::new(None);

fn store_path() -> std::path::PathBuf {
    crate::core::paths::cache_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("thresholds_learned.json")
}

fn epoch_day(now_secs: u64) -> u64 {
    now_secs / 86_400
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl ThresholdLearner {
    fn load_from_disk() -> Self {
        let path = store_path();
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(learner) = serde_json::from_str::<ThresholdLearner>(&content)
        {
            return learner;
        }
        ThresholdLearner {
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

    /// Team-merge (#550): sample-weighted average of deltas, never a blind
    /// overwrite. `samples = max(...)` (not the sum) and weighted-mean deltas
    /// make re-importing the same bundle a no-op (idempotent roundtrip) and
    /// prevent double counting. Clamps stay authoritative.
    pub fn merge_from(&mut self, other: &Self) {
        for (ext, theirs) in &other.per_ext {
            match self.per_ext.get_mut(ext) {
                None => {
                    let mut d = theirs.clone();
                    d.delta_entropy = d.delta_entropy.clamp(-CLAMP, CLAMP);
                    self.per_ext.insert(ext.clone(), d);
                }
                Some(ours) => {
                    let total = u64::from(ours.samples) + u64::from(theirs.samples);
                    if theirs.samples == 0 || total == 0 {
                        continue;
                    }
                    let weighted = (ours.delta_entropy * f64::from(ours.samples)
                        + theirs.delta_entropy * f64::from(theirs.samples))
                        / total as f64;
                    ours.delta_entropy = weighted.clamp(-CLAMP, CLAMP);
                    ours.samples = ours.samples.max(theirs.samples);
                    ours.last_decay_day = ours.last_decay_day.max(theirs.last_decay_day);
                }
            }
        }
    }

    /// Apply one quality signal for `ext` at `now` (unix seconds).
    pub fn record(&mut self, ext: &str, signal: QualitySignal, now: u64) {
        let ext = normalize_ext(ext);
        if ext.is_empty() {
            return;
        }
        let day = epoch_day(now);
        let entry = self.per_ext.entry(ext).or_default();
        Self::apply_decay(entry, day);
        entry.delta_entropy = (entry.delta_entropy + LR * signal.weight()).clamp(-CLAMP, CLAMP);
        entry.samples = entry.samples.saturating_add(1);
    }

    /// Additive entropy-threshold delta for `ext`, or 0.0 before `MIN_SAMPLES`.
    pub fn delta_for(&mut self, ext: &str, now: u64) -> f64 {
        let ext = normalize_ext(ext);
        let day = epoch_day(now);
        match self.per_ext.get_mut(&ext) {
            Some(entry) => {
                Self::apply_decay(entry, day);
                if entry.samples >= MIN_SAMPLES {
                    entry.delta_entropy
                } else {
                    0.0
                }
            }
            None => 0.0,
        }
    }

    fn apply_decay(entry: &mut LearnedDelta, today: u64) {
        if entry.last_decay_day == 0 {
            entry.last_decay_day = today;
            return;
        }
        let days = today.saturating_sub(entry.last_decay_day);
        if days > 0 {
            // Cap the exponent: after ~1 year of inactivity the delta is ~0 anyway.
            let factor = DAILY_DECAY.powi(days.min(365) as i32);
            entry.delta_entropy *= factor;
            entry.last_decay_day = today;
        }
    }

    /// One line per learned extension, for `ctx_metrics`.
    #[must_use]
    pub fn report_lines(&self) -> Vec<String> {
        let mut exts: Vec<_> = self.per_ext.iter().collect();
        exts.sort_by(|a, b| a.0.cmp(b.0));
        exts.iter()
            .map(|(ext, d)| {
                let active = if d.samples >= MIN_SAMPLES {
                    "active"
                } else {
                    "warmup"
                };
                format!(
                    "  .{ext}: delta={:+.3} (n={}, {active})",
                    d.delta_entropy, d.samples
                )
            })
            .collect()
    }
}

fn normalize_ext(ext: &str) -> String {
    ext.trim_start_matches('.').to_ascii_lowercase()
}

fn with_buffer<R>(f: impl FnOnce(&mut ThresholdLearner) -> R) -> R {
    let mut guard = BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some((ThresholdLearner::load_from_disk(), Instant::now()));
    }
    let (learner, last_flush) = guard.as_mut().expect("buffer initialized above");
    let result = f(learner);
    if last_flush.elapsed().as_secs() >= FLUSH_SECS {
        learner.save_to_disk();
        *last_flush = Instant::now();
    }
    result
}

/// Process-global: record a quality signal for the extension of `path`.
pub fn record_signal(path: &str, signal: QualitySignal) {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    if ext.is_empty() {
        return;
    }
    with_buffer(|l| l.record(&ext, signal, now_secs()));
}

/// Process-global: learned additive delta for the extension (0.0 in warmup).
#[must_use]
pub fn learned_delta(ext: &str) -> f64 {
    with_buffer(|l| l.delta_for(ext, now_secs()))
}

/// Process-global: flush the buffer to disk (call from shutdown paths).
pub fn flush() {
    let guard = BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((ref learner, _)) = *guard {
        learner.save_to_disk();
    }
}

/// Process-global: report lines for `ctx_metrics`.
#[must_use]
pub fn report() -> Vec<String> {
    with_buffer(|l| l.report_lines())
}

/// Process-global: machine-readable snapshot for the dashboard (#548),
/// sorted by extension.
#[must_use]
pub fn snapshot() -> Vec<(String, LearnedDelta)> {
    with_buffer(|l| {
        let mut v: Vec<_> = l
            .per_ext
            .iter()
            .map(|(k, d)| (k.clone(), d.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    })
}

/// Process-global: clone of the full learner state for export (#550).
#[must_use]
pub fn export_state() -> ThresholdLearner {
    with_buffer(|l| l.clone())
}

/// Process-global: merge a foreign learner state in and persist (#550).
pub fn merge_state(other: &ThresholdLearner) {
    with_buffer(|l| l.merge_from(other));
    flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_780_000_000;

    #[test]
    fn warmup_returns_zero_delta() {
        let mut l = ThresholdLearner::default();
        for _ in 0..MIN_SAMPLES - 1 {
            l.record("rs", QualitySignal::Bounce, NOW);
        }
        assert_eq!(l.delta_for("rs", NOW), 0.0);
        l.record("rs", QualitySignal::Bounce, NOW);
        assert!(l.delta_for("rs", NOW) < 0.0);
    }

    #[test]
    fn bounce_burst_lowers_threshold() {
        let mut l = ThresholdLearner::default();
        for _ in 0..20 {
            l.record("yml", QualitySignal::Bounce, NOW);
        }
        let d = l.delta_for("yml", NOW);
        assert!(d < -0.05, "bounce burst should push delta down, got {d}");
    }

    #[test]
    fn edit_fail_is_stronger_than_bounce() {
        let mut l = ThresholdLearner::default();
        for _ in 0..MIN_SAMPLES {
            l.record("a", QualitySignal::Bounce, NOW);
            l.record("b", QualitySignal::EditFail, NOW);
        }
        assert!(l.delta_for("b", NOW) <= l.delta_for("a", NOW));
    }

    #[test]
    fn waste_burst_raises_threshold() {
        let mut l = ThresholdLearner::default();
        for _ in 0..20 {
            l.record("json", QualitySignal::WastedFull, NOW);
        }
        let d = l.delta_for("json", NOW);
        assert!(d > 0.05, "waste burst should push delta up, got {d}");
    }

    #[test]
    fn clamp_holds_under_extreme_signals() {
        let mut l = ThresholdLearner::default();
        for _ in 0..500 {
            l.record("rs", QualitySignal::EditFail, NOW);
        }
        assert!(l.delta_for("rs", NOW) >= -CLAMP - f64::EPSILON);
        for _ in 0..2000 {
            l.record("rs", QualitySignal::WastedFull, NOW);
        }
        assert!(l.delta_for("rs", NOW) <= CLAMP + f64::EPSILON);
    }

    #[test]
    fn decay_drifts_back_toward_zero() {
        let mut l = ThresholdLearner::default();
        for _ in 0..30 {
            l.record("rs", QualitySignal::Bounce, NOW);
        }
        let before = l.delta_for("rs", NOW);
        let after = l.delta_for("rs", NOW + 30 * 86_400);
        assert!(
            after.abs() < before.abs(),
            "30 days of decay should shrink |delta|: {before} -> {after}"
        );
    }

    #[test]
    fn clean_reads_recover_after_bounces() {
        let mut l = ThresholdLearner::default();
        for _ in 0..15 {
            l.record("ts", QualitySignal::Bounce, NOW);
        }
        let low = l.delta_for("ts", NOW);
        for _ in 0..200 {
            l.record("ts", QualitySignal::CleanCompressed, NOW);
        }
        assert!(l.delta_for("ts", NOW) > low);
    }

    #[test]
    fn ext_normalization() {
        let mut l = ThresholdLearner::default();
        for _ in 0..MIN_SAMPLES {
            l.record(".RS", QualitySignal::Bounce, NOW);
        }
        assert!(l.delta_for("rs", NOW) < 0.0);
    }
}
