use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const BOUNCE_WINDOW: u64 = 5;
const BOUNCE_RATE_THRESHOLD: f64 = 0.30;
/// Seq-tick window during which a full read is treated as *edit-forced* rather
/// than a compression bounce. Must match the window `should_force_full` uses to
/// escalate post-edit reads to `full`, so the read we forced is never blamed on
/// the compression arm (GL #622).
const EDIT_FORCE_WINDOW: u64 = 10;
/// Outer-map retention: a path whose newest activity is older than this many seq ticks
/// can no longer satisfy `BOUNCE_WINDOW` (5) or the edit-force window (10), so it is inert
/// and safe to evict. Kept well above both windows to never change detection outcomes.
const TRACKED_PATH_TTL_SEQ: u64 = 64;

#[derive(Debug, Clone)]
struct ReadEvent {
    _mode: String,
    tokens_sent: usize,
    _original_tokens: usize,
    seq: u64,
    was_compressed: bool,
}

#[derive(Debug, Default)]
struct BounceStats {
    total_reads: u64,
    bounces: u64,
    wasted_tokens: usize,
}

#[derive(Debug, Default)]
pub struct BounceTracker {
    recent_reads: HashMap<String, Vec<ReadEvent>>,
    per_extension: HashMap<String, BounceStats>,
    recently_edited: HashMap<String, u64>,
    seq_counter: u64,
    total_bounces: u64,
    total_wasted_tokens: usize,
    /// When true (only for the process-global tracker), detected bounces are appended to
    /// the persistent savings ledger so a fresh `gain` process sees historical bounce.
    /// Local trackers in unit tests leave this `false` to avoid touching the real ledger.
    persist: bool,
}

fn is_compressed_mode(mode: &str) -> bool {
    !matches!(mode, "full" | "diff")
}

fn extension_of(path: &str) -> String {
    path.rsplit('.')
        .next()
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default()
}

