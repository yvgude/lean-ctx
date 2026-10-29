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
        | "ctx_discover"
        | "ctx_discover_tools"
        | "ctx_load_tools"
        | "ctx_dedup"
        | "ctx_preload"
        | "ctx_prefetch"
        | "ctx_compress_memory"
        | "ctx_shape" => ToolCategory::Internal,

        // Core: always visible. Must cover every CORE_TOOL_NAMES entry —
        // otherwise the category gate silently drops a lazy-core tool for
        // list_changed-capable clients (ctx_expand was lost this way, #575).
        // ctx_callgraph joined the lazy core in #578 (INTENT routes
        // callers/impact to it), so it must be Core here too.
        // ctx_patch joined in #1008 (anchored editing is a core workflow).
        "ctx_read" | "ctx_search" | "ctx_shell" | "shell" | "ctx_tree" | "ctx_edit"
        | "ctx_patch" | "ctx_session" | "ctx_checkpoint" | "ctx_knowledge" | "ctx_graph"
        | "ctx_callgraph" | "ctx_call" | "ctx_compress" | "ctx_expand" | "ctx_delta"
        | "ctx_outline" | "ctx_context" => ToolCategory::Core,

        // Arch: on-demand index maintenance
        "ctx_index" => ToolCategory::Arch,

        // Debug/Verify: on-demand verification
        "ctx_verify" | "ctx_profile" | "ctx_proof" => ToolCategory::Debug,

        // Provider + URL/Git readers + the MCP gateway are Core: gateways to
        // external context (GitHub issues, Jira, Postgres, web pages, YouTube,
        // remote git repos, downstream MCP servers) — always available.
        "ctx_provider" | "ctx_url_read" | "ctx_git_read" | "ctx_tools" => ToolCategory::Core,

        // Memory: on-demand artifact retrieval
        "ctx_artifacts" => ToolCategory::Memory,

        // Batch: on-demand batch/PR/sandbox tools
        "ctx_execute" | "ctx_pack" | "ctx_control" => ToolCategory::Metrics,

        // Multi-agent: on-demand collaboration
        "ctx_agent" | "ctx_share" | "ctx_handoff" | "ctx_work_graph" | "ctx_tasks"
        | "ctx_compute" => ToolCategory::Session,

        _ => ToolCategory::Core,
    }
}

/// A tool that was removed: folded into a primary tool (#509) or retired
/// without a successor (focus release, 2026-10). Only the name survives, so a
/// call from an older agent prompt or config gets the way forward instead of
/// a bare "Unknown tool" (`docs/migration/focus-2026-10.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovedTool {
    /// The primary tool that covers this use case (e.g. `"ctx_read"`), or
    /// `None` for a tool retired from the Context Gateway without a successor.
    pub replacement: Option<&'static str>,
    /// One-line migration hint (e.g. how the primary covers this use case).
    pub hint: &'static str,
}

const fn superseded(replacement: &'static str, hint: &'static str) -> Option<RemovedTool> {
    Some(RemovedTool {
        replacement: Some(replacement),
        hint,
    })
}

const fn retired(hint: &'static str) -> Option<RemovedTool> {
    Some(RemovedTool {
        replacement: None,
        hint,
    })
}

