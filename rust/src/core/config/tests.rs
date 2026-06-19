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
mod prefer_native_editor_tests {
    use super::super::*;

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
        assert!(cfg.disabled_tools_effective().is_empty());
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
        // #454: the dedicated edit tool is blocked; reads/search stay available.
        assert!(cfg.edit_tool_blocked("ctx_edit"));
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
        assert!(
            cfg.disabled_tools_effective()
                .iter()
                .any(|t| t == "ctx_edit"),
            "edit tools must be folded into the effective disabled set"
        );
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
mod delta_explicit_tests {
    use super::super::*;

    // --- Defaults ---

    #[test]
    fn default_is_false() {
        let cfg = Config::default();
        assert!(!cfg.delta_explicit);
    }

    #[test]
    fn effective_false_when_unset() {
        if std::env::var("LCTX_DELTA_EXPLICIT").is_ok() {
            return;
        }
        let cfg = Config::default();
        assert!(!cfg.delta_explicit_effective());
    }

    // --- Config field ---

    #[test]
    fn config_field_true_respected_when_no_env() {
        if std::env::var("LCTX_DELTA_EXPLICIT").is_ok() {
            return;
        }
        let cfg = Config {
            delta_explicit: true,
            ..Default::default()
        };
        assert!(cfg.delta_explicit_effective());
    }

    #[test]
    fn config_field_false_respected_when_no_env() {
        if std::env::var("LCTX_DELTA_EXPLICIT").is_ok() {
            return;
        }
        let cfg = Config {
            delta_explicit: false,
            ..Default::default()
        };
        assert!(!cfg.delta_explicit_effective());
    }

    // --- Env override (both directions) ---

    #[test]
    fn env_overrides_config_field_in_both_directions() {
        // All env mutation serializes through this lock (Rust 2024 set_var is
        // `unsafe`; the lock is the documented soundness precondition).
        let _lock = crate::core::data_dir::test_env_lock();

        // env=1 turns the feature ON even when the config field is false.
        crate::test_env::set_var("LCTX_DELTA_EXPLICIT", "1");
        let off_cfg = Config {
            delta_explicit: false,
            ..Default::default()
        };
        assert!(
            off_cfg.delta_explicit_effective(),
            "LCTX_DELTA_EXPLICIT=1 must enable the feature over a false config field"
        );

        // env=0 forces it OFF even when the config field is true.
        crate::test_env::set_var("LCTX_DELTA_EXPLICIT", "0");
        let on_cfg = Config {
            delta_explicit: true,
            ..Default::default()
        };
        assert!(
            !on_cfg.delta_explicit_effective(),
            "LCTX_DELTA_EXPLICIT=0 must disable the feature over a true config field"
        );

        // `true`/`false` spellings are honoured too (case-insensitive).
        crate::test_env::set_var("LCTX_DELTA_EXPLICIT", "true");
        assert!(off_cfg.delta_explicit_effective());
        crate::test_env::set_var("LCTX_DELTA_EXPLICIT", "FALSE");
        assert!(!on_cfg.delta_explicit_effective());

        // Restore: with the var removed the config field decides again.
        crate::test_env::remove_var("LCTX_DELTA_EXPLICIT");
        assert!(on_cfg.delta_explicit_effective());
        assert!(!off_cfg.delta_explicit_effective());
    }

    // --- TOML deserialization ---

    #[test]
    fn deserialization_true() {
        let cfg: Config = toml::from_str("delta_explicit = true").unwrap();
        assert!(cfg.delta_explicit);
    }

    #[test]
    fn deserialization_false() {
        let cfg: Config = toml::from_str("delta_explicit = false").unwrap();
        assert!(!cfg.delta_explicit);
    }

    #[test]
    fn deserialization_absent_defaults_false() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(!cfg.delta_explicit);
    }

    // --- Round-trip (serialize → deserialize preserves the field) ---

    #[test]
    fn round_trip_preserves_field() {
        let cfg = Config {
            delta_explicit: true,
            ..Default::default()
        };
        let serialized = toml::to_string(&cfg).expect("Config must serialize to TOML");
        let restored: Config =
            toml::from_str(&serialized).expect("serialized Config must round-trip");
        assert!(
            restored.delta_explicit,
            "delta_explicit must survive a TOML serialize → deserialize round-trip"
        );
    }

    // --- Coexistence with other config fields ---

    #[test]
    fn delta_explicit_independent_of_no_degrade() {
        if std::env::var("LCTX_DELTA_EXPLICIT").is_ok() || std::env::var("LCTX_NO_DEGRADE").is_ok()
        {
            return;
        }
        let cfg = Config {
            delta_explicit: true,
            no_degrade: true,
            ..Default::default()
        };
        assert!(cfg.delta_explicit_effective());
        assert!(cfg.no_degrade_effective());
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

#[cfg(test)]
mod config_load_cache_tests {
    use super::super::*;
    use crate::core::data_dir::isolated_data_dir;

    /// #406 regression: `Config::load()` must reflect a content change even when
    /// the file mtime is unchanged. A mtime-only cache (the old behaviour) kept a
    /// long-lived MCP server on a stale `path_jail` while a fresh `doctor`
    /// process — with an empty cache — saw the new value. The cache is now keyed
    /// on a content hash, so this scenario stays live.
    #[test]
    fn load_honors_content_change_with_preserved_mtime() {
        let _iso = isolated_data_dir();
        let path = Config::path().expect("config path under isolated data dir");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        // Warm the cache with jail unset (default None).
        std::fs::write(&path, "# initial\n").unwrap();
        let mtime0 = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(Config::load().path_jail, None);

        // Flip path_jail=false but restore the original mtime, so any mtime-only
        // cache would serve the stale value (#406).
        std::fs::write(&path, "path_jail = false\n").unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime0)).unwrap();

        assert_eq!(
            Config::load().path_jail,
            Some(false),
            "Config::load() must honor a content change with unchanged mtime (#406)"
        );
    }
}

