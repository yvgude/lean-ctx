use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    Core,
    Internal,
    Arch,
    Debug,
    Memory,
    Metrics,
    Session,
}

impl ToolCategory {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "core" => Some(Self::Core),
            "arch" | "architecture" => Some(Self::Arch),
            "debug" | "profiling" => Some(Self::Debug),
            "memory" | "semantic" => Some(Self::Memory),
            "metrics" | "stats" => Some(Self::Metrics),
            "session" => Some(Self::Session),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Internal => "internal",
            Self::Arch => "arch",
            Self::Debug => "debug",
            Self::Memory => "memory",
            Self::Metrics => "metrics",
            Self::Session => "session",
        }
    }
}

#[allow(clippy::match_same_arms)]
pub fn categorize_tool(name: &str) -> ToolCategory {
    match name {
        // Internal: meta/self-referential + automated mechanisms (never exposed)
        "ctx_metrics"
        | "ctx_cost"
        | "ctx_gain"
        | "ctx_radar"
        | "ctx_heatmap"
        | "ctx_feedback"
        | "ctx_intent"
        | "ctx_response"
        | "ctx_discover"
        | "ctx_discover_tools"
        | "ctx_load_tools"
        | "ctx_dedup"
        | "ctx_preload"
        | "ctx_prefetch"
        | "ctx_compress_memory" => ToolCategory::Internal,

        // Core: always visible. Must cover every CORE_TOOL_NAMES entry —
        // otherwise the category gate silently drops a lazy-core tool for
        // list_changed-capable clients (ctx_expand was lost this way, #575).
        "ctx_read" | "ctx_search" | "ctx_shell" | "shell" | "ctx_tree" | "ctx_edit"
        | "ctx_session" | "ctx_checkpoint" | "ctx_knowledge" | "ctx_overview" | "ctx_graph"
        | "ctx_call" | "ctx_compress" | "ctx_cache" | "ctx_retrieve" | "ctx_expand" => {
            ToolCategory::Core
        }

        // Merged tools (redirects in registry, treated as Core for backward compat)
        "ctx_multi_read" | "ctx_smart_read" | "ctx_delta" | "ctx_outline" | "ctx_context" => {
            ToolCategory::Core
        }

        // Arch: on-demand architecture analysis
        "ctx_architecture" | "ctx_impact" | "ctx_callgraph" | "ctx_refactor" | "ctx_symbol"
        | "ctx_routes" | "ctx_smells" | "ctx_quality" | "ctx_index" => ToolCategory::Arch,

        // Debug/Verify: on-demand quality analysis
        "ctx_benchmark" | "ctx_verify" | "ctx_analyze" | "ctx_profile" | "ctx_proof"
        | "ctx_review" | "ctx_compare" => ToolCategory::Debug,

        // Provider + URL/Git readers + the MCP gateway are Core: gateways to
        // external context (GitHub issues, Jira, Postgres, web pages, YouTube,
        // remote git repos, downstream MCP servers) — always available.
        // ctx_semantic_search is a first-class retrieval tool (advertised in the
        // lean core, #422) — keep it Core so the default category gate never hides
        // it, the very reason agents stopped reaching for it.
        "ctx_provider" | "ctx_url_read" | "ctx_git_read" | "ctx_tools" | "ctx_semantic_search" => {
            ToolCategory::Core
        }

        // Memory: on-demand artifact retrieval
        "ctx_artifacts" => ToolCategory::Memory,

        // Batch: on-demand batch/PR/sandbox tools
        "ctx_fill" | "ctx_execute" | "ctx_pack" | "ctx_plan" | "ctx_control" | "ctx_compile" => {
            ToolCategory::Metrics
        }

        // Multi-agent: on-demand collaboration
        "ctx_agent" | "ctx_share" | "ctx_task" | "ctx_handoff" | "ctx_workflow" => {
            ToolCategory::Session
        }

        _ => ToolCategory::Core,
    }
}

/// A deprecated tool that has been folded into a primary tool (#509 Phase 1).
///
/// The alias stays **registered and callable** (directly and via `ctx_call`) for
/// one release so nothing breaks, but it is hidden from `tools/list` and warns on
/// use, steering agents to the consolidated primary. Removal happens in Phase 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeprecatedAlias {
    /// The primary tool that supersedes this alias (e.g. `"ctx_read"`).
    pub replacement: &'static str,
    /// One-line migration hint (e.g. how the primary covers this use case).
    pub hint: &'static str,
}