impl BounceTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_seq(&mut self) -> u64 {
        self.seq_counter += 1;
        self.seq_counter
    }

    pub fn set_seq(&mut self, seq: u64) {
        self.seq_counter = seq;
    }

    pub fn record_read(
        &mut self,
        path: &str,
        mode: &str,
        tokens_sent: usize,
        original_tokens: usize,
    ) {
        let norm = crate::core::pathutil::normalize_tool_path(path);
        let seq = self.seq_counter;
        let compressed = is_compressed_mode(mode);

        if !compressed {
            self.detect_bounce(&norm, seq);
        }
        if self.persist {
            // Keep the long-term majority rule honest: clean reads dilute
            // historical bounces (#496).
            crate::core::path_mode_memory::record_read_if_tracked(&norm);
        }

        let events = self.recent_reads.entry(norm).or_default();
        events.push(ReadEvent {
            _mode: mode.to_string(),
            tokens_sent,
            _original_tokens: original_tokens,
            seq,
            was_compressed: compressed,
        });

        if events.len() > 10 {
            events.drain(..events.len() - 10);
        }

        let ext = extension_of(path);
        if !ext.is_empty() {
            let stats = self.per_extension.entry(ext).or_default();
            stats.total_reads += 1;
        }

        self.prune_stale_paths();
    }

    fn detect_bounce(&mut self, norm_path: &str, full_seq: u64) {
        // A full re-read the system itself forced after an edit is not a
        // compression failure (GL #622): `should_force_full` returns `full` for
        // EDIT_FORCE_WINDOW ticks post-edit, so the agent had no choice. Counting
        // it as a bounce would penalize the compression arm for the edit and
        // inflate the per-extension bounce rate until `should_force_full` pins the
        // whole extension to `full` — a self-reinforcing loss of compression.
        if let Some(&edit_seq) = self.recently_edited.get(norm_path)
            && full_seq.saturating_sub(edit_seq) <= EDIT_FORCE_WINDOW
        {
            return;
        }

        let Some(events) = self.recent_reads.get(norm_path) else {
            return;
        };

        if let Some(ev) = events.iter().next_back()
            && ev.was_compressed
            && full_seq.saturating_sub(ev.seq) <= BOUNCE_WINDOW
        {
            let wasted = ev.tokens_sent;
            self.total_bounces += 1;
            self.total_wasted_tokens += wasted;

            let ext = extension_of(norm_path);
            if !ext.is_empty() {
                let stats = self.per_extension.entry(ext).or_default();
                stats.bounces += 1;
                stats.wasted_tokens += wasted;
            }

            if self.persist {
                crate::core::savings_ledger::record_bounce_event(wasted);
                // Long-term per-path memory (#496): remember which exact
                // files keep bouncing so auto-mode learns across restarts.
                crate::core::path_mode_memory::record_bounce(norm_path);
                // Quality signal (#538): bounces push the learned entropy
                // threshold down for this extension (compress less) and
                // penalize the bandit arm that produced the read (#593).
                crate::core::adaptive_thresholds::record_quality_signal(
                    norm_path,
                    crate::core::threshold_learning::QualitySignal::Bounce,
                );
                // Stigmergy (#540): a bounce marks this path as Stuck so
                // other agents see friction here. Background: lock may block.
                let scent_path = norm_path.to_string();
                std::thread::spawn(move || {
                    crate::core::scent_field::deposit(
                        crate::core::scent_field::scent_agent_id(),
                        crate::core::scent_field::ScentKind::Stuck,
                        &scent_path,
                        0.5,
                    );
                });
            }
        }
    }

    pub fn record_shell_file_access(&mut self, path: &str) {
        let norm = crate::core::pathutil::normalize_tool_path(path);
        let seq = self.seq_counter;
        self.detect_bounce(&norm, seq);
    }

    pub fn record_edit(&mut self, path: &str) {
        let norm = crate::core::pathutil::normalize_tool_path(path);
        self.recently_edited.insert(norm, self.seq_counter);
        self.prune_stale_paths();
    }

    /// Evict outer-map entries whose newest seq is older than the detection windows —
    /// they can no longer affect bounce detection or `should_force_full`. Bounds the
    /// `recent_reads` / `recently_edited` maps on a long-lived process.
    fn prune_stale_paths(&mut self) {
        let seq = self.seq_counter;
        self.recent_reads.retain(|_, events| {
            events
                .last()
                .is_some_and(|e| seq.saturating_sub(e.seq) <= TRACKED_PATH_TTL_SEQ)
        });
        self.recently_edited
            .retain(|_, &mut edit_seq| seq.saturating_sub(edit_seq) <= TRACKED_PATH_TTL_SEQ);
    }

    #[must_use]
    pub fn should_force_full(&self, path: &str) -> bool {
        let norm = crate::core::pathutil::normalize_tool_path(path);

        if let Some(&edit_seq) = self.recently_edited.get(&norm)
            && self.seq_counter.saturating_sub(edit_seq) <= EDIT_FORCE_WINDOW
        {
            return true;
        }

        let ext = extension_of(path);
        if !ext.is_empty()
            && let Some(stats) = self.per_extension.get(&ext)
            && stats.total_reads >= 3
        {
            let rate = stats.bounces as f64 / stats.total_reads as f64;
            if rate >= BOUNCE_RATE_THRESHOLD {
                return true;
            }
        }

        false
    }

    #[must_use]
    pub fn bounce_rate_for_extension(&self, path: &str) -> Option<f64> {
        let ext = extension_of(path);
        self.per_extension.get(&ext).and_then(|s| {
            if s.total_reads >= 3 {
                Some(s.bounces as f64 / s.total_reads as f64)
            } else {
                None
            }
        })
    }

    #[must_use]
    pub fn total_bounces(&self) -> u64 {
        self.total_bounces
    }

    #[must_use]
    pub fn total_wasted_tokens(&self) -> usize {
        self.total_wasted_tokens
    }

    #[must_use]
    pub fn adjusted_savings(&self, raw_savings: usize) -> isize {
        raw_savings as isize - self.total_wasted_tokens as isize
    }

    #[must_use]
    pub fn per_extension_json(&self) -> Vec<serde_json::Value> {
        let mut exts: Vec<_> = self
            .per_extension
            .iter()
            .filter(|(_, s)| s.total_reads > 0)
            .collect();
        exts.sort_by_key(|a| std::cmp::Reverse(a.1.bounces));
        exts.iter()
            .take(10)
            .map(|(ext, stats)| {
                let rate = if stats.total_reads > 0 {
                    stats.bounces as f64 / stats.total_reads as f64
                } else {
                    0.0
                };
                serde_json::json!({
                    "ext": ext,
                    "reads": stats.total_reads,
                    "bounces": stats.bounces,
                    "wasted_tokens": stats.wasted_tokens,
                    "rate": (rate * 1000.0).round() / 1000.0,
                })
            })
            .collect()
    }

    #[must_use]
    pub fn format_summary(&self) -> String {
        if self.total_bounces == 0 {
            return "Bounces: 0".to_string();
        }
        let mut lines = vec![format!(
            "Bounces: {} ({} wasted tokens)",
            self.total_bounces, self.total_wasted_tokens
        )];
        let mut exts: Vec<_> = self
            .per_extension
            .iter()
            .filter(|(_, s)| s.bounces > 0)
            .collect();
        exts.sort_by_key(|a| std::cmp::Reverse(a.1.bounces));
        for (ext, stats) in exts.iter().take(5) {
            let rate = if stats.total_reads > 0 {
                stats.bounces as f64 / stats.total_reads as f64 * 100.0
            } else {
                0.0
            };
            lines.push(format!(
                "  {ext}: {}/{} reads bounced ({rate:.0}%), {} tok wasted",
                stats.bounces, stats.total_reads, stats.wasted_tokens,
            ));
        }
        lines.join("\n")
    }
}

