use super::builtins::{builtin_exploration, builtin_profiles};
use super::*;

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

#[test]
fn hotfix_has_minimal_budget() {
    let p = builtin_profiles().remove("hotfix").unwrap();
    assert_eq!(p.budget.max_context_tokens_effective(), 30_000);
    assert_eq!(p.budget.max_shell_invocations_effective(), 20);
    assert_eq!(p.read.default_mode_effective(), "signatures");
    assert_eq!(p.compression.output_density_effective(), "ultra");
}

#[test]
fn exploration_has_broad_context() {
    let p = builtin_profiles().remove("exploration").unwrap();
    assert_eq!(p.budget.max_context_tokens_effective(), 200_000);
    assert_eq!(p.read.default_mode_effective(), "map");
    assert!(p.read.prefer_cache_effective());
}

#[test]
fn profile_roundtrip_toml() {
    let original = builtin_exploration();
    let toml_str = format_as_toml(&original);
    let parsed: Profile = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.profile.name, "exploration");
    assert_eq!(parsed.read.default_mode_effective(), "map");
    assert_eq!(parsed.budget.max_context_tokens_effective(), 200_000);
}

#[test]
fn engine_config_projects_consumed_mechanisms_without_changing_legacy_toml() {
    let profile = builtin_exploration();
    let before = format_as_toml(&profile);
    let engine = profile.engine_config();

    assert_eq!(engine.read.default_mode, "map");
    assert_eq!(engine.compression.crp_mode, "tdd");
    assert_eq!(format_as_toml(&profile), before);
}

#[test]
fn engine_config_excludes_product_policy() {
    let mut profile = builtin_exploration();
    profile.read.prefer_cache = Some(false);
    profile.routing.max_model_tier = Some("fast".into());
    profile.budget.max_context_tokens = Some(1);
    profile.constraints.quality_floor = Some(0.1);
    profile.autonomy.auto_prefetch = Some(true);
    profile.degradation.enforce = Some(true);

    let engine = profile.engine_config();
    assert_eq!(engine.read.default_mode, "map");
    assert_eq!(engine.compression.crp_mode, "tdd");
}

#[test]
fn merge_child_overrides_parent() {
    let parent = builtin_exploration();
    let child = Profile {
        profile: ProfileMeta {
            name: "custom".to_string(),
            inherits: Some("exploration".to_string()),
            description: String::new(),
        },
        read: ReadConfig {
            default_mode: Some("signatures".to_string()),
            ..ReadConfig::default()
        },
        compression: CompressionConfig::default(),
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(10_000),
            ..BudgetConfig::default()
        },
        constraints: ConstraintsConfig::default(),
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig::default(),
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints::default(),
    };

    let merged = merge_profiles(parent, child);
    assert_eq!(merged.read.default_mode_effective(), "signatures");
    assert_eq!(merged.budget.max_context_tokens_effective(), 10_000);
    assert_eq!(
        merged.profile.description,
        "Broad context for understanding codebases"
    );
}