/// The successor and migration hint for a removed tool name, else `None`.
#[must_use]
pub fn removed_tool(name: &str) -> Option<RemovedTool> {
    match name {
        "ctx_smart_read" => superseded(
            "ctx_read",
            "ctx_read auto-selects the mode (omit `mode`, or pass mode=\"auto\")",
        ),
        "ctx_multi_read" => superseded(
            "ctx_read",
            "ctx_read now batch-reads via paths=[\"a.rs\",\"b.rs\"]",
        ),
        // #509 search consolidation: one ctx_search entry, `action` picks the engine.
        "ctx_semantic_search" => superseded(
            "ctx_search",
            "ctx_search with action=\"semantic\" (query=…); reindex/find_related are actions too",
        ),
        "ctx_symbol" => superseded(
            "ctx_search",
            "ctx_search with action=\"symbol\" (name=…, optional file/kind)",
        ),
        // Focus release 2026-10-15, Select: one task-ranked entry point.
        "ctx_overview" | "ctx_summary" | "ctx_explore" => superseded(
            "ctx_compose",
            "ctx_compose(task=…) returns the ranked files, symbols and structure for a task",
        ),
        "ctx_plan" | "ctx_compile" | "ctx_fill" => superseded(
            "ctx_compose",
            "ctx_compose selects under the turn budget; read chosen files with ctx_read",
        ),
        // One code-graph tool beside ctx_callgraph.
        "ctx_impact" => superseded(
            "ctx_graph",
            "ctx_graph action=\"impact\" (path=…), action=\"path\" for chains, action=\"diff\" for changes",
        ),
        "ctx_architecture" | "ctx_repomap" => superseded(
            "ctx_graph",
            "ctx_graph action=\"diagram\"|\"explain\"|\"related\"; ctx_tree for the directory map",
        ),
        "ctx_routes" => superseded(
            "ctx_search",
            "ctx_search for route definitions (pattern=…), ctx_graph action=\"related\" for handlers",
        ),
        "ctx_retrieve" => superseded(
            "ctx_expand",
            "ctx_expand id=… returns withheld or archived content",
        ),
        // Maintenance, not an agent decision: the CLI keeps it. (`ctx_index`
        // stays public: it carries `ctx_search`'s former reindex action, #1624.)
        "ctx_cache" => superseded(
            "ctx_read",
            "ctx_read fresh=true re-reads; maintenance runs via `lean-ctx cache prune|reset`",
        ),
        // Focus release 2026-10-15, retired: not part of Select · Control · Prove.
        "ctx_smells" | "ctx_quality" | "ctx_optimize" | "ctx_review" => {
            retired("code review and linting belong to the host's own tooling")
        }
        "ctx_quality_lab" | "ctx_benchmark" | "ctx_perf" | "ctx_cognitive" | "ctx_ledger"
        | "ctx_live_zone" => retired(
            "measurement and diagnostics live in `lean-ctx gain`, `lean-ctx doctor` and the dashboard",
        ),
        "ctx_refactor" => retired(
            "write refactorings are host execution; ctx_callgraph keeps the LSP read side (references)",
        ),
        "ctx_analyze" | "ctx_compare" => retired(
            "ctx_read mode=\"auto\" picks the mode; `lean-ctx compress diff <file>` previews compression",
        ),
        "ctx_crush" => {
            retired("ctx_read and ctx_shell apply the JSON/CSV/YAML crushers automatically")
        }
        "ctx_response" => {
            retired("LeanCTX shapes the context the model reads, not the model's own responses")
        }
        "ctx_skillify" | "ctx_rules" => retired(
            "agent rule files are host configuration; use `lean-ctx rules status|diff` or `lean-ctx doctor`",
        ),
        "ctx_workflow" => retired(
            "workflow execution belongs to the host; ctx_work_graph keeps the shared coordination state",
        ),
        // Retired 2026-10-17: no calls from any installation in 30 days.
        "ctx_task" => retired(
            "task assignment is host execution; ctx_work_graph keeps the shared coordination state",
        ),
        "ctx_intent" => retired(
            "LeanCTX infers intent from the tool calls; ctx_compose(task=…) selects context for a task",
        ),
        _ => None,
    }
}

/// Local collaboration substrate that remains callable for installed users but
/// is excluded from every public MCP tool surface.
///
/// LeanCTX is a Context SDK, not an agent coordination product. These tools
/// support local development compatibility only; they do not imply a supported
/// public agent bus, task scheduler, workflow engine, or A2A product.
pub const LOCAL_COLLABORATION_COMPATIBILITY_TOOLS: &[&str] =
    &["ctx_agent", "ctx_share", "ctx_handoff"];

