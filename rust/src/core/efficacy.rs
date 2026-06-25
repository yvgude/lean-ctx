//! Learning-efficacy evidence (#549, VIS-3).
//!
//! The learning layers (#538-#544) adapt continuously — this module proves
//! whether the adaptation WORKS, with real telemetry instead of claims:
//!
//! - **Bounce-rate trend** straight from the savings ledger
//!   (`daily_bounce_trend`, #507): week-over-week rate of compressed reads
//!   that had to be re-read full. Learned thresholds (#538) must push this
//!   down.
//! - **LITM placement snapshots**: daily cumulative hit/miss counters from
//!   the calibration store (#539) so hit-rate movement is visible over time.
//! - **Playbook survival** (#541): share of entries that stayed net-helpful
//!   past 10 turns — the ACE quality proxy for "facts worth keeping".
//! - **Prevented duplicate work** (#540): lifetime count of rejected scent
//!   claims.
//!
//! Snapshots live in `~/.lean-ctx/efficacy_snapshots.json`, bounded to a
//! 30-day ring, captured lazily on every `ctx_metrics` call and on server
//! shutdown — no timers, no daemons.

use serde::{Deserialize, Serialize};

/// Ring size: 30 calendar days of snapshots.
const MAX_SNAPSHOTS: usize = 30;
/// Playbook entries older than this many turns count toward survival stats.
const SURVIVAL_AGE_TURNS: u32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EfficacySnapshot {
    /// Calendar day, `YYYY-MM-DD` (UTC).
    pub day: String,
    /// Cumulative LITM counters at capture time (#539).
    pub litm_begin_hits: u32,
    pub litm_begin_misses: u32,
    pub litm_end_hits: u32,
    pub litm_end_misses: u32,
    /// Cumulative rejected scent claims at capture time (#540).
    pub claims_rejected: u64,
    /// Playbook size and net-helpful aged entries at capture time (#541).
    pub playbook_entries: usize,
    pub playbook_aged_helpful: usize,
    pub playbook_aged_total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EfficacyStore {
    pub snapshots: Vec<EfficacySnapshot>,
    pub schema_version: u32,
}

fn store_path() -> std::path::PathBuf {
    crate::core::paths::cache_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("efficacy_snapshots.json")
}

impl EfficacyStore {
    fn load() -> Self {
        if let Ok(content) = std::fs::read_to_string(store_path())
            && let Ok(s) = serde_json::from_str::<EfficacyStore>(&content)
        {
            return s;
        }
        EfficacyStore {
            schema_version: 1,
            ..Default::default()
        }
    }

    fn save(&self) {
        let path = store_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Upsert today's snapshot (cumulative counters only move forward, so
    /// re-capturing within the same day just refreshes the values) and trim
    /// the ring.
    pub fn upsert(&mut self, snap: EfficacySnapshot) {
        match self.snapshots.iter_mut().find(|s| s.day == snap.day) {
            Some(existing) => *existing = snap,
            None => self.snapshots.push(snap),
        }
        self.snapshots.sort_by(|a, b| a.day.cmp(&b.day));
        if self.snapshots.len() > MAX_SNAPSHOTS {
            let excess = self.snapshots.len() - MAX_SNAPSHOTS;
            self.snapshots.drain(0..excess);
        }
    }
}

/// Build today's snapshot from the live stores.
fn current_snapshot() -> EfficacySnapshot {
    let (bh, bm, eh, em) = crate::core::litm_calibration::totals();

    let (entries, aged_helpful, aged_total) = crate::core::session::SessionState::load_latest()
        .map_or((0, 0, 0), |s| {
            let turn = s.stats.total_tool_calls;
            let aged: Vec<_> = s
                .playbook
                .entries
                .iter()
                .filter(|e| turn.saturating_sub(e.created_turn) >= SURVIVAL_AGE_TURNS)
                .collect();
            let helpful = aged
                .iter()
                .filter(|e| e.helpful_votes >= e.harmful_votes)
                .count();
            (s.playbook.entries.len(), helpful, aged.len())
        });

    EfficacySnapshot {
        day: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        litm_begin_hits: bh,
        litm_begin_misses: bm,
        litm_end_hits: eh,
        litm_end_misses: em,
        claims_rejected: crate::core::scent_field::claims_rejected_total(),
        playbook_entries: entries,
        playbook_aged_helpful: aged_helpful,
        playbook_aged_total: aged_total,
    }
}

/// Capture (upsert) today's snapshot. Called from `ctx_metrics` and shutdown.
pub fn capture() {
    let mut store = EfficacyStore::load();
    store.upsert(current_snapshot());
    store.save();
}

/// Week-over-week bounce rates from the ledger: `(previous, recent)` as
/// `(rate, reads)` tuples over 7-day windows. `None` when a window has no
/// compressed reads to be honest about.
fn bounce_week_over_week() -> (Option<(f64, u64)>, Option<(f64, u64)>) {
    let trend = crate::core::savings_ledger::daily_bounce_trend(14);
    if trend.is_empty() {
        return (None, None);
    }
    let today = chrono::Utc::now().date_naive();
    let mut prev = (0u64, 0u64); // (bounces, reads) days 8-14
    let mut recent = (0u64, 0u64); // days 0-7
    for (day, bounces, reads) in &trend {
        let Ok(d) = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
            continue;
        };
        let age = (today - d).num_days();
        if age < 7 {
            recent.0 += bounces;
            recent.1 += reads;
        } else {
            prev.0 += bounces;
            prev.1 += reads;
        }
    }
    let rate = |(b, r): (u64, u64)| {
        if r == 0 {
            None
        } else {
            Some((b as f64 / r as f64, r))
        }
    };
    (rate(prev), rate(recent))
}

fn fmt_pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

/// Human-readable efficacy section for `ctx_metrics`.
#[must_use]
pub fn report() -> Vec<String> {
    let mut out = Vec::new();

    match bounce_week_over_week() {
        (Some((prev, prev_n)), Some((rec, rec_n))) => {
            let arrow = if rec < prev {
                "improving"
            } else if rec > prev {
                "regressing"
            } else {
                "flat"
            };
            out.push(format!(
                "bounce rate: {} (prev 7d, n={prev_n}) -> {} (last 7d, n={rec_n}) [{arrow}]",
                fmt_pct(prev),
                fmt_pct(rec)
            ));
        }
        (None, Some((rec, rec_n))) => {
            out.push(format!(
                "bounce rate: {} (last 7d, n={rec_n}) — no prior week yet",
                fmt_pct(rec)
            ));
        }
        _ => {}
    }

    let store = EfficacyStore::load();
    if let (Some(first), Some(last)) = (store.snapshots.first(), store.snapshots.last())
        && first.day != last.day
    {
        let hit_rate = |s: &EfficacySnapshot| {
            let hits = u64::from(s.litm_begin_hits) + u64::from(s.litm_end_hits);
            let total = hits + u64::from(s.litm_begin_misses) + u64::from(s.litm_end_misses);
            if total == 0 {
                None
            } else {
                Some(hits as f64 / total as f64)
            }
        };
        if let (Some(a), Some(b)) = (hit_rate(first), hit_rate(last)) {
            out.push(format!(
                "litm placement hits: {} ({}) -> {} ({})",
                fmt_pct(a),
                first.day,
                fmt_pct(b),
                last.day
            ));
        }
        let delta = last.claims_rejected.saturating_sub(first.claims_rejected);
        if delta > 0 {
            out.push(format!(
                "duplicate work prevented: {delta} rejected claim(s) since {}",
                first.day
            ));
        }
    }
    if let Some(last) = store.snapshots.last()
        && last.playbook_aged_total > 0
    {
        out.push(format!(
            "playbook survival: {}/{} aged entries net-helpful ({})",
            last.playbook_aged_helpful,
            last.playbook_aged_total,
            fmt_pct(last.playbook_aged_helpful as f64 / last.playbook_aged_total as f64)
        ));
    }

    out
}

/// Machine-readable efficacy for the dashboard (#548).
#[must_use]
pub fn report_json() -> serde_json::Value {
    let (prev, recent) = bounce_week_over_week();
    let store = EfficacyStore::load();
    serde_json::json!({
        "bounce": {
            "prev_week": prev.map(|(r, n)| serde_json::json!({"rate": r, "reads": n})),
            "last_week": recent.map(|(r, n)| serde_json::json!({"rate": r, "reads": n})),
            "daily": crate::core::savings_ledger::daily_bounce_trend(14)
                .into_iter()
                .map(|(d, b, r)| serde_json::json!({"day": d, "bounces": b, "reads": r}))
                .collect::<Vec<_>>(),
        },
        "snapshots": store.snapshots,
        "claims_rejected_total": crate::core::scent_field::claims_rejected_total(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(day: &str, claims: u64) -> EfficacySnapshot {
        EfficacySnapshot {
            day: day.to_string(),
            litm_begin_hits: 10,
            litm_begin_misses: 2,
            litm_end_hits: 5,
            litm_end_misses: 3,
            claims_rejected: claims,
            playbook_entries: 4,
            playbook_aged_helpful: 3,
            playbook_aged_total: 4,
        }
    }

    #[test]
    fn upsert_replaces_same_day_and_appends_new() {
        let mut s = EfficacyStore::default();
        s.upsert(snap("2026-06-10", 1));
        s.upsert(snap("2026-06-10", 2));
        assert_eq!(s.snapshots.len(), 1);
        assert_eq!(s.snapshots[0].claims_rejected, 2);
        s.upsert(snap("2026-06-11", 3));
        assert_eq!(s.snapshots.len(), 2);
    }

    #[test]
    fn ring_is_bounded_to_30_days() {
        let mut s = EfficacyStore::default();
        for i in 0..40 {
            s.upsert(snap(&format!("2026-05-{:02}", i % 31 + 1), i));
        }
        // 31 distinct days collapse into <=30 after trimming, oldest dropped.
        assert!(s.snapshots.len() <= MAX_SNAPSHOTS);
        assert!(s.snapshots.first().unwrap().day.as_str() > "2026-05-01");
    }

    #[test]
    fn snapshots_stay_sorted_by_day() {
        let mut s = EfficacyStore::default();
        s.upsert(snap("2026-06-11", 1));
        s.upsert(snap("2026-06-09", 1));
        s.upsert(snap("2026-06-10", 1));
        let days: Vec<&str> = s.snapshots.iter().map(|x| x.day.as_str()).collect();
        assert_eq!(days, vec!["2026-06-09", "2026-06-10", "2026-06-11"]);
    }

    #[test]
    fn current_snapshot_has_today() {
        let snap = current_snapshot();
        assert_eq!(snap.day, chrono::Utc::now().format("%Y-%m-%d").to_string());
    }
}
