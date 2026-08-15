//! Privacy-first local efficiency leaderboard.
//!
//! Rankings are calculated from local savings and published aggregate percentile
//! bands. No local session data or agent identity is transmitted.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Duration, Utc};

use crate::core::savings_tracker::{self, SessionSavings, SessionSavingsTracker};
use crate::core::session::SessionState;

/// The local tracker used to calculate this agent's savings rank.
pub type SavingsTracker = SessionSavingsTracker;

/// Aggregate efficiency measurements at one percentile.
///
/// `cpao` is cost per accepted outcome in microdollars; lower values are better.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PercentileMetrics {
    pub savings_pct: f32,
    pub tokens_per_session: u64,
    pub cpao: u64,
}

/// Published anonymous aggregate usage bands for LeanCTX agents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PercentileTable {
    pub p10: PercentileMetrics,
    pub p25: PercentileMetrics,
    pub p50: PercentileMetrics,
    pub p75: PercentileMetrics,
    pub p90: PercentileMetrics,
    pub p95: PercentileMetrics,
    pub p99: PercentileMetrics,
}

/// Representative aggregate bands: normal sessions process 50k--500k tokens
/// and save 20--70%; the long tail captures power users and larger codebases.
pub const PERCENTILES: PercentileTable = PercentileTable {
    p10: PercentileMetrics {
        savings_pct: 20.0,
        tokens_per_session: 50_000,
        cpao: 80_000,
    },
    p25: PercentileMetrics {
        savings_pct: 28.0,
        tokens_per_session: 100_000,
        cpao: 60_000,
    },
    p50: PercentileMetrics {
        savings_pct: 40.0,
        tokens_per_session: 200_000,
        cpao: 40_000,
    },
    p75: PercentileMetrics {
        savings_pct: 55.0,
        tokens_per_session: 350_000,
        cpao: 25_000,
    },
    p90: PercentileMetrics {
        savings_pct: 65.0,
        tokens_per_session: 500_000,
        cpao: 16_000,
    },
    p95: PercentileMetrics {
        savings_pct: 70.0,
        tokens_per_session: 750_000,
        cpao: 12_000,
    },
    p99: PercentileMetrics {
        savings_pct: 80.0,
        tokens_per_session: 1_000_000,
        cpao: 6_000,
    },
};

/// A local-only view of the current agent's efficiency.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LeaderboardEntry {
    pub agent_id: String,
    pub sessions_count: u32,
    pub total_tokens_saved: u64,
    pub avg_savings_pct: f32,
    pub percentile_rank: u8,
    pub streak_days: u16,
}

const RANK_INTERVAL_MS: u64 = 30 * 60 * 1_000;
static LAST_RANK_SHOWN_MS: AtomicU64 = AtomicU64::new(0);

/// Computes a local rank using the current session's measured compression savings.
#[must_use]
pub fn compute_rank(savings: &SavingsTracker) -> LeaderboardEntry {
    compute_rank_from_summary(&savings.session_summary())
}

/// Computes the current process's local rank without exposing data externally.
#[must_use]
pub fn compute_current_rank() -> LeaderboardEntry {
    compute_rank_from_summary(&savings_tracker::session_summary())
}

fn compute_rank_from_summary(summary: &SessionSavings) -> LeaderboardEntry {
    let sessions = SessionState::list_sessions();
    let sessions_count = u32::try_from(sessions.len()).unwrap_or(u32::MAX);
    let saved_from_sessions = sessions.iter().map(|session| session.tokens_saved).sum();
    let total_tokens_saved = if saved_from_sessions == 0 {
        summary.savings_tokens
    } else {
        saved_from_sessions
    };

    LeaderboardEntry {
        agent_id: std::env::var("LEAN_CTX_AGENT_ID")
            .ok()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| "local-agent".to_owned()),
        sessions_count,
        total_tokens_saved,
        avg_savings_pct: summary.savings_percent as f32,
        percentile_rank: savings_percentile(summary.savings_percent as f32),
        streak_days: efficiency_streak_days(&sessions, summary.savings_tokens),
    }
}

fn savings_percentile(savings_pct: f32) -> u8 {
    if savings_pct <= 0.0 {
        return 0;
    }

    let points = [
        (PERCENTILES.p10.savings_pct, 10.0),
        (PERCENTILES.p25.savings_pct, 25.0),
        (PERCENTILES.p50.savings_pct, 50.0),
        (PERCENTILES.p75.savings_pct, 75.0),
        (PERCENTILES.p90.savings_pct, 90.0),
        (PERCENTILES.p95.savings_pct, 95.0),
        (PERCENTILES.p99.savings_pct, 99.0),
    ];

    if savings_pct < points[0].0 {
        return (savings_pct / points[0].0 * points[0].1).round() as u8;
    }

    for window in points.windows(2) {
        let [(lower_savings, lower_rank), (upper_savings, upper_rank)] = window else {
            unreachable!("percentile table windows always contain two entries");
        };
        if savings_pct <= *upper_savings {
            let fraction = (savings_pct - *lower_savings) / (*upper_savings - *lower_savings);
            return (*lower_rank + fraction * (*upper_rank - *lower_rank)).round() as u8;
        }
    }

    let max_savings = PERCENTILES.p99.savings_pct;
    (99.0 + (savings_pct - max_savings) / (100.0 - max_savings))
        .clamp(99.0, 100.0)
        .round() as u8
}