/// Whether a registered tool is retained only for local compatibility and must
/// never be advertised from a public tool-definition path.
#[must_use]
pub fn is_local_collaboration_compatibility_tool(name: &str) -> bool {
    LOCAL_COLLABORATION_COMPATIBILITY_TOOLS.contains(&name)
}

/// Host-integration hooks: called by a host extension (the Claude Code mod's
/// `$.mcp.call`, through the listed `ctx_call` since hosts may refuse unlisted
/// tools, #2089), never by an agent. Advertising them would only spend prompt
/// tokens on a tool the model must not use.
pub const INTERNAL_HOST_TOOLS: &[&str] = &["ctx_shape"];

/// Whether a call — direct, or through `ctx_call`/`ctx` — targets an internal
/// host hook. A host replaces a native tool's output with the hook's success
/// body, so every gate refusal on such a call must read as an error: then the
/// host keeps the original output instead of a block message (#2089).
#[must_use]
pub fn targets_internal_host_tool(
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    crate::tools::registered::ctx_call::resolve(name, args)
        .is_ok_and(|(inner, _)| INTERNAL_HOST_TOOLS.contains(&inner.as_str()))
}

/// Hive MCP tools. They ship with the connector but follow its release switch
/// (`core::hive::available_locally`, owner 28.10.): while it is off they are
/// in no tool list and every call answers [`crate::core::hive::TOOL_NOT_AVAILABLE`].
pub const HIVE_TOOLS: &[&str] = &["ctx_tasks", "ctx_compute"];

/// Whether `name` is a Hive tool withheld by the release switch.
#[must_use]
pub fn is_withheld_hive_tool(name: &str) -> bool {
    HIVE_TOOLS.contains(&name) && !crate::core::hive::available_locally()
}

/// The fixed answer for a call — direct, or through `ctx_call`/`ctx` — to a
/// withheld Hive tool, else `None`. Stable text (#498).
#[must_use]
pub fn withheld_hive_tool_refusal(
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<&'static str> {
    crate::tools::registered::ctx_call::resolve(name, args)
        .is_ok_and(|(inner, _)| is_withheld_hive_tool(&inner))
        .then_some(crate::core::hive::TOOL_NOT_AVAILABLE)
}

/// Whether a registered tool belongs on a public MCP tool surface.
///
/// Local collaboration substrate and internal host hooks remain directly
/// callable, including through `ctx_call`; this gate only controls discovery.
/// Withheld Hive tools are neither advertised nor callable.
#[must_use]
pub fn is_publicly_advertised_tool(name: &str) -> bool {
    !is_local_collaboration_compatibility_tool(name)
        && !INTERNAL_HOST_TOOLS.contains(&name)
        && !is_withheld_hive_tool(name)
}

/// The error text for a call to a removed tool. Stable per name (no
/// timestamps/counters) so provider-side prompt caching stays byte-stable (#498).
#[must_use]
pub fn removed_tool_message(name: &str) -> Option<String> {
    removed_tool(name).map(|r| match r.replacement {
        Some(replacement) => format!(
            "{name} was removed; use {replacement} instead — {}.",
            r.hint
        ),
        None => format!("{name} was removed — {}.", r.hint),
    })
}