/// Single source of truth for read-cluster deprecations (#509). Returns the
/// replacement + migration hint when `name` is a deprecated alias, else `None`.
///
/// Used by [`crate::server::tool_visibility::is_tool_visible`] to hide the alias
/// from `tools/list`, and by the dispatch layer to prepend a one-line
/// deprecation notice to the alias's output. Keeping both behaviours keyed off
/// this one function guarantees "hidden" and "warned" can never drift apart.
#[must_use]
pub fn deprecated_alias(name: &str) -> Option<DeprecatedAlias> {
    match name {
        "ctx_smart_read" => Some(DeprecatedAlias {
            replacement: "ctx_read",
            hint: "ctx_read auto-selects the mode (omit `mode`, or pass mode=\"auto\")",
        }),
        "ctx_multi_read" => Some(DeprecatedAlias {
            replacement: "ctx_read",
            hint: "ctx_read now batch-reads via paths=[\"a.rs\",\"b.rs\"]",
        }),
        // #509 search consolidation: one ctx_search entry, `action` picks the
        // engine. Aliases stay callable for one release so nothing breaks.
        "ctx_semantic_search" => Some(DeprecatedAlias {
            replacement: "ctx_search",
            hint: "ctx_search with action=\"semantic\" (query=…); reindex/find_related are actions too",
        }),
        "ctx_symbol" => Some(DeprecatedAlias {
            replacement: "ctx_search",
            hint: "ctx_search with action=\"symbol\" (name=…, optional file/kind)",
        }),
        _ => None,
    }
}

/// Whether `name` is a deprecated alias hidden from `tools/list` (#509).
#[must_use]
pub fn is_deprecated_alias(name: &str) -> bool {
    deprecated_alias(name).is_some()
}

/// The one-line deprecation notice prepended to a deprecated alias's output.
/// Stable per tool (no timestamps/counters) so provider-side prompt caching
/// stays byte-stable (#498).
#[must_use]
pub fn deprecation_notice(name: &str) -> Option<String> {
    deprecated_alias(name).map(|d| {
        format!(
            "[DEPRECATED] {name} is superseded by {} — {}. This alias is hidden from \
             tools/list and will be removed in a future release.",
            d.replacement, d.hint
        )
    })
}

pub fn is_readonly_tool(name: &str) -> bool {
    matches!(
        name,
        "ctx_read"
            | "ctx_search"
            | "ctx_tree"
            | "ctx_overview"
            | "ctx_plan"
            | "ctx_metrics"
            | "ctx_compress"
            | "ctx_session"
            | "ctx_knowledge"
            | "ctx_graph"
            | "ctx_retrieve"
            | "ctx_provider"
            | "ctx_multi_read"
            | "ctx_smart_read"
            | "ctx_delta"
            | "ctx_outline"
            | "ctx_context"
            | "ctx_call"
            | "ctx_url_read"
            | "ctx_git_read"
            | "ctx_architecture"
            | "ctx_impact"
            | "ctx_callgraph"
            | "ctx_symbol"
            | "ctx_routes"
            | "ctx_smells"
            | "ctx_quality"
            | "ctx_index"
            | "ctx_semantic_search"
            | "ctx_explore"
            | "ctx_artifacts"
            | "ctx_cost"
            | "ctx_gain"
            | "ctx_heatmap"
            | "ctx_compare"
    )
}

#[derive(Debug)]
pub struct DynamicToolState {
    active_categories: HashSet<ToolCategory>,
    supports_list_changed: bool,
}

impl Default for DynamicToolState {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicToolState {
    pub fn new() -> Self {
        let mut active = HashSet::new();
        active.insert(ToolCategory::Core);
        active.insert(ToolCategory::Session);
        Self {
            active_categories: active,
            supports_list_changed: false,
        }
    }