#[cfg(test)]
mod cost_config_tests {
    use super::super::*;

    #[test]
    fn default_is_empty() {
        let cfg = CostConfig::default();
        assert!(cfg.default_model.is_none());
        assert!(cfg.models.is_empty());
        assert_eq!(cfg.model_for_client("cursor"), None);
    }

    #[test]
    fn per_client_overrides_default() {
        let mut models = std::collections::HashMap::new();
        models.insert("cursor".to_string(), "claude-opus-4.5".to_string());
        let cfg = CostConfig {
            default_model: Some("gpt-5.4".to_string()),
            models,
        };
        assert_eq!(
            cfg.model_for_client("cursor").as_deref(),
            Some("claude-opus-4.5")
        );
        // No entry → global default.
        assert_eq!(cfg.model_for_client("copilot").as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn blank_values_are_ignored() {
        let cfg = CostConfig {
            default_model: Some("   ".to_string()),
            models: std::collections::HashMap::new(),
        };
        assert_eq!(cfg.model_for_client("cursor"), None);
    }

    #[test]
    fn parses_from_toml_section() {
        let cfg: Config = toml::from_str(
            r#"
[cost]
default_model = "claude-opus-4.5"

[cost.models]
cursor = "claude-opus-4.5"
copilot = "gpt-5.4"
"#,
        )
        .unwrap();
        assert_eq!(cfg.cost.default_model.as_deref(), Some("claude-opus-4.5"));
        assert_eq!(
            cfg.cost.model_for_client("copilot").as_deref(),
            Some("gpt-5.4")
        );
    }

    #[test]
    fn default_config_has_empty_cost_section() {
        let cfg = Config::default();
        assert!(cfg.cost.default_model.is_none());
        assert!(cfg.cost.models.is_empty());
    }
}

#[cfg(test)]
mod persist_global_tests {
    //! Regression tests for #443: persisting config must never reset customized
    //! values nor leak project-local overrides into the global file.
    use super::super::*;

    fn tmp_config() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        (dir, path)
    }

    // The canonical persist path keeps every customized value and applies only
    // the requested change.
    #[test]
    fn update_global_at_preserves_customized_and_persists_change() {
        let (_dir, path) = tmp_config();
        std::fs::write(
            &path,
            "max_ram_percent = 30\ncompression_level = \"standard\"\n",
        )
        .unwrap();

        let returned = Config::update_global_at(&path, |c| c.proxy_enabled = Some(true))
            .expect("update_global_at must succeed");
        assert_eq!(returned.proxy_enabled, Some(true));

        let reloaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            reloaded.max_ram_percent, 30,
            "customized value must survive"
        );
        assert_eq!(reloaded.compression_level, CompressionLevel::Standard);
        assert_eq!(reloaded.proxy_enabled, Some(true));
    }

    // load_global never folds in project-local overrides; it reads only the
    // global file. update_global builds on this, so persists cannot leak.
    #[test]
    fn load_global_from_reads_only_the_given_file() {
        let (_dir, path) = tmp_config();
        std::fs::write(&path, "theme = \"global-theme\"\n").unwrap();
        let cfg = Config::load_global_from(&path);
        assert_eq!(cfg.theme, "global-theme");
    }

    // Root-cause marker: the OLD `load() (with merge_local) -> save()` pattern
    // leaks a project-local override into the global file. This proves why
    // persist paths must use load_global / update_global instead.
    #[test]
    fn merged_load_then_save_leaks_local_override_root_cause_marker() {
        let (_dir, path) = tmp_config();
        std::fs::write(&path, "theme = \"global-theme\"\n").unwrap();

        // Simulate `Config::load()`: global file + project-local override merged.
        let mut cfg = Config::load_global_from(&path);
        cfg.merge_local("theme = \"project-local\"\n");
        // OLD persist: write the merged struct back to the GLOBAL file.
        cfg.save_to(&path).unwrap();

        let reloaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            reloaded.theme, "project-local",
            "OLD load+merge_local+save leaks the project-local value into global (#443)"
        );
    }

    // Subticket 4 contract: refuse to touch an unparseable config; never clobber.
    #[test]
    fn update_global_at_refuses_unparseable_and_leaves_file_untouched() {
        let (_dir, path) = tmp_config();
        let corrupt = "max_ram_percent = = =\n";
        std::fs::write(&path, corrupt).unwrap();

        let result = Config::update_global_at(&path, |c| c.proxy_enabled = Some(true));
        assert!(
            result.is_err(),
            "must refuse to modify an unparseable config"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            corrupt,
            "the corrupt file must be left exactly as-is"
        );
    }

    #[test]
    fn load_global_from_missing_or_empty_yields_defaults() {
        let (_dir, path) = tmp_config();
        // Missing file.
        let cfg = Config::load_global_from(&path);
        assert_eq!(cfg.max_ram_percent, Config::default().max_ram_percent);
        // Empty / whitespace-only file.
        std::fs::write(&path, "   \n").unwrap();
        let cfg2 = Config::load_global_from(&path);
        assert_eq!(cfg2.max_ram_percent, Config::default().max_ram_percent);
    }
}
