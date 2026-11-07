// SPDX-License-Identifier: Apache-2.0
//! #2175: a config migration journal left by an earlier release must not
//! block later migrations, and a journal that really cannot be verified must
//! neither flip telemetry consent in memory nor warn on every command.

use super::Config;

/// Real artifacts of the official 3.11.0 release binary (macOS arm64), taken
/// from an isolated HOME after one `lean-ctx config` run on `pre_v4.toml`.
const PRE_V4: &str = include_str!("../../../tests/fixtures/config_migration_v3_11_0/pre_v4.toml");
const MIGRATED_BY_3_11_0: &str =
    include_str!("../../../tests/fixtures/config_migration_v3_11_0/migrated_by_3_11_0.toml");
const RECEIPT_3_11_0: &str =
    include_str!("../../../tests/fixtures/config_migration_v3_11_0/receipt_3_11_0.json");

fn journal_paths(config: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        config.with_file_name("config.toml.v4-migration.bak"),
        config.with_file_name("config.toml.v4-migration.json"),
    )
}

fn write_config(raw: &str) -> std::path::PathBuf {
    let path = Config::path().expect("isolated config path");
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("config dir");
    std::fs::write(&path, raw).expect("config");
    path
}

fn load_and_migrate(raw: &str) -> Config {
    let mut cfg: Config = toml::from_str(raw).expect("config parses");
    cfg.migrate_v4_config_on_disk();
    cfg
}

fn pause_marker() -> std::path::PathBuf {
    let path = Config::path().expect("isolated config path");
    super::loader::config_migration_pause_marker(&path).expect("isolated state dir")
}

/// Asserts that the last migration was persisted; a refusal names its pause
/// state instead of failing on a bare `!marker.exists()` (#2184).
fn assert_migration_persisted(path: &std::path::Path) {
    if let Some(pause) = super::config_migration_pause_status(path) {
        panic!(
            "the migration was refused ({}): {}",
            pause.label,
            pause.message()
        );
    }
}

/// The exact 3.11.0 → 3.12 sequence from the issue: 3.11.0 migrated a legacy
/// contributor and left its journal; 3.12 retires the `[cloud]` table that
/// migration wrote and was refused on every command.
#[test]
fn journal_left_by_3_11_0_no_longer_blocks_the_next_migration() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let path = write_config(MIGRATED_BY_3_11_0);
    let (backup, receipt) = journal_paths(&path);
    std::fs::write(&backup, PRE_V4).unwrap();
    std::fs::write(&receipt, RECEIPT_3_11_0).unwrap();
    // 3.11.0 also left its empty lock file beside the config.
    let sibling_lock = path.with_file_name("config.toml.v4-migration.lock");
    std::fs::write(&sibling_lock, "").unwrap();

    let cfg = load_and_migrate(MIGRATED_BY_3_11_0);

    let migrated = std::fs::read_to_string(&path).unwrap();
    assert!(!migrated.contains("[cloud]"), "{migrated}");
    assert!(
        sibling_lock.exists(),
        "the legacy 3.11 lock file is left alone (deleting it could split the lock)"
    );
    assert!(cfg.telemetry.enabled, "3.11.0 recorded an explicit opt-in");
    assert!(!pause_marker().exists(), "nothing was refused");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), PRE_V4);

    // Stable afterwards: the next load proposes nothing.
    load_and_migrate(&migrated);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), migrated);

    // The extended journal still restores the user's original pre-v4 bytes.
    crate::config_io::rollback_config_migration(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PRE_V4);
    assert!(!backup.exists() && !receipt.exists());
}

/// A journal that cannot be verified keeps the migration paused. The pending
/// legacy telemetry reset must not take effect in memory while the file still
/// says `enabled = false`, and stays pending for a later successful run.
#[test]
fn refused_persist_keeps_the_saved_opt_out_and_reports_once() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let raw = "terse_agent = \"full\"\n\n[telemetry]\nenabled = false\n";
    let path = write_config(raw);
    let (backup, receipt) = journal_paths(&path);
    // Receipt from another file: the backup does not match it.
    std::fs::write(&backup, "tampered = true\n").unwrap();
    std::fs::write(&receipt, RECEIPT_3_11_0).unwrap();
    assert!(crate::core::telemetry_consent::legacy_opt_out_reset_pending());

    let cfg = load_and_migrate(raw);

    assert!(
        !cfg.telemetry.enabled,
        "on-disk opt-out stays authoritative"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    assert!(crate::core::telemetry_consent::legacy_opt_out_reset_pending());
    let first = std::fs::read_to_string(pause_marker()).expect("reported once");

    // Same state: same fingerprint, no new report.
    let cfg = load_and_migrate(raw);
    assert!(!cfg.telemetry.enabled);
    assert_eq!(std::fs::read_to_string(pause_marker()).unwrap(), first);

    // A changed journal is a new state and is reported again.
    std::fs::write(&backup, "tampered = false\n").unwrap();
    load_and_migrate(raw);
    assert_ne!(std::fs::read_to_string(pause_marker()).unwrap(), first);

    // Once the user moves the journal away, the migration and the reset run.
    std::fs::remove_file(&backup).unwrap();
    std::fs::remove_file(&receipt).unwrap();
    let cfg = load_and_migrate(raw);
    assert_migration_persisted(&path);
    assert!(cfg.telemetry.enabled, "the persisted reset applies");
    assert!(!crate::core::telemetry_consent::legacy_opt_out_reset_pending());
    assert!(!pause_marker().exists());
    let migrated = std::fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("compression_level"), "{migrated}");
}