#[test]
fn merge_partial_child_inherits_parent_fields() {
    let parent = builtin_exploration();
    let child = Profile {
        profile: ProfileMeta {
            name: "partial".to_string(),
            inherits: Some("exploration".to_string()),
            description: String::new(),
        },
        read: ReadConfig {
            default_mode: Some("map".to_string()),
            ..ReadConfig::default()
        },
        compression: CompressionConfig::default(),
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig::default(),
        constraints: ConstraintsConfig::default(),
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig::default(),
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints::default(),
    };

    let merged = merge_profiles(parent, child);
    assert_eq!(merged.read.default_mode_effective(), "map");
    assert_eq!(
        merged.read.max_tokens_per_file_effective(),
        80_000,
        "should inherit max_tokens_per_file from parent"
    );
    assert!(
        merged.read.prefer_cache_effective(),
        "should inherit prefer_cache from parent"
    );
    assert_eq!(
        merged.budget.max_context_tokens_effective(),
        200_000,
        "should inherit budget from parent"
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
fn default_profile_has_sane_values() {
    let p = Profile {
        profile: ProfileMeta::default(),
        read: ReadConfig::default(),
        compression: CompressionConfig::default(),
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig::default(),
        constraints: ConstraintsConfig::default(),
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig::default(),
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints::default(),
    };
    assert_eq!(p.read.default_mode_effective(), "auto");
    assert_eq!(p.compression.crp_mode_effective(), "tdd");
    assert_eq!(p.budget.max_context_tokens_effective(), 200_000);
    assert!(p.pipeline.compression_effective());
    assert!(p.pipeline.intent_effective());
}

#[test]
fn pipeline_layers_configurable() {
    let toml_str = r#"
[profile]
name = "no-intent"

[pipeline]
intent = false
relevance = false
"#;
    let p: Profile = toml::from_str(toml_str).unwrap();
    assert!(!p.pipeline.intent_effective());
    assert!(!p.pipeline.relevance_effective());
    assert!(p.pipeline.compression_effective());
    assert!(p.pipeline.translation_effective());
}

#[test]
fn partial_toml_fills_defaults() {
    let toml_str = r#"
[profile]
name = "minimal"

[read]
default_mode = "entropy"
"#;
    let p: Profile = toml::from_str(toml_str).unwrap();
    assert_eq!(p.read.default_mode_effective(), "entropy");
    assert_eq!(p.read.max_tokens_per_file_effective(), 50_000);
    assert_eq!(p.budget.max_context_tokens_effective(), 200_000);
    assert_eq!(p.compression.crp_mode_effective(), "tdd");
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
    assert_eq!(p.read.max_tokens_per_file, None);
    assert_eq!(p.read.prefer_cache, None);
    assert_eq!(p.budget.max_context_tokens, None);
    assert_eq!(p.compression.crp_mode, None);
}

#[test]
fn constraints_merge_field_by_field() {
    let mut parent = builtin_exploration();
    parent.constraints = ConstraintsConfig {
        quality_floor: Some(0.99),
        max_cost_usd: Some(2.50),
        max_latency_ms: Some(500),
        max_context_tokens: Some(80_000),
        require_verification: Some(true),
    };
    let mut child = builtin_exploration();
    child.constraints = ConstraintsConfig {
        quality_floor: Some(0.97),
        max_cost_usd: None,
        max_latency_ms: Some(250),
        max_context_tokens: None,
        require_verification: None,
    };

    let merged = merge_profiles(parent, child);

    assert_eq!(merged.constraints.quality_floor, Some(0.97));
    assert_eq!(merged.constraints.max_cost_usd, Some(2.50));
    assert_eq!(merged.constraints.max_latency_ms, Some(250));
    assert_eq!(merged.constraints.max_context_tokens, Some(80_000));
    assert_eq!(merged.constraints.require_verification, Some(true));
}

#[test]
fn capabilities_merge_field_by_field() {
    let mut parent = builtin_exploration();
    parent.capabilities = CapabilitiesConfig {
        code_context: Some(CapabilityBinding {
            provider: Some("leanctx".to_string()),
            strategy: Some("structural".to_string()),
            version: Some("1.0".to_string()),
        }),
        knowledge: Some(CapabilityBinding {
            provider: Some("leanctx".to_string()),
            strategy: Some("semantic".to_string()),
            version: None,
        }),
        ..CapabilitiesConfig::default()
    };
    let mut child = builtin_exploration();
    child.capabilities = CapabilitiesConfig {
        code_context: Some(CapabilityBinding {
            provider: None,
            strategy: Some("adaptive".to_string()),
            version: Some("2.0".to_string()),
        }),
        routing: Some(CapabilityBinding {
            provider: Some("custom".to_string()),
            strategy: None,
            version: None,
        }),
        ..CapabilitiesConfig::default()
    };

    let merged = merge_profiles(parent, child);
    let code_context = merged.capabilities.code_context.unwrap();

    assert_eq!(code_context.provider.as_deref(), Some("leanctx"));
    assert_eq!(code_context.strategy.as_deref(), Some("adaptive"));
    assert_eq!(code_context.version.as_deref(), Some("2.0"));
    assert_eq!(
        merged
            .capabilities
            .knowledge
            .as_ref()
            .and_then(|binding| binding.strategy.as_deref()),
        Some("semantic")
    );
    assert_eq!(
        merged
            .capabilities
            .routing
            .as_ref()
            .and_then(|binding| binding.provider.as_deref()),
        Some("custom")
    );
}

#[test]
fn constraints_effective_defaults_are_unbounded() {
    let constraints = ConstraintsConfig::default();

    assert_eq!(constraints.quality_floor_effective(), 0.95);
    assert_eq!(constraints.max_cost_usd_effective(), f64::MAX);
    assert_eq!(constraints.max_latency_ms_effective(), u64::MAX);
    assert_eq!(constraints.max_context_tokens_effective(), usize::MAX);
    assert!(!constraints.require_verification_effective());
}

#[test]
fn builtin_constraints_match_profile_purpose() {
    let builtins = builtin_profiles();

    assert_eq!(
        builtins["coder"].constraints.quality_floor_effective(),
        0.95
    );
    assert_eq!(
        builtins["exploration"]
            .constraints
            .quality_floor_effective(),
        0.90
    );
    assert_eq!(
        builtins["review"].constraints.quality_floor_effective(),
        0.98
    );
    assert_eq!(builtins["coder"].constraints.max_cost_usd, None);
}
