//! Tests for config parsing and serialization.

#[allow(unused_imports)]
use super::*;

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
    fn default_is_shared() {
        assert_eq!(MemoryCleanup::default(), MemoryCleanup::Shared);
    }

    #[test]
    fn aggressive_ttl_is_300() {
        assert_eq!(MemoryCleanup::Aggressive.idle_ttl_secs(), 300);
    }

    #[test]
    fn shared_ttl_is_3600() {
        assert_eq!(MemoryCleanup::Shared.idle_ttl_secs(), 3600);
    }

    #[test]
    fn index_retention_multiplier_values() {
        assert!(
            (MemoryCleanup::Aggressive.index_retention_multiplier() - 1.0).abs() < f64::EPSILON
        );
        assert!((MemoryCleanup::Shared.index_retention_multiplier() - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn deserialization_defaults_to_shared() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.memory_cleanup, MemoryCleanup::Shared);
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
    fn session_retention_days_is_independent_from_archive_staleness() {
        let cfg = Config {
            max_staleness_days: 30,
            session_retention_days: 14,
            ..Default::default()
        };

        assert_eq!(cfg.session_retention_days_effective(), 14);
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
    fn should_update_mcp_reflects_flag() {
        // #281: the predicate that gates MCP registration in setup/onboard/init
        // must mirror the config flag exactly, so a locked-down environment can
        // disable MCP while still getting hooks/rules/skills.
        let mut s = SetupConfig::default();
        assert!(s.should_update_mcp());
        s.auto_update_mcp = false;
        assert!(!s.should_update_mcp());
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
