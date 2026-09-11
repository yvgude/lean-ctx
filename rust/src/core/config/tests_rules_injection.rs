use super::*;

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

// --- #1754: which settings silence in-band steering ---

#[test]
fn off_declines_rule_steering() {
    // #1599's rule reaches the in-band setup tip too: off means off, on every
    // channel. Under `off` the tip is also unclearable — `lean-ctx setup`
    // removes the rules block rather than writing it.
    for raw in ["off", "none", "disabled"] {
        let cfg = Config {
            rules_injection: Some(raw.to_string()),
            ..Default::default()
        };
        assert!(
            cfg.declines_rule_steering(),
            "{raw:?} must silence in-band steering"
        );
    }
}

#[test]
fn explicit_auto_inject_false_declines_rule_steering() {
    // "never inject" said in the setup section is the same refusal.
    let cfg = Config {
        setup: SetupConfig {
            auto_inject_rules: Some(false),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(cfg.declines_rule_steering());
}

#[test]
fn auto_and_explicit_true_still_accept_rule_steering() {
    // `None` is auto, not a refusal: rules are simply not present yet, which is
    // exactly the case the tip exists for. `Some(true)` wants them outright.
    assert!(!Config::default().declines_rule_steering());

    let explicit_on = Config {
        setup: SetupConfig {
            auto_inject_rules: Some(true),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(!explicit_on.declines_rule_steering());
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
    base.merge_local(r#"rules_injection = "dedicated""#, true);
    assert_eq!(base.rules_injection_effective(), RulesInjection::Dedicated);
}
