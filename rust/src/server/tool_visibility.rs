//! Pure tool-visibility policy for the MCP `tools/list` response.
//!
//! Extracted from the (async, server-bound) `list_tools` handler so the policy
//! is unit-testable in isolation. The handler resolves the candidate set
//! (lazy-core vs profile-authoritative vs full registry) and the per-call gates
//! (role, workflow), then defers to these helpers for the stable rules:
//!   * Internal/meta tools are never advertised.
//!   * The active profile, `disabled_tools`, and the Zed `ctx_edit` quirk filter
//!     the candidates.
//!   * The universal invoker (`ctx_call`) is force-advertised in non-full mode so
//!     tools hidden by lazy/profile filtering stay reachable.

use super::dynamic_tools::{ToolCategory, categorize_tool};
use crate::core::tool_profiles::ToolProfile;

/// The universal invoker tool name. A static-list MCP client can call any
/// registered tool through it, even when that tool isn't advertised.
pub const INVOKER: &str = "ctx_call";

/// Which candidate pool `tools/list` starts from, before per-tool gates run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSet {
    /// Full registry (`LEAN_CTX_FULL_TOOLS=1` / `LEAN_CTX_LAZY_TOOLS=0`).
    Full,
    /// Consolidated unified surface (`LEAN_CTX_UNIFIED`).
    Unified,
    /// The user pinned a profile — it is authoritative and resolves against
    /// the full registry (#358), so `standard` advertises its complete set.
    ProfileAuthoritative,
    /// Lean default: only `CORE_TOOL_NAMES` are advertised; everything else
    /// stays reachable through [`INVOKER`] (#575).
    LazyCore,
}

/// Decides the candidate pool. Single source of truth for the `tools/list`
/// handler AND offline measurement (`doctor overhead`), so the advertised
/// surface and the reported overhead can never drift apart.
#[must_use]
pub fn candidate_set(full_mode: bool, unified_env: bool, explicit_profile: bool) -> CandidateSet {
    if full_mode {
        CandidateSet::Full
    } else if unified_env {
        CandidateSet::Unified
    } else if explicit_profile {
        CandidateSet::ProfileAuthoritative
    } else {
        CandidateSet::LazyCore
    }
}

/// Whether the user explicitly pinned a tool profile (config key, custom tool
/// list, or env var) — the trigger for [`CandidateSet::ProfileAuthoritative`].
#[must_use]
pub fn explicit_profile(cfg: &crate::core::config::Config) -> bool {
    cfg.tool_profile.is_some()
        || !cfg.tools_enabled.is_empty()
        || std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok()
}

/// Decides whether a tool name should appear in `tools/list`.
///
/// `role_allows` is supplied by the caller (it depends on the active role, which
/// is resolved outside this pure function). Internal tools are hidden
/// unconditionally — they're invoked automatically or via [`INVOKER`].
#[must_use]
pub fn is_tool_visible(
    name: &str,
    profile: &ToolProfile,
    disabled: &[String],
    is_zed: bool,
    role_allows: bool,
) -> bool {
    if categorize_tool(name) == ToolCategory::Internal {
        return false;
    }
    if !profile.is_tool_enabled(name) {
        return false;
    }
    if disabled.iter().any(|d| d == name) {
        return false;
    }
    if is_zed && name == "ctx_edit" {
        return false;
    }
    role_allows
}

