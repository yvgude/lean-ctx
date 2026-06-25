use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::core::autonomy_drivers::{
    AutonomyDriverDecisionV1, AutonomyDriverEventV1, AutonomyDriverKindV1, AutonomyPhaseV1,
    AutonomyVerdictV1,
};
use crate::core::cache::SessionCache;
use crate::core::config::AutonomyConfig;
use crate::core::graph_provider::GraphProvider;
use crate::core::protocol;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

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

/// Tracks autonomous action state: session init, dedup, and consolidation timing.
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
    #[must_use]
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
    /// the idle gap since the last call for that key is at least five minutes (50ms in unit tests).
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

fn profile_autonomy() -> crate::core::profiles::ProfileAutonomy {
    crate::core::profiles::active_profile().autonomy
}

fn autonomy_enabled_effective(state: &AutonomyState) -> bool {
    state.is_enabled() && profile_autonomy().enabled_effective()
}

fn policy_allows(tool: &str) -> Result<(), (String, String)> {
    let policy = crate::core::degradation_policy::evaluate_v1_for_tool(tool, None);
    match policy.decision.verdict {
        crate::core::degradation_policy::DegradationVerdictV1::Ok
        | crate::core::degradation_policy::DegradationVerdictV1::Warn => Ok(()),
        crate::core::degradation_policy::DegradationVerdictV1::Throttle
        | crate::core::degradation_policy::DegradationVerdictV1::Block => {
            Err((policy.decision.reason_code, policy.decision.reason))
        }
    }
}

fn record_event(
    phase: AutonomyPhaseV1,
    tool: &str,
    action: Option<&str>,
    decisions: Vec<AutonomyDriverDecisionV1>,
) {
    let mut store = crate::core::autonomy_drivers::AutonomyDriversV1::load();
    let ev = AutonomyDriverEventV1 {
        seq: 0,
        created_at: chrono::Utc::now().to_rfc3339(),
        phase,
        role: crate::core::roles::active_role_name(),
        profile: crate::core::profiles::active_profile_name(),
        tool: tool.to_string(),
        action: action.map(std::string::ToString::to_string),
        decisions,
    };
    store.record(ev);
    let _ = store.save();
}

/// Auto-preloads project context on the first tool call of a session.
pub fn session_lifecycle_pre_hook(
    state: &AutonomyState,
    tool_name: &str,
    cache: Option<&mut SessionCache>,
    task: Option<&str>,
    project_root: Option<&str>,
    crp_mode: CrpMode,
) -> Option<String> {
    if !autonomy_enabled_effective(state) {
        return None;
    }

    if tool_name == "ctx_overview" || tool_name == "ctx_preload" {
        return None;
    }

    let prof = profile_autonomy();
    let root = match project_root {
        Some(r) if !r.is_empty() && r != "." => r.to_string(),
        _ => return None,
    };

    if state
        .session_initialized
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return None;
    }

    let mut decisions = Vec::new();

    if !state.config.auto_preload || !prof.auto_preload_effective() {
        decisions.push(AutonomyDriverDecisionV1 {
            driver: AutonomyDriverKindV1::Preload,
            verdict: AutonomyVerdictV1::Skip,
            reason_code: "disabled".to_string(),
            reason: "auto_preload disabled by config/profile".to_string(),
            detail: None,
        });
        record_event(AutonomyPhaseV1::PreCall, tool_name, None, decisions);
        return None;
    }

    let chosen_tool = if task.is_some() {
        "ctx_preload"
    } else {
        "ctx_overview"
    };
    if let Err((code, reason)) = policy_allows(chosen_tool) {
        decisions.push(AutonomyDriverDecisionV1 {
            driver: AutonomyDriverKindV1::Preload,
            verdict: AutonomyVerdictV1::Skip,
            reason_code: code,
            reason,
            detail: Some("policy guard (budget/slo)".to_string()),
        });
        record_event(AutonomyPhaseV1::PreCall, tool_name, None, decisions);
        return None;
    }

    let result = if let Some(task_desc) = task {
        let cache = cache.expect("session_lifecycle_pre_hook: cache required for ctx_preload");
        crate::tools::ctx_preload::handle(cache, task_desc, Some(&root), crp_mode)
    } else {
        crate::tools::ctx_overview::handle(None, Some(&root))
    };

    let empty = result.trim().is_empty()
        || result.contains("No directly relevant files")
        || result.contains("INDEXING IN PROGRESS");
    decisions.push(AutonomyDriverDecisionV1 {
        driver: AutonomyDriverKindV1::Preload,
        verdict: AutonomyVerdictV1::Run,
        reason_code: "session_start".to_string(),
        reason: "first tool call in session".to_string(),
        detail: Some(format!("tool={chosen_tool} empty={empty}")),
    });
    record_event(AutonomyPhaseV1::PreCall, tool_name, None, decisions);

    if empty {
        return None;
    }

    Some(format!(
        "--- AUTO CONTEXT ---\n{result}\n--- END AUTO CONTEXT ---"
    ))
}

