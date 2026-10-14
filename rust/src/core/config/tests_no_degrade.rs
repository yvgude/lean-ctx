use super::*;

// --- Defaults ---

#[test]
fn default_is_false() {
    let cfg = Config::default();
    assert!(!cfg.no_degrade);
}

#[test]
fn effective_false_when_unset() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let cfg = Config::default();
    assert!(!cfg.no_degrade_effective());
}

// --- Config field ---

#[test]
fn config_field_true_respected_when_no_env() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let cfg = Config {
        no_degrade: true,
        ..Default::default()
    };
    assert!(cfg.no_degrade_effective());
}

#[test]
fn config_field_false_respected_when_no_env() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let cfg = Config {
        no_degrade: false,
        ..Default::default()
    };
    assert!(!cfg.no_degrade_effective());
}

// --- TOML deserialization ---

#[test]
fn deserialization_true() {
    let cfg: Config = toml::from_str("no_degrade = true").unwrap();
    assert!(cfg.no_degrade);
}

#[test]
fn deserialization_false() {
    let cfg: Config = toml::from_str("no_degrade = false").unwrap();
    assert!(!cfg.no_degrade);
}

#[test]
fn deserialization_absent_defaults_false() {
    let cfg: Config = toml::from_str("").unwrap();
    assert!(!cfg.no_degrade);
}

// --- Coexistence with other config fields ---

#[test]
fn no_degrade_independent_of_disabled_tools() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let cfg = Config {
        no_degrade: true,
        disabled_tools: vec!["ctx_graph".to_string()],
        ..Default::default()
    };
    assert!(cfg.no_degrade_effective());
    assert!(
        !cfg.disabled_tools.is_empty(),
        "`cfg.disabled_tools` is empty"
    );
}

#[test]
fn no_degrade_independent_of_tool_categories() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() || std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok()
    {
        return;
    }
    let cfg = Config {
        no_degrade: true,
        default_tool_categories: vec!["core".to_string(), "arch".to_string()],
        ..Default::default()
    };
    assert!(cfg.no_degrade_effective());
    assert_eq!(
        cfg.default_tool_categories_effective(),
        vec!["core", "arch"]
    );
}
