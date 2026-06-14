//! Unit tests for [`Config`] parsing, defaults, and section behaviour.
//!
//! Extracted verbatim from `config/mod.rs` to keep that module focused.
//! These modules live at `config::tests::*`, so `super::super::*` resolves
//! to the `config` module (identical to the original `super::*`).

#[cfg(test)]
mod disabled_tools_tests {
    use super::super::*;

    #[test]
    fn config_field_default_is_empty() {
        let cfg = Config::default();
        assert!(cfg.disabled_tools.is_empty());
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
        assert!(result.is_empty());
    }

    #[test]
    fn disabled_tools_deserialization_defaults_to_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.disabled_tools.is_empty());
    }

    #[test]
    fn disabled_tools_deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"disabled_tools = ["ctx_graph", "ctx_agent"]"#).unwrap();
        assert_eq!(cfg.disabled_tools, vec!["ctx_graph", "ctx_agent"]);
    }
}

#[cfg(test)]
mod default_tool_categories_tests {
    use super::super::*;

    // --- Defaults ---

    #[test]
    fn default_returns_core_and_session() {
        if std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok() {
            return;
        }
        let cfg = Config::default();
        assert_eq!(
            cfg.default_tool_categories_effective(),
            vec!["core", "session"]
        );
    }

    #[test]
    fn default_struct_field_is_empty_vec() {
        let cfg = Config::default();
        assert!(cfg.default_tool_categories.is_empty());
    }

    // --- Config field overrides ---

