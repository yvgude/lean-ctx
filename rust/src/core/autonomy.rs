use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::core::config::AutonomyConfig;

#[cfg(test)]
const SEARCH_REPEAT_IDLE_RESET: Duration = Duration::from_millis(500);
#[cfg(not(test))]
const SEARCH_REPEAT_IDLE_RESET: Duration = Duration::from_mins(5);

/// Per-key stats for progressive search hints (`ctx_search` / `ctx_semantic_search`).
#[derive(Debug, Clone)]
pub struct SearchHistory {
    pub call_count: u32,
    pub last_call: Instant,
}

/// Tracks autonomous action state independently of the MCP tool layer.
pub struct AutonomyState {
    pub session_initialized: AtomicBool,
    pub dedup_applied: AtomicBool,
    pub last_consolidation_unix: AtomicU64,
    pub config: AutonomyConfig,
    /// Repeated `pattern|path` keys for search tools (see [`AutonomyState::track_search`]).
    pub search_repetition: Mutex<HashMap<String, SearchHistory>>,
    /// One-shot keys for large-output hints (`ctx_shell` bytes, `ctx_read` full tokens).
    pub large_output_hints_shown: Mutex<HashSet<String>>,
}

impl Default for AutonomyState {
    fn default() -> Self {
        Self::new()
    }
}

impl AutonomyState {
    /// Creates a new autonomy state with config loaded from disk.
    pub fn new() -> Self {
        Self {
            session_initialized: AtomicBool::new(false),
            dedup_applied: AtomicBool::new(false),
            last_consolidation_unix: AtomicU64::new(0),
            config: AutonomyConfig::load(),
            search_repetition: Mutex::new(HashMap::new()),
            large_output_hints_shown: Mutex::new(HashSet::new()),
        }
    }

    /// Returns true if autonomous actions are enabled in configuration.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Records a search (`pattern` + `path` key) and returns a progressive hint after repeated calls.
    ///
    /// Uses interior mutability so this can be called on `Arc<AutonomyState>`. Counters reset when
    /// the idle gap since the last call for that key is at least five minutes (500ms in unit tests).
    pub fn track_search(&self, pattern: &str, path: &str) -> Option<String> {
        if !autonomy_enabled_effective(self) {
            return None;
        }
        let key = format!("{pattern}|{path}");
        let now = Instant::now();
        let mut map = self
            .search_repetition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hist = map.entry(key).or_insert(SearchHistory {
            call_count: 0,
            last_call: now,
        });
        if hist.last_call.elapsed() >= SEARCH_REPEAT_IDLE_RESET {
            hist.call_count = 0;
        }
        hist.call_count = hist.call_count.saturating_add(1);
        hist.last_call = now;
        let n = hist.call_count;

        match n {
            1..=3 => None,
            4..=6 => Some(format!(
                "[hint: repeated search ({n}/6). Consider ctx_knowledge remember to store findings]"
            )),
            _ => Some(format!(
                "[throttle: search repeated {n} times on same pattern. Use ctx_pack or ctx_knowledge to consolidate]"
            )),
        }
    }
}

fn autonomy_enabled_effective(state: &AutonomyState) -> bool {
    state.is_enabled()
        && crate::core::profiles::active_profile()
            .autonomy
            .enabled_effective()
}

/// Returns true if enough tool calls have elapsed to trigger auto-consolidation.
pub fn should_auto_consolidate(state: &AutonomyState, tool_calls: u32) -> bool {
    if !state.is_enabled() || !state.config.auto_consolidate {
        return false;
    }
    let every = state.config.consolidate_every_calls.max(1);
    if !tool_calls.is_multiple_of(every) {
        return false;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let last = state.last_consolidation_unix.load(Ordering::SeqCst);
    if now.saturating_sub(last) < state.config.consolidate_cooldown_secs {
        return false;
    }
    state.last_consolidation_unix.store(now, Ordering::SeqCst);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consolidation_respects_call_interval_and_cooldown() {
        let mut state = AutonomyState::new();
        state.config.enabled = true;
        state.config.auto_consolidate = true;
        state.config.consolidate_every_calls = 5;
        state.config.consolidate_cooldown_secs = 60;

        assert!(!should_auto_consolidate(&state, 4));
        assert!(should_auto_consolidate(&state, 5));
        assert!(!should_auto_consolidate(&state, 10));
    }

    #[test]
    fn consolidation_disabled_never_triggers() {
        let mut state = AutonomyState::new();
        state.config.enabled = false;
        state.config.auto_consolidate = true;
        state.config.consolidate_every_calls = 1;
        state.config.consolidate_cooldown_secs = 0;

        assert!(!should_auto_consolidate(&state, 1));
    }
}
