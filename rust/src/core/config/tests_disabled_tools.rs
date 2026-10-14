use super::*;

#[test]
fn config_field_default_is_empty() {
    let cfg = Config::default();
    assert!(cfg.disabled_tools.is_empty(), "{:?}", cfg.disabled_tools);
}

#[test]
fn effective_returns_config_field_when_no_env_var() {
    // Only meaningful when LEAN_CTX_DISABLED_TOOLS is unset; skip otherwise.
    if std::env::var("LEAN_CTX_DISABLED_TOOLS").is_ok() {
        return;
    }
    let cfg = Config {
        disabled_tools: vec!["ctx_graph".to_string(), "ctx_agent".to_string()],
        ..Default::default()
    };
    assert_eq!(
        cfg.disabled_tools_effective(),
        vec!["ctx_graph", "ctx_agent"]
    );
}

#[test]
fn parse_env_basic() {
    let result = Config::parse_disabled_tools_env("ctx_graph,ctx_agent");
    assert_eq!(result, vec!["ctx_graph", "ctx_agent"]);
}

#[test]
fn parse_env_trims_whitespace_and_skips_empty() {
    let result = Config::parse_disabled_tools_env(" ctx_graph , , ctx_agent ");
    assert_eq!(result, vec!["ctx_graph", "ctx_agent"]);
}

#[test]
fn parse_env_single_entry() {
    let result = Config::parse_disabled_tools_env("ctx_graph");
    assert_eq!(result, vec!["ctx_graph"]);
}

#[test]
fn parse_env_empty_string_returns_empty() {
    let result = Config::parse_disabled_tools_env("");
    assert!(result.is_empty(), "{result:?}");
}

#[test]
fn disabled_tools_deserialization_defaults_to_empty() {
    let cfg: Config = toml::from_str("").unwrap();
    assert!(cfg.disabled_tools.is_empty(), "{:?}", cfg.disabled_tools);
}

#[test]
fn disabled_tools_deserialization_from_toml() {
    let cfg: Config = toml::from_str(r#"disabled_tools = ["ctx_graph", "ctx_agent"]"#).unwrap();
    assert_eq!(cfg.disabled_tools, vec!["ctx_graph", "ctx_agent"]);
}
