// SPDX-License-Identifier: Apache-2.0
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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::dynamic_tools::{ToolCategory, categorize_tool};
use crate::core::tool_profiles::ToolProfile;

// ── Auto-profile session signals ─────────────────────────────

static AUTO_TURN_COUNT: AtomicU64 = AtomicU64::new(0);
static AUTO_CTX_TOOLS_USED: AtomicBool = AtomicBool::new(false);
static AUTO_SYSTEM_PROMPT_TOKENS: AtomicUsize = AtomicUsize::new(0);

/// Increment the Auto-profile turn counter (call once per tools/list request).
pub fn record_auto_turn() {
    AUTO_TURN_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Mark that the agent has invoked a ctx_* MCP tool in this session.
pub fn mark_auto_ctx_tool_used() {
    AUTO_CTX_TOOLS_USED.store(true, Ordering::Relaxed);
}

/// Resolve `ToolProfile::Auto` to a concrete profile using session signals.
/// Non-Auto profiles are returned as-is.
#[must_use]
pub fn resolve_auto_profile(profile: &ToolProfile) -> ToolProfile {
    if *profile != ToolProfile::Auto {
        return profile.clone();
    }
    ToolProfile::resolve_auto(
        AUTO_TURN_COUNT.load(Ordering::Relaxed),
        AUTO_CTX_TOOLS_USED.load(Ordering::Relaxed),
        AUTO_SYSTEM_PROMPT_TOKENS.load(Ordering::Relaxed),
    )
}

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
    /// Deprecated (GH #1474): was the hook-covered-only surface (just ctx_call).
    /// Never returned by `candidate_set` anymore — kept only for exhaustive
    /// match compatibility until fully removed.
    #[allow(dead_code)]
    ShadowOnly,
}

/// Inputs to [`candidate_set`]. Avoids a long bool parameter list (#clippy::fn_params_excessive_bools).
pub struct CandidateInputs {
    pub full_mode: bool,
    pub unified_env: bool,
    pub explicit_profile: bool,
    pub hook_covered: bool,
}

/// Decides the candidate pool. Single source of truth for the `tools/list`
/// handler AND offline measurement (`doctor overhead`), so the advertised
/// surface and the reported overhead can never drift apart.
#[must_use]
pub fn candidate_set(inp: &CandidateInputs) -> CandidateSet {
    if inp.full_mode {
        CandidateSet::Full
    } else if inp.unified_env {
        CandidateSet::Unified
    } else if inp.explicit_profile {
        CandidateSet::ProfileAuthoritative
    } else {
        // GH #1474: all MCP-connected clients (hook-covered or not) get the
        // lazy-core surface. Shadow-only (ctx_call alone) caused agent confusion
        // because they could not discover ctx_read/ctx_search/etc. by name.
        CandidateSet::LazyCore
    }
}

