//! The session wake-up briefing and one-shot efficiency hints. The read-path drivers (session-start preload,
//! related hints, silent import preload, prefetch, auto-dedup) were retired on
//! 2026-10-17: an A/B on the retrieval corpus returned byte-identical responses
//! and equal R@5/MRR with them off, while the session start cost 2-14 s more.

use std::sync::atomic::Ordering;

pub use crate::core::autonomy::{AutonomyState, SearchHistory, should_auto_consolidate};
use crate::core::tokens::count_tokens;

fn profile_autonomy() -> crate::core::profiles::ProfileAutonomy {
    crate::core::profiles::active_profile().autonomy
}

fn autonomy_enabled_effective(state: &AutonomyState) -> bool {
    state.is_enabled() && profile_autonomy().enabled_effective()
}

/// Whether this call is the one that may carry the session briefing and the
/// shared-context hint: `ctx_session` with `load`, `status` or `show`, also
/// through `ctx_call`. Every other tool result stays a deterministic function
/// of its inputs (#498); the briefing carries per-run peer ids and memory.
pub fn delivers_session_briefing(
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    let (name, args) = if name == "ctx_call" {
        (
            args.and_then(|a| a.get("name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
            args.and_then(|a| a.get("arguments"))
                .and_then(serde_json::Value::as_object),
        )
    } else {
        (name, args)
    };
    name == "ctx_session"
        && matches!(
            args.and_then(|a| a.get("action"))
                .and_then(serde_json::Value::as_str),
            Some("load" | "status" | "show")
        )
}

/// Applies the auto-load context packages silently on the first call of a
/// session that does not deliver the briefing; the result text is unchanged
/// and the briefing names them later.
pub fn apply_auto_load_packages_once(state: &AutonomyState, project_root: Option<&str>) {
    if crate::core::policy::runtime::is_active() || !autonomy_enabled_effective(state) {
        return;
    }
    let Some(root) = project_root.filter(|r| !r.is_empty() && *r != ".") else {
        return;
    };
    if state
        .packages_applied
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        let loaded = crate::core::context_package::auto_load_packages(root);
        state
            .packages_loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(loaded);
    }
}

/// The wake-up briefing (last task, recent decisions, project facts, active
/// peers), as `--- AUTO CONTEXT ---`, on the first briefing-delivering call
/// of a session (see [`delivers_session_briefing`]).
///
/// It used to be appended to a full project overview, which on any real
/// project exceeded the injection cap, so the briefing was built (2-14 s) and
/// dropped. Only the briefing is built now, without the graph, and its facts
/// are fitted to the cap: on 932 real knowledge stores the facts block alone
/// exceeded it for 19 % (p50 319, p90 715 tokens).
pub fn session_wakeup_briefing(
    state: &AutonomyState,
    tool_name: &str,
    task: Option<&str>,
    project_root: Option<&str>,
) -> Option<String> {
    // Memory text carries no original-source authority under a policy.
    if crate::core::policy::runtime::is_active() || !autonomy_enabled_effective(state) {
        return None;
    }
    // ctx_preload renders the briefing itself; it keeps the slot for the next call.
    if tool_name == "ctx_preload" {
        return None;
    }
    let root = project_root.filter(|r| !r.is_empty() && *r != ".")?;
    if state
        .session_briefed
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return None;
    }
    // Auto-load packages not yet applied by an earlier call are applied here;
    // both those and the ones a silent first call applied are named.
    let mut loaded = std::mem::take(
        &mut *state
            .packages_loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    if !state.packages_applied.swap(true, Ordering::SeqCst) {
        loaded.extend(crate::core::context_package::auto_load_packages(root));
    }
    let packages = (!loaded.is_empty())
        .then(|| format!("CONTEXT PACKAGES AUTO-LOADED: {}", loaded.join(", ")));
    // Hive tasks (H2): the claims this machine holds here, then open tasks.
    let tasks = crate::core::hive::tasks::briefing(std::path::Path::new(root));
    let frame = |briefing: &str| format!("{AUTO_CONTEXT_OPEN}\n{briefing}\n{AUTO_CONTEXT_CLOSE}");
    let briefing = if crate::core::config::Config::load().enable_wakeup_ctx {
        let reserved = count_tokens(&frame(&format!(
            "{}\n{}",
            packages.as_deref().unwrap_or(""),
            tasks.as_deref().unwrap_or("")
        )));
        let budget = AUTO_CONTEXT_MAX_TOKENS.saturating_sub(reserved);
        crate::tools::ctx_overview::build_wakeup_briefing_within(root, task, budget)
    } else {
        String::new()
    };
    let body = [
        packages.unwrap_or_default(),
        tasks.unwrap_or_default(),
        briefing,
    ]
    .into_iter()
    .filter(|part| !part.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n");
    (!body.is_empty()).then(|| frame(&body))
}

/// The largest `--- AUTO CONTEXT ---` block a reply carries; larger blocks are
/// dropped, so the briefing is fitted to it.
pub const AUTO_CONTEXT_MAX_TOKENS: usize = 400;
const AUTO_CONTEXT_OPEN: &str = "--- AUTO CONTEXT ---";
const AUTO_CONTEXT_CLOSE: &str = "--- END AUTO CONTEXT ---";

/// Suggests a more token-efficient lean-ctx tool when shell compression is low.
pub fn shell_efficiency_hint(
    state: &AutonomyState,
    command: &str,
    input_tokens: usize,
    output_tokens: usize,
) -> Option<String> {
    if !autonomy_enabled_effective(state) {
        return None;
    }

    if input_tokens == 0 {
        return None;
    }

    let savings_pct =
        (input_tokens.saturating_sub(output_tokens) as f64 / input_tokens as f64) * 100.0;
    if savings_pct >= 20.0 {
        return None;
    }

    let cmd_lower = command.to_lowercase();
    if cmd_lower.starts_with("grep ")
        || cmd_lower.starts_with("rg ")
        || cmd_lower.starts_with("find ")
        || cmd_lower.starts_with("ag ")
    {
        return Some("[hint: ctx_search is more token-efficient for code search]".to_string());
    }

    if cmd_lower.starts_with("cat ") || cmd_lower.starts_with("head ") {
        return Some("[hint: ctx_read provides cached, compressed file access]".to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ctx_session_load_or_status_delivers_the_briefing() {
        let args = |v: serde_json::Value| v.as_object().cloned();
        let status = args(serde_json::json!({"action": "status"}));
        assert!(delivers_session_briefing("ctx_session", status.as_ref()));
        let wrapped =
            args(serde_json::json!({"name": "ctx_session", "arguments": {"action": "load"}}));
        assert!(delivers_session_briefing("ctx_call", wrapped.as_ref()));
        let task = args(serde_json::json!({"action": "task", "value": "x"}));
        assert!(!delivers_session_briefing("ctx_session", task.as_ref()));
        for tool in ["ctx_compose", "ctx_search", "ctx_read", "ctx_shell"] {
            assert!(!delivers_session_briefing(tool, status.as_ref()), "{tool}");
        }
    }

    #[test]
    fn shell_hint_for_grep() {
        let state = AutonomyState::new();
        let hint = shell_efficiency_hint(&state, "grep -rn foo .", 100, 95);
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("ctx_search"));
        let hint = shell_efficiency_hint(&state, "cat src/main.rs", 100, 95);
        assert!(hint.is_some_and(|h| h.contains("ctx_read")));
    }

    #[test]
    fn shell_hint_none_when_good_savings() {
        let state = AutonomyState::new();
        let hint = shell_efficiency_hint(&state, "grep -rn foo .", 100, 50);
        assert!(hint.is_none());
    }

    #[test]
    fn shell_hint_none_for_unknown_command() {
        let state = AutonomyState::new();
        let hint = shell_efficiency_hint(&state, "cargo build", 100, 95);
        assert!(hint.is_none());
    }

    #[test]
    fn disabled_state_blocks_all() {
        let mut state = AutonomyState::new();
        state.config.enabled = false;
        assert!(!state.is_enabled());
        let hint = shell_efficiency_hint(&state, "grep foo", 100, 95);
        assert!(hint.is_none());
    }

    #[test]
    fn track_search_none_first_three() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = AutonomyState::new();
        assert!(state.track_search("foo", "src").is_none());
        assert!(state.track_search("foo", "src").is_none());
        assert!(state.track_search("foo", "src").is_none());
    }

    #[test]
    fn track_search_hint_band() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = AutonomyState::new();
        for _ in 0..3 {
            assert!(state.track_search("bar", ".").is_none());
        }
        let h = state.track_search("bar", ".").expect("hint on 4th");
        assert!(h.starts_with("[hint: repeated search (4/6)."));
        assert!(h.contains("ctx_knowledge"));
    }

    #[test]
    fn track_search_throttle_seventh() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = AutonomyState::new();
        for _ in 0..6 {
            let _ = state.track_search("baz", "p");
        }
        let h = state.track_search("baz", "p").expect("throttle on 7th");
        assert!(h.starts_with("[throttle: search repeated 7 times"));
        assert!(h.contains("ctx_pack"));
    }

    #[test]
    fn track_search_resets_after_idle() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = AutonomyState::new();
        for _ in 0..3 {
            assert!(state.track_search("idle", "x").is_none());
        }
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(
            state.track_search("idle", "x").is_none(),
            "count should reset after idle window"
        );
    }

    #[test]
    fn track_search_disabled_no_tracking_messages() {
        let _lock = crate::core::data_dir::test_env_lock();
        let mut state = AutonomyState::new();
        state.config.enabled = false;
        for _ in 0..8 {
            assert!(state.track_search("q", "/").is_none());
        }
    }

    #[test]
    fn track_search_distinct_keys() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = AutonomyState::new();
        assert!(state.track_search("pat", "a").is_none());
        assert!(state.track_search("pat", "a").is_none());
        assert!(state.track_search("pat", "a").is_none());
        assert!(state.track_search("pat", "a").is_some());
        assert!(state.track_search("pat", "b").is_none());
    }
}
