use super::*;

fn env_clean() -> bool {
    std::env::var("LEAN_CTX_PREFER_NATIVE_EDITOR").is_err()
        && std::env::var("LEAN_CTX_DISABLED_TOOLS").is_err()
}

#[test]
fn default_is_off_and_blocks_nothing() {
    if !env_clean() {
        return;
    }
    let cfg = Config::default();
    assert!(!cfg.prefer_native_editor);
    assert!(!cfg.prefer_native_editor_effective());
    assert!(!cfg.edit_tool_blocked("ctx_edit"));
    {
        let found = &cfg.disabled_tools_effective();
        assert!(found.is_empty(), "{found:?}");
    }
}

#[test]
fn enabled_blocks_only_edit_tools() {
    if !env_clean() {
        return;
    }
    let cfg = Config {
        prefer_native_editor: true,
        ..Default::default()
    };
    // #454: the dedicated edit tools are blocked; reads/search stay available.
    assert!(cfg.edit_tool_blocked("ctx_edit"));
    // #1008: the anchored editor honors the same preference.
    assert!(cfg.edit_tool_blocked("ctx_patch"));
    assert!(!cfg.edit_tool_blocked("ctx_read"));
    assert!(!cfg.edit_tool_blocked("ctx_search"));
    assert!(!cfg.edit_tool_blocked("ctx_refactor"));
}

#[test]
fn enabled_hides_edit_tools_from_list() {
    if !env_clean() {
        return;
    }
    let cfg = Config {
        prefer_native_editor: true,
        ..Default::default()
    };
    let disabled = cfg.disabled_tools_effective();
    for name in crate::core::config::EDIT_TOOL_NAMES {
        assert!(
            disabled.iter().any(|t| t == name),
            "{name} must be folded into the effective disabled set"
        );
    }
}

#[test]
fn merges_existing_disabled_without_duplication() {
    if !env_clean() {
        return;
    }
    let cfg = Config {
        prefer_native_editor: true,
        disabled_tools: vec!["ctx_graph".to_string(), "ctx_edit".to_string()],
        ..Default::default()
    };
    let eff = cfg.disabled_tools_effective();
    assert_eq!(
        eff.iter().filter(|t| *t == "ctx_edit").count(),
        1,
        "ctx_edit must not be duplicated when already disabled"
    );
    assert!(eff.iter().any(|t| t == "ctx_graph"));
}