    /// Creates state with categories from config (env var > config.toml > default).
    pub fn from_config(categories: &[String]) -> Self {
        let mut active = HashSet::new();
        active.insert(ToolCategory::Core);
        for cat_str in categories {
            if let Some(cat) = ToolCategory::parse(cat_str) {
                active.insert(cat);
            }
        }
        Self {
            active_categories: active,
            supports_list_changed: false,
        }
    }

    pub fn all_enabled() -> Self {
        let mut active = HashSet::new();
        active.insert(ToolCategory::Core);
        active.insert(ToolCategory::Arch);
        active.insert(ToolCategory::Debug);
        active.insert(ToolCategory::Memory);
        active.insert(ToolCategory::Metrics);
        active.insert(ToolCategory::Session);
        Self {
            active_categories: active,
            supports_list_changed: false,
        }
    }

    pub fn set_supports_list_changed(&mut self, val: bool) {
        self.supports_list_changed = val;
    }

    pub fn supports_list_changed(&self) -> bool {
        self.supports_list_changed
    }

    pub fn load_category(&mut self, cat: ToolCategory) -> bool {
        self.active_categories.insert(cat)
    }

    pub fn unload_category(&mut self, cat: ToolCategory) -> bool {
        if cat == ToolCategory::Core || cat == ToolCategory::Internal {
            return false;
        }
        self.active_categories.remove(&cat)
    }

    pub fn is_tool_active(&self, name: &str) -> bool {
        let cat = categorize_tool(name);
        if cat == ToolCategory::Internal {
            return false;
        }
        if !self.supports_list_changed {
            return true;
        }
        self.active_categories.contains(&cat)
    }

    pub fn active_categories(&self) -> Vec<&'static str> {
        let mut cats: Vec<_> = self
            .active_categories
            .iter()
            .map(ToolCategory::as_str)
            .collect();
        cats.sort_unstable();
        cats
    }

    pub fn all_categories() -> Vec<&'static str> {
        vec!["core", "arch", "debug", "memory", "metrics", "session"]
    }
}

static GLOBAL: OnceLock<Mutex<DynamicToolState>> = OnceLock::new();

pub fn global() -> &'static Mutex<DynamicToolState> {
    GLOBAL.get_or_init(|| Mutex::new(DynamicToolState::new()))
}

pub fn init_all_enabled() {
    let _ = GLOBAL.set(Mutex::new(DynamicToolState::all_enabled()));
}