    #[test]
    fn config_field_overrides_default() {
        if std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok() {
            return;
        }
        let cfg = Config {
            default_tool_categories: vec![
                "core".to_string(),
                "arch".to_string(),
                "memory".to_string(),
            ],
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
        assert!(cfg.default_tool_categories.is_empty());
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
        assert!(cfg.default_tool_categories.is_empty());
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
}

#[cfg(test)]
mod no_degrade_tests {
    use super::super::*;

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
        assert!(!cfg.disabled_tools.is_empty());
    }

    #[test]
    fn no_degrade_independent_of_tool_categories() {
        if std::env::var("LCTX_NO_DEGRADE").is_ok()
            || std::env::var("LCTX_DEFAULT_CATEGORIES").is_ok()
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
}

#[cfg(test)]
mod rules_scope_tests {
    use super::super::*;

    #[test]
    fn default_is_both() {
        let cfg = Config::default();
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn config_global() {
        let cfg = Config {
            rules_scope: Some("global".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Global);
    }

    #[test]
    fn config_project() {
        let cfg = Config {
            rules_scope: Some("project".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Project);
    }

    #[test]
    fn unknown_value_falls_back_to_both() {
        let cfg = Config {
            rules_scope: Some("nonsense".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn deserialization_none_by_default() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.rules_scope.is_none());
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"rules_scope = "project""#).unwrap();
        assert_eq!(cfg.rules_scope.as_deref(), Some("project"));
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Project);
    }
}

#[cfg(test)]
mod rules_injection_tests {
    use super::super::*;

    #[test]
    fn default_is_shared() {
        let cfg = Config::default();
        assert_eq!(cfg.rules_injection_effective(), RulesInjection::Shared);
    }

    #[test]
    fn config_dedicated() {
        let cfg = Config {
            rules_injection: Some("dedicated".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_injection_effective(), RulesInjection::Dedicated);
    }

    #[test]
    fn config_off() {
        for raw in ["off", "none", "disabled"] {
            let cfg = Config {
                rules_injection: Some(raw.to_string()),
                ..Default::default()
            };
            assert_eq!(
                cfg.rules_injection_effective(),
                RulesInjection::Off,
                "{raw:?} should resolve to Off"
            );
        }
    }

    #[test]
    fn off_disables_dedicated_session_context() {
        let cfg = Config {
            rules_injection: Some("off".to_string()),
            ..Default::default()
        };
        assert!(!cfg.dedicated_session_context_active());
    }

    #[test]
    fn unknown_value_falls_back_to_shared() {
        let cfg = Config {
            rules_injection: Some("nonsense".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_injection_effective(), RulesInjection::Shared);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"rules_injection = "dedicated""#).unwrap();
        assert_eq!(cfg.rules_injection.as_deref(), Some("dedicated"));
        assert_eq!(cfg.rules_injection_effective(), RulesInjection::Dedicated);
    }

    #[test]
    fn dedicated_session_context_gated_by_scope() {
        // Dedicated + non-project scope → SessionStart summary active.
        let cfg = Config {
            rules_injection: Some("dedicated".to_string()),
            ..Default::default()
        };
        assert!(cfg.dedicated_session_context_active());

        // Dedicated + project scope → global summary suppressed (project files only).
        let cfg = Config {
            rules_injection: Some("dedicated".to_string()),
            rules_scope: Some("project".to_string()),
            ..Default::default()
        };
        assert!(!cfg.dedicated_session_context_active());

        // Shared (default) → never the SessionStart summary path.
        let cfg = Config::default();
        assert!(!cfg.dedicated_session_context_active());
    }

    #[test]
    fn local_override_merges() {
        let mut base = Config::default();
        base.merge_local(r#"rules_injection = "dedicated""#);
        assert_eq!(base.rules_injection_effective(), RulesInjection::Dedicated);
    }
}

#[cfg(test)]
mod permission_inheritance_tests {
    use super::super::*;

    #[test]
    fn default_is_off() {
        // Guard against a stray env var leaking into the test process.
        if std::env::var("LEAN_CTX_PERMISSION_INHERITANCE").is_ok() {
            return;
        }
        let cfg = Config::default();
        assert_eq!(
            cfg.permission_inheritance_effective(),
            PermissionInheritance::Off
        );
    }

    #[test]
    fn config_on() {
        if std::env::var("LEAN_CTX_PERMISSION_INHERITANCE").is_ok() {
            return;
        }
        let cfg = Config {
            permission_inheritance: Some("on".to_string()),
            ..Default::default()
        };
        assert_eq!(
            cfg.permission_inheritance_effective(),
            PermissionInheritance::On
        );
    }

    #[test]
    fn unknown_value_falls_back_to_off() {
        if std::env::var("LEAN_CTX_PERMISSION_INHERITANCE").is_ok() {
            return;
        }
        let cfg = Config {
            permission_inheritance: Some("nonsense".to_string()),
            ..Default::default()
        };
        assert_eq!(
            cfg.permission_inheritance_effective(),
            PermissionInheritance::Off
        );
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"permission_inheritance = "on""#).unwrap();
        assert_eq!(cfg.permission_inheritance.as_deref(), Some("on"));
    }

    #[test]
    fn local_override_merges() {
        if std::env::var("LEAN_CTX_PERMISSION_INHERITANCE").is_ok() {
            return;
        }
        let mut base = Config::default();
        base.merge_local(r#"permission_inheritance = "on""#);
        assert_eq!(
            base.permission_inheritance_effective(),
            PermissionInheritance::On
        );
    }
}

#[cfg(test)]
mod loop_detection_config_tests {
    use super::super::*;

    #[test]
    fn defaults_are_reasonable() {
        let cfg = LoopDetectionConfig::default();
        assert_eq!(cfg.normal_threshold, 2);
        assert_eq!(cfg.reduced_threshold, 4);
        // 0 = blocking disabled by default (LeanCTX philosophy: always help, never block)
        assert_eq!(cfg.blocked_threshold, 0);
        assert_eq!(cfg.window_secs, 300);
        assert_eq!(cfg.search_group_limit, 10);
    }

    #[test]
    fn deserialization_defaults_when_missing() {
        let cfg: Config = toml::from_str("").unwrap();
        // 0 = blocking disabled by default
        assert_eq!(cfg.loop_detection.blocked_threshold, 0);
        assert_eq!(cfg.loop_detection.search_group_limit, 10);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(
            r"
            [loop_detection]
            normal_threshold = 1
            reduced_threshold = 3
            blocked_threshold = 5
            window_secs = 120
            search_group_limit = 8
            ",
        )
        .unwrap();
        assert_eq!(cfg.loop_detection.normal_threshold, 1);
        assert_eq!(cfg.loop_detection.reduced_threshold, 3);
        assert_eq!(cfg.loop_detection.blocked_threshold, 5);
        assert_eq!(cfg.loop_detection.window_secs, 120);
        assert_eq!(cfg.loop_detection.search_group_limit, 8);
    }

    #[test]
    fn partial_override_keeps_defaults() {
        let cfg: Config = toml::from_str(
            r"
            [loop_detection]
            blocked_threshold = 10
            ",
        )
        .unwrap();
        assert_eq!(cfg.loop_detection.blocked_threshold, 10);
        assert_eq!(cfg.loop_detection.normal_threshold, 2);
        assert_eq!(cfg.loop_detection.search_group_limit, 10);
    }
}

#[cfg(test)]
mod extra_roots_tests {
    use super::super::*;

    #[test]
    fn default_is_empty() {
        let cfg = Config::default();
        assert!(cfg.extra_roots.is_empty());
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"extra_roots = ["/data/store", "/test/env"]"#).unwrap();
        assert_eq!(cfg.extra_roots, vec!["/data/store", "/test/env"]);
    }

    #[test]
    fn merge_extends() {
        let mut base = Config {
            extra_roots: vec!["/base".to_string()],
            ..Config::default()
        };
        base.merge_local(r#"extra_roots = ["/local"]"#);
        assert_eq!(base.extra_roots, vec!["/base", "/local"]);
    }

    #[test]
    fn merge_local_omitting_shell_allowlist_keeps_global() {
        // Regression: the field defaults (via serde) to the full built-in list, so a
        // local override that never mentions `shell_allowlist` must NOT clobber a
        // deliberately shorter global allowlist.
        let mut base = Config {
            shell_allowlist: vec!["git".to_string(), "cargo".to_string()],
            ..Config::default()
        };
        base.merge_local(r"minimal_overhead = true");
        assert_eq!(base.shell_allowlist, vec!["git", "cargo"]);
    }

    #[test]
    fn merge_local_defining_shell_allowlist_overrides() {
        let mut base = Config {
            shell_allowlist: vec!["git".to_string(), "cargo".to_string()],
            ..Config::default()
        };
        base.merge_local(r#"shell_allowlist = ["npm"]"#);
        assert_eq!(base.shell_allowlist, vec!["npm"]);
    }

    #[test]
    fn merge_local_empty_shell_allowlist_disables_restriction() {
        // Explicit empty list = intentional blocklist-only mode; must be honored.
        let mut base = Config {
            shell_allowlist: vec!["git".to_string()],
            ..Config::default()
        };
        base.merge_local(r"shell_allowlist = []");
        assert!(base.shell_allowlist.is_empty());
    }
}

#[cfg(test)]
mod compression_level_tests {
    use super::super::*;

    #[test]
    fn default_is_lite() {
        // Friendly default: plain-English concise guidance, not the symbolic
        // dense/expert-terse styles (those are opt-in power modes).
        assert_eq!(CompressionLevel::default(), CompressionLevel::Lite);
    }

    #[test]
    fn to_components_off() {
        let (ta, od, crp, tm) = CompressionLevel::Off.to_components();
        assert_eq!(ta, TerseAgent::Off);
        assert_eq!(od, OutputDensity::Normal);
        assert_eq!(crp, "off");
        assert!(!tm);
    }

    #[test]
    fn to_components_lite() {
        let (ta, od, crp, tm) = CompressionLevel::Lite.to_components();
        assert_eq!(ta, TerseAgent::Lite);
        assert_eq!(od, OutputDensity::Terse);
        assert_eq!(crp, "off");
        assert!(tm);
    }

    #[test]
    fn to_components_standard() {
        let (ta, od, crp, tm) = CompressionLevel::Standard.to_components();
        assert_eq!(ta, TerseAgent::Full);
        assert_eq!(od, OutputDensity::Terse);
        assert_eq!(crp, "compact");
        assert!(tm);
    }

    #[test]
    fn to_components_max() {
        let (ta, od, crp, tm) = CompressionLevel::Max.to_components();
        assert_eq!(ta, TerseAgent::Ultra);
        assert_eq!(od, OutputDensity::Ultra);
        assert_eq!(crp, "tdd");
        assert!(tm);
    }

    #[test]
    fn from_legacy_ultra_agent_maps_to_max() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Ultra, &OutputDensity::Normal),
            CompressionLevel::Max
        );
    }

    #[test]
    fn from_legacy_ultra_density_maps_to_max() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Off, &OutputDensity::Ultra),
            CompressionLevel::Max
        );
    }

    #[test]
    fn from_legacy_full_agent_maps_to_standard() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Full, &OutputDensity::Normal),
            CompressionLevel::Standard
        );
    }

    #[test]
    fn from_legacy_lite_agent_maps_to_lite() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Lite, &OutputDensity::Normal),
            CompressionLevel::Lite
        );
    }

    #[test]
    fn from_legacy_terse_density_maps_to_lite() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Off, &OutputDensity::Terse),
            CompressionLevel::Lite
        );
    }

    #[test]
    fn from_legacy_both_off_maps_to_off() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Off, &OutputDensity::Normal),
            CompressionLevel::Off
        );
    }

    #[test]
    fn labels_match() {
        assert_eq!(CompressionLevel::Off.label(), "off");
        assert_eq!(CompressionLevel::Lite.label(), "lite");
        assert_eq!(CompressionLevel::Standard.label(), "standard");
        assert_eq!(CompressionLevel::Max.label(), "max");
    }

    #[test]
    fn is_active_false_for_off() {
        assert!(!CompressionLevel::Off.is_active());
    }

    #[test]
    fn is_active_true_for_all_others() {
        assert!(CompressionLevel::Lite.is_active());
        assert!(CompressionLevel::Standard.is_active());
        assert!(CompressionLevel::Max.is_active());
    }

    #[test]
    fn deserialization_defaults_to_lite() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.compression_level, CompressionLevel::Lite);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"compression_level = "standard""#).unwrap();
        assert_eq!(cfg.compression_level, CompressionLevel::Standard);
    }

    #[test]
    fn roundtrip_all_levels() {
        for level in [
            CompressionLevel::Off,
            CompressionLevel::Lite,
            CompressionLevel::Standard,
            CompressionLevel::Max,
        ] {
            let (ta, od, crp, tm) = level.to_components();
            assert!(!crp.is_empty());
            if level == CompressionLevel::Off {
                assert!(!tm);
                assert_eq!(ta, TerseAgent::Off);
                assert_eq!(od, OutputDensity::Normal);
            } else {
                assert!(tm);
            }
        }
    }
}