/// Computes the tool set this install advertises to a default client
/// (no Zed quirk, no role restriction, no workflow gate, static tool list),
/// including the live description compression. Offline counterpart of the
/// `tools/list` handler for `doctor overhead` / `ContextOverhead::measure` —
/// kept next to the pure gates so measurement cannot drift from policy.
#[must_use]
pub fn advertised_tool_defs_default() -> Vec<rmcp::model::Tool> {
    let cfg = crate::core::config::Config::load();
    let disabled = cfg.disabled_tools_effective();
    let profile = cfg.tool_profile_effective();
    let full_mode = crate::tool_defs::is_full_mode();
    let registry = crate::server::registry::build_registry();

    let candidate = candidate_set(
        full_mode,
        std::env::var("LEAN_CTX_UNIFIED").is_ok(),
        explicit_profile(&cfg),
    );
    let pool: Vec<rmcp::model::Tool> = match candidate {
        CandidateSet::Full | CandidateSet::ProfileAuthoritative => registry.tool_defs(),
        CandidateSet::Unified => crate::tool_defs::unified_tool_defs(),
        CandidateSet::LazyCore => {
            let core = crate::tool_defs::core_tool_names();
            registry
                .tool_defs()
                .into_iter()
                .filter(|t| core.contains(&t.name.as_ref()))
                .collect()
        }
    };

    let mut tools: Vec<_> = pool
        .into_iter()
        .filter(|t| is_tool_visible(t.name.as_ref(), &profile, &disabled, false, true))
        .collect();

    let already = tools.iter().any(|t| t.name.as_ref() == INVOKER);
    if needs_invoker(full_mode, already, true, &disabled)
        && let Some(def) = registry
            .tool_defs()
            .into_iter()
            .find(|t| t.name.as_ref() == INVOKER)
    {
        tools.push(def);
    }

    let level = crate::core::config::CompressionLevel::effective(&cfg);
    let mode = crate::core::terse::mcp_compress::DescriptionMode::from_compression_level(&level);
    if mode == crate::core::terse::mcp_compress::DescriptionMode::Full {
        return tools;
    }
    tools
        .into_iter()
        .map(|mut t| {
            let compressed = crate::core::terse::mcp_compress::compress_description(
                t.name.as_ref(),
                t.description.as_deref().unwrap_or(""),
                mode,
            );
            t.description = Some(compressed.into());
            t
        })
        .collect()
}

/// Whether the lazy per-category gate should filter the advertised tool set.
///
/// The dynamic-tools category gate (load tools on demand, signalled via
/// `notifications/tools/list_changed`) exists to keep the *default* lean-core
/// surface small for capable clients. An explicit profile is the user's chosen,
/// authoritative surface, so it must be advertised in full — otherwise category
/// gating silently drops profile-enabled tools (e.g. Standard's
/// `ctx_architecture` / `ctx_semantic_search`) for clients like Codex, and the
/// advertised set stops matching `lean-ctx tools show` (#358).
#[must_use]
pub fn category_gate_applies(supports_list_changed: bool, explicit_profile: bool) -> bool {
    supports_list_changed && !explicit_profile
}

