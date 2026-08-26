//! In-band behavior nudges + turn-economy telemetry.
//!
//! `tools health` measured two waste patterns this module targets (#848
//! follow-up):
//!
//! 1. **Heavy reads** — explicit `mode=full`/`raw` dominated recorded
//!    `ctx_read` modes (62% at introduction time) while the structural modes
//!    (`signatures`/`map`) cover orientation at a fraction of the tokens.
//! 2. **Tool-call chains** — `ctx_search` → `ctx_read` → `ctx_search`/
//!    `ctx_callgraph` sequences that a single `ctx_compose` call bundles;
//!    every extra turn re-reads the whole conversation server-side.
//!
//! The mechanism is one trailing hint line appended to a tool result at the
//! moment of waste — in-conversation context outweighs static rules. Hints
//! are budgeted hard: at most one hint per pattern per server process
//! (~30 tokens each), gated by the `behavior_nudges` config key
//! (`"auto"` default | `"off"`). Every detection is also counted in a small
//! local store so `tools health` reports the pattern honestly even with
//! hints off. Deterministic thresholds, local-only state, no network.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// An explicit full/raw read below this many output tokens is cheap enough to
/// ignore — nudging small reads would be noise.
const HEAVY_READ_TOKEN_FLOOR: usize = 1500;
/// Chain steps further apart than this are unrelated work, not a chain.
const CHAIN_WINDOW: Duration = Duration::from_mins(2);

/// Cumulative detection counters, persisted across sessions for
/// `tools health` (mirrors the adaptive-mode policy store pattern).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct BehaviorStore {
    /// Explicit full/raw reads at or above [`HEAVY_READ_TOKEN_FLOOR`].
    pub heavy_reads_detected: u64,
    /// search→read→search/callgraph sequences inside [`CHAIN_WINDOW`] steps.
    pub chains_detected: u64,
    /// Hints actually appended to a tool result.
    pub hints_shown: u64,
    pub updated_at: Option<String>,
}