#[cfg(test)]
mod memory_cleanup_tests {
    use super::super::*;

    #[test]
    fn default_is_aggressive() {
        assert_eq!(MemoryCleanup::default(), MemoryCleanup::Aggressive);
    }

    #[test]
    fn aggressive_ttl_is_300() {
        assert_eq!(MemoryCleanup::Aggressive.idle_ttl_secs(), 300);
    }

    #[test]
    fn shared_ttl_is_1800() {
        assert_eq!(MemoryCleanup::Shared.idle_ttl_secs(), 1800);
    }

    #[test]
    fn index_retention_multiplier_values() {
        assert!(
            (MemoryCleanup::Aggressive.index_retention_multiplier() - 1.0).abs() < f64::EPSILON
        );
        assert!((MemoryCleanup::Shared.index_retention_multiplier() - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn deserialization_defaults_to_aggressive() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.memory_cleanup, MemoryCleanup::Aggressive);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"memory_cleanup = "shared""#).unwrap();
        assert_eq!(cfg.memory_cleanup, MemoryCleanup::Shared);
    }

    #[test]
    fn effective_uses_config_when_no_env() {
        let cfg = Config {
            memory_cleanup: MemoryCleanup::Shared,
            ..Default::default()
        };
        let eff = MemoryCleanup::effective(&cfg);
        assert_eq!(eff, MemoryCleanup::Shared);
    }
}

#[cfg(test)]
mod simplified_config_tests {
    use super::super::*;

