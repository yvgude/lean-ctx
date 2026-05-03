use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::cache::SessionCache;
use crate::core::config::AutonomyConfig;
use crate::core::graph_index::ProjectIndex;
use crate::core::protocol;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

/// Tracks autonomous action state: session init, dedup, and consolidation timing.
pub struct AutonomyState {
    pub session_initialized: AtomicBool,
    pub dedup_applied: AtomicBool,
    pub last_consolidation_unix: AtomicU64,
    pub config: AutonomyConfig,
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
        }
    }

    /// Returns true if autonomous actions are enabled in configuration.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

/// Auto-preloads project context on the first tool call of a session.
pub fn session_lifecycle_pre_hook(
    state: &AutonomyState,
    tool_name: &str,
    cache: &mut SessionCache,
    task: Option<&str>,
    project_root: Option<&str>,
    crp_mode: CrpMode,
) -> Option<String> {
    if !state.is_enabled() || !state.config.auto_preload {
        return None;
    }

    if tool_name == "ctx_overview" || tool_name == "ctx_preload" {
        return None;
    }

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

    let result = if let Some(task_desc) = task {
        crate::tools::ctx_preload::handle(cache, task_desc, Some(&root), crp_mode)
    } else {
        let cache_readonly = &*cache;
        crate::tools::ctx_overview::handle(cache_readonly, None, Some(&root), crp_mode)
    };

    if result.contains("No directly relevant files") || result.trim().is_empty() {
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
) -> EnrichResult {
    let mut result = EnrichResult::default();

    if !state.is_enabled() {
        return result;
    }

    let root = match project_root {
        Some(r) if !r.is_empty() && r != "." => r.to_string(),
        _ => return result,
    };

    let index = crate::core::graph_index::load_or_build(&root);
    if index.files.is_empty() {
        return result;
    }

    if state.config.auto_related {
        result.related_hint = build_related_hints(cache, file_path, &index);
    }

    if state.config.silent_preload {
        silent_preload_imports(cache, file_path, &index, &root);
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
    index: &ProjectIndex,
) -> Option<String> {
    let related: Vec<_> = index
        .edges
        .iter()
        .filter(|e| e.from == file_path || e.to == file_path)
        .map(|e| if e.from == file_path { &e.to } else { &e.from })
        .filter(|path| cache.get(path).is_none())
        .take(3)
        .collect();

    if related.is_empty() {
        return None;
    }

    let hints: Vec<String> = related.iter().map(|p| protocol::shorten_path(p)).collect();

    Some(format!("[related: {}]", hints.join(", ")))
}

fn silent_preload_imports(
    cache: &mut SessionCache,
    file_path: &str,
    index: &ProjectIndex,
    _project_root: &str,
) {
    let imports: Vec<String> = index
        .edges
        .iter()
        .filter(|e| e.from == file_path)
        .map(|e| e.to.clone())
        .filter(|path| cache.get(path).is_none())
        .take(2)
        .collect();

    for path in imports {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let tokens = count_tokens(&content);
            if tokens < 5000 {
                cache.store(&path, content);
            }
        }
    }
}

/// Runs cache deduplication once the entry count exceeds the configured threshold.
pub fn maybe_auto_dedup(state: &AutonomyState, cache: &mut SessionCache) {
    if !state.is_enabled() || !state.config.auto_dedup {
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
    if entries.len() < state.config.dedup_threshold {
        state.dedup_applied.store(false, Ordering::SeqCst);
        return;
    }

    crate::tools::ctx_dedup::handle_action(cache, "apply");
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

/// Suggests a more token-efficient lean-ctx tool when shell compression is low.
pub fn shell_efficiency_hint(
    state: &AutonomyState,
    command: &str,
    input_tokens: usize,
    output_tokens: usize,
) -> Option<String> {
    if !state.is_enabled() {
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
    fn disabled_state_blocks_all() {
        let mut state = AutonomyState::new();
        state.config.enabled = false;
        assert!(!state.is_enabled());
        let hint = shell_efficiency_hint(&state, "grep foo", 100, 95);
        assert!(hint.is_none());
    }
}
