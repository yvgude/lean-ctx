//! Stigmergic pheromone signals for multi-agent coordination.
//!
//! Agents deposit ephemeral signals on files and symbols; strength decays
//! over time via periodic evaporation.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A pheromone signal left by an agent on a file/symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PheromoneSignal {
    /// Agent identifier (e.g., "cursor-12345").
    pub agent_id: String,
    /// Signal type.
    pub kind: SignalKind,
    /// File path this signal is attached to.
    pub path: String,
    /// Optional symbol name within the file.
    pub symbol: Option<String>,
    /// Signal strength (0.0-1.0), decays over time.
    pub strength: f64,
    /// When the signal was deposited.
    pub deposited_at: DateTime<Utc>,
    /// Contextual note.
    pub note: Option<String>,
}

/// Category of stigmergic signal deposited on a file or symbol.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SignalKind {
    /// Agent is actively working on this file.
    Active,
    /// Agent found this area complex/tricky.
    Complexity,
    /// Agent made changes that need review.
    ReviewNeeded,
    /// Agent encountered an issue here.
    Issue,
    /// Agent completed work here successfully.
    Completed,
    /// Agent explored/read this file without claiming active work.
    Exploration,
    /// Agent modified this file.
    Modification,
}

/// In-memory signal store (per-session; persisted via IPC for cross-agent).
static SIGNALS: Mutex<Vec<PheromoneSignal>> = Mutex::new(Vec::new());

/// Deposit a new pheromone signal (alias: emit a signal into the store).
pub fn deposit_signal(mut signal: PheromoneSignal) {
    signal.strength = bounded(signal.strength);
    signals().push(signal);
}

/// Read all signals for a given path, optionally filtered by kind.
pub fn read_signals(path: &str, kind: Option<SignalKind>) -> Vec<PheromoneSignal> {
    signals()
        .iter()
        .filter(|signal| signal.path == path && kind.is_none_or(|kind| signal.kind == kind))
        .cloned()
        .collect()
}

/// Evaporate signals: reduce strength by decay_rate, remove signals below threshold.
/// Called periodically to prevent stale signals from accumulating.
pub fn evaporate(decay_rate: f64, threshold: f64) {
    let decay_rate = bounded(decay_rate);
    let threshold = bounded(threshold);
    let mut signals = signals();

    for signal in signals.iter_mut() {
        signal.strength *= 1.0 - decay_rate;
    }
    signals.retain(|signal| signal.strength >= threshold);
}

/// Reset in-memory signals (called at session start).
pub fn reset_signals() {
    signals().clear();
}

fn signals() -> std::sync::MutexGuard<'static, Vec<PheromoneSignal>> {
    SIGNALS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn bounded(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn signal(path: &str, kind: SignalKind, strength: f64) -> PheromoneSignal {
        PheromoneSignal {
            agent_id: "codex-test".to_string(),
            kind,
            path: path.to_string(),
            symbol: None,
            strength,
            deposited_at: Utc::now(),
            note: None,
        }
    }

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_signals();
        guard
    }

    #[test]
    fn deposit_and_read_signals() {
        let _guard = setup();
        deposit_signal(signal("src/lib.rs", SignalKind::Active, 0.8));

        let found = read_signals("src/lib.rs", None);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agent_id, "codex-test");
        assert_eq!(found[0].strength, 0.8);
    }

    #[test]
    fn read_filters_by_path() {
        let _guard = setup();
        deposit_signal(signal("src/lib.rs", SignalKind::Active, 0.8));
        deposit_signal(signal("src/main.rs", SignalKind::Active, 0.6));

        let found = read_signals("src/main.rs", None);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "src/main.rs");
    }

    #[test]
    fn read_filters_by_kind() {
        let _guard = setup();
        deposit_signal(signal("src/lib.rs", SignalKind::Active, 0.8));
        deposit_signal(signal("src/lib.rs", SignalKind::ReviewNeeded, 0.6));

        let found = read_signals("src/lib.rs", Some(SignalKind::ReviewNeeded));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, SignalKind::ReviewNeeded);
    }

    #[test]
    fn evaporate_reduces_strength() {
        let _guard = setup();
        deposit_signal(signal("src/lib.rs", SignalKind::Active, 0.8));

        evaporate(0.25, 0.0);

        let found = read_signals("src/lib.rs", None);
        assert!((found[0].strength - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn evaporate_removes_weak_signals() {
        let _guard = setup();
        deposit_signal(signal("src/lib.rs", SignalKind::Active, 0.2));

        evaporate(0.5, 0.11);

        assert!(read_signals("src/lib.rs", None).is_empty());
    }
}