impl BehaviorStore {
    pub(crate) fn load() -> Self {
        let Some(path) = store_path() else {
            return Self::default();
        };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = store_path() else {
            return;
        };
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        if let Ok(json) = serde_json::to_string(self) {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

fn store_path() -> Option<std::path::PathBuf> {
    crate::core::paths::state_dir()
        .ok()
        .map(|d| d.join("behavior_nudge.json"))
}

/// Per-process (session) state: the short recent-call window for chain
/// detection plus the once-per-session hint gates.
#[derive(Debug, Default)]
struct SessionState {
    recent: Vec<(&'static str, Instant)>,
    heavy_hint_shown: bool,
    chain_hint_shown: bool,
}

static SESSION: Mutex<Option<SessionState>> = Mutex::new(None);

/// Which detection (if any) a call triggered.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Detection {
    HeavyRead,
    Chain,
}

/// Classifies a tool call for the chain window. Only orientation tools
/// participate; a `ctx_compose` call is the desired end state and resets the
/// window instead of extending it.
fn chain_step(name: &str) -> Option<&'static str> {
    match name {
        "ctx_search" => Some("search"),
        "ctx_read" => Some("read"),
        "ctx_callgraph" => Some("graph"),
        _ => None,
    }
}

/// Pure detection core, separated from global state and the clock for tests.
fn detect(
    state: &mut SessionState,
    name: &str,
    explicit_mode: Option<&str>,
    output_tokens: usize,
    now: Instant,
) -> Option<Detection> {
    // Heavy read: explicit full/raw at meaningful size.
    if name == "ctx_read"
        && matches!(explicit_mode, Some("full" | "raw"))
        && output_tokens >= HEAVY_READ_TOKEN_FLOOR
    {
        return Some(Detection::HeavyRead);
    }

    if name == "ctx_compose" {
        state.recent.clear();
        return None;
    }
    let step = chain_step(name)?;
    state
        .recent
        .retain(|(_, at)| now.saturating_duration_since(*at) <= CHAIN_WINDOW);
    state.recent.push((step, now));
    if state.recent.len() > 3 {
        let excess = state.recent.len() - 3;
        state.recent.drain(0..excess);
    }
    let steps: Vec<&str> = state.recent.iter().map(|(s, _)| *s).collect();
    if steps == ["search", "read", "search"] || steps == ["search", "read", "graph"] {
        state.recent.clear();
        return Some(Detection::Chain);
    }
    None
}

/// The one-line hints. Kept short on purpose — a nudge that costs hundreds of
/// tokens would defeat itself.
fn hint_for(detection: Detection) -> &'static str {
    match detection {
        Detection::HeavyRead => {
            "[lean-ctx nudge] large explicit full/raw read — mode=signatures/map covers orientation at a fraction of the tokens; reserve full/raw for edit prep. (once per session; behavior_nudges=off to silence)"
        }
        Detection::Chain => {
            "[lean-ctx nudge] search→read→search chain — one ctx_compose call bundles search+read+symbols and saves the extra turns. (once per session; behavior_nudges=off to silence)"
        }
    }
}

/// Observes one dispatched tool call. Counts detections in the persistent
/// store and returns a hint line to append at most once per pattern per
/// session — `None` when there is nothing to say. `can_show` suppresses the
/// hint (machine-readable or firewalled results) without burning the
/// once-per-session budget; detections are counted either way. Called
/// post-dispatch, so it must stay cheap: no I/O unless a (rare) detection
/// fired.
pub(crate) fn observe(
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
    output_tokens: usize,
    can_show: bool,
) -> Option<String> {
    let nudges_on = can_show && crate::core::config::Config::load().behavior_nudges != "off";
    let explicit_mode = args
        .and_then(|a| a.get("mode"))
        .and_then(|m| m.as_str())
        .map(str::to_string);
    let mut guard = SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = guard.get_or_insert_with(SessionState::default);
    let detection = detect(
        state,
        name,
        explicit_mode.as_deref(),
        output_tokens,
        Instant::now(),
    )?;

    let mut store = BehaviorStore::load();
    match detection {
        Detection::HeavyRead => store.heavy_reads_detected += 1,
        Detection::Chain => store.chains_detected += 1,
    }
    let show = nudges_on
        && match detection {
            Detection::HeavyRead => !std::mem::replace(&mut state.heavy_hint_shown, true),
            Detection::Chain => !std::mem::replace(&mut state.chain_hint_shown, true),
        };
    if show {
        store.hints_shown += 1;
    }
    store.updated_at = Some(chrono::Utc::now().to_rfc3339());
    store.save();
    show.then(|| hint_for(detection).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SessionState {
        SessionState::default()
    }

    #[test]
    fn heavy_read_needs_explicit_heavy_mode_and_size() {
        let now = Instant::now();
        let mut s = state();
        assert_eq!(
            detect(&mut s, "ctx_read", Some("full"), 2000, now),
            Some(Detection::HeavyRead)
        );
        assert_eq!(detect(&mut s, "ctx_read", Some("full"), 200, now), None);
        assert_eq!(
            detect(&mut s, "ctx_read", Some("signatures"), 5000, now),
            None
        );
        assert_eq!(
            detect(&mut s, "ctx_read", None, 5000, now),
            None,
            "auto-resolved reads are the resolver's business, not the agent's"
        );
        assert_eq!(
            detect(&mut s, "ctx_read", Some("raw"), 1500, now),
            Some(Detection::HeavyRead)
        );
    }

    #[test]
    fn chain_detected_for_search_read_search_within_window() {
        let now = Instant::now();
        let mut s = state();
        assert_eq!(detect(&mut s, "ctx_search", None, 10, now), None);
        assert_eq!(detect(&mut s, "ctx_read", Some("map"), 10, now), None);
        assert_eq!(
            detect(&mut s, "ctx_search", None, 10, now),
            Some(Detection::Chain)
        );
        // Window cleared after a detection — no immediate re-fire.
        assert_eq!(detect(&mut s, "ctx_search", None, 10, now), None);
    }

    #[test]
    fn chain_accepts_callgraph_as_final_step() {
        let now = Instant::now();
        let mut s = state();
        detect(&mut s, "ctx_search", None, 10, now);
        detect(&mut s, "ctx_read", Some("map"), 10, now);
        assert_eq!(
            detect(&mut s, "ctx_callgraph", None, 10, now),
            Some(Detection::Chain)
        );
    }

    #[test]
    fn compose_resets_the_chain_window() {
        let now = Instant::now();
        let mut s = state();
        detect(&mut s, "ctx_search", None, 10, now);
        detect(&mut s, "ctx_read", Some("map"), 10, now);
        detect(&mut s, "ctx_compose", None, 10, now);
        assert_eq!(
            detect(&mut s, "ctx_search", None, 10, now),
            None,
            "compose is the desired end state — it must not complete a chain"
        );
    }

    #[test]
    fn stale_steps_age_out_of_the_window() {
        let start = Instant::now();
        let later = start + CHAIN_WINDOW + Duration::from_secs(1);
        let mut s = state();
        detect(&mut s, "ctx_search", None, 10, start);
        detect(&mut s, "ctx_read", Some("map"), 10, start);
        assert_eq!(
            detect(&mut s, "ctx_search", None, 10, later),
            None,
            "steps older than the window are unrelated work"
        );
    }

    #[test]
    fn unrelated_tools_do_not_extend_chains() {
        let now = Instant::now();
        let mut s = state();
        detect(&mut s, "ctx_search", None, 10, now);
        detect(&mut s, "ctx_read", Some("map"), 10, now);
        assert_eq!(detect(&mut s, "ctx_shell", None, 10, now), None);
        // ctx_shell is invisible to the window; the chain can still complete.
        assert_eq!(
            detect(&mut s, "ctx_search", None, 10, now),
            Some(Detection::Chain)
        );
    }

    #[test]
    fn hints_stay_short() {
        for d in [Detection::HeavyRead, Detection::Chain] {
            assert!(
                hint_for(d).len() < 220,
                "a nudge must cost far less than what it saves"
            );
        }
    }
}
