use super::*;

#[test]
fn timeout_overrides_default_to_none() {
    let cfg = Config::default();
    assert_eq!(cfg.shell_timeout_secs, None);
    assert_eq!(cfg.shell_heavy_timeout_secs, None);
}

#[test]
fn timeout_overrides_parse_from_toml() {
    let cfg: Config =
        toml::from_str("shell_timeout_secs = 90\nshell_heavy_timeout_secs = 1800").unwrap();
    assert_eq!(cfg.shell_timeout_secs, Some(90));
    assert_eq!(cfg.shell_heavy_timeout_secs, Some(1800));
}

#[test]
fn allow_writes_default_is_false() {
    assert!(!Config::default().shell_allow_writes);
}

#[test]
fn allow_writes_parses_from_toml() {
    let cfg: Config = toml::from_str("shell_allow_writes = true").unwrap();
    assert!(cfg.shell_allow_writes);
    let absent: Config = toml::from_str("").unwrap();
    assert!(!absent.shell_allow_writes);
}

#[test]
fn allow_writes_round_trips_through_toml() {
    let cfg = Config {
        shell_allow_writes: true,
        ..Default::default()
    };
    let serialized = toml::to_string(&cfg).expect("Config must serialize to TOML");
    let restored: Config = toml::from_str(&serialized).expect("Config must round-trip");
    assert!(restored.shell_allow_writes);
}

#[test]
fn allow_writes_effective_env_overrides_config() {
    let _lock = crate::core::data_dir::test_env_lock();
    let saved = std::env::var("LEAN_CTX_SHELL_ALLOW_WRITES").ok();

    // Env truthy wins even when config is false.
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOW_WRITES", "1");
    assert!(Config::default().shell_allow_writes_effective());

    // Env falsey/unknown falls back to the config field.
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOW_WRITES", "nope");
    let cfg_true = Config {
        shell_allow_writes: true,
        ..Default::default()
    };
    assert!(!Config::default().shell_allow_writes_effective());
    // ...but with the env var present-but-falsey, config is ignored (explicit
    // operator intent), so a true config still reads false here.
    assert!(!cfg_true.shell_allow_writes_effective());

    // With no env var, the config field decides.
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOW_WRITES");
    assert!(cfg_true.shell_allow_writes_effective());
    assert!(!Config::default().shell_allow_writes_effective());

    if let Some(v) = saved {
        crate::test_env::set_var("LEAN_CTX_SHELL_ALLOW_WRITES", v);
    }
}

#[test]
fn write_allow_paths_default_to_os_temp() {
    let cfg = Config::default();
    assert!(
        cfg.write_allow_paths.is_empty(),
        "{:?}",
        cfg.write_allow_paths
    );
    let effective = cfg.shell_write_allow_paths_effective();
    assert!(
        effective
            .iter()
            .any(|path| { path == &std::env::temp_dir().to_string_lossy() })
    );
}

#[test]
fn write_allow_paths_parse_from_toml() {
    let cfg: Config =
        toml::from_str("write_allow_paths = [\"/var/agent-scratch\", \"/opt/logs\"]").unwrap();
    assert_eq!(
        cfg.write_allow_paths,
        vec!["/var/agent-scratch", "/opt/logs"]
    );
    assert_eq!(cfg.shell_write_allow_paths_effective().len(), 2);
}

#[test]
fn write_allow_paths_round_trip_through_toml() {
    let cfg = Config {
        write_allow_paths: vec!["/var/agent-scratch".to_string()],
        ..Default::default()
    };
    let serialized = toml::to_string(&cfg).expect("Config must serialize to TOML");
    let restored: Config = toml::from_str(&serialized).expect("Config must round-trip");
    assert_eq!(restored.write_allow_paths, cfg.write_allow_paths);
}
