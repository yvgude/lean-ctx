//! Unified conservative quality loop (#CQ-07).
//!
//! High-confidence compressed-read edit misses and same-path compressed→full
//! bounces feed one bounded estimator keyed by extension, size bucket, and
//! delivered strategy. Two attributed negative samples enter risk; exponential
//! time decay and a lower exit threshold return the resolver to its configured
//! default. Clean outcomes do not promote compression. The v1 per-path edit
//! retry and anchored retry remain one-shot recovery behavior; legacy v1 pair
//! state is retained and migrated additively.
//!
//! Storage remains `~/.lean-ctx/edit_quality.json` (respecting
//! `LEAN_CTX_DATA_DIR`), atomic write (tmp+rename), loaded once per process,
//! flushed periodically like `path_mode_memory`.

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

/// Runtime signal state uses the existing edit-quality file and follows its
/// 30-day evidence horizon. A 15-day half-life makes two fresh high-confidence
/// signals fall below the 0.5 exit threshold at about the existing horizon.
const ESTIMATOR_HALF_LIFE_SECS: f64 = 15.0 * 24.0 * 3600.0;
// Half-life decay makes adjacent events fractional; 1.5 still requires two
// recent negative events while allowing their seconds-apart decay to count.
const ESTIMATOR_MIN_SAMPLES: f64 = 1.5;
const ESTIMATOR_ENTER_EVIDENCE: f64 = 1.5;
const ESTIMATOR_EXIT_EVIDENCE: f64 = 0.5;
const MAX_ESTIMATES: usize = 200;

