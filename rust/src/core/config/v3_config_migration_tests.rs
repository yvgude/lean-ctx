// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn v3_ultra_config_migrates_to_max() {
    let migrated = Config::migrate_v3_compression_document(
        "terse_agent = \"ultra\"\noutput_density = \"normal\"\n",
    )
    .expect("valid v3 config")
    .expect("legacy keys require migration");

    assert!(migrated.contains("compression_level = \"max\""));
    assert!(!migrated.contains("terse_agent"));
    assert!(!migrated.contains("output_density"));
}

#[test]
fn explicit_v4_off_wins_over_legacy_max() {
    let migrated = Config::migrate_v3_compression_document(
        "compression_level = \"off\"\nultra_compact = true\n",
    )
    .expect("valid mixed config")
    .expect("legacy keys require migration");

    assert!(migrated.contains("compression_level = \"off\""));
    assert!(!migrated.contains("ultra_compact"));
}

#[test]
fn migration_is_idempotent() {
    let migrated = Config::migrate_v3_compression_document("output_density = \"terse\"\n")
        .expect("valid v3 config")
        .expect("legacy key requires migration");
    assert!(
        Config::migrate_v3_compression_document(&migrated)
            .expect("valid v4 config")
            .is_none()
    );
}

#[test]
fn malformed_config_fails_closed() {
    assert!(Config::migrate_v3_compression_document("terse_agent = [\n").is_err());
}

#[test]
fn semantically_invalid_legacy_values_fail_closed() {
    for raw in [
        "terse_agent = 7\n",
        "terse_agent = \"unknown\"\n",
        "output_density = false\n",
        "output_density = \"unknown\"\n",
        "ultra_compact = \"yes\"\n",
        "compression_level = 7\nterse_agent = \"full\"\n",
        "terse_agent = \"full\"\noutput_density = \"unknown\"\n",
    ] {
        assert!(
            Config::migrate_v3_compression_document(raw).is_err(),
            "invalid config must not be rewritten: {raw:?}"
        );
    }
}

#[test]
fn selected_profile_is_migrated_before_deserialization() {
    let cfg = super::loader::parse_config_with_profile(
        "[profiles.power]\nterse_agent = \"ultra\"\n",
        Some("power"),
    )
    .expect("valid v3 profile");
    assert_eq!(cfg.compression_level, CompressionLevel::Max);
}

#[test]
fn local_v3_config_parser_preserves_legacy_semantics() {
    let cfg = super::loader::parse_config_with_profile("output_density = \"ultra\"\n", None)
        .expect("valid project-local v3 config");
    assert_eq!(cfg.compression_level, CompressionLevel::Max);
}

#[test]
fn invalid_selected_profile_prevents_persistable_migration() {
    let raw = "[profiles.power]\nterse_agent = \"ultra\"\nconfig_profile = \"nested\"\n";
    assert!(Config::validated_v3_compression_document(raw, Some("power")).is_err());
}
