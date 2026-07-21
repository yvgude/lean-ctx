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
