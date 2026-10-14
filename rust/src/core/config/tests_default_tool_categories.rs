use super::*;

// --- Defaults ---

#[test]
fn default_returns_core_only() {
    if std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok() {
        return;
    }
    let cfg = Config::default();
    assert_eq!(cfg.default_tool_categories_effective(), vec!["core"]);
}

#[test]
fn default_struct_field_is_empty_vec() {
    let cfg = Config::default();
    assert!(
        cfg.default_tool_categories.is_empty(),
        "{:?}",
        cfg.default_tool_categories
    );
}

// --- Config field overrides ---

#[test]
fn config_field_overrides_default() {
    if std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok() {
        return;
    }
    let cfg = Config {
        default_tool_categories: vec!["core".to_string(), "arch".to_string(), "memory".to_string()],
        ..Default::default()
    };
    assert_eq!(
        cfg.default_tool_categories_effective(),
        vec!["core", "arch", "memory"]
    );
}

#[test]
fn single_category_in_config() {
    if std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok() {
        return;
    }
    let cfg = Config {
        default_tool_categories: vec!["debug".to_string()],
        ..Default::default()
    };
    assert_eq!(cfg.default_tool_categories_effective(), vec!["debug"]);
}

#[test]
fn all_six_categories_in_config() {
    if std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok() {
        return;
    }
    let cfg = Config {
        default_tool_categories: vec![
            "core".to_string(),
            "arch".to_string(),
            "debug".to_string(),
            "memory".to_string(),
            "metrics".to_string(),
            "session".to_string(),
        ],
        ..Default::default()
    };
    let effective = cfg.default_tool_categories_effective();
    assert_eq!(effective.len(), 6);
    assert!(effective.contains(&"core".to_string()));
    assert!(effective.contains(&"metrics".to_string()));
}

// --- TOML deserialization ---

#[test]
fn deserialization_defaults_to_empty() {
    let cfg: Config = toml::from_str("").unwrap();
    assert!(
        cfg.default_tool_categories.is_empty(),
        "{:?}",
        cfg.default_tool_categories
    );
}

#[test]
fn deserialization_from_toml() {
    let cfg: Config =
        toml::from_str(r#"default_tool_categories = ["core", "arch", "debug"]"#).unwrap();
    assert_eq!(cfg.default_tool_categories, vec!["core", "arch", "debug"]);
}

#[test]
fn deserialization_empty_array() {
    let cfg: Config = toml::from_str(r"default_tool_categories = []").unwrap();
    assert!(
        cfg.default_tool_categories.is_empty(),
        "{:?}",
        cfg.default_tool_categories
    );
}

#[test]
fn deserialization_single_entry() {
    let cfg: Config = toml::from_str(r#"default_tool_categories = ["memory"]"#).unwrap();
    assert_eq!(cfg.default_tool_categories, vec!["memory"]);
}

// --- Edge cases ---

#[test]
fn effective_normalizes_config_to_lowercase() {
    if std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok() {
        return;
    }
    let cfg = Config {
        default_tool_categories: vec!["ARCH".to_string(), "Debug".to_string()],
        ..Default::default()
    };
    let effective = cfg.default_tool_categories_effective();
    assert_eq!(effective, vec!["arch", "debug"]);
}
