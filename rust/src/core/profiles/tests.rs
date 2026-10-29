use super::builtins::{builtin_exploration, builtin_profiles};
use super::*;
use crate::core::protocol::CrpMode;

#[test]
fn builtin_profiles_count() {
    let builtins = builtin_profiles();
    assert_eq!(builtins.len(), 7);
    assert!(builtins.contains_key("coder"));
    assert!(builtins.contains_key("exploration"));
    assert!(builtins.contains_key("bugfix"));
    assert!(builtins.contains_key("hotfix"));
    assert!(builtins.contains_key("ci-debug"));
    assert!(builtins.contains_key("review"));
    assert!(builtins.contains_key("passthrough"));
}

/// Profile files written before the 2026-10-16 cleanup carry sections and
/// keys that never had an effect. They must keep loading, and a profile that
/// is written back no longer suggests them (G07: no model tier either).
#[test]
fn legacy_profile_with_retired_keys_loads_and_is_written_back_without_them() {
    let legacy = r#"
[profile]
name = "legacy"
inherits = "exploration"

[read]
default_mode = "signatures"
max_tokens_per_file = 2000
prefer_cache = true

[compression]
crp_mode = "tdd"
output_density = "ultra"
terse_mode = true
adaptive = false
entropy_threshold = 0.4

[translation]
enabled = true
ruleset = "auto"

[layout]
enabled = true

[budget]
max_context_tokens = 30000

[constraints]
quality_floor = 0.9

[capabilities.code_context]
provider = "leanctx"

[pipeline]
intent = false

[routing]
max_model_tier = "fast"
degrade_under_pressure = false

[autonomy]
auto_response = true
response_min_tokens = 100
checkpoint_interval = 7
"#;
    let p: Profile = toml::from_str(legacy).expect("legacy profile still loads");
    assert_eq!(p.read.default_mode_effective(), "signatures");
    assert_eq!(p.compression.crp_mode.as_deref(), Some("tdd"));
    assert!(!p.routing.degrade_under_pressure_effective());
    assert_eq!(p.autonomy.checkpoint_interval_effective(), 7);

    let written = format_as_toml(&p);
    for retired in [
        "max_tokens_per_file",
        "prefer_cache",
        "output_density",
        "terse_mode",
        "adaptive",
        "entropy_threshold",
        "[translation]",
        "[layout]",
        "[budget]",
        "[constraints]",
        "[capabilities",
        "[pipeline]",
        "max_model_tier",
        "auto_response",
        "response_min_tokens",
    ] {
        assert!(!written.contains(retired), "{retired} written back");
    }
    for (name, profile) in builtin_profiles() {
        assert!(
            !format_as_toml(&profile).contains("max_model_tier"),
            "{name}"
        );
    }
}

#[test]
fn hotfix_reads_signatures_with_tdd_output() {
    let p = builtin_profiles().remove("hotfix").unwrap();
    assert_eq!(p.read.default_mode_effective(), "signatures");
    assert_eq!(p.compression.crp_mode.as_deref(), Some("tdd"));
    assert_eq!(p.autonomy.checkpoint_interval_effective(), 5);
}

#[test]
fn profile_roundtrip_toml() {
    let original = builtin_exploration();
    let toml_str = format_as_toml(&original);
    let parsed: Profile = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.profile.name, "exploration");
    assert_eq!(parsed.read.default_mode_effective(), "map");
    assert!(parsed.output_hints.related_hint());
}

#[test]
fn legacy_degradation_section_still_parses() {
    let parsed: Profile = toml::from_str(
        "[profile]\nname = \"legacy\"\n\n[degradation]\nenforce = true\nthrottle_ms = 500\n",
    )
    .expect("retired [degradation] section must be ignored");
    assert_eq!(parsed.profile.name, "legacy");
}

#[test]
fn engine_config_projects_consumed_mechanisms_without_changing_legacy_toml() {
    let profile = builtin_exploration();
    let before = format_as_toml(&profile);
    let engine = profile.engine_config();

    assert_eq!(engine.read.default_mode, "map");
    assert_eq!(
        engine.compression.crp_mode,
        CrpMode::resolve(builtin_exploration().compression.crp_mode.as_deref()).as_str()
    );
    assert_eq!(format_as_toml(&profile), before);
}

#[test]
fn engine_config_excludes_product_policy() {
    let mut profile = builtin_exploration();
    profile.autonomy.enabled = Some(false);

    let engine = profile.engine_config();
    assert_eq!(engine.read.default_mode, "map");
    assert_eq!(
        engine.compression.crp_mode,
        CrpMode::resolve(builtin_exploration().compression.crp_mode.as_deref()).as_str()
    );
}

#[test]
fn merge_child_overrides_parent_and_inherits_the_rest() {
    let parent = builtin_exploration();
    let child: Profile = toml::from_str(
        r#"
[profile]
name = "custom"
inherits = "exploration"

[read]
default_mode = "signatures"

[output_hints]
related_hint = false
"#,
    )
    .unwrap();

    let merged = merge_profiles(parent, child);
    assert_eq!(merged.read.default_mode_effective(), "signatures");
    assert!(!merged.output_hints.related_hint());
    assert!(
        merged.output_hints.cross_source_hint(),
        "should inherit unset hints from parent"
    );
    assert_eq!(
        merged.profile.description,
        "Broad context for understanding codebases"
    );
}

#[test]
fn load_builtin_by_name() {
    let p = load_profile("hotfix").unwrap();
    assert_eq!(p.profile.name, "hotfix");
    assert_eq!(p.read.default_mode_effective(), "signatures");
}

#[test]
fn load_nonexistent_returns_none() {
    assert!(load_profile("does-not-exist-xyz").is_none());
}

#[test]
fn list_profiles_includes_builtins() {
    let list = list_profiles();
    assert!(list.len() >= 5);
    let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"exploration"));
    assert!(names.contains(&"hotfix"));
    assert!(names.contains(&"review"));
}

#[test]
fn active_profile_defaults_to_coder() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROFILE");
    let p = active_profile();
    assert_eq!(p.profile.name, "coder");
}

#[test]
fn active_profile_from_env() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_PROFILE", "hotfix");
    let name = active_profile_name();
    assert_eq!(name, "hotfix");
    crate::test_env::remove_var("LEAN_CTX_PROFILE");
}

#[test]
fn profile_source_display() {
    assert_eq!(ProfileSource::Builtin.to_string(), "built-in");
    assert_eq!(ProfileSource::Global.to_string(), "global");
    assert_eq!(ProfileSource::Project.to_string(), "project");
}

#[test]
fn partial_toml_leaves_unset_as_none() {
    let toml_str = r#"
[profile]
name = "sparse"

[read]
default_mode = "map"
"#;
    let p: Profile = toml::from_str(toml_str).unwrap();
    assert_eq!(p.read.default_mode, Some("map".to_string()));
    assert_eq!(p.compression.crp_mode, None);
    assert_eq!(p.autonomy.checkpoint_interval, None);
}