/// Whether the shadow-only tool surface is enabled (config or env).
#[allow(dead_code)]
fn is_shadow_surface_enabled() -> bool {
    if let Ok(v) = std::env::var("LEAN_CTX_TOOL_SURFACE") {
        return v.eq_ignore_ascii_case("shadow") || v.eq_ignore_ascii_case("auto");
    }
    let cfg = crate::core::config::Config::load();
    // Only "mcp" explicitly disables shadow surface; "auto", "shadow", and
    // unset all enable it (when the caller already confirmed hook_covered).
    !matches!(cfg.tool_surface.as_deref(), Some("mcp"))
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
    // Deprecated aliases and the local-only collaboration substrate are hidden
    // from every advertised surface but remain registered and directly callable.
    if !super::dynamic_tools::is_publicly_advertised_tool(name) {
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

/// The tool set agent-facing guidance may name for `client_name`: the MCP
/// `instructions` and the injected rule files (#1849). It passes the same
/// gates as `tools/list` — the effective profile, `disabled_tools` (which
/// folds in `prefer_native_editor`) and the client's quirks — so the text
/// never points the agent at a tool its session cannot call.
#[must_use]
pub fn guidance_profile(cfg: &crate::core::config::Config, client_name: &str) -> ToolProfile {
    let candidate = candidate_set(&CandidateInputs {
        full_mode: crate::tool_defs::is_full_mode(),
        unified_env: std::env::var("LEAN_CTX_UNIFIED").is_ok(),
        explicit_profile: explicit_profile(cfg),
        hook_covered: false, // no longer affects the candidate set (GH #1474)
    });
    narrow_to_surface(
        lazy_core_surface(cfg.tool_profile_effective(), candidate),
        &cfg.disabled_tools_effective(),
        ClientQuirks::resolve(client_name, candidate),
    )
}

/// The unpinned default resolves to `Power`, but `tools/list` advertises only
/// `CORE_TOOL_NAMES` there; the rest is reached through `ctx_call`. Guidance
/// written for `Power` named tools such as `ctx_knowledge` that OpenCode and
/// other lazy-core clients never saw, and agents called them directly.
fn lazy_core_surface(profile: ToolProfile, candidate: CandidateSet) -> ToolProfile {
    if candidate != CandidateSet::LazyCore {
        return profile;
    }
    ToolProfile::Custom(
        crate::tool_defs::CORE_TOOL_NAMES
            .iter()
            .filter(|name| profile.is_tool_enabled(name))
            .map(|name| (*name).to_string())
            .collect(),
    )
}

/// Pure core of [`guidance_profile`]: `profile` minus every tool the surface
/// withholds. `Auto` is taken at its starting point (minimal) — guidance is
/// written once per session and must not promise a tool before escalation.
#[must_use]
pub fn narrow_to_surface(
    profile: ToolProfile,
    disabled: &[String],
    quirks: ClientQuirks,
) -> ToolProfile {
    let profile = if profile == ToolProfile::Auto {
        ToolProfile::Minimal
    } else {
        profile
    };
    let mut hidden: Vec<&str> = disabled.iter().map(String::as_str).collect();
    if quirks.hide_ctx_edit {
        hidden.push("ctx_edit");
    }
    if quirks.hide_ctx_patch {
        hidden.push("ctx_patch");
    }
    hidden
        .into_iter()
        .fold(profile, |p, name| p.without_tool(name))
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
    let catalog = crate::core::tool_catalog::tool_defs();

    let candidate = candidate_set(&CandidateInputs {
        full_mode,
        unified_env: std::env::var("LEAN_CTX_UNIFIED").is_ok(),
        explicit_profile: explicit_profile(&cfg),
        hook_covered: false, // offline measurement uses worst-case (no hook coverage)
    });
    let pool: Vec<rmcp::model::Tool> = match candidate {
        CandidateSet::Full | CandidateSet::ProfileAuthoritative => catalog.clone(),
        CandidateSet::Unified => crate::tool_defs::unified_tool_defs(),
        // GH #1474: ShadowOnly no longer returned; treat as LazyCore.
        CandidateSet::ShadowOnly | CandidateSet::LazyCore => {
            let core = crate::tool_defs::core_tool_names();
            catalog
                .iter()
                .filter(|t| core.contains(&t.name.as_ref()))
                .cloned()
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
        && let Some(def) = catalog.into_iter().find(|t| t.name.as_ref() == INVOKER)
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
        assert!(is_tool_visible("ctx_compose", &p, &[], no_quirks(), true));
        assert!(is_tool_visible("ctx_callgraph", &p, &[], no_quirks(), true));
        assert!(is_tool_visible("ctx_graph", &p, &[], no_quirks(), true));
        // #1008: anchored editing ships with the pinned Standard profile.
        assert!(is_tool_visible("ctx_patch", &p, &[], no_quirks(), true));
    }

    #[test]
    fn minimal_hides_non_minimal_tools() {
        let p = ToolProfile::Minimal;
        assert!(is_tool_visible("ctx_read", &p, &[], no_quirks(), true));
        assert!(!is_tool_visible("ctx_index", &p, &[], no_quirks(), true));
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

    /// Every tool the guidance names under `profile`, checked against the
    /// same gate `tools/list` applies (#1849).
    fn guidance_names(profile: &ToolProfile) -> String {
        use crate::core::config::CompressionLevel;
        use crate::core::rules_canonical::{Wrapper, render};
        let mut text = String::new();
        for shadow in [true, false] {
            text.push_str(&render(
                shadow,
                Wrapper::Bare,
                CompressionLevel::Off,
                profile,
            ));
        }
        text
    }

    #[test]
    fn disabled_tools_leave_the_guidance() {
        // #1849: `disabled_tools = ["ctx_callgraph"]` hid the tool from
        // `tools/list`, but the instructions still said "ctx_callgraph (callers)".
        let disabled = vec!["ctx_callgraph".to_string()];
        let p = narrow_to_surface(ToolProfile::Power, &disabled, ClientQuirks::default());
        assert!(!p.is_tool_enabled("ctx_callgraph"));
        assert!(
            !is_tool_visible(
                "ctx_callgraph",
                &ToolProfile::Power,
                &disabled,
                ClientQuirks::default(),
                true
            ),
            "precondition: tools/list hides it"
        );
        let text = guidance_names(&p);
        assert!(
            !text.contains("ctx_callgraph"),
            "guidance names a disabled tool:\n{text}"
        );
        // Nothing else is lost on the way.
        assert!(text.contains("ctx_patch"));
        assert!(text.contains("ctx_compose"));
    }

    #[test]
    fn lazy_core_guidance_names_only_core_tools() {
        // The unpinned default is `Power`, yet a lazy-core session (OpenCode,
        // Cursor, …) is offered only CORE_TOOL_NAMES. Its rules named
        // ctx_knowledge, which the agent then called and could not find.
        let core = lazy_core_surface(ToolProfile::Power, CandidateSet::LazyCore);
        let native = ClientQuirks {
            hide_ctx_edit: false,
            hide_ctx_patch: true,
        };
        let surface = narrow_to_surface(core, &[], native);
        let text = guidance_names(&surface);
        for absent in ["ctx_knowledge", "ctx_patch", "ctx_graph", "ctx_overview"] {
            assert!(!text.contains(absent), "names {absent}:\n{text}");
        }
        for present in ["ctx_compose", "ctx_session", "ctx_callgraph", "ctx_call("] {
            assert!(text.contains(present), "misses {present}:\n{text}");
        }
        // A pinned profile is authoritative and keeps its full set.
        assert_eq!(
            lazy_core_surface(ToolProfile::Power, CandidateSet::ProfileAuthoritative),
            ToolProfile::Power
        );
    }

    #[test]
    fn a_hidden_ctx_patch_leaves_the_guidance() {
        // #1849: `prefer_native_editor` folds ctx_patch into the disabled
        // list; the native-editor quirk hides it on the lazy surface. Either
        // way "if denied, use ctx_patch" points at a tool the agent lacks.
        let quirk = ClientQuirks {
            hide_ctx_edit: false,
            hide_ctx_patch: true,
        };
        let by_quirk = narrow_to_surface(ToolProfile::Power, &[], quirk);
        let disabled = vec!["ctx_edit".to_string(), "ctx_patch".to_string()];
        let by_config =
            narrow_to_surface(ToolProfile::Standard, &disabled, ClientQuirks::default());
        for p in [by_quirk, by_config] {
            let text = guidance_names(&p);
            assert!(
                !text.contains("ctx_patch"),
                "{p}: guidance names ctx_patch:\n{text}"
            );
            assert!(
                text.contains("the host's native edit tool"),
                "{p}: edit line lost"
            );
        }
    }

    #[test]
    fn auto_guidance_starts_at_minimal() {
        // Guidance is written once per session; Auto must not promise the
        // tools it only escalates to later.
        let p = narrow_to_surface(ToolProfile::Auto, &[], ClientQuirks::default());
        assert_eq!(p, ToolProfile::Minimal);
        assert!(!guidance_names(&p).contains("ctx_compose"));
    }

    #[test]
    fn an_unrestricted_surface_keeps_the_profile() {
        for p in [
            ToolProfile::Power,
            ToolProfile::Standard,
            ToolProfile::Minimal,
        ] {
            assert_eq!(
                narrow_to_surface(p.clone(), &[], ClientQuirks::default()),
                p
            );
        }
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

    #[test]
    fn resolve_auto_returns_non_auto_unchanged() {
        assert_eq!(
            resolve_auto_profile(&ToolProfile::Power),
            ToolProfile::Power
        );
        assert_eq!(
            resolve_auto_profile(&ToolProfile::Minimal),
            ToolProfile::Minimal
        );
    }

    #[test]
    fn resolve_auto_resolves_to_concrete_profile() {
        let resolved = resolve_auto_profile(&ToolProfile::Auto);
        assert_ne!(resolved, ToolProfile::Auto);
    }

    // ── Hook-covered clients get LazyCore (#1474) ──────────────

    #[test]
    fn hook_covered_gets_lazy_core_not_shadow_only() {
        // GH #1474: hook_covered=true no longer restricts to ShadowOnly
        let c = candidate_set(&CandidateInputs {
            full_mode: false,
            unified_env: false,
            explicit_profile: false,
            hook_covered: true,
        });
        assert_eq!(c, CandidateSet::LazyCore);
    }

    #[test]
    fn shadow_only_overridden_by_full_mode() {
        let c = candidate_set(&CandidateInputs {
            full_mode: true,
            unified_env: false,
            explicit_profile: false,
            hook_covered: true,
        });
        assert_eq!(
            c,
            CandidateSet::Full,
            "LEAN_CTX_FULL_TOOLS=1 must override shadow-only"
        );
    }

    #[test]
    fn shadow_only_overridden_by_explicit_profile() {
        let c = candidate_set(&CandidateInputs {
            full_mode: false,
            unified_env: false,
            explicit_profile: true,
            hook_covered: true,
        });
        assert_eq!(
            c,
            CandidateSet::ProfileAuthoritative,
            "explicit profile must override shadow-only"
        );
    }

    #[test]
    fn lazy_core_when_not_hook_covered() {
        let c = candidate_set(&CandidateInputs {
            full_mode: false,
            unified_env: false,
            explicit_profile: false,
            hook_covered: false,
        });
        assert_eq!(
            c,
            CandidateSet::LazyCore,
            "non-hook client must get LazyCore"
        );
    }
}