pub fn is_readonly_tool(name: &str) -> bool {
    matches!(
        name,
        "ctx_read"
            | "ctx_search"
            | "ctx_tree"
            | "ctx_metrics"
            | "ctx_compress"
            | "ctx_session"
            | "ctx_knowledge"
            | "ctx_graph"
            | "ctx_provider"
            | "ctx_delta"
            | "ctx_outline"
            | "ctx_context"
            | "ctx_call"
            | "ctx_url_read"
            | "ctx_git_read"
            | "ctx_callgraph"
            | "ctx_index"
            | "ctx_artifacts"
            | "ctx_cost"
            | "ctx_gain"
            | "ctx_heatmap"
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
            // Clients without `list_changed` cannot progressively reveal a
            // category. Keep experimental local collaboration out of their
            // default surface too; an explicit config category still enables it.
            return cat != ToolCategory::Session || self.active_categories.contains(&cat);
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
        assert!(!state.is_tool_active("ctx_agent"));
    }

    #[test]
    fn removed_tools_point_at_a_live_public_tool() {
        assert_eq!(
            removed_tool("ctx_multi_read").unwrap().replacement,
            Some("ctx_read")
        );
        assert!(removed_tool("ctx_read").is_none());
        // No removed name may send agents to a tool that is gone or hidden.
        let registry = crate::server::registry::build_registry();
        let live = registry.names();
        for name in [
            "ctx_smart_read",
            "ctx_multi_read",
            "ctx_semantic_search",
            "ctx_symbol",
            "ctx_overview",
            "ctx_summary",
            "ctx_explore",
            "ctx_plan",
            "ctx_compile",
            "ctx_fill",
            "ctx_impact",
            "ctx_architecture",
            "ctx_repomap",
            "ctx_routes",
            "ctx_retrieve",
            "ctx_cache",
        ] {
            let replacement = removed_tool(name).and_then(|r| r.replacement).unwrap();
            assert!(!live.contains(&name), "{name} is still registered");
            assert!(
                live.contains(&replacement) && is_publicly_advertised_tool(replacement),
                "{name} points at {replacement}, which is not a live public tool"
            );
        }
    }

    #[test]
    fn local_collaboration_substrate_is_compatibility_only() {
        for name in LOCAL_COLLABORATION_COMPATIBILITY_TOOLS {
            assert!(is_local_collaboration_compatibility_tool(name));
            assert!(!is_publicly_advertised_tool(name));
        }
        assert!(!is_local_collaboration_compatibility_tool("ctx_session"));
        assert!(is_publicly_advertised_tool("ctx_session"));
        // Host hooks are callable but must never cost agents prompt tokens.
        assert!(!is_publicly_advertised_tool("ctx_shape"));
    }

    /// #2089: hosts reach ctx_shape through the listed ctx_call, so a call is a
    /// host-hook call in every spelling — and only then.
    #[test]
    fn host_hook_calls_are_recognised_in_every_spelling() {
        let args = |v: serde_json::Value| v.as_object().cloned();
        let shape = args(serde_json::json!({"tool": "Bash", "output": "x"}));
        assert!(targets_internal_host_tool("ctx_shape", shape.as_ref()));
        let via_call =
            args(serde_json::json!({"name": "ctx_shape", "arguments": {"tool": "Bash"}}));
        assert!(targets_internal_host_tool("ctx_call", via_call.as_ref()));
        let via_ctx = args(serde_json::json!({"tool": "shape", "output": "x"}));
        assert!(targets_internal_host_tool("ctx", via_ctx.as_ref()));

        let read = args(serde_json::json!({"name": "ctx_read", "arguments": {"path": "a"}}));
        assert!(!targets_internal_host_tool("ctx_call", read.as_ref()));
        assert!(!targets_internal_host_tool("ctx_read", None));
        assert!(!targets_internal_host_tool("ctx_call", None));
    }

    #[test]
    fn removed_tool_message_names_the_way_forward() {
        let message = removed_tool_message("ctx_multi_read").unwrap();
        assert!(message.starts_with("ctx_multi_read was removed; use ctx_read instead — "));
        assert!(message.contains("paths="));
        assert!(removed_tool_message("ctx_read").is_none());
        let retired = removed_tool_message("ctx_smells").unwrap();
        assert!(retired.starts_with("ctx_smells was removed — "));
    }

    #[test]
    fn dynamic_tools_filtered_when_list_changed() {
        let mut state = DynamicToolState::new();
        state.set_supports_list_changed(true);
        assert!(!state.is_tool_active("ctx_verify"));
        assert!(!state.is_tool_active("ctx_index"));
        assert!(state.is_tool_active("ctx_read"));
    }

    #[test]
    fn load_category_enables_tools() {
        let mut state = DynamicToolState::new();
        state.set_supports_list_changed(true);
        assert!(!state.is_tool_active("ctx_index"));
        state.load_category(ToolCategory::Arch);
        assert!(state.is_tool_active("ctx_index"));
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
        assert!(!state.is_tool_active("ctx_agent"));
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
        assert!(state.is_tool_active("ctx_index"));
        assert!(state.is_tool_active("ctx_artifacts"));
        assert!(!state.is_tool_active("ctx_verify"));
        assert!(!state.is_tool_active("ctx_pack"));
    }

    #[test]
    fn from_config_empty_still_has_core() {
        let mut state = DynamicToolState::from_config(&[]);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(!state.is_tool_active("ctx_index"));
        assert!(!state.is_tool_active("ctx_verify"));
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
        assert!(state.is_tool_active("ctx_index"));
        assert!(state.is_tool_active("ctx_verify"));
        assert!(state.is_tool_active("ctx_provider"));
        assert!(state.is_tool_active("ctx_pack"));
        assert!(state.is_tool_active("ctx_work_graph"));
        assert!(!state.is_tool_active("ctx_metrics"));
    }

    // --- from_config: single category ---

    #[test]
    fn from_config_only_debug() {
        let cats = vec!["debug".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(state.is_tool_active("ctx_verify"));
        assert!(!state.is_tool_active("ctx_index"));
        assert!(!state.is_tool_active("ctx_work_graph"));
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
        assert!(!state.is_tool_active("ctx_index"));
    }

    #[test]
    fn from_config_only_invalid_still_has_core() {
        let cats = vec!["invalid".to_string(), "bogus".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_read"));
        assert!(!state.is_tool_active("ctx_verify"));
    }

    // --- from_config: duplicate categories are idempotent ---

    #[test]
    fn from_config_duplicates_are_harmless() {
        let cats = vec!["arch".to_string(), "arch".to_string(), "arch".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_index"));
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
        assert!(state.is_tool_active("ctx_index"));
    }

    #[test]
    fn from_config_alias_profiling_maps_to_debug() {
        let cats = vec!["profiling".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_verify"));
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
        assert!(!state.is_tool_active("ctx_index"));
        state.load_category(ToolCategory::Arch);
        assert!(state.is_tool_active("ctx_index"));
    }

    #[test]
    fn from_config_then_unload_non_core_category() {
        let cats = vec!["core".to_string(), "arch".to_string()];
        let mut state = DynamicToolState::from_config(&cats);
        state.set_supports_list_changed(true);
        assert!(state.is_tool_active("ctx_index"));
        state.unload_category(ToolCategory::Arch);
        assert!(!state.is_tool_active("ctx_index"));
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
        assert!(state.is_tool_active("ctx_index"));
        assert!(state.is_tool_active("ctx_verify"));
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
        // #1008: anchored editing is a core workflow — an explicit Core entry so
        // the category gate can never hide the lazy-core editor.
        assert_eq!(categorize_tool("ctx_patch"), ToolCategory::Core);
        assert_eq!(categorize_tool("ctx_verify"), ToolCategory::Debug);
        assert_eq!(categorize_tool("ctx_provider"), ToolCategory::Core);
        assert_eq!(categorize_tool("ctx_artifacts"), ToolCategory::Memory);
        assert_eq!(categorize_tool("ctx_metrics"), ToolCategory::Internal);
        assert_eq!(categorize_tool("ctx_work_graph"), ToolCategory::Session);
    }

    #[test]
    fn readonly_classification() {
        assert!(is_readonly_tool("ctx_read"));
        assert!(is_readonly_tool("ctx_search"));
        assert!(is_readonly_tool("ctx_tree"));
        assert!(is_readonly_tool("ctx_graph"));
        assert!(is_readonly_tool("ctx_provider"));

        assert!(!is_readonly_tool("ctx_edit"));
        assert!(!is_readonly_tool("ctx_shell"));
        assert!(!is_readonly_tool("ctx_patch"));
        assert!(!is_readonly_tool("ctx_execute"));
        assert!(!is_readonly_tool("ctx_work_graph"));
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