fn efficiency_streak_days(
    sessions: &[crate::core::session::SessionSummary],
    current_session_saved: u64,
) -> u16 {
    let today = Utc::now().date_naive();
    let mut active_days: HashSet<_> = sessions
        .iter()
        .filter(|session| session.tokens_saved > 0)
        .map(|session| session.updated_at.date_naive())
        .collect();
    if current_session_saved > 0 {
        active_days.insert(today);
    }

    let mut streak = 0_u16;
    while active_days.contains(&(today - Duration::days(i64::from(streak)))) {
        streak = streak.saturating_add(1);
    }
    streak
}

/// Formats the social-proof message displayed to the local agent.
#[must_use]
pub fn format_rank_message(entry: &LeaderboardEntry) -> String {
    let top_percent = 100_u8.saturating_sub(entry.percentile_rank).max(1);
    let mut message = format!(
        "Your agent is more efficient than {}% of LeanCTX users\nThis session saved {} tokens (top {}%)",
        entry.percentile_rank,
        format_tokens(entry.total_tokens_saved),
        top_percent,
    );
    if entry.streak_days > 0 {
        message.push_str(&format!(
            "\n{}-day efficiency streak! Keep it up.",
            entry.streak_days
        ));
    }
    message
}

fn format_tokens(tokens: u64) -> String {
    let mut formatted = tokens.to_string();
    let mut index = formatted.len() as isize - 3;
    while index > 0 {
        formatted.insert(index as usize, ',');
        index -= 3;
    }
    formatted
}

/// Returns whether a rank notification is outside the 30-minute cooldown.
#[must_use]
pub fn should_show_rank(last_shown: Option<u64>) -> bool {
    let now = now_ms();
    last_shown.is_none_or(|shown| now.saturating_sub(shown) >= RANK_INTERVAL_MS)
}

/// Supplies the optional proxy header only for an impressive, non-spammy rank.
pub fn rank_header_if_due() -> Option<String> {
    let entry = compute_current_rank();
    if entry.percentile_rank <= 50 {
        return None;
    }

    let last_shown = LAST_RANK_SHOWN_MS.load(Ordering::Relaxed);
    let last_shown = (last_shown != 0).then_some(last_shown);
    if !should_show_rank(last_shown) {
        return None;
    }

    LAST_RANK_SHOWN_MS.store(now_ms(), Ordering::Relaxed);
    Some(format!(
        "top-{}%",
        100_u8.saturating_sub(entry.percentile_rank).max(1)
    ))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_savings_is_zero_percentile() {
        let tracker = SavingsTracker::default();
        assert_eq!(compute_rank(&tracker).percentile_rank, 0);
    }

    #[test]
    fn typical_savings_land_near_the_middle() {
        let mut tracker = SavingsTracker::default();
        tracker.record_compression(1_000, 600, "test");
        let rank = compute_rank(&tracker).percentile_rank;
        assert!(
            (50..=70).contains(&rank),
            "expected middle rank, got {rank}"
        );
    }

    #[test]
    fn excellent_savings_are_ninetieth_percentile_or_higher() {
        let mut tracker = SavingsTracker::default();
        tracker.record_compression(1_000, 300, "test");
        assert!(compute_rank(&tracker).percentile_rank >= 90);
    }

    #[test]
    fn rank_notification_is_rate_limited() {
        let now = now_ms();
        assert!(should_show_rank(None));
        assert!(!should_show_rank(Some(now - RANK_INTERVAL_MS + 1)));
        assert!(should_show_rank(Some(now - RANK_INTERVAL_MS)));
    }

    #[test]
    fn rank_message_uses_header_friendly_top_percent() {
        let entry = LeaderboardEntry {
            agent_id: "local-agent".to_owned(),
            sessions_count: 1,
            total_tokens_saved: 4_200,
            avg_savings_pct: 62.0,
            percentile_rank: 85,
            streak_days: 3,
        };
        assert_eq!(
            format_rank_message(&entry),
            "Your agent is more efficient than 85% of LeanCTX users\nThis session saved 4,200 tokens (top 15%)\n3-day efficiency streak! Keep it up."
        );
    }
}
