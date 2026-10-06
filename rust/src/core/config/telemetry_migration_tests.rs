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
    let current = "[cloud]\ncontribute_enabled = false\n\n[telemetry]\nenabled = false\n";
    assert_eq!(
        Config::migrate_v4_config_document(current, None, false).expect("valid current config"),
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
