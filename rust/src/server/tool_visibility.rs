//! Pure tool-visibility policy for the MCP `tools/list` response.
//!
//! Extracted from the (async, server-bound) `list_tools` handler so the policy
//! is unit-testable in isolation. The handler resolves the candidate set
//! (lazy-core vs profile-authoritative vs full registry) and the per-call gates
//! (role, workflow), then defers to these helpers for the stable rules:
//!   * Internal/meta tools are never advertised.
//!   * The active profile, `disabled_tools`, and the per-client
//!     [`ClientQuirks`] (Zed `ctx_edit`, native-editor `ctx_patch`) filter the
//!     candidates.
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

/// Client-specific advertising quirks, resolved once per `tools/list` from the
/// MCP `clientInfo` name and the candidate set.
///
/// [`ClientQuirks::default`] (no quirks) is the "default client" used by
/// offline measurement — the worst-case surface, nothing hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClientQuirks {
    /// Zed cannot handle `ctx_edit` (schema quirk) — hide it there.
    pub hide_ctx_edit: bool,
    /// Lazy-core only (#1008): the client ships a reliable native str-replace
    /// editor, so the *default* surface skips `ctx_patch` — those sessions pay
    /// zero extra schema tokens. A pinned profile is the user's explicit,
    /// client-agnostic choice and always advertises its full set.
    pub hide_ctx_patch: bool,
}

impl ClientQuirks {
    /// Resolve the quirks for one `tools/list` answer.
    #[must_use]
    pub fn resolve(client_name: &str, candidate: CandidateSet) -> Self {
        let lower = client_name.to_lowercase();
        Self {
            hide_ctx_edit: lower.contains("zed"),
            hide_ctx_patch: candidate == CandidateSet::LazyCore && has_native_editor(&lower),
        }
    }
}

/// Clients whose built-in edit tool is reliable enough that the default
/// (lazy-core) surface need not advertise `ctx_patch`: Cursor, Zed,
/// Windsurf/Codeium, Antigravity, OpenCode. Everyone else gets the anchored
/// editor — Claude Code (the hook read-redirect breaks its native
/// read-before-write guard, #637), CodeBuddy, pi/SDK harnesses and
/// unknown/headless clients that have no native editor at all.
fn has_native_editor(lower_client_name: &str) -> bool {
    [
        "cursor",
        "zed",
        "windsurf",
        "codeium",
        "antigravity",
        "opencode",
    ]
    .iter()
    .any(|c| lower_client_name.contains(c))
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
    quirks: ClientQuirks,
    role_allows: bool,
) -> bool {
    if categorize_tool(name) == ToolCategory::Internal {
        return false;
    }
    // #509: deprecated read-cluster aliases (ctx_smart_read, ctx_multi_read) are
    // hidden from the advertised surface but stay callable for one release.
    if super::dynamic_tools::is_deprecated_alias(name) {
        return false;
    }
    if !profile.is_tool_enabled(name) {
        return false;
    }
    if disabled.iter().any(|d| d == name) {
        return false;
    }
    if quirks.hide_ctx_edit && name == "ctx_edit" {
        return false;
    }
    if quirks.hide_ctx_patch && name == "ctx_patch" {
        return false;
    }
    role_allows
}

