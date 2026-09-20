//! Contract tests: verify that canonical rules rendering is consistent.
//!
//! Tests use `render(false, ..., CompressionLevel::Off, &ToolProfile::Power)` directly
//! to bypass config-driven shadow mode, ensuring the non-shadow baseline is always tested.

use lean_ctx::core::config::CompressionLevel;
use lean_ctx::core::rules_canonical;
use lean_ctx::core::tool_profiles::ToolProfile;

fn tp() -> ToolProfile {
    ToolProfile::Power
}

#[test]
fn shared_non_shadow_contains_never() {
    let content = rules_canonical::render(
        false,
        rules_canonical::Wrapper::Shared,
        CompressionLevel::Off,
        &tp(),
    );
    assert!(
        content.contains("NEVER"),
        "shared non-shadow must contain NEVER"
    );
}

#[test]
fn dedicated_non_shadow_contains_never() {
    let content = rules_canonical::render(
        false,
        rules_canonical::Wrapper::Dedicated,
        CompressionLevel::Off,
        &tp(),
    );
    assert!(
        content.contains("NEVER"),
        "dedicated non-shadow must contain NEVER"
    );
}

#[test]
fn dedicated_non_shadow_contains_intent_and_anti() {
    let content = rules_canonical::render(
        false,
        rules_canonical::Wrapper::Dedicated,
        CompressionLevel::Off,
        &tp(),
    );
    assert!(
        content.contains("Anti-patterns"),
        "dedicated must have anti-patterns"
    );
    assert!(
        content.contains("ctx_compose"),
        "dedicated must mention ctx_compose"
    );
}

#[test]
fn shared_non_shadow_contains_mapping() {
    let content = rules_canonical::render(
        false,
        rules_canonical::Wrapper::Shared,
        CompressionLevel::Off,
        &tp(),
    );
    assert!(
        content.contains("MANDATORY MAPPING"),
        "shared must have mapping"
    );
}

#[test]
fn dedicated_has_markers() {
    let content = rules_canonical::render(
        false,
        rules_canonical::Wrapper::Dedicated,
        CompressionLevel::Off,
        &tp(),
    );
    assert!(content.contains(rules_canonical::START_MARK));
    assert!(content.contains(rules_canonical::END_MARK));
    assert!(content.contains("CRITICAL"));
}

#[test]
fn bare_has_no_markers() {
    let content = rules_canonical::render(
        false,
        rules_canonical::Wrapper::Bare,
        CompressionLevel::Off,
        &tp(),
    );
    assert!(!content.contains(rules_canonical::START_MARK));
    assert!(!content.contains(rules_canonical::END_MARK));
}

/// #1788: the steering has to say what it governs. Unqualified, "ALWAYS use
/// ctx_* — NOT optional" reads as a ranking over every tool the host exposes,
/// so a model with an IDE/LSP, database or issue-tracker MCP server attached
/// routed those questions here too. The rule is about the host's *built-in*
/// file/search/shell tools; every profile that carries the rule must carry the
/// boundary with it, or the same misreading returns through whichever profile
/// forgot it.
#[test]
fn every_steering_profile_states_what_it_governs() {
    for wrapper in [
        rules_canonical::Wrapper::Dedicated,
        rules_canonical::Wrapper::Shared,
        rules_canonical::Wrapper::Bare,
    ] {
        let content = rules_canonical::render(false, wrapper, CompressionLevel::Off, &tp());
        // Only profiles that actually steer need the boundary.
        if !content.contains("NEVER use") && !content.contains("CRITICAL:") {
            continue;
        }
        assert!(
            content.contains("built-in"),
            "{wrapper:?} steers but never says the rule is about built-in tools:\n{content}"
        );
        assert!(
            content.contains("Other MCP servers keep their own jobs."),
            "{wrapper:?} steers but never exempts other MCP servers:\n{content}"
        );
    }
}

/// The boundary must not be phrased as a licence to skip ctx_* for ordinary
/// reads and searches — that is the behaviour the layer exists for.
#[test]
fn the_boundary_does_not_weaken_the_built_in_tool_rule() {
    let content = rules_canonical::render(
        false,
        rules_canonical::Wrapper::Dedicated,
        CompressionLevel::Off,
        &tp(),
    );
    assert!(
        content.contains("NEVER use built-in Read/Grep/Shell/Glob"),
        "the built-in mapping must stay absolute:\n{content}"
    );
    assert!(
        content.contains("MANDATORY MAPPING"),
        "the mapping itself must survive the rewording:\n{content}"
    );
}

#[test]
fn all_wrappers_use_current_version() {
    let version = format!("version: {}", rules_canonical::RULES_VERSION);
    for wrapper in [
        rules_canonical::Wrapper::Dedicated,
        rules_canonical::Wrapper::Shared,
    ] {
        let content = rules_canonical::render(false, wrapper, CompressionLevel::Off, &tp());
        assert!(
            content.contains(&version),
            "{wrapper:?} must use current version"
        );
    }
    let bare = rules_canonical::render(
        false,
        rules_canonical::Wrapper::Bare,
        CompressionLevel::Off,
        &tp(),
    );
    assert!(
        !bare.contains("version:"),
        "bare must not have version comment"
    );
}
