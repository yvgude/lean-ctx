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

/// The wake-up briefing (last task, recent decisions, project facts, active
/// peers) for the first tool call of a session, as `--- AUTO CONTEXT ---`.
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
    // Context packages marked auto-load were applied by the retired
    // session-start overview; they still apply once, on the first call.
    let loaded = crate::core::context_package::auto_load_packages(root);
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

fn take_large_output_hint_once(state: &AutonomyState, key: &str) -> bool {
    if !autonomy_enabled_effective(state) {
        return false;
    }
    let mut set = state
        .large_output_hints_shown
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    set.insert(key.to_string())
}

/// `ctx_shell`: suggest sandbox / read modes when final output is large (bytes).
pub fn large_ctx_shell_output_hint(
    state: &AutonomyState,
    command: &str,
    output_bytes: usize,
) -> Option<String> {
    const THRESHOLD_BYTES: usize = 5000;
    if output_bytes <= THRESHOLD_BYTES {
        return None;
    }
    if !take_large_output_hint_once(state, "ctx_shell_large_bytes") {
        return None;
    }
    let n = output_bytes;
    if shell_command_looks_structured(command) {
        Some(format!(
            "[hint: large output ({n} bytes). For structured output (e.g. cargo test, npm test, grep), use ctx_execute for automatic compression; for file contents use ctx_read(mode=\"aggressive\")]"
        ))
    } else {
        Some(format!(
            "[hint: large output ({n} bytes). Consider piping through ctx_execute for automatic compression, or use ctx_read(mode=\"aggressive\") for file contents]"
        ))
    }
}

fn shell_command_looks_structured(cmd: &str) -> bool {
    let t = cmd.trim();
    let lower = t.to_lowercase();
    lower.contains("cargo test")
        || lower.contains("npm test")
        || t.starts_with("grep ")
        || t.starts_with("rg ")
}

/// `ctx_read` full mode: suggest compressed read modes when output is very large (tokens).
pub fn large_ctx_read_full_hint(
    state: &AutonomyState,
    mode: Option<&str>,
    output: &str,
) -> Option<String> {
    const THRESHOLD_TOKENS: usize = 10_000;
    let m = mode.unwrap_or("").trim();
    if m != "full" {
        return None;
    }
    let n = count_tokens(output);
    if n <= THRESHOLD_TOKENS {
        return None;
    }
    if !take_large_output_hint_once(state, "ctx_read_full_large_tokens") {
        return None;
    }
    Some(format!(
        "[hint: large file ({n} tokens). Consider mode=\"map\" or mode=\"aggressive\" for compressed view]"
    ))
}

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
    fn large_shell_hint_once_per_session() {
        let state = AutonomyState::new();
        let h1 = large_ctx_shell_output_hint(&state, "ls -la", 5001).expect("first");
        assert!(h1.contains("5001 bytes"));
        assert!(h1.contains("ctx_execute"));
        assert!(large_ctx_shell_output_hint(&state, "ls -la", 5001).is_none());
    }

    #[test]
    fn large_shell_structured_hint_mentions_execute() {
        let state = AutonomyState::new();
        let h = large_ctx_shell_output_hint(&state, "cargo test", 6000).expect("hint");
        assert!(h.contains("structured"));
        assert!(h.contains("ctx_execute"));
    }

    #[test]
    fn large_read_full_hint_respects_mode() {
        let state = AutonomyState::new();
        let big = "word ".repeat(20_000);
        assert!(large_ctx_read_full_hint(&state, Some("map"), &big).is_none());
        let h = large_ctx_read_full_hint(&state, Some("full"), &big).expect("hint");
        assert!(h.contains("tokens"));
        assert!(h.contains("aggressive"));
        assert!(large_ctx_read_full_hint(&state, Some("full"), &big).is_none());
    }

    #[test]
    fn large_hints_disabled_when_autonomy_off() {
        let mut state = AutonomyState::new();
        state.config.enabled = false;
        let big = "word ".repeat(20_000);
        assert!(large_ctx_shell_output_hint(&state, "cargo test", 6000).is_none());
        assert!(large_ctx_read_full_hint(&state, Some("full"), &big).is_none());
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