/// Whether [`INVOKER`] must be force-added to the advertised set.
///
/// True only in non-full mode when it isn't already present, the role permits
/// it, and it isn't explicitly disabled. In full mode every tool is already
/// listed, so no gateway is needed.
#[must_use]
pub fn needs_invoker(
    full_mode: bool,
    already_present: bool,
    invoker_role_allowed: bool,
    disabled: &[String],
) -> bool {
    !full_mode && !already_present && invoker_role_allowed && !disabled.iter().any(|d| d == INVOKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_tools_never_visible_even_in_power() {
        // Power enables everything, but Internal/meta tools must still be hidden.
        let p = ToolProfile::Power;
        assert!(!is_tool_visible("ctx_metrics", &p, &[], false, true));
        assert!(!is_tool_visible("ctx_cost", &p, &[], false, true));
        assert!(!is_tool_visible("ctx_discover_tools", &p, &[], false, true));
    }

    #[test]
    fn core_tool_visible_under_power() {
        assert!(is_tool_visible(
            "ctx_read",
            &ToolProfile::Power,
            &[],
            false,
            true
        ));
    }

    #[test]
    fn standard_exposes_its_advertised_tools() {
        // These are in STANDARD_TOOLS but were dropped by the old
        // `core ∩ standard` intersection. Profile-authoritative resolution must
        // surface them.
        let p = ToolProfile::Standard;
        assert!(is_tool_visible("ctx_execute", &p, &[], false, true));
        assert!(is_tool_visible("ctx_semantic_search", &p, &[], false, true));
        assert!(is_tool_visible("ctx_callgraph", &p, &[], false, true));
        assert!(is_tool_visible("ctx_graph", &p, &[], false, true));
    }

    #[test]
    fn minimal_hides_non_minimal_tools() {
        let p = ToolProfile::Minimal;
        assert!(is_tool_visible("ctx_read", &p, &[], false, true));
        assert!(!is_tool_visible("ctx_architecture", &p, &[], false, true));
    }

    #[test]
    fn disabled_list_filters() {
        let disabled = vec!["ctx_read".to_string()];
        assert!(!is_tool_visible(
            "ctx_read",
            &ToolProfile::Power,
            &disabled,
            false,
            true
        ));
    }

    #[test]
    fn zed_hides_ctx_edit_only() {
        let p = ToolProfile::Power;
        assert!(!is_tool_visible("ctx_edit", &p, &[], true, true));
        assert!(is_tool_visible("ctx_read", &p, &[], true, true));
    }

    #[test]
    fn role_block_hides_tool() {
        assert!(!is_tool_visible(
            "ctx_read",
            &ToolProfile::Power,
            &[],
            false,
            false
        ));
    }

    #[test]
    fn category_gate_only_in_default_lean_mode() {
        // Lazy gate applies only when the client supports list_changed AND no
        // explicit profile is set.
        assert!(category_gate_applies(true, false));
        // Explicit profile is authoritative — never gated (#358).
        assert!(!category_gate_applies(true, true));
        // Static-list clients are never gated regardless of profile.
        assert!(!category_gate_applies(false, false));
        assert!(!category_gate_applies(false, true));
    }

    #[test]
    fn invoker_added_when_missing_in_lazy_mode() {
        assert!(needs_invoker(false, false, true, &[]));
    }

    #[test]
    fn invoker_not_added_in_full_mode() {
        assert!(!needs_invoker(true, false, true, &[]));
    }

    #[test]
    fn invoker_not_duplicated_when_present() {
        assert!(!needs_invoker(false, true, true, &[]));
    }

    #[test]
    fn invoker_respects_role_and_disabled() {
        assert!(!needs_invoker(false, false, false, &[]));
        assert!(!needs_invoker(
            false,
            false,
            true,
            &["ctx_call".to_string()]
        ));
    }

    /// #576 schema diet: the lazy-core surface is the default fixed cost every
    /// session pays — keep it bounded. Per-tool cap keeps any single schema
    /// from bloating; the total cap keeps the whole advertised surface lean.
    /// (Raw registry defs, before description compression — worst case.)
    ///
    /// The total grew with the 14th core tool, `ctx_semantic_search` (#422):
    /// it joined the lean core so agents discover semantic search by default
    /// instead of never reaching for it. The per-tool cap (300) still guards
    /// individual bloat; the total budget is sized to that 14-tool surface.
    ///
    /// Bumped to 2260 for #432: `ctx_read` now advertises the `offset`/`limit`
    /// aliases (so agents trained on the native Read tool discover them), a
    /// deliberate +~32 tok. Descriptions are kept terse to limit the cost.
    ///
    /// Bumped to 2275 for #451: `ctx_shell` now states it runs the system shell
    /// profile-free (no rc/profile sourced), a deliberate +~13 tok so agents stop
    /// mistaking it for a config-loaded interactive bash. Kept to one terse clause.
    ///
    /// Bumped to per-tool 335 / total 2310 for #513: `ctx_read` now documents the
    /// verbatim escape hatch (`raw=true` arg + `raw` mode) so agents — especially
    /// non-Opus models that fought the compression — discover how to get exact
    /// bytes for review/audit instead of guessing. `ctx_read` is the richest core
    /// tool and is the only one that crosses 300; the per-tool cap still guards
    /// every other tool from bloat. Kept to terse clauses (+~33 tok on ctx_read).
    #[test]
    fn core_tool_surface_stays_within_budget() {
        const PER_TOOL_BUDGET: usize = 335;
        const TOTAL_BUDGET: usize = 2310;

        let _guard = crate::core::data_dir::isolated_data_dir();
        let core = crate::tool_defs::core_tool_names();
        let defs: Vec<_> = crate::server::registry::build_registry()
            .tool_defs()
            .into_iter()
            .filter(|t| core.contains(&t.name.as_ref()))
            .collect();
        assert_eq!(defs.len(), core.len(), "every core tool must be registered");

        let mut total = 0usize;
        for t in &defs {
            let desc = t.description.as_deref().unwrap_or("");
            let schema = serde_json::to_string(&t.input_schema).unwrap_or_default();
            let cost = crate::core::tokens::count_tokens(desc)
                + crate::core::tokens::count_tokens(&schema);
            eprintln!("{:24} {cost:4} tok", t.name.as_ref());
            assert!(
                cost <= PER_TOOL_BUDGET,
                "{} costs {cost} tok (budget {PER_TOOL_BUDGET}) — trim its description/schema",
                t.name
            );
            total += cost;
        }
        eprintln!("CORE TOTAL: {total} tok / {} tools", defs.len());
        assert!(
            total <= TOTAL_BUDGET,
            "core surface costs {total} tok (budget {TOTAL_BUDGET})"
        );
    }
}