static GLOBAL_TRACKER: OnceLock<Mutex<BounceTracker>> = OnceLock::new();

pub fn global() -> &'static Mutex<BounceTracker> {
    GLOBAL_TRACKER.get_or_init(|| {
        // Seed from the persistent ledger so every process (including a fresh `gain`)
        // accounts for historical bounce, then mark this tracker as the persisting one.
        let summary = crate::core::savings_ledger::summary();
        let mut bt = BounceTracker::new();
        bt.total_wasted_tokens = summary.bounce_tokens as usize;
        bt.total_bounces = summary.bounce_events as u64;
        bt.persist = true;
        Mutex::new(bt)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_bounce_when_first_read_is_full() {
        let mut bt = BounceTracker::new();
        bt.seq_counter = 1;
        bt.record_read("src/main.rs", "full", 500, 500);
        assert_eq!(bt.total_bounces(), 0);
        assert_eq!(bt.total_wasted_tokens(), 0);
    }

    #[test]
    fn bounce_detected_on_compressed_then_full() {
        let mut bt = BounceTracker::new();
        bt.seq_counter = 1;
        bt.record_read("src/main.rs", "map", 50, 500);
        bt.seq_counter = 2;
        bt.record_read("src/main.rs", "full", 500, 500);
        assert_eq!(bt.total_bounces(), 1);
        assert_eq!(bt.total_wasted_tokens(), 50);
    }

    #[test]
    fn no_bounce_outside_window() {
        let mut bt = BounceTracker::new();
        bt.seq_counter = 1;
        bt.record_read("src/main.rs", "map", 50, 500);
        bt.seq_counter = 10;
        bt.record_read("src/main.rs", "full", 500, 500);
        assert_eq!(bt.total_bounces(), 0);
    }

    #[test]
    fn shell_access_triggers_bounce() {
        let mut bt = BounceTracker::new();
        bt.seq_counter = 1;
        bt.record_read("config.yml", "signatures", 30, 400);
        bt.seq_counter = 3;
        bt.record_shell_file_access("config.yml");
        assert_eq!(bt.total_bounces(), 1);
        assert_eq!(bt.total_wasted_tokens(), 30);
    }

    #[test]
    fn edit_forced_full_read_is_not_a_bounce() {
        // GL #622: a compressed overview read, then an edit, then the `full`
        // re-read that `should_force_full` mandates must NOT register as a bounce
        // — the edit forced it, the compression did not fail.
        let mut bt = BounceTracker::new();
        bt.seq_counter = 1;
        bt.record_read("src/lib.rs", "map", 40, 500);
        bt.seq_counter = 2;
        bt.record_edit("src/lib.rs");
        bt.seq_counter = 4;
        bt.record_read("src/lib.rs", "full", 500, 500);
        assert_eq!(
            bt.total_bounces(),
            0,
            "edit-forced full read must not count as a compression bounce"
        );
    }

    #[test]
    fn full_read_without_edit_still_bounces() {
        // The guard is scoped to edit-forced reads only: an unprompted full
        // re-read after a compressed read is still a real bounce.
        let mut bt = BounceTracker::new();
        bt.seq_counter = 1;
        bt.record_read("src/lib.rs", "map", 40, 500);
        bt.seq_counter = 3;
        bt.record_read("src/lib.rs", "full", 500, 500);
        assert_eq!(bt.total_bounces(), 1);
    }

    #[test]
    fn should_force_full_after_edit() {
        let mut bt = BounceTracker::new();
        bt.seq_counter = 5;
        bt.record_edit("src/lib.rs");
        bt.seq_counter = 8;
        assert!(bt.should_force_full("src/lib.rs"));
        bt.seq_counter = 20;
        assert!(!bt.should_force_full("src/lib.rs"));
    }

    #[test]
    fn should_force_full_by_extension_bounce_rate() {
        let mut bt = BounceTracker::new();
        for i in 1..=6 {
            bt.seq_counter = i * 2 - 1;
            bt.record_read(&format!("f{i}.yml"), "map", 30, 400);
            bt.seq_counter = i * 2;
            bt.record_read(&format!("f{i}.yml"), "full", 400, 400);
        }
        assert!(bt.should_force_full("new.yml"));
    }

    #[test]
    fn adjusted_savings_subtracts_waste() {
        let mut bt = BounceTracker::new();
        bt.seq_counter = 1;
        bt.record_read("a.rs", "map", 50, 500);
        bt.seq_counter = 2;
        bt.record_read("a.rs", "full", 500, 500);
        assert_eq!(bt.adjusted_savings(1000), 950);
    }

    #[test]
    fn bounce_rate_for_extension_below_minimum() {
        let bt = BounceTracker::new();
        assert!(bt.bounce_rate_for_extension("test.rs").is_none());
    }

    #[test]
    fn format_summary_empty() {
        let bt = BounceTracker::new();
        assert_eq!(bt.format_summary(), "Bounces: 0");
    }
}
