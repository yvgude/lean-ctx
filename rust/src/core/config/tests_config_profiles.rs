use super::*;

const PROFILED_CONFIG: &str = r#"
theme = "base"
max_ram_percent = 20
config_profile = "local"

[archive]
max_disk_mb = 100

[profiles.local]
theme = "local"

[profiles.local.archive]
max_disk_mb = 25

[profiles.cloud]
theme = "cloud"
"#;

#[test]
fn configured_profile_overrides_base_recursively() {
    let cfg = super::super::loader::parse_config_with_profile(PROFILED_CONFIG, None).unwrap();

    assert_eq!(cfg.theme, "local");
    assert_eq!(cfg.max_ram_percent, 20);
    assert_eq!(cfg.archive.max_disk_mb, 25);
    assert_eq!(cfg.config_profile.as_deref(), Some("local"));
    assert_eq!(cfg.profiles.len(), 2);
}

#[test]
fn explicit_profile_takes_precedence_over_persisted_selector() {
    let cfg =
        super::super::loader::parse_config_with_profile(PROFILED_CONFIG, Some("cloud")).unwrap();

    assert_eq!(cfg.theme, "cloud");
    assert_eq!(cfg.archive.max_disk_mb, 100);
    assert_eq!(cfg.config_profile.as_deref(), Some("local"));
}

#[test]
fn selected_profile_must_exist() {
    let error = super::super::loader::parse_config_with_profile(PROFILED_CONFIG, Some("missing"))
        .unwrap_err();

    assert_eq!(error, "config profile 'missing' is not defined");
}

#[test]
fn profile_cannot_change_profile_selection() {
    let error = super::super::loader::parse_config_with_profile(
        r#"
config_profile = "local"

[profiles.local]
config_profile = "cloud"
"#,
        None,
    )
    .unwrap_err();

    assert_eq!(
        error,
        "config profile 'local' cannot override reserved profile keys"
    );
}

#[test]
fn project_local_config_applies_its_selected_profile() {
    let mut cfg = Config::default();

    cfg.merge_local(PROFILED_CONFIG, true);

    // `archive.max_disk_mb` is a project key; `theme` is the user's own.
    assert_eq!(cfg.archive.max_disk_mb, 25);
    assert_eq!(cfg.theme, Config::default().theme);
}

/// A global profile selector applies to a project file that defines profiles;
/// a file without profiles is used as written instead of being rejected.
#[test]
fn project_file_without_profiles_ignores_the_global_selector() {
    let mut cfg = Config {
        config_profile: Some("cloud".to_string()),
        ..Config::default()
    };
    cfg.merge_local("checkpoint_interval = 13\n", true);
    assert_eq!(cfg.checkpoint_interval, 13);
}

#[test]
fn project_local_config_inherits_the_global_selector() {
    let mut cfg = Config {
        config_profile: Some("cloud".to_string()),
        ..Config::default()
    };

    cfg.merge_local(
        r"
[profiles.local]
checkpoint_interval = 11

[profiles.cloud]
checkpoint_interval = 22
",
        true,
    );

    assert_eq!(cfg.checkpoint_interval, 22);
}

/// The project file speaks for its repository (2026-10-16): a project key
/// wins wherever the file sets it — also back to the default value; a
/// user-level key (theme) and a loosened one-way switch are not applied and
/// are reported; retired keys are reported elsewhere.
#[test]
fn project_keys_win_and_global_keys_are_reported() {
    let mut cfg = Config {
        checkpoint_interval: 40,
        theme: "user-theme".to_string(),
        ..Config::default()
    };
    cfg.autonomy.enabled = false;
    let default_interval = Config::default().checkpoint_interval;
    let mut ignored = cfg.merge_local_fields(
        &format!(
            "checkpoint_interval = {default_interval}\nturn_fresh_limit = 7\n\
             session_token_limit = 9\ntheme = \"repo-theme\"\n\
             [autonomy]\nenabled = true\n\
             [memory.knowledge]\nmax_history = 77\n"
        ),
        true,
    );
    assert_eq!(cfg.checkpoint_interval, default_interval);
    assert_eq!(cfg.turn_fresh_limit, 7);
    assert_eq!(cfg.memory.knowledge.max_history, 77);
    assert_eq!(cfg.theme, "user-theme");
    assert!(!cfg.autonomy.enabled);
    ignored.sort();
    assert_eq!(ignored, ["autonomy.enabled", "theme"]);
}

/// An omitted key never resets the global value (`autonomy.silent_preload`
/// used to be copied unconditionally).
#[test]
fn an_omitted_project_key_keeps_the_global_value() {
    let mut cfg = Config {
        turn_fresh_limit: 7,
        ..Config::default()
    };
    cfg.merge_local_fields("checkpoint_interval = 3\n", true);
    assert_eq!(cfg.turn_fresh_limit, 7);
}

/// Keys that widen what lean-ctx may write or read apply only in a trusted
/// workspace.
#[test]
fn untrusted_projects_cannot_widen_write_or_read_scope() {
    let file = "write_allow_paths = [\"/tmp/out\"]\nread_only_roots = [\"/srv\"]\nread_redirect = \"off\"\n";
    let mut untrusted = Config::default();
    untrusted.merge_local_fields(file, false);
    assert!(untrusted.write_allow_paths.is_empty());
    assert!(untrusted.read_only_roots.is_empty());
    assert_eq!(untrusted.read_redirect, Config::default().read_redirect);
    let mut trusted = Config::default();
    trusted.merge_local_fields(file, true);
    assert_eq!(trusted.write_allow_paths, ["/tmp/out"]);
    assert_eq!(trusted.read_only_roots, ["/srv"]);
}

/// Every project key names a real config key (no silent drift on renames).
#[test]
fn project_keys_name_real_config_keys() {
    let known = super::schema::ConfigSchema::generate().known_keys();
    for key in super::merge::PROJECT_KEYS {
        assert!(
            known.iter().any(|k| k == key
                || k.starts_with(&format!("{key}."))
                || key.starts_with(&format!("{k}."))),
            "{key} is not a config key"
        );
    }
}