/// Appends related-file hints and silently preloads imports after a file read.
pub fn enrich_after_read(
    state: &AutonomyState,
    cache: &mut SessionCache,
    file_path: &str,
    project_root: Option<&str>,
    task: Option<&str>,
    crp_mode: CrpMode,
    minimal_overhead: bool,
) -> EnrichResult {
    let mut result = EnrichResult::default();

    if !autonomy_enabled_effective(state) {
        return result;
    }

    let prof = profile_autonomy();
    let root = match project_root {
        Some(r) if !r.is_empty() && r != "." => r.to_string(),
        _ => return result,
    };

    let Some(open) = crate::core::graph_provider::open_or_build(&root) else {
        return result;
    };
    let provider = &open.provider;
    if provider.file_count() == 0 {
        return result;
    }

    if state.config.auto_related && prof.auto_related_effective() {
        result.related_hint = build_related_hints(cache, file_path, provider);
    }

    if state.config.silent_preload && prof.silent_preload_effective() {
        silent_preload_imports(cache, file_path, provider, &root);
    }

    if !minimal_overhead && prof.auto_prefetch_effective() {
        let mut decisions = Vec::new();
        if let Err((code, reason)) = policy_allows("ctx_prefetch") {
            decisions.push(AutonomyDriverDecisionV1 {
                driver: AutonomyDriverKindV1::Prefetch,
                verdict: AutonomyVerdictV1::Skip,
                reason_code: code,
                reason,
                detail: Some("policy guard (budget/slo)".to_string()),
            });
            record_event(AutonomyPhaseV1::PostRead, "ctx_read", None, decisions);
        } else {
            let changed = vec![file_path.to_string()];
            let out = crate::tools::ctx_prefetch::handle(
                cache,
                &root,
                task,
                Some(&changed),
                prof.prefetch_budget_tokens_effective(),
                Some(prof.prefetch_max_files_effective()),
                crp_mode,
            );
            let summary = out.lines().next().unwrap_or("").trim().to_string();
            decisions.push(AutonomyDriverDecisionV1 {
                driver: AutonomyDriverKindV1::Prefetch,
                verdict: AutonomyVerdictV1::Run,
                reason_code: "after_read".to_string(),
                reason: "bounded prefetch after ctx_read".to_string(),
                detail: if summary.is_empty() {
                    None
                } else {
                    Some(summary.clone())
                },
            });
            record_event(AutonomyPhaseV1::PostRead, "ctx_read", None, decisions);
            let _ = summary;
        }
    }

    result
}

/// Output from post-read enrichment: optional related-file hints.
#[derive(Default)]
pub struct EnrichResult {
    pub related_hint: Option<String>,
}

fn build_related_hints(
    cache: &SessionCache,
    file_path: &str,
    provider: &GraphProvider,
) -> Option<String> {
    let mut related: Vec<String> = Vec::new();
    for path in provider
        .dependencies(file_path)
        .into_iter()
        .chain(provider.dependents(file_path))
    {
        if related.len() >= 3 {
            break;
        }
        if cache.get(&path).is_none() && !related.contains(&path) {
            related.push(path);
        }
    }

    if related.is_empty() {
        return None;
    }

    let hints: Vec<String> = related.iter().map(|p| protocol::shorten_path(p)).collect();

    Some(format!("[related: {}]", hints.join(", ")))
}

fn silent_preload_imports(
    cache: &mut SessionCache,
    file_path: &str,
    provider: &GraphProvider,
    project_root: &str,
) {
    let imports: Vec<String> = provider
        .dependencies(file_path)
        .into_iter()
        .take(2)
        .collect();

    let jail_root = std::path::Path::new(project_root);
    for path in imports {
        let candidate = std::path::Path::new(&path);
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            jail_root.join(&path)
        };
        let Ok((jailed, warning)) = crate::core::io_boundary::jail_and_check_path(
            "autonomy:silent_preload",
            &candidate,
            jail_root,
        ) else {
            continue;
        };
        if warning.is_some() {
            continue;
        }
        let jailed_s = jailed.to_string_lossy().to_string();
        if cache.get(&jailed_s).is_some() {
            continue;
        }
        // Don't hydrate cloud placeholders during automatic import preload (#363).
        if crate::core::cloud_files::is_cloud_placeholder(&jailed) {
            continue;
        }

        if let Ok(content) = std::fs::read_to_string(&jailed) {
            let tokens = count_tokens(&content);
            if tokens < 5000 {
                cache.store(&jailed_s, &content);
            }
        }
    }
}

