// SPDX-License-Identifier: Apache-2.0

use super::Config;

#[test]
fn legacy_contributor_retains_explicit_preference() {
    let migrated =
        Config::migrate_v4_config_document("[cloud]\ncontribute_enabled = true\n", None, false)
            .expect("valid legacy config")
            .expect("legacy flag requires migration");
    let parsed: Config = toml::from_str(&migrated).expect("migrated config parses");
    assert!(!parsed.cloud.contribute_enabled);
    assert!(parsed.telemetry.enabled);
    assert_eq!(
        parsed.telemetry.preference,
        super::TelemetryPreference::ExplicitlyEnabled
    );
}

#[test]
fn legacy_contributor_explicit_opt_out_survives_migration() {
    let migrated = Config::migrate_v4_config_document(
        "[cloud]\ncontribute_enabled = true\n\n[telemetry]\nenabled = false\n",
        None,
        false,
    )
    .expect("valid legacy config")
    .expect("legacy flag requires migration");
    let parsed: Config = toml::from_str(&migrated).expect("migrated config parses");
    assert!(!parsed.cloud.contribute_enabled);
    assert!(!parsed.telemetry.enabled);
}

#[test]
fn telemetry_migration_is_idempotent_after_legacy_flag_is_cleared() {
    let legacy = "[cloud]\ncontribute_enabled = false\n\n[telemetry]\nenabled = false\n";
    // The retired `[cloud]` table is removed once; the opt-out stays.
    let current = Config::migrate_v4_config_document(legacy, None, false)
        .expect("valid legacy config")
        .expect("the retired table is removed");
    assert!(!current.contains("[cloud]"), "{current}");
    let parsed: Config = toml::from_str(&current).expect("migrated config parses");
    assert!(!parsed.telemetry.enabled);
    assert_eq!(
        Config::migrate_v4_config_document(&current, None, false).expect("valid current config"),
        None
    );
}

#[test]
fn malformed_legacy_config_fails_closed_without_rewrite() {
    assert!(
        Config::migrate_v4_config_document("[cloud\n", None, false).is_err(),
        "unparseable user config must not be rewritten"
    );
}

#[test]
fn pre_v2_implicit_opt_out_is_reenabled_once_on_upgrade() {
    for legacy in [
        "[telemetry]\nenabled = false\n",
        "[telemetry]\nenabled = false\npreference = \"default_on\"\nlast_heartbeat = \"2026-07-01\"\n",
    ] {
        let reset = super::loader::reset_legacy_telemetry_opt_out(legacy)
            .expect("implicit pre-v2 opt-out is reset");
        let parsed: Config = toml::from_str(&reset).expect("reset config parses");
        assert!(parsed.telemetry.enabled);
        assert_eq!(
            parsed.telemetry.preference,
            super::TelemetryPreference::DefaultOn
        );
        assert!(parsed.telemetry.send_eligible(None, None));
    }
}

#[test]
fn explicit_or_environment_opt_out_is_never_overridden() {
    for kept in [
        "[telemetry]\nenabled = false\npreference = \"explicitly_disabled\"\n",
        "[telemetry]\nenabled = true\npreference = \"explicitly_disabled\"\n",
        "[telemetry]\nenabled = true\n",
        "[cloud]\ncontribute_enabled = false\n",
        "",
    ] {
        assert_eq!(
            super::loader::reset_legacy_telemetry_opt_out(kept),
            None,
            "{kept}"
        );
    }
    let reset = super::loader::reset_legacy_telemetry_opt_out("[telemetry]\nenabled = false\n")
        .expect("implicit opt-out is reset");
    let parsed: Config = toml::from_str(&reset).expect("reset config parses");
    assert!(!parsed.telemetry.send_eligible(Some("1"), None));
    assert!(!parsed.telemetry.send_eligible(None, Some("off")));
}