/// Computes the tool set this install advertises to a default client
/// (no client quirks, no role restriction, no workflow gate, static tool list),
/// including the live description compression. Offline counterpart of the
/// `tools/list` handler for `doctor overhead` / `ContextOverhead::measure` —
/// kept next to the pure gates so measurement cannot drift from policy.
/// "No quirks" is the worst case: a client without a native editor sees
/// `ctx_patch` too, so the reported overhead never understates.
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
        .filter(|t| {
            is_tool_visible(
                t.name.as_ref(),
                &profile,
                &disabled,
                ClientQuirks::default(),
                true,
            )
        })
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

    /// No client quirks — the default/measurement client.
    fn no_quirks() -> ClientQuirks {
        ClientQuirks::default()
    }

    #[test]
    fn internal_tools_never_visible_even_in_power() {
        // Power enables everything, but Internal/meta tools must still be hidden.
        let p = ToolProfile::Power;
        assert!(!is_tool_visible("ctx_metrics", &p, &[], no_quirks(), true));
        assert!(!is_tool_visible("ctx_cost", &p, &[], no_quirks(), true));
        assert!(!is_tool_visible(
            "ctx_discover_tools",
            &p,
            &[],
            no_quirks(),
            true
        ));
    }

    #[test]
    fn deprecated_aliases_never_visible_even_in_power() {
        // #509: folded read-cluster aliases are hidden from tools/list in every
        // mode (Power enables everything) — but stay registered + callable.
        let p = ToolProfile::Power;
        assert!(!is_tool_visible(
            "ctx_smart_read",
            &p,
            &[],
            no_quirks(),
            true
        ));
        assert!(!is_tool_visible(
            "ctx_multi_read",
            &p,
            &[],
            no_quirks(),
            true
        ));
    }

    #[test]
    fn deprecated_aliases_stay_registered_and_callable() {
        // The non-breaking contract (#509): hidden from the advertised surface,
        // but still in the registry so direct calls and ctx_call keep working
        // for one release. Removal is Phase 2.
        let _guard = crate::core::data_dir::isolated_data_dir();
        let defs = crate::server::registry::build_registry().tool_defs();
        for name in [
            "ctx_smart_read",
            "ctx_multi_read",
            "ctx_semantic_search",
            "ctx_symbol",
        ] {
            assert!(
                defs.iter().any(|t| t.name.as_ref() == name),
                "{name} must stay registered (callable) even though hidden"
            );
            assert!(
                !is_tool_visible(name, &ToolProfile::Power, &[], no_quirks(), true),
                "{name} must be hidden from tools/list"
            );
        }
    }

    #[test]
    fn core_tool_visible_under_power() {
        assert!(is_tool_visible(
            "ctx_read",
            &ToolProfile::Power,
            &[],
            no_quirks(),
            true
        ));
    }

    #[test]
    fn standard_exposes_its_advertised_tools() {
        // These are in STANDARD_TOOLS but were dropped by the old
        // `core ∩ standard` intersection. Profile-authoritative resolution must
        // surface them.
        let p = ToolProfile::Standard;
        assert!(is_tool_visible("ctx_execute", &p, &[], no_quirks(), true));
        assert!(is_tool_visible("ctx_explore", &p, &[], no_quirks(), true));
        assert!(is_tool_visible("ctx_callgraph", &p, &[], no_quirks(), true));
        assert!(is_tool_visible("ctx_graph", &p, &[], no_quirks(), true));
        // #1008: anchored editing ships with the pinned Standard profile.
        assert!(is_tool_visible("ctx_patch", &p, &[], no_quirks(), true));
    }

    #[test]
    fn folded_search_aliases_never_visible() {
        // #509: ctx_semantic_search + ctx_symbol are consolidated into ctx_search
        // (action=…). Hidden from tools/list in every mode, but stay callable.
        let p = ToolProfile::Power;
        assert!(!is_tool_visible(
            "ctx_semantic_search",
            &p,
            &[],
            no_quirks(),
            true
        ));
        assert!(!is_tool_visible("ctx_symbol", &p, &[], no_quirks(), true));
        assert!(is_tool_visible("ctx_search", &p, &[], no_quirks(), true));
    }

    #[test]
    fn minimal_hides_non_minimal_tools() {
        let p = ToolProfile::Minimal;
        assert!(is_tool_visible("ctx_read", &p, &[], no_quirks(), true));
        assert!(!is_tool_visible(
            "ctx_architecture",
            &p,
            &[],
            no_quirks(),
            true
        ));
    }

    #[test]
    fn disabled_list_filters() {
        let disabled = vec!["ctx_read".to_string()];
        assert!(!is_tool_visible(
            "ctx_read",
            &ToolProfile::Power,
            &disabled,
            no_quirks(),
            true
        ));
    }

    #[test]
    fn zed_hides_ctx_edit_only() {
        let p = ToolProfile::Power;
        let zed = ClientQuirks {
            hide_ctx_edit: true,
            hide_ctx_patch: false,
        };
        assert!(!is_tool_visible("ctx_edit", &p, &[], zed, true));
        assert!(is_tool_visible("ctx_read", &p, &[], zed, true));
    }

    #[test]
    fn native_editor_quirk_hides_ctx_patch_only() {
        // #1008: a native-editor client in the lazy default drops ctx_patch —
        // and nothing else.
        let p = ToolProfile::Power;
        let native = ClientQuirks {
            hide_ctx_edit: false,
            hide_ctx_patch: true,
        };
        assert!(!is_tool_visible("ctx_patch", &p, &[], native, true));
        assert!(is_tool_visible("ctx_read", &p, &[], native, true));
        assert!(is_tool_visible("ctx_edit", &p, &[], native, true));
    }

    #[test]
    fn quirks_resolution_is_client_and_candidate_aware() {
        // Native-editor clients skip ctx_patch in the lazy default…
        for client in ["Cursor", "zed 0.164", "Windsurf", "antigravity", "opencode"] {
            let q = ClientQuirks::resolve(client, CandidateSet::LazyCore);
            assert!(q.hide_ctx_patch, "{client}: lazy core must hide ctx_patch");
        }
        // …clients without a reliable native editor get it (#637: Claude Code's
        // read-before-write guard breaks under the read-redirect hook).
        for client in ["claude-code", "CodeBuddy", "pi", "", "my-sdk-harness"] {
            let q = ClientQuirks::resolve(client, CandidateSet::LazyCore);
            assert!(
                !q.hide_ctx_patch,
                "{client:?}: lazy core must show ctx_patch"
            );
        }
        // A pinned profile is client-agnostic — never hide ctx_patch there.
        for candidate in [
            CandidateSet::ProfileAuthoritative,
            CandidateSet::Full,
            CandidateSet::Unified,
        ] {
            let q = ClientQuirks::resolve("Cursor", candidate);
            assert!(
                !q.hide_ctx_patch,
                "{candidate:?}: pinned/full surfaces are client-agnostic"
            );
        }
        // The Zed ctx_edit quirk is independent of the candidate set.
        assert!(ClientQuirks::resolve("zed", CandidateSet::Full).hide_ctx_edit);
        assert!(!ClientQuirks::resolve("Cursor", CandidateSet::Full).hide_ctx_edit);
    }

    #[test]
    fn role_block_hides_tool() {
        assert!(!is_tool_visible(
            "ctx_read",
            &ToolProfile::Power,
            &[],
            no_quirks(),
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
    ///
    /// Bumped to per-tool 360 / total 2340 for #509: `ctx_read` absorbs the
    /// `ctx_multi_read` batch capability via a `paths` array, so two tools collapse
    /// into one (`ctx_smart_read` + `ctx_multi_read` are now deprecated aliases
    /// hidden from the surface). The net effect REDUCES the advertised surface; the
    /// only local cost is +~18 tok on `ctx_read`'s schema for the new `paths` arg.
    ///
    /// #509 search consolidation (cont.): `ctx_search` now subsumes semantic
    /// search + symbol lookup via an `action` enum, so `ctx_semantic_search` left
    /// the core set (it + `ctx_symbol` are deprecated aliases). `ctx_search` grew
    /// (~196 → ~318 tok) but the core total DROPPED (~2298 → ~2150, one fewer
    /// tool), so the budgets were left unchanged with comfortable headroom.
    ///
    /// #578 schema diet: redundant per-property descriptions dropped (names +
    /// enums self-explain), teaching paragraphs tightened, and `ctx_callgraph`
    /// (~147 tok) replaced `ctx_graph` (~300 tok) in the lazy core so the
    /// advertised set matches the injected INTENT playbook. Measured ~1685 tok
    /// → budgets lowered 360→300 per tool, 2340→1780 total. What remains is
    /// functional teaching (ctx_read mode enum, ctx_search action routing,
    /// compose-first) — cut below this only with A/B efficacy evidence.
    ///
    /// Bumped to 2050 total for #1008: `ctx_patch` (anchored editing, ~263 tok
    /// after its schema diet) joined the lazy core so the injected "edit after
    /// reading → ctx_patch" rule points at an advertised tool. This is the
    /// worst case (no client quirks): clients with a reliable native editor
    /// (Cursor, Zed, Windsurf, …) skip `ctx_patch` via `ClientQuirks` and stay
    /// at the previous ~1685-tok surface.
    ///
    /// Bumped to 2060 for #870: `ctx_search` gained `exclude`/`exclude_pattern`
    /// negative filters (+~7 tok on its schema). The per-tool 300 gate is
    /// unchanged — ctx_search stays well under it (~292 tok).
    #[test]
    fn core_tool_surface_stays_within_budget() {
        const PER_TOOL_BUDGET: usize = 300;
        const TOTAL_BUDGET: usize = 2060;

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