    #[test]
    fn max_disk_mb_zero_means_disabled() {
        let cfg = Config::default();
        assert_eq!(cfg.max_disk_mb, 0);
        assert_eq!(cfg.max_disk_mb_effective(), 0);
    }

    #[test]
    fn archive_derives_from_disk_budget() {
        let cfg = Config {
            max_disk_mb: 4000,
            ..Default::default()
        };
        assert_eq!(cfg.archive_max_disk_mb_effective(), 1000);
    }

    #[test]
    fn archive_explicit_overrides_derived() {
        let cfg = Config {
            max_disk_mb: 4000,
            archive: ArchiveConfig {
                max_disk_mb: 800,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(cfg.archive_max_disk_mb_effective(), 800);
    }

    #[test]
    fn bm25_derives_from_disk_budget() {
        let cfg = Config {
            max_disk_mb: 4000,
            ..Default::default()
        };
        assert_eq!(cfg.bm25_max_cache_mb_effective(), 400);
    }

    #[test]
    fn bm25_explicit_overrides_derived() {
        let cfg = Config {
            max_disk_mb: 4000,
            bm25_max_cache_mb: 256,
            ..Default::default()
        };
        assert_eq!(cfg.bm25_max_cache_mb_effective(), 256);
    }

    #[test]
    fn bm25_pure_default_is_generous_not_ram_profile() {
        // No explicit cap and no disk budget: must fall back to the generous disk
        // default (512), NOT the RAM-profile value (which starved large repos and
        // caused perpetual cold rebuilds, issue #249).
        let cfg = Config {
            memory_profile: MemoryProfile::Balanced,
            ..Default::default()
        };
        assert_eq!(cfg.bm25_max_cache_mb_effective(), DEFAULT_BM25_PERSIST_MB);
    }

    #[test]
    fn staleness_days_derives_archive_age() {
        let cfg = Config {
            max_staleness_days: 30,
            ..Default::default()
        };
        assert_eq!(cfg.archive_max_age_hours_effective(), 720);
    }

    #[test]
    fn staleness_explicit_archive_age_overrides() {
        let cfg = Config {
            max_staleness_days: 30,
            archive: ArchiveConfig {
                max_age_hours: 96,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(cfg.archive_max_age_hours_effective(), 96);
    }

    #[test]
    fn no_budget_returns_defaults() {
        let cfg = Config::default();
        assert_eq!(
            cfg.archive_max_disk_mb_effective(),
            ArchiveConfig::default().max_disk_mb
        );
        assert_eq!(
            cfg.archive_max_age_hours_effective(),
            ArchiveConfig::default().max_age_hours
        );
    }

    #[test]
    fn memory_limits_scale_with_disk_budget() {
        let cfg = Config {
            max_disk_mb: 2000,
            ..Default::default()
        };
        let policy = cfg.memory_policy_effective().unwrap();
        // factor = 2000/500 = 4.0
        assert_eq!(policy.knowledge.max_facts, 800);
        assert_eq!(policy.knowledge.max_patterns, 200);
        assert_eq!(policy.episodic.max_episodes, 2000);
        assert_eq!(policy.procedural.max_procedures, 400);
    }

    #[test]
    fn memory_limits_clamped_at_max_factor() {
        let cfg = Config {
            max_disk_mb: 50_000,
            ..Default::default()
        };
        let policy = cfg.memory_policy_effective().unwrap();
        // factor clamped at 10.0
        assert_eq!(policy.knowledge.max_facts, 2000);
        assert_eq!(policy.episodic.max_episodes, 5000);
    }

    #[test]
    fn memory_limits_unchanged_when_no_budget() {
        let cfg = Config::default();
        let policy = cfg.memory_policy_effective().unwrap();
        assert_eq!(policy.knowledge.max_facts, 200);
        assert_eq!(policy.episodic.max_episodes, 500);
    }

    #[test]
    fn simplified_template_is_valid_toml() {
        let parsed: Result<toml::Table, _> = toml::from_str(crate::cli::SIMPLIFIED_TEMPLATE);
        assert!(parsed.is_ok(), "Template must be valid TOML");
    }
}

#[cfg(test)]
mod setup_config_tests {
    use super::super::*;

    #[test]
    fn default_is_none_for_rules_and_skills() {
        let cfg = SetupConfig::default();
        assert!(cfg.auto_inject_rules.is_none());
        assert!(cfg.auto_inject_skills.is_none());
        assert!(cfg.auto_update_mcp);
    }

    #[test]
    fn explicit_true_injects() {
        let cfg = SetupConfig {
            auto_inject_rules: Some(true),
            auto_inject_skills: Some(true),
            auto_update_mcp: true,
        };
        assert!(cfg.should_inject_rules());
        assert!(cfg.should_inject_skills());
    }

    #[test]
    fn explicit_false_skips() {
        let cfg = SetupConfig {
            auto_inject_rules: Some(false),
            auto_inject_skills: Some(false),
            auto_update_mcp: true,
        };
        assert!(!cfg.should_inject_rules());
        assert!(!cfg.should_inject_skills());
    }

    #[test]
    fn deserialization_defaults_when_absent() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.setup.auto_inject_rules.is_none());
        assert!(cfg.setup.auto_inject_skills.is_none());
        assert!(cfg.setup.auto_update_mcp);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(
            r"
            [setup]
            auto_inject_rules = true
            auto_inject_skills = false
            auto_update_mcp = true
            ",
        )
        .unwrap();
        assert_eq!(cfg.setup.auto_inject_rules, Some(true));
        assert_eq!(cfg.setup.auto_inject_skills, Some(false));
        assert!(cfg.setup.auto_update_mcp);
    }

    #[test]
    fn deserialization_null_values() {
        let cfg: Config = toml::from_str(
            r"
            [setup]
            auto_update_mcp = false
            ",
        )
        .unwrap();
        assert!(cfg.setup.auto_inject_rules.is_none());
        assert!(cfg.setup.auto_inject_skills.is_none());
        assert!(!cfg.setup.auto_update_mcp);
    }

    #[test]
    fn roundtrip_serialize_deserialize() {
        let original = Config {
            setup: SetupConfig {
                auto_inject_rules: Some(true),
                auto_inject_skills: Some(false),
                auto_update_mcp: true,
            },
            ..Config::default()
        };
        let toml_str = toml::to_string_pretty(&original).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.setup.auto_inject_rules, Some(true));
        assert_eq!(parsed.setup.auto_inject_skills, Some(false));
        assert!(parsed.setup.auto_update_mcp);
    }

    #[test]
    fn fresh_install_no_rules_should_not_inject() {
        let cfg = SetupConfig::default();
        // On a test machine without lean-ctx rules in home, None should resolve to false
        // (rules_already_present checks real filesystem — on CI this is always false)
        let result = cfg.should_inject_rules();
        // We can't assert false here because the test machine might have lean-ctx installed.
        // Instead, verify the method doesn't panic and returns a bool.
        let _ = result;
    }

    #[test]
    fn tool_profile_serializes_as_root_key_not_under_table() {
        // Regression: a stray `tool_profile` once landed under [secret_detection]
        // because whole-struct serialization placed the scalar after a table.
        // It must always serialize as a root-level key and round-trip.
        let original = Config {
            tool_profile: Some("standard".to_string()),
            ..Config::default()
        };
        let toml_str = toml::to_string_pretty(&original).unwrap();
        let tp_pos = toml_str
            .find("tool_profile")
            .expect("tool_profile should be serialized");
        let first_table = toml_str.find("\n[").unwrap_or(toml_str.len());
        assert!(
            tp_pos < first_table,
            "tool_profile must be a root key, not nested under a [table]:\n{toml_str}"
        );
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.tool_profile.as_deref(), Some("standard"));
    }
}