/// Initializes the global state from user config (env var > config.toml > default).
/// Call once during server startup after config is loaded.
/// If the global was already initialized (e.g. by a concurrent `global()` call),
/// applies the categories to the existing state instead.
pub fn init_from_config(categories: &[String]) {
    if GLOBAL
        .set(Mutex::new(DynamicToolState::from_config(categories)))
        .is_err()
        && let Ok(mut state) = global().lock()
    {
        let desired = DynamicToolState::from_config(categories);
        *state = desired;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_tools_always_active() {
        let state = DynamicToolState::new();
        assert!(state.is_tool_active("ctx_read"));
        assert!(state.is_tool_active("ctx_search"));
    }

    #[test]
    fn deprecated_alias_maps_read_cluster_to_ctx_read() {
        // #509: only the folded read-cluster tools are deprecated; everything
        // else (incl. the primary) returns None.
        assert_eq!(
            deprecated_alias("ctx_smart_read").unwrap().replacement,
            "ctx_read"
        );
        assert_eq!(
            deprecated_alias("ctx_multi_read").unwrap().replacement,
            "ctx_read"
        );
        assert!(deprecated_alias("ctx_read").is_none());
        assert!(deprecated_alias("ctx_search").is_none());
        assert!(is_deprecated_alias("ctx_multi_read"));
        assert!(!is_deprecated_alias("ctx_read"));

        // #509 search consolidation: the folded search tools point at ctx_search.
        assert_eq!(
            deprecated_alias("ctx_semantic_search").unwrap().replacement,
            "ctx_search"
        );
        assert_eq!(
            deprecated_alias("ctx_symbol").unwrap().replacement,
            "ctx_search"
        );
        assert!(is_deprecated_alias("ctx_symbol"));
    }

    #[test]
    fn deprecation_notice_is_stable_and_names_replacement() {
        // Stable text (no timestamps/counters) for cache-byte-stability (#498),
        // and it must point at the primary so agents know where to go.
        let notice = deprecation_notice("ctx_multi_read").unwrap();
        assert!(notice.starts_with("[DEPRECATED] ctx_multi_read is superseded by ctx_read"));
        assert!(notice.contains("paths="));
        assert_eq!(notice, deprecation_notice("ctx_multi_read").unwrap());
        assert!(deprecation_notice("ctx_read").is_none());
    }

    #[test]
    fn dynamic_tools_filtered_when_list_changed() {
        let mut state = DynamicToolState::new();
        state.set_supports_list_changed(true);
        assert!(!state.is_tool_active("ctx_benchmark"));
        assert!(!state.is_tool_active("ctx_architecture"));
        assert!(state.is_tool_active("ctx_read"));
    }

    #[test]
    fn load_category_enables_tools() {
        let mut state = DynamicToolState::new();
        state.set_supports_list_changed(true);
        assert!(!state.is_tool_active("ctx_architecture"));
        state.load_category(ToolCategory::Arch);
        assert!(state.is_tool_active("ctx_architecture"));
    }

    #[test]
    fn cannot_unload_core() {
        let mut state = DynamicToolState::new();
        assert!(!state.unload_category(ToolCategory::Core));
    }

    #[test]
    fn all_tools_visible_without_list_changed() {
        let state = DynamicToolState::new();
        assert!(state.is_tool_active("ctx_graph"));
        assert!(!state.is_tool_active("ctx_metrics")); // Internal tools never active
    }

    #[test]
    fn internal_tools_never_active() {
        let state = DynamicToolState::all_enabled();
        assert!(!state.is_tool_active("ctx_metrics"));
        assert!(!state.is_tool_active("ctx_cost"));
        assert!(!state.is_tool_active("ctx_discover_tools"));
        assert!(!state.is_tool_active("ctx_dedup"));
    }

    // --- from_config: basic scenarios ---

    #[test]
    fn from_config_core_arch_memory() {
        let cats = vec!["core".to_string(), "arch".to_string(), "memory".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(state.is_tool_active("ctx_architecture"));
        assert!(state.is_tool_active("ctx_artifacts"));
        assert!(!state.is_tool_active("ctx_benchmark"));
        assert!(!state.is_tool_active("ctx_fill"));
    }

    #[test]
    fn from_config_empty_still_has_core() {
        let mut state = DynamicToolState::from_config(&[]);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(!state.is_tool_active("ctx_architecture"));
        assert!(!state.is_tool_active("ctx_benchmark"));
        assert!(!state.is_tool_active("ctx_artifacts"));
    }

    // --- from_config: all categories ---

    #[test]
    fn from_config_all_categories_enables_everything_except_internal() {
        let cats = vec![
            "core".to_string(),
            "arch".to_string(),
            "debug".to_string(),
            "memory".to_string(),
            "metrics".to_string(),
            "session".to_string(),
        ];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(state.is_tool_active("ctx_architecture"));
        assert!(state.is_tool_active("ctx_benchmark"));
        assert!(state.is_tool_active("ctx_semantic_search"));
        assert!(state.is_tool_active("ctx_fill"));
        assert!(state.is_tool_active("ctx_workflow"));
        assert!(!state.is_tool_active("ctx_metrics"));
    }

    // --- from_config: single category ---

    #[test]
    fn from_config_only_debug() {
        let cats = vec!["debug".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(state.is_tool_active("ctx_benchmark"));
        assert!(!state.is_tool_active("ctx_architecture"));
        assert!(!state.is_tool_active("ctx_workflow"));
    }

    // --- from_config: invalid categories are silently ignored ---

    #[test]
    fn from_config_ignores_unknown_categories() {
        let cats = vec![
            "core".to_string(),
            "nonexistent".to_string(),
            "foobar".to_string(),
        ];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(!state.is_tool_active("ctx_architecture"));
    }

    #[test]
    fn from_config_only_invalid_still_has_core() {
        let cats = vec!["invalid".to_string(), "bogus".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(!state.is_tool_active("ctx_benchmark"));
    }

    // --- from_config: duplicate categories are idempotent ---

    #[test]
    fn from_config_duplicates_are_harmless() {
        let cats = vec!["arch".to_string(), "arch".to_string(), "arch".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_architecture"));
        let active = state.active_categories();
        let arch_count = active.iter().filter(|&&c| c == "arch").count();
        assert_eq!(arch_count, 1);
    }

    // --- from_config: internal category is never user-activatable ---

    #[test]
    fn from_config_internal_category_not_parseable() {
        assert!(ToolCategory::parse("internal").is_none());
    }

    // --- from_config: category aliases work ---

    #[test]
    fn from_config_alias_architecture_maps_to_arch() {
        let cats = vec!["architecture".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_architecture"));
    }

    #[test]
    fn from_config_alias_profiling_maps_to_debug() {
        let cats = vec!["profiling".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_benchmark"));
    }

    #[test]
    fn from_config_alias_semantic_maps_to_memory() {
        let cats = vec!["semantic".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_artifacts"));
    }

    // --- from_config: subsequent load/unload still works ---

    #[test]
    fn from_config_then_load_additional_category() {
        let cats = vec!["core".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(!state.is_tool_active("ctx_architecture"));
        state.load_category(ToolCategory::Arch);
        assert!(state.is_tool_active("ctx_architecture"));
    }

    #[test]
    fn from_config_then_unload_non_core_category() {
        let cats = vec!["core".to_string(), "arch".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_architecture"));
        state.unload_category(ToolCategory::Arch);
        assert!(!state.is_tool_active("ctx_architecture"));
    }

    #[test]
    fn from_config_cannot_unload_core() {
        let cats = vec!["core".to_string(), "arch".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        assert!(!state.unload_category(ToolCategory::Core));
    }

    // --- from_config: without list_changed, all tools visible ---

    #[test]
    fn from_config_without_list_changed_shows_all() {
        let cats = vec!["core".to_string()];
        let state = DynamicToolState::from_config(&cats);
        assert!(state.is_tool_active("ctx_architecture"));
        assert!(state.is_tool_active("ctx_benchmark"));
        assert!(!state.is_tool_active("ctx_metrics"));
    }

    #[test]
    fn lazy_core_tools_survive_default_category_gate() {
        // Regression for #575: every advertised lazy-core tool must stay
        // active under the default category gate (Core + Session) when the
        // client supports list_changed — otherwise Cursor silently loses
        // tools like ctx_expand from the 13-tool core set.
        let mut state = DynamicToolState::new();
        state.set_supports_list_changed(true);
        for name in crate::tool_defs::core_tool_names() {
            assert!(
                state.is_tool_active(name),
                "{name} is in CORE_TOOL_NAMES but dropped by the default category gate"
            );
        }
    }

    #[test]
    fn categorize_known_tools() {
        assert_eq!(categorize_tool("ctx_read"), ToolCategory::Core);
        assert_eq!(categorize_tool("ctx_graph"), ToolCategory::Core);
        assert_eq!(categorize_tool("ctx_benchmark"), ToolCategory::Debug);
        assert_eq!(categorize_tool("ctx_semantic_search"), ToolCategory::Core);
        assert_eq!(categorize_tool("ctx_artifacts"), ToolCategory::Memory);
        assert_eq!(categorize_tool("ctx_metrics"), ToolCategory::Internal);
        assert_eq!(categorize_tool("ctx_workflow"), ToolCategory::Session);
    }

    #[test]
    fn readonly_classification() {
        assert!(is_readonly_tool("ctx_read"));
        assert!(is_readonly_tool("ctx_search"));
        assert!(is_readonly_tool("ctx_tree"));
        assert!(is_readonly_tool("ctx_overview"));
        assert!(is_readonly_tool("ctx_provider"));

        assert!(!is_readonly_tool("ctx_edit"));
        assert!(!is_readonly_tool("ctx_shell"));
        assert!(!is_readonly_tool("ctx_compile"));
        assert!(!is_readonly_tool("ctx_execute"));
        assert!(!is_readonly_tool("ctx_cache"));
    }

    #[test]
    fn plan_mode_tools_are_all_readonly() {
        for tool in crate::core::editor_registry::plan_mode::plan_mode_tools() {
            assert!(
                is_readonly_tool(tool),
                "{tool} is listed as plan mode tool but not marked readonly"
            );
        }
    }
}
