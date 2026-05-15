use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    Core,
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
        "ctx_read" | "ctx_search" | "ctx_shell" | "ctx_tree" | "ctx_edit" | "ctx_plan"
        | "ctx_control" | "ctx_compress" | "ctx_session" | "ctx_knowledge" | "ctx_agent"
        | "ctx_overview" | "ctx_preload" | "ctx_dedup" | "ctx_expand" | "ctx_multi_read"
        | "ctx_smart_read" | "ctx_delta" | "ctx_prefetch" | "ctx_compile" | "ctx_fill"
        | "ctx_execute" | "ctx_context" | "ctx_cache" | "ctx_retrieve" | "ctx_discover_tools"
        | "ctx_pack" | "ctx_feedback" => ToolCategory::Core,

        "ctx_graph" | "ctx_architecture" | "ctx_impact" | "ctx_callgraph" | "ctx_refactor"
        | "ctx_symbol" | "ctx_routes" | "ctx_smells" | "ctx_index" => ToolCategory::Arch,

        "ctx_benchmark" | "ctx_heatmap" | "ctx_verify" | "ctx_analyze" | "ctx_profile"
        | "ctx_proof" | "ctx_review" => ToolCategory::Debug,

        "ctx_semantic_search"
        | "ctx_compress_memory"
        | "ctx_discover"
        | "ctx_provider"
        | "ctx_artifacts" => ToolCategory::Memory,

        "ctx_metrics" | "ctx_cost" | "ctx_gain" | "ctx_intent" | "ctx_response" | "ctx_outline"
        | "ctx_radar" => ToolCategory::Metrics,

        "ctx_share" | "ctx_task" | "ctx_handoff" | "ctx_workflow" => ToolCategory::Session,

        _ => ToolCategory::Core,
    }
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
        if cat == ToolCategory::Core {
            return false;
        }
        self.active_categories.remove(&cat)
    }

    pub fn is_tool_active(&self, name: &str) -> bool {
        if !self.supports_list_changed {
            return true;
        }
        let cat = categorize_tool(name);
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
    fn dynamic_tools_filtered_when_list_changed() {
        let mut state = DynamicToolState::new();
        state.set_supports_list_changed(true);
        assert!(!state.is_tool_active("ctx_graph"));
        assert!(!state.is_tool_active("ctx_benchmark"));
        assert!(state.is_tool_active("ctx_read"));
    }

    #[test]
    fn load_category_enables_tools() {
        let mut state = DynamicToolState::new();
        state.set_supports_list_changed(true);
        assert!(!state.is_tool_active("ctx_graph"));
        state.load_category(ToolCategory::Arch);
        assert!(state.is_tool_active("ctx_graph"));
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
        assert!(state.is_tool_active("ctx_metrics"));
    }

    #[test]
    fn categorize_known_tools() {
        assert_eq!(categorize_tool("ctx_read"), ToolCategory::Core);
        assert_eq!(categorize_tool("ctx_graph"), ToolCategory::Arch);
        assert_eq!(categorize_tool("ctx_benchmark"), ToolCategory::Debug);
        assert_eq!(categorize_tool("ctx_semantic_search"), ToolCategory::Memory);
        assert_eq!(categorize_tool("ctx_metrics"), ToolCategory::Metrics);
        assert_eq!(categorize_tool("ctx_workflow"), ToolCategory::Session);
    }
}