/// Runs cache deduplication once the entry count exceeds the configured threshold.
pub fn maybe_auto_dedup(state: &AutonomyState, cache: &mut SessionCache, trigger_tool: &str) {
    if !autonomy_enabled_effective(state) {
        return;
    }

    let prof = profile_autonomy();
    if !state.config.auto_dedup || !prof.auto_dedup_effective() {
        return;
    }

    if state
        .dedup_applied
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let entries = cache.get_all_entries();
    let threshold = state
        .config
        .dedup_threshold
        .max(prof.dedup_threshold_effective())
        .max(1);
    if entries.len() < threshold {
        state.dedup_applied.store(false, Ordering::SeqCst);
        return;
    }

    let mut decisions = Vec::new();
    if let Err((code, reason)) = policy_allows("ctx_dedup") {
        decisions.push(AutonomyDriverDecisionV1 {
            driver: AutonomyDriverKindV1::Dedup,
            verdict: AutonomyVerdictV1::Skip,
            reason_code: code,
            reason,
            detail: Some("policy guard (budget/slo)".to_string()),
        });
        record_event(AutonomyPhaseV1::PostRead, trigger_tool, None, decisions);
        state.dedup_applied.store(false, Ordering::SeqCst);
        return;
    }

    let out = crate::tools::ctx_dedup::handle_action(cache, "apply");
    let summary = out.lines().next().unwrap_or("").trim().to_string();
    decisions.push(AutonomyDriverDecisionV1 {
        driver: AutonomyDriverKindV1::Dedup,
        verdict: AutonomyVerdictV1::Run,
        reason_code: "threshold_reached".to_string(),
        reason: format!("cache entries >= {threshold}"),
        detail: if summary.is_empty() {
            None
        } else {
            Some(summary)
        },
    });
    record_event(AutonomyPhaseV1::PostRead, trigger_tool, None, decisions);
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

fn looks_like_json(text: &str) -> bool {
    let t = text.trim();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(t).is_ok()
}

/// Applies `ctx_response` automatically for large outputs (guarded + bounded).
/// Never runs on JSON outputs to avoid breaking machine-readable responses.
pub fn maybe_auto_response(
    state: &AutonomyState,
    tool_name: &str,
    action: Option<&str>,
    output: &str,
    crp_mode: CrpMode,
    minimal_overhead: bool,
) -> String {
    if minimal_overhead || !autonomy_enabled_effective(state) {
        return output.to_string();
    }

    let prof = profile_autonomy();
    if !prof.auto_response_effective() {
        return output.to_string();
    }
    if tool_name == "ctx_response" {
        return output.to_string();
    }

    let input_tokens = count_tokens(output);
    if input_tokens < prof.response_min_tokens_effective() {
        return output.to_string();
    }
    if looks_like_json(output) {
        record_event(
            AutonomyPhaseV1::PostCall,
            tool_name,
            action,
            vec![AutonomyDriverDecisionV1 {
                driver: AutonomyDriverKindV1::Response,
                verdict: AutonomyVerdictV1::Skip,
                reason_code: "json_output".to_string(),
                reason: "skip response shaping for JSON outputs".to_string(),
                detail: None,
            }],
        );
        return output.to_string();
    }

    if let Err((code, reason)) = policy_allows("ctx_response") {
        record_event(
            AutonomyPhaseV1::PostCall,
            tool_name,
            action,
            vec![AutonomyDriverDecisionV1 {
                driver: AutonomyDriverKindV1::Response,
                verdict: AutonomyVerdictV1::Skip,
                reason_code: code,
                reason,
                detail: Some("policy guard (budget/slo)".to_string()),
            }],
        );
        return output.to_string();
    }

    let start = std::time::Instant::now();
    let compressed = crate::tools::ctx_response::handle(output, crp_mode);
    let duration = start.elapsed();
    let output_tokens = count_tokens(&compressed);

    let (verdict, reason_code, reason) = if compressed == output {
        (
            AutonomyVerdictV1::Skip,
            "no_savings".to_string(),
            "ctx_response made no changes".to_string(),
        )
    } else {
        (
            AutonomyVerdictV1::Run,
            "output_large".to_string(),
            "response shaping applied".to_string(),
        )
    };

    record_event(
        AutonomyPhaseV1::PostCall,
        tool_name,
        action,
        vec![AutonomyDriverDecisionV1 {
            driver: AutonomyDriverKindV1::Response,
            verdict,
            reason_code,
            reason,
            detail: Some(format!(
                "tokens {}→{} in {:.1}ms",
                input_tokens,
                output_tokens,
                duration.as_micros() as f64 / 1000.0
            )),
        }],
    );

    compressed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomy_state_starts_uninitialized() {
        let state = AutonomyState::new();
        assert!(!state.session_initialized.load(Ordering::SeqCst));
        assert!(!state.dedup_applied.load(Ordering::SeqCst));
    }

    #[test]
    fn session_initialized_fires_once() {
        let state = AutonomyState::new();
        let first = state.session_initialized.compare_exchange(
            false,
            true,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        assert!(first.is_ok());
        let second = state.session_initialized.compare_exchange(
            false,
            true,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        assert!(second.is_err());
    }

    #[test]
    fn shell_hint_for_grep() {
        let state = AutonomyState::new();
        let hint = shell_efficiency_hint(&state, "grep -rn foo .", 100, 95);
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("ctx_search"));
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