static STORE: OnceLock<Mutex<EditQualityStore>> = OnceLock::new();
static RECORD_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct PairStats {
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

/// Size buckets use the same token boundaries that divide the built-in auto
/// resolver's small, medium, large, and very-large paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
enum SizeBucket {
    Small,
    Medium,
    Large,
    VeryLarge,
    Unknown,
}

impl SizeBucket {
    fn from_tokens(tokens: usize) -> Self {
        match tokens {
            0 => Self::Unknown,
            1..=500 => Self::Small,
            501..=2_000 => Self::Medium,
            2_001..=8_000 => Self::Large,
            _ => Self::VeryLarge,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct EstimateKey {
    language_or_ext: String,
    size_bucket: SizeBucket,
    mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EstimateStats {
    negative_evidence: f64,
    effective_samples: f64,
    edit_successes: f64,
    edit_failures: f64,
    full_reread_bounces: f64,
    risky: bool,
    last_updated_unix: u64,
    last_activity_unix: u64,
}

impl EstimateStats {
    fn decay(&mut self, now: u64) {
        if self.last_updated_unix == 0 {
            // A fresh entry has no elapsed evidence to decay. For older
            // partially populated state, start from its last activity so that
            // persisted evidence ages from the signal rather than the epoch.
            self.last_updated_unix = if self.last_activity_unix == 0 {
                now
            } else {
                self.last_activity_unix
            };
        }
        let elapsed = now.saturating_sub(self.last_updated_unix);
        if elapsed > 0 {
            let factor = 2.0_f64.powf(-(elapsed as f64) / ESTIMATOR_HALF_LIFE_SECS);
            self.negative_evidence *= factor;
            self.effective_samples *= factor;
            self.edit_successes *= factor;
            self.edit_failures *= factor;
            self.full_reread_bounces *= factor;
            self.last_updated_unix = now;
        }
        self.update_risk();
    }

    fn add_negative(&mut self, kind: RuntimeSignalKind, weight: f64, now: u64) {
        self.decay(now);
        self.negative_evidence += weight;
        self.effective_samples += weight;
        match kind {
            RuntimeSignalKind::EditFailureAfterCompressedRead => self.edit_failures += weight,
            RuntimeSignalKind::FullRereadBounce => self.full_reread_bounces += weight,
        }
        self.last_activity_unix = now;
        self.last_updated_unix = now;
        self.update_risk();
    }

    fn add_clean_edit(&mut self, weight: f64, now: u64) {
        self.decay(now);
        self.effective_samples += weight;
        self.edit_successes += weight;
        self.last_activity_unix = now;
        self.last_updated_unix = now;
        self.update_risk();
    }

    fn update_risk(&mut self) {
        if self.risky {
            if self.negative_evidence < ESTIMATOR_EXIT_EVIDENCE {
                self.risky = false;
            }
        } else if self.effective_samples >= ESTIMATOR_MIN_SAMPLES
            && self.negative_evidence >= ESTIMATOR_ENTER_EVIDENCE
            && self.negative_evidence / self.effective_samples >= RISKY_ENTER_RATE
        {
            self.risky = true;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EstimateEntry {
    key: EstimateKey,
    stats: EstimateStats,
    last_used: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConservativeQualityEstimator {
    entries: Vec<EstimateEntry>,
    access_clock: u64,
}

/// Only runtime events with a direct same-file, same-mode link enter the
/// estimator. The provenance and confidence are carried with the event until
/// it is recorded, then counters are persisted under the strategy key.
#[derive(Debug, Clone, Copy)]
enum RuntimeSignalKind {
    EditFailureAfterCompressedRead,
    FullRereadBounce,
}

#[derive(Debug, Clone, Copy)]
enum SignalProvenance {
    CtxEditOldStringMiss,
    CtxEditReplacementApplied,
    SamePathCompressedThenFullRead,
}

#[derive(Debug, Clone, Copy)]
enum AttributionConfidence {
    High,
}

impl AttributionConfidence {
    fn weight(self) -> f64 {
        match self {
            Self::High => 1.0,
        }
    }
}

#[derive(Debug, Clone)]
struct AttributedRuntimeSignal {
    kind: RuntimeSignalKind,
    provenance: SignalProvenance,
    confidence: AttributionConfidence,
    path: String,
    mode: String,
    original_tokens: usize,
}

#[derive(Debug, Clone)]
struct AttributedCleanEdit {
    provenance: SignalProvenance,
    confidence: AttributionConfidence,
    path: String,
    mode: String,
    original_tokens: usize,
}

impl ConservativeQualityEstimator {
    fn record(&mut self, signal: &AttributedRuntimeSignal, now: u64) -> bool {
        let provenance_matches = matches!(
            (signal.kind, signal.provenance),
            (
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                SignalProvenance::CtxEditOldStringMiss
            ) | (
                RuntimeSignalKind::FullRereadBounce,
                SignalProvenance::SamePathCompressedThenFullRead
            )
        );
        if !provenance_matches {
            return false;
        }
        if !is_compressed_read_mode(&signal.mode) {
            return false;
        }
        let Some(mode) = estimator_mode(&signal.mode) else {
            return false;
        };
        let key = EstimateKey {
            language_or_ext: ext_of(&signal.path),
            size_bucket: SizeBucket::from_tokens(signal.original_tokens),
            mode,
        };
        let index = self.ensure_entry(key, now);
        self.entries[index]
            .stats
            .add_negative(signal.kind, signal.confidence.weight(), now);
        true
    }

    fn record_clean_edit(&mut self, sample: &AttributedCleanEdit, now: u64) -> bool {
        if !matches!(
            sample.provenance,
            SignalProvenance::CtxEditReplacementApplied
        ) {
            return false;
        }
        if !is_compressed_read_mode(&sample.mode) {
            return false;
        }
        let Some(mode) = estimator_mode(&sample.mode) else {
            return false;
        };
        let key = EstimateKey {
            language_or_ext: ext_of(&sample.path),
            size_bucket: SizeBucket::from_tokens(sample.original_tokens),
            mode,
        };
        let index = self.ensure_entry(key, now);
        self.entries[index]
            .stats
            .add_clean_edit(sample.confidence.weight(), now);
        true
    }

    fn is_risky(&mut self, path: &str, mode: &str, original_tokens: usize, now: u64) -> bool {
        let Some(mode) = estimator_mode(mode) else {
            return false;
        };
        self.decay_and_prune(now);
        let ext = ext_of(path);
        let size_bucket = SizeBucket::from_tokens(original_tokens);
        let exact = EstimateKey {
            language_or_ext: ext.clone(),
            size_bucket,
            mode: mode.clone(),
        };
        let legacy = EstimateKey {
            language_or_ext: ext,
            size_bucket: SizeBucket::Unknown,
            mode,
        };
        let mut risky = false;
        for index in 0..self.entries.len() {
            let key = &self.entries[index].key;
            if key == &exact || (key == &legacy && legacy != exact) {
                self.access_clock = self.access_clock.saturating_add(1);
                self.entries[index].last_used = self.access_clock;
                self.entries[index].stats.decay(now);
                risky |= self.entries[index].stats.risky;
            }
        }
        risky
    }

    fn import_legacy_risk(
        &mut self,
        ext: &str,
        mode: &str,
        fails: u32,
        successes: u32,
        last_fail: u64,
    ) -> bool {
        if !is_compressed_read_mode(mode) {
            return false;
        }
        let Some(mode) = estimator_mode(mode) else {
            return false;
        };
        let key = EstimateKey {
            language_or_ext: ext.to_ascii_lowercase(),
            size_bucket: SizeBucket::Unknown,
            mode,
        };
        if self.entries.iter().any(|entry| entry.key == key) {
            return false;
        }
        let index = self.ensure_entry(key, last_fail);
        let stats = &mut self.entries[index].stats;
        let imported_fails = f64::from(fails.max(RISKY_MIN_FAILS));
        let imported_successes = f64::from(successes);
        stats.negative_evidence = stats.negative_evidence.max(imported_fails);
        stats.effective_samples = stats
            .effective_samples
            .max(imported_fails + imported_successes);
        stats.edit_failures = stats.edit_failures.max(imported_fails);
        stats.edit_successes = stats.edit_successes.max(imported_successes);
        stats.risky = true;
        stats.last_updated_unix = last_fail;
        stats.last_activity_unix = last_fail;
        true
    }

    fn decay_and_prune(&mut self, now: u64) {
        for entry in &mut self.entries {
            entry.stats.decay(now);
        }
        let before = self.entries.len();
        self.entries
            .retain(|entry| now.saturating_sub(entry.stats.last_activity_unix) <= DECAY_SECS);
        if self.entries.len() != before {
            self.evict_to_cap();
        }
    }

    fn ensure_entry(&mut self, key: EstimateKey, now: u64) -> usize {
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            self.access_clock = self.access_clock.saturating_add(1);
            self.entries[index].last_used = self.access_clock;
            self.entries[index].stats.decay(now);
            return index;
        }
        if self.entries.len() >= MAX_ESTIMATES {
            self.evict_lru();
        }
        self.access_clock = self.access_clock.saturating_add(1);
        self.entries.push(EstimateEntry {
            key,
            stats: EstimateStats::default(),
            last_used: self.access_clock,
        });
        self.entries.len() - 1
    }

    fn evict_lru(&mut self) {
        if let Some((index, _)) = self
            .entries
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                left.last_used
                    .cmp(&right.last_used)
                    .then_with(|| left.key.cmp(&right.key))
            })
        {
            self.entries.remove(index);
        }
    }

    fn evict_to_cap(&mut self) -> bool {
        let mut evicted = false;
        while self.entries.len() > MAX_ESTIMATES {
            self.evict_lru();
            evicted = true;
        }
        evicted
    }
}

fn estimator_mode(mode: &str) -> Option<String> {
    use crate::tools::ctx_read::ReadMode;

    let parsed = mode.parse::<ReadMode>().ok()?;
    Some(match parsed {
        ReadMode::Lines(_) | ReadMode::LinesMulti(_) | ReadMode::LinesTail(_) => "lines".into(),
        ReadMode::Anchored(_) => "anchored".into(),
        ReadMode::Density(_) => "density".into(),
        other => other.to_string(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct EditQualityStore {
    /// Legacy key: `"{ext}|{mode}"` (e.g. `"rs|map"`). Kept for migration and
    /// existing metrics; new mode decisions use `ConservativeQualityEstimator`.
    pub pairs: HashMap<String, PairStats>,
    /// Normalized path -> unix time of the compression-correlated edit fail.
    pub pending_escalations: HashMap<String, u64>,
    /// Normalized path -> unix time of an anchored-edit (`ctx_patch`) staleness
    /// miss. The next auto read of that path resolves to `anchored` (not `full`),
    /// so the model gets fresh line anchors to retry by reference (#1008).
    /// `#[serde(default)]` keeps stores written before anchored editing loadable.
    #[serde(default)]
    pub pending_anchored_escalations: HashMap<String, u64>,
    /// Bounded strategy-keyed estimator added additively to the legacy store.
    /// Older `pairs` remain readable for metrics and migration.
    #[serde(default)]
    estimator: ConservativeQualityEstimator,
    /// All-time counter of consumed escalations (observability).
    #[serde(default)]
    pub escalations_served: u64,
    #[serde(skip)]
    dirty: bool,
}

fn pair_key(ext: &str, mode: &str) -> String {
    format!("{ext}|{mode}")
}

/// Evict the oldest entries of a `path -> timestamp` pending map down to
/// [`MAX_PENDING`]; flips `dirty` when anything was dropped. Shared by the
/// `full` and `anchored` escalation maps.
fn evict_pending_to_cap(map: &mut HashMap<String, u64>, dirty: &mut bool) {
    if map.len() <= MAX_PENDING {
        return;
    }
    let mut items: Vec<(String, u64)> = map.iter().map(|(k, ts)| (k.clone(), *ts)).collect();
    items.sort_by_key(|(_, ts)| *ts);
    let drop_n = map.len() - MAX_PENDING;
    for (key, _) in items.into_iter().take(drop_n) {
        map.remove(&key);
    }
    *dirty = true;
}

impl EditQualityStore {
    fn load_from_disk() -> Self {
        let Ok(raw) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        let mut store: Self = serde_json::from_str(&raw).unwrap_or_default();
        let now = now_unix();
        store.decay(now);
        store.migrate_legacy_risks();
        store
    }

    /// Copy active v1 `(ext, mode)` risk into an unknown-size estimator bucket.
    /// The old data has no size dimension, so this fallback preserves its
    /// conservative effect until its original 30-day evidence window expires.
    fn migrate_legacy_risks(&mut self) {
        let mut legacy: Vec<(String, PairStats)> = self
            .pairs
            .iter()
            .filter(|(_, stats)| stats.risky)
            .map(|(key, stats)| (key.clone(), stats.clone()))
            .collect();
        legacy.sort_by(|left, right| left.0.cmp(&right.0));
        for (key, stats) in legacy {
            let Some((ext, mode)) = key.split_once('|') else {
                continue;
            };
            self.dirty |= self.estimator.import_legacy_risk(
                ext,
                mode,
                stats.fails,
                stats.successes,
                stats.last_fail_unix,
            );
        }
        self.dirty |= self.estimator.evict_to_cap();
    }

    fn decay(&mut self, now: u64) {
        let before = self.pairs.len()
            + self.pending_escalations.len()
            + self.pending_anchored_escalations.len();
        let estimate_count = self.estimator.entries.len();
        self.pairs
            .retain(|_, s| now.saturating_sub(s.last_fail_unix) <= DECAY_SECS);
        self.pending_escalations
            .retain(|_, ts| now.saturating_sub(*ts) <= ESCALATION_TTL_SECS);
        self.pending_anchored_escalations
            .retain(|_, ts| now.saturating_sub(*ts) <= ESCALATION_TTL_SECS);
        self.estimator.decay_and_prune(now);
        if self.pairs.len()
            + self.pending_escalations.len()
            + self.pending_anchored_escalations.len()
            != before
            || self.estimator.entries.len() != estimate_count
        {
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
        evict_pending_to_cap(&mut self.pending_escalations, &mut self.dirty);
        evict_pending_to_cap(&mut self.pending_anchored_escalations, &mut self.dirty);
        self.dirty |= self.estimator.evict_to_cap();
    }

    pub(crate) fn record_failure(&mut self, ext: &str, mode: &str, now: u64) {
        let entry = self.pairs.entry(pair_key(ext, mode)).or_default();
        entry.fails = entry.fails.saturating_add(1);
        entry.last_fail_unix = now;
        entry.update_risky();
        self.dirty = true;
        self.evict_to_caps();
    }

    pub(crate) fn record_success(&mut self, ext: &str, mode: &str) {
        let entry = self.pairs.entry(pair_key(ext, mode)).or_default();
        entry.successes = entry.successes.saturating_add(1);
        entry.update_risky();
        self.dirty = true;
    }

    fn record_attributed_signal(&mut self, signal: &AttributedRuntimeSignal, now: u64) {
        if self.estimator.record(signal, now) {
            self.dirty = true;
            self.evict_to_caps();
        }
    }

    fn record_attributed_clean_edit(&mut self, sample: &AttributedCleanEdit, now: u64) {
        if self.estimator.record_clean_edit(sample, now) {
            self.dirty = true;
            self.evict_to_caps();
        }
    }

    pub(crate) fn set_pending_escalation(&mut self, norm_path: &str, now: u64) {
        self.pending_escalations.insert(norm_path.to_string(), now);
        self.dirty = true;
        self.evict_to_caps();
    }

    /// Consumes the escalation for this path if present and not expired.
    pub(crate) fn take_pending_escalation(&mut self, norm_path: &str, now: u64) -> bool {
        Self::take_from(
            &mut self.pending_escalations,
            norm_path,
            now,
            &mut self.escalations_served,
            &mut self.dirty,
        )
    }

    pub(crate) fn set_pending_anchored_escalation(&mut self, norm_path: &str, now: u64) {
        self.pending_anchored_escalations
            .insert(norm_path.to_string(), now);
        self.dirty = true;
        self.evict_to_caps();
    }

    /// Consumes the anchored escalation for this path if present and not expired.
    pub(crate) fn take_pending_anchored_escalation(&mut self, norm_path: &str, now: u64) -> bool {
        Self::take_from(
            &mut self.pending_anchored_escalations,
            norm_path,
            now,
            &mut self.escalations_served,
            &mut self.dirty,
        )
    }

    /// Shared one-shot consume: remove `norm_path`, count it served when still
    /// within [`ESCALATION_TTL_SECS`], else drop it silently.
    fn take_from(
        map: &mut HashMap<String, u64>,
        norm_path: &str,
        now: u64,
        served: &mut u64,
        dirty: &mut bool,
    ) -> bool {
        match map.remove(norm_path) {
            Some(ts) if now.saturating_sub(ts) <= ESCALATION_TTL_SECS => {
                *served += 1;
                *dirty = true;
                true
            }
            Some(_) => {
                *dirty = true;
                false
            }
            None => false,
        }
    }

    fn is_risky(&mut self, path: &str, mode: &str, original_tokens: usize, now: u64) -> bool {
        let risky = self.estimator.is_risky(path, mode, original_tokens, now);
        // Querying advances exponential decay and LRU recency; flush those
        // persisted fields through the existing every-10-recordings cadence.
        self.dirty = true;
        risky
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
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Process-global: record the outcome of an edit, correlated with the mode of
/// the last read of that file. `last_mode` must be the recorded read mode
/// (empty = file was never read through lean-ctx → no signal, skipped).
/// Compression-correlated failures additionally arm the one-shot per-path
/// escalation so the next auto read of `path` resolves to `full`.
pub(crate) fn record_edit_outcome(path: &str, last_mode: &str, success: bool) {
    record_outcome_with(path, last_mode, success, Escalation::Full);
}

/// Like [`record_edit_outcome`], but a failure is a `ctx_patch` anchor-staleness
/// miss: the recovery is a *fresh anchored read* (the model edits by reference),
/// so the next auto read escalates to `anchored` instead of `full` (#1008).
pub(crate) fn record_anchored_edit_outcome(path: &str, last_mode: &str, success: bool) {
    record_outcome_with(path, last_mode, success, Escalation::Anchored);
}

/// Which read mode the *next* auto read escalates to after a correlated edit
/// failure. Both are high-signal "the context the model edited against was
/// wrong" events; they differ only in the recovery view handed back.
#[derive(Clone, Copy)]
enum Escalation {
    /// str_replace miss → give the real body (`full`).
    Full,
    /// anchored miss → give fresh line anchors (`anchored`).
    Anchored,
}

impl Escalation {
    /// The read mode that fully neutralizes this failure class, hence the value
    /// to *not* re-arm against (escalating `full→full` / `anchored→anchored` is a
    /// no-op).
    fn target_mode(self) -> &'static str {
        match self {
            Escalation::Full => "full",
            Escalation::Anchored => "anchored",
        }
    }
}

fn record_outcome_with(path: &str, last_mode: &str, success: bool, esc: Escalation) {
    if last_mode.is_empty() {
        return;
    }
    // Read provenance before taking the edit-quality lock. Bounce recording
    // takes its lock before this store, so reversing that order could deadlock.
    let (signal, clean_edit) = if success {
        (
            None,
            clean_edit_sample(
                path,
                last_mode,
                crate::core::bounce_tracker::last_read_context(path),
                esc,
            ),
        )
    } else {
        (
            edit_failure_signal(
                path,
                last_mode,
                crate::core::bounce_tracker::last_read_context(path),
                esc,
            ),
            None,
        )
    };
    let attributed = signal.is_some();
    let ext = ext_of(path);
    let Ok(mut store) = global().lock() else {
        return;
    };
    if success {
        store.record_success(&ext, last_mode);
        if let Some(sample) = clean_edit {
            store.record_attributed_clean_edit(&sample, now_unix());
        }
    } else {
        let now = now_unix();
        if signal.is_some() || matches!(esc, Escalation::Anchored) {
            store.record_failure(&ext, last_mode, now);
        }
        if let Some(signal) = signal {
            store.record_attributed_signal(&signal, now);
        }
        if last_mode != esc.target_mode() {
            let norm = crate::core::pathutil::normalize_tool_path(path);
            match esc {
                Escalation::Full => store.set_pending_escalation(&norm, now),
                Escalation::Anchored => store.set_pending_anchored_escalation(&norm, now),
            }
            // Keep the existing bandit penalty only for the same high-confidence
            // compressed-read miss admitted to the conservative estimator.
            if attributed {
                crate::core::adaptive_thresholds::record_quality_signal(
                    path,
                    crate::core::threshold_learning::QualitySignal::EditFail,
                );
            }
            // Stigmergy (#540): edit failures mark the path as Stuck ("context
            // drifted"), the explicit anchor-miss signal called for in #1008.
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

fn edit_failure_signal(
    path: &str,
    last_mode: &str,
    read_context: Option<(String, usize)>,
    escalation: Escalation,
) -> Option<AttributedRuntimeSignal> {
    if !matches!(escalation, Escalation::Full) {
        return None;
    }
    let (mode, original_tokens) = read_context?;
    if mode != last_mode || !is_compressed_read_mode(&mode) {
        return None;
    }
    Some(AttributedRuntimeSignal {
        kind: RuntimeSignalKind::EditFailureAfterCompressedRead,
        provenance: SignalProvenance::CtxEditOldStringMiss,
        confidence: AttributionConfidence::High,
        path: path.to_string(),
        mode,
        original_tokens,
    })
}

fn clean_edit_sample(
    path: &str,
    last_mode: &str,
    read_context: Option<(String, usize)>,
    escalation: Escalation,
) -> Option<AttributedCleanEdit> {
    if !matches!(escalation, Escalation::Full) {
        return None;
    }
    let (mode, original_tokens) = read_context?;
    if mode != last_mode || !is_compressed_read_mode(&mode) {
        return None;
    }
    Some(AttributedCleanEdit {
        provenance: SignalProvenance::CtxEditReplacementApplied,
        confidence: AttributionConfidence::High,
        path: path.to_string(),
        mode,
        original_tokens,
    })
}

/// Process-global: one-shot check-and-consume of the per-path `full` escalation.
pub(crate) fn take_pending_escalation(path: &str) -> bool {
    consume_escalation(path, false)
}

/// Process-global: one-shot check-and-consume of the per-path `anchored`
/// escalation (armed by [`record_anchored_edit_outcome`]).
pub(crate) fn take_pending_anchored_escalation(path: &str) -> bool {
    consume_escalation(path, true)
}

/// Record a full re-read bounce only when the tracker observed the same path,
/// compressed source mode, and non-edit-forced follow-up in its short window.
pub(crate) fn record_full_reread_bounce(path: &str, mode: &str, original_tokens: usize) {
    if !is_compressed_read_mode(mode) {
        return;
    }
    let signal = AttributedRuntimeSignal {
        kind: RuntimeSignalKind::FullRereadBounce,
        provenance: SignalProvenance::SamePathCompressedThenFullRead,
        confidence: AttributionConfidence::High,
        path: path.to_string(),
        mode: mode.to_string(),
        original_tokens,
    };
    let Ok(mut store) = global().lock() else {
        return;
    };
    store.record_attributed_signal(&signal, now_unix());
    maybe_flush(&mut store);
}

fn consume_escalation(path: &str, anchored: bool) -> bool {
    let norm = crate::core::pathutil::normalize_tool_path(path);
    let Ok(mut store) = global().lock() else {
        return false;
    };
    let now = now_unix();
    let hit = if anchored {
        store.take_pending_anchored_escalation(&norm, now)
    } else {
        store.take_pending_escalation(&norm, now)
    };
    if hit {
        maybe_flush(&mut store);
    }
    hit
}

fn is_compressed_read_mode(mode: &str) -> bool {
    mode.parse::<crate::tools::ctx_read::ReadMode>()
        .is_ok_and(|parsed| parsed.counts_as_compressed())
}

/// Process-global: is this `(extension, size bucket, mode)` currently risky?
pub(crate) fn is_risky_mode(path: &str, mode: &str, original_tokens: usize) -> bool {
    let Ok(mut store) = global().lock() else {
        return false;
    };
    let risky = store.is_risky(path, mode, original_tokens, now_unix());
    if store.dirty {
        maybe_flush(&mut store);
    }
    risky
}

/// Snapshot for `ctx_metrics`: (risky pairs, per-pair stats, escalations served).
pub(crate) fn metrics_snapshot() -> serde_json::Value {
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
    let mut estimates: Vec<serde_json::Value> = store
        .estimator
        .entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "language_or_ext": entry.key.language_or_ext,
                "size_bucket": entry.key.size_bucket,
                "mode": entry.key.mode,
                "negative_evidence": (entry.stats.negative_evidence * 1000.0).round() / 1000.0,
                "effective_samples": (entry.stats.effective_samples * 1000.0).round() / 1000.0,
                "edit_successes": (entry.stats.edit_successes * 1000.0).round() / 1000.0,
                "edit_failures": (entry.stats.edit_failures * 1000.0).round() / 1000.0,
                "full_reread_bounces": (entry.stats.full_reread_bounces * 1000.0).round() / 1000.0,
                "risky": entry.stats.risky,
            })
        })
        .collect();
    estimates.sort_by(|a, b| {
        a["language_or_ext"]
            .as_str()
            .cmp(&b["language_or_ext"].as_str())
            .then_with(|| {
                a["size_bucket"]
                    .to_string()
                    .cmp(&b["size_bucket"].to_string())
            })
            .then_with(|| a["mode"].as_str().cmp(&b["mode"].as_str()))
    });
    serde_json::json!({
        "pairs": pairs,
        "strategies": estimates,
        "pending_escalations": store.pending_escalations.len(),
        "pending_anchored_escalations": store.pending_anchored_escalations.len(),
        "escalations_served": store.escalations_served,
    })
}

pub(crate) fn flush() {
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
    fn legacy_pairs_keep_their_v1_hysteresis_for_additive_migration() {
        let mut s = EditQualityStore::default();
        s.record_failure("rs", "map", 1000);
        assert!(!s.pairs.get("rs|map").is_some_and(|p| p.risky));
        s.record_failure("rs", "map", 1001);
        assert!(s.pairs.get("rs|map").is_some_and(|p| p.risky));

        for _ in 0..11 {
            s.record_success("rs", "map");
        }
        assert!(s.pairs.get("rs|map").is_some_and(|p| p.risky));
        s.record_success("rs", "map");
        assert!(!s.pairs.get("rs|map").is_some_and(|p| p.risky));
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
        assert!(!s.pairs.get("ts|signatures").is_some_and(|p| p.risky));
        s.record_failure("ts", "signatures", 1002);
        // 3/10 = 0.30 — now risky.
        assert!(s.pairs.get("ts|signatures").is_some_and(|p| p.risky));
    }

    #[test]
    fn legacy_risk_migrates_to_unknown_size_without_removing_v1_data() {
        let mut s = EditQualityStore::default();
        s.record_failure("rs", "map", 1000);
        s.record_failure("rs", "map", 1001);
        s.migrate_legacy_risks();
        assert!(s.pairs.contains_key("rs|map"), "v1 data remains readable");
        assert!(s.estimator.is_risky("src/main.rs", "map", 2_000, 1001));
        assert!(!s.estimator.is_risky("src/main.py", "map", 2_000, 1001));
        assert!(
            !s.estimator
                .is_risky("src/main.rs", "signatures", 2_000, 1001)
        );
    }

    fn signal(
        kind: RuntimeSignalKind,
        path: &str,
        mode: &str,
        tokens: usize,
    ) -> AttributedRuntimeSignal {
        let provenance = match kind {
            RuntimeSignalKind::EditFailureAfterCompressedRead => {
                SignalProvenance::CtxEditOldStringMiss
            }
            RuntimeSignalKind::FullRereadBounce => SignalProvenance::SamePathCompressedThenFullRead,
        };
        AttributedRuntimeSignal {
            kind,
            provenance,
            confidence: AttributionConfidence::High,
            path: path.to_string(),
            mode: mode.to_string(),
            original_tokens: tokens,
        }
    }

    fn clean_sample(path: &str, mode: &str, tokens: usize) -> AttributedCleanEdit {
        clean_edit_sample(
            path,
            mode,
            Some((mode.to_string(), tokens)),
            Escalation::Full,
        )
        .expect("matching compressed ctx_edit success is attributable")
    }

    #[test]
    fn estimator_preserves_v1_minimum_and_enter_rate() {
        let mut store = EditQualityStore::default();
        for i in 0..7 {
            store.record_attributed_clean_edit(&clean_sample("a.rs", "map", 900), 1_000 + i);
        }
        store.record_attributed_signal(
            &signal(
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                "a.rs",
                "map",
                900,
            ),
            1_010,
        );
        assert!(!store.is_risky("a.rs", "map", 900, 1_010));
        store.record_attributed_signal(
            &signal(
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                "a.rs",
                "map",
                900,
            ),
            1_011,
        );
        // 2/9 is below the retained v1 25% enter rate.
        assert!(!store.is_risky("a.rs", "map", 900, 1_011));
        store.record_attributed_signal(
            &signal(
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                "a.rs",
                "map",
                900,
            ),
            1_012,
        );
        // 3/10 clears both the two-failure minimum and the 25% rate threshold.
        assert!(store.is_risky("a.rs", "map", 900, 1_012));
    }

    #[test]
    fn two_attributed_negative_samples_only_escalate_their_strategy_key() {
        let mut s = EditQualityStore::default();
        s.record_attributed_signal(
            &signal(
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                "src/main.rs",
                "map",
                1_000,
            ),
            1_000,
        );
        assert!(!s.is_risky("src/main.rs", "map", 1_000, 1_000));
        assert_eq!(
            crate::tools::ctx_read::mode::more_conservative("map", "full"),
            "full"
        );

        s.record_attributed_signal(
            &signal(
                RuntimeSignalKind::FullRereadBounce,
                "src/main.rs",
                "map",
                1_000,
            ),
            1_001,
        );
        assert!(s.is_risky("src/main.rs", "map", 1_000, 1_001));
        assert!(!s.is_risky("src/other.py", "map", 1_000, 1_001));
        assert!(!s.is_risky("src/main.rs", "signatures", 1_000, 1_001));
        assert!(!s.is_risky("src/main.rs", "map", 3_000, 1_001));
    }

    #[test]
    fn hysteresis_holds_risk_between_enter_and_exit_after_mixed_signals() {
        let mut store = EditQualityStore::default();
        let t0 = 1_000;
        store.record_attributed_signal(
            &signal(
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                "a.rs",
                "map",
                900,
            ),
            t0,
        );
        store.record_attributed_signal(
            &signal(RuntimeSignalKind::FullRereadBounce, "a.rs", "map", 900),
            t0 + 1,
        );
        assert!(store.is_risky("a.rs", "map", 900, t0 + 1));
        for i in 0..8 {
            store.record_attributed_clean_edit(&clean_sample("a.rs", "map", 900), t0 + 2 + i);
        }

        let half_life = ESTIMATOR_HALF_LIFE_SECS as u64;
        assert!(store.is_risky("a.rs", "map", 900, t0 + half_life));
        store.record_attributed_signal(
            &signal(
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                "a.rs",
                "map",
                900,
            ),
            t0 + half_life,
        );
        assert!(store.is_risky("a.rs", "map", 900, t0 + half_life));
    }

    #[test]
    fn decayed_evidence_returns_to_the_configured_default() {
        let mut estimator = ConservativeQualityEstimator::default();
        estimator.record(
            &signal(
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                "a.rs",
                "map",
                900,
            ),
            1_000,
        );
        estimator.record(
            &signal(
                RuntimeSignalKind::EditFailureAfterCompressedRead,
                "a.rs",
                "map",
                900,
            ),
            1_001,
        );
        assert!(estimator.is_risky("a.rs", "map", 900, 1_001));

        let default = "signatures";
        let after_decay = if estimator.is_risky("a.rs", "map", 900, 1_001 + DECAY_SECS + 1) {
            crate::tools::ctx_read::mode::more_conservative(default, "full")
        } else {
            default.to_string()
        };
        assert_eq!(after_decay, default);
    }

    #[test]
    fn estimator_is_bounded_and_evicts_least_recently_used_key() {
        let mut estimator = ConservativeQualityEstimator::default();
        let key = |i: usize| EstimateKey {
            language_or_ext: format!("e{i}"),
            size_bucket: SizeBucket::Small,
            mode: "map".to_string(),
        };
        for i in 0..MAX_ESTIMATES {
            estimator.ensure_entry(key(i), i as u64);
        }
        estimator.ensure_entry(key(0), MAX_ESTIMATES as u64);
        estimator.ensure_entry(key(MAX_ESTIMATES), (MAX_ESTIMATES + 1) as u64);
        assert_eq!(estimator.entries.len(), MAX_ESTIMATES);
        assert!(estimator.entries.iter().any(|entry| entry.key == key(0)));
        assert!(!estimator.entries.iter().any(|entry| entry.key == key(1)));
    }

    #[test]
    fn bare_or_mismatched_edit_failures_are_not_attributed() {
        assert!(edit_failure_signal("a.rs", "map", None, Escalation::Full).is_none());
        assert!(
            edit_failure_signal(
                "a.rs",
                "map",
                Some(("full".to_string(), 900)),
                Escalation::Full
            )
            .is_none()
        );
        assert!(
            edit_failure_signal(
                "a.rs",
                "map",
                Some(("signatures".to_string(), 900)),
                Escalation::Full,
            )
            .is_none()
        );
        assert!(
            edit_failure_signal(
                "a.rs",
                "full",
                Some(("full".to_string(), 900)),
                Escalation::Full
            )
            .is_none()
        );
        assert!(
            edit_failure_signal(
                "a.rs",
                "map",
                Some(("map".to_string(), 900)),
                Escalation::Anchored,
            )
            .is_none()
        );
        assert!(clean_edit_sample("a.rs", "map", None, Escalation::Full).is_none());
        assert!(
            clean_edit_sample(
                "a.rs",
                "map",
                Some(("full".to_string(), 900)),
                Escalation::Full,
            )
            .is_none()
        );
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
    fn anchored_escalation_is_independent_and_one_shot() {
        // #1008: the anchored map is separate from the `full` map — arming one
        // must never consume the other, so str_replace and ctx_patch recoveries
        // don't cross-talk.
        let mut s = EditQualityStore::default();
        s.set_pending_anchored_escalation("src/a.rs", 1000);
        assert!(
            !s.take_pending_escalation("src/a.rs", 1100),
            "anchored arming must not satisfy a full escalation"
        );
        assert!(s.take_pending_anchored_escalation("src/a.rs", 1100));
        assert!(
            !s.take_pending_anchored_escalation("src/a.rs", 1101),
            "anchored escalation is one-shot"
        );
        assert_eq!(s.escalations_served, 1);
    }

    #[test]
    fn anchored_outcome_arms_anchored_not_full() {
        // A miss after an anchored read arms only the anchored escalation.
        let mut s = EditQualityStore::default();
        s.record_failure("rs", "anchored", 1000);
        s.set_pending_anchored_escalation("src/x.rs", 1000);
        assert!(s.pending_escalations.is_empty());
        assert_eq!(s.pending_anchored_escalations.len(), 1);
    }

    #[test]
    fn store_without_anchored_field_deserializes() {
        // Back-compat (#1008): a store written before anchored editing has no
        // `pending_anchored_escalations` key; `#[serde(default)]` must fill it.
        let legacy = r#"{"pairs":{},"pending_escalations":{"old.rs":42}}"#;
        let s: EditQualityStore = serde_json::from_str(legacy).unwrap();
        assert!(s.pending_anchored_escalations.is_empty());
        assert!(s.pending_escalations.contains_key("old.rs"));
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