/// Review #6: the message names both journal files and the action that works
/// in the specific state; rollback is only offered where it can act.
#[test]
fn pause_message_is_state_specific_and_deterministic() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let path = write_config(MIGRATED_BY_3_11_0);
    let (backup, receipt) = journal_paths(&path);
    std::fs::write(&backup, "tampered = true\n").unwrap();
    std::fs::write(&receipt, RECEIPT_3_11_0).unwrap();

    let pause = super::loader::describe_config_migration_pause(&path);
    let message = pause.message();
    assert_eq!(pause.label, "backup-mismatch");
    assert!(
        message.contains("config.toml.v4-migration.bak"),
        "{message}"
    );
    assert!(
        message.contains("config.toml.v4-migration.json"),
        "{message}"
    );
    assert!(message.contains("move both out of"), "{message}");
    assert!(
        !message.contains("--rollback` restores"),
        "rollback refuses this state and must not be recommended: {message}"
    );
    assert_eq!(
        message,
        super::loader::describe_config_migration_pause(&path).message()
    );

    // A verified (completed) journal with a refused write: rollback works.
    std::fs::write(&backup, PRE_V4).unwrap();
    let pause = super::loader::describe_config_migration_pause(&path);
    assert_eq!(pause.label, "write-refused");
    assert!(
        pause.action.contains("lean-ctx migrate config --rollback"),
        "{}",
        pause.action
    );
    assert!(pause.original.is_none(), "no journal was archived yet");
}

/// Review #2: refusals whose error text names random temporary files are one
/// state, reported once.
#[test]
fn refusals_with_different_temp_names_report_once() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let raw = "terse_agent = \"full\"\n";
    let updated = "compression_level = \"standard\"\n";
    let path = write_config(raw);
    let error = |uuid: &str| {
        format!(
            "create {}: Permission denied (os error 13)",
            path.with_file_name(format!(".config.toml.v4-migration.bak.{uuid}.tmp"))
                .display()
        )
    };

    assert!(super::loader::warn_config_migration_paused_once(
        &path,
        raw,
        updated,
        &error("0f5c2a4e9b7d4c1e8a3f6b2d9c0e1a7b")
    ));
    assert!(!super::loader::warn_config_migration_paused_once(
        &path,
        raw,
        updated,
        &error("7d1e3b9a2c4f4e8b9a0c1d2e3f4a5b6c")
    ));
    let marker = std::fs::read_to_string(pause_marker()).unwrap();
    assert!(marker.ends_with("write-refused\n"), "{marker}");
    let status = super::config_migration_pause_status(&path).expect("doctor sees the pause");
    assert_eq!(status.label, "write-refused");

    // A different proposed output is a new state.
    assert!(super::loader::warn_config_migration_paused_once(
        &path,
        raw,
        "compression_level = \"lite\"\n",
        &error("0f5c2a4e9b7d4c1e8a3f6b2d9c0e1a7b")
    ));
}

/// Review #4: the marker is keyed by the config path, deterministically, and
/// a successful migration clears only its own marker.
#[test]
fn pause_marker_is_keyed_by_config_path() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let path = write_config("terse_agent = \"full\"\n");
    let other = path.with_file_name("other.toml");
    std::fs::write(&other, "terse_agent = \"full\"\n").unwrap();

    let marker = super::loader::config_migration_pause_marker(&path).unwrap();
    let other_marker = super::loader::config_migration_pause_marker(&other).unwrap();
    assert_ne!(marker, other_marker);
    assert_eq!(
        marker,
        super::loader::config_migration_pause_marker(&path).unwrap()
    );
    let name = marker.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.starts_with("config_migration_paused."), "{name}");
    assert_eq!(name.len(), "config_migration_paused.".len() + 16, "{name}");

    let updated = "compression_level = \"standard\"\n";
    assert!(super::loader::warn_config_migration_paused_once(
        &path, "a", updated, "x"
    ));
    assert!(super::loader::warn_config_migration_paused_once(
        &other, "a", updated, "x"
    ));
    assert!(marker.exists() && other_marker.exists());

    // The real config migrates; only its marker goes.
    load_and_migrate("terse_agent = \"full\"\n");
    assert_migration_persisted(&path);
    assert!(!marker.exists());
    assert!(other_marker.exists());
    assert!(super::config_migration_pause_status(&path).is_none());
}

/// Owner 02.11.: once a journal was archived, the warning names the archive
/// that keeps the original pre-v4 config.
#[test]
fn pause_message_names_the_original_pre_v4_archive() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let path = write_config(MIGRATED_BY_3_11_0);
    let (backup, receipt) = journal_paths(&path);
    std::fs::write(&backup, PRE_V4).unwrap();
    std::fs::write(&receipt, RECEIPT_3_11_0).unwrap();
    // A user edit supersedes the 3.11.0 journal; the next migration archives it.
    let edited = format!("{MIGRATED_BY_3_11_0}\n# edited\n");
    std::fs::write(&path, &edited).unwrap();
    load_and_migrate(&edited);

    let original = crate::config_io::original_config_migration_backup(&path)
        .expect("the 3.11.0 journal is the original archive");
    assert_eq!(std::fs::read_to_string(&original).unwrap(), PRE_V4);
    let name = original.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        name.starts_with("config.toml.v4-migration.original."),
        "{name}"
    );

    std::fs::write(&backup, "tampered = true\n").unwrap();
    let message = super::loader::describe_config_migration_pause(&path).message();
    assert!(message.contains(&name), "{message}");
    assert!(
        message.contains("original pre-migration config"),
        "{message}"
    );
}
