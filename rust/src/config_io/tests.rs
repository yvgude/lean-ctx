// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn merge_preserves_comments_and_unknown_keys() {
    let existing = "\
# My custom config — do not delete!
ultra_compact = true  # inline note

# Section about the proxy
[proxy]
enabled = false
custom_user_key = \"keep-me\"
";
    let incoming = "\
ultra_compact = false

[proxy]
enabled = true
";
    let merged = merge_toml(existing, incoming).unwrap();

    // Comments survive.
    assert!(merged.contains("# My custom config — do not delete!"));
    assert!(merged.contains("# inline note"));
    assert!(merged.contains("# Section about the proxy"));
    // Unknown / user keys survive.
    assert!(merged.contains("custom_user_key = \"keep-me\""));
    // Values are updated.
    assert!(merged.contains("ultra_compact = false"));
    assert!(merged.contains("enabled = true"));
    assert!(!merged.contains("enabled = false"));
}

#[test]
fn minimal_mode_skips_unset_defaults_but_keeps_existing() {
    // On-disk: only ultra_compact is explicitly set, with a comment.
    let existing = "# my config\nultra_compact = true\n";
    // Incoming: full serialization (all fields present).
    let incoming = "ultra_compact = false\ncheckpoint_interval = 15\ntheme = \"default\"\n";
    // Defaults: what an untouched config would serialize to.
    let defaults = "ultra_compact = false\ncheckpoint_interval = 15\ntheme = \"default\"\n";

    let merged = merge_toml_inner(existing, incoming, Some(defaults)).unwrap();

    // Existing key updated + comment preserved.
    assert!(merged.contains("# my config"));
    assert!(merged.contains("ultra_compact = false"));
    // Default-valued keys that were never on disk are NOT added (stay minimal).
    assert!(!merged.contains("checkpoint_interval"));
    assert!(!merged.contains("theme"));
}

#[test]
fn minimal_mode_writes_non_default_values() {
    let existing = "";
    let incoming = "ultra_compact = false\ncheckpoint_interval = 42\n";
    let defaults = "ultra_compact = false\ncheckpoint_interval = 15\n";

    let merged = merge_toml_inner(existing, incoming, Some(defaults)).unwrap();

    // Non-default value is written, default value is skipped.
    assert!(merged.contains("checkpoint_interval = 42"));
    assert!(!merged.contains("ultra_compact"));
}

#[test]
fn minimal_mode_drops_empty_default_tables() {
    let existing = "";
    let incoming = "[proxy]\nenabled = false\n\n[lsp]\n";
    let defaults = "[proxy]\nenabled = false\n\n[lsp]\n";

    let merged = merge_toml_inner(existing, incoming, Some(defaults)).unwrap();

    // Everything equals default and nothing exists on disk → empty output.
    assert!(!merged.contains("[lsp]"));
    assert!(!merged.contains("[proxy]"));
}

#[test]
fn merge_adds_new_keys_and_sections() {
    let existing = "ultra_compact = true\n";
    let incoming = "ultra_compact = true\nnew_key = 42\n\n[updates]\nauto_update = true\n";
    let merged = merge_toml(existing, incoming).unwrap();
    assert!(merged.contains("new_key = 42"));
    assert!(merged.contains("[updates]"));
    assert!(merged.contains("auto_update = true"));
}

fn unique_tmp(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("lc_{tag}_{}_{nanos}", std::process::id()))
}

#[test]
fn write_toml_preserving_backs_up_and_keeps_comments() {
    let tmp = unique_tmp("cfg_test");
    let _ = std::fs::create_dir_all(&tmp);
    let path = tmp.join("config.toml");
    std::fs::write(&path, "# keep\nultra_compact = true\n").unwrap();

    write_toml_preserving(&path, "ultra_compact = false\n").unwrap();

    let result = std::fs::read_to_string(&path).unwrap();
    assert!(result.contains("# keep"));
    assert!(result.contains("ultra_compact = false"));
    // Backup created.
    assert!(path.with_file_name("config.toml.bak").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn write_toml_preserving_handles_missing_file() {
    let tmp = unique_tmp("cfg_new");
    let _ = std::fs::remove_dir_all(&tmp);
    let path = tmp.join("config.toml");
    write_toml_preserving(&path, "ultra_compact = true\n").unwrap();
    let result = std::fs::read_to_string(&path).unwrap();
    assert!(result.contains("ultra_compact = true"));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn minimal_mode_refuses_to_clobber_unparseable_existing() {
    // #443: a corrupt config must never be silently replaced with defaults.
    let tmp = unique_tmp("cfg_corrupt");
    let _ = std::fs::create_dir_all(&tmp);
    let path = tmp.join("config.toml");
    let corrupt = "broken = = =\n";
    std::fs::write(&path, corrupt).unwrap();

    let result =
        write_toml_preserving_minimal(&path, "ultra_compact = false\n", "ultra_compact = false\n");

    assert!(
        result.is_err(),
        "must refuse to overwrite an unparseable config"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        corrupt,
        "the corrupt file must be left exactly as-is"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn backup_failure_aborts_before_mutating_config() {
    let tmp = unique_tmp("cfg_backup_failure");
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("config.toml");
    let backup = tmp.join("config.toml.bak");
    std::fs::write(&path, "old = true\n").unwrap();
    std::fs::create_dir(&backup).unwrap();

    let error = write_atomic_with_backup(&path, "new = true\n").unwrap_err();

    assert!(error.contains("cannot create config backup"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old = true\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn config_migration_rejects_stale_source_without_creating_journal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "compression_level = \"balanced\"\n").unwrap();
    assert!(
        write_atomic_config_migration_checked(
            &path,
            "compression_level = \"max\"\n",
            Some(b"stale")
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "compression_level = \"balanced\"\n"
    );
    let (backup, receipt) = config_migration_paths(&path).unwrap();
    assert!(!backup.exists());
    assert!(!receipt.exists());
}

#[test]
fn config_migration_rollback_restores_exact_source() {
    let tmp = unique_tmp("cfg_migration_rollback");
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("config.toml");
    let original = "# retained\nterse_agent = true\n";
    let migrated = "# retained\ncompression_level = \"max\"\n";
    std::fs::write(&path, original).unwrap();

    write_atomic_config_migration(&path, migrated).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), migrated);
    rollback_config_migration(&path).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(!tmp.join("config.toml.v4-migration.bak").exists());
    assert!(!tmp.join("config.toml.v4-migration.json").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn config_migration_rollback_refuses_changed_post_state() {
    let tmp = unique_tmp("cfg_migration_changed");
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("config.toml");
    std::fs::write(&path, "terse_agent = true\n").unwrap();
    write_atomic_config_migration(&path, "compression_level = \"max\"\n").unwrap();
    std::fs::write(&path, "compression_level = \"off\"\n").unwrap();

    let error = rollback_config_migration(&path).unwrap_err();

    assert!(error.contains("state changed"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "compression_level = \"off\"\n"
    );
    assert!(tmp.join("config.toml.v4-migration.bak").exists());
    assert!(tmp.join("config.toml.v4-migration.json").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn config_migration_rollback_cleans_prewrite_journal_state() {
    let tmp = unique_tmp("cfg_migration_prewrite");
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("config.toml");
    let original = "terse_agent = true\n";
    std::fs::write(&path, original).unwrap();
    write_atomic_config_migration(&path, "compression_level = \"max\"\n").unwrap();
    std::fs::write(&path, original).unwrap();

    rollback_config_migration(&path).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(!tmp.join("config.toml.v4-migration.bak").exists());
    assert!(!tmp.join("config.toml.v4-migration.json").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn config_migration_rollback_recovers_after_backup_cleanup_crash() {
    let tmp = unique_tmp("cfg_migration_cleanup_crash");
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("config.toml");
    let original = "terse_agent = true\n";
    std::fs::write(&path, original).unwrap();
    write_atomic_config_migration(&path, "compression_level = \"max\"\n").unwrap();
    std::fs::write(&path, original).unwrap();
    std::fs::remove_file(tmp.join("config.toml.v4-migration.bak")).unwrap();

    rollback_config_migration(&path).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(!tmp.join("config.toml.v4-migration.json").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn config_migration_recovers_matching_orphan_backup() {
    let tmp = unique_tmp("cfg_migration_orphan_backup");
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("config.toml");
    let original = "terse_agent = true\n";
    std::fs::write(&path, original).unwrap();
    std::fs::write(tmp.join("config.toml.v4-migration.bak"), original).unwrap();

    write_atomic_config_migration(&path, "compression_level = \"max\"\n").unwrap();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "compression_level = \"max\"\n"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

fn journal_receipt(path: &Path) -> ConfigMigrationReceipt {
    let (_, receipt) = config_migration_paths(path).unwrap();
    serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap()
}

/// #2175: a completed journal from an earlier release no longer blocks a
/// later migration step; the journal is extended so rollback still
/// restores the original pre-v4 bytes.
#[test]
fn completed_journal_is_extended_by_a_later_migration_step() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let original = "terse_agent = \"full\"\n[cloud]\ncontribute_enabled = true\n";
    let first = "compression_level = \"standard\"\n[cloud]\ncontribute_enabled = false\n";
    let second = "compression_level = \"standard\"\n";
    std::fs::write(&path, original).unwrap();
    write_atomic_config_migration(&path, first).unwrap();

    write_atomic_config_migration_checked(&path, second, Some(first.as_bytes())).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), second);
    let (backup, _) = config_migration_paths(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
    let receipt = journal_receipt(&path);
    assert_eq!(receipt.source_sha256, sha256_hex(original.as_bytes()));
    assert_eq!(receipt.output_sha256, sha256_hex(second.as_bytes()));
    rollback_config_migration(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

/// #2175 (Windows): the journal was written but the config replace did not
/// happen (e.g. the file was held open). The next run resumes.
#[test]
fn interrupted_migration_resumes_from_its_journal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let original = "terse_agent = \"full\"\n";
    let migrated = "compression_level = \"standard\"\n";
    std::fs::write(&path, original).unwrap();
    let (backup, receipt_path) = config_migration_paths(&path).unwrap();
    std::fs::write(&backup, original).unwrap();
    let stale = ConfigMigrationReceipt {
        version: CONFIG_MIGRATION_RECEIPT_VERSION,
        source_file: "config.toml".into(),
        backup_file: "config.toml.v4-migration.bak".into(),
        source_sha256: sha256_hex(original.as_bytes()),
        // An older release proposed different output.
        output_sha256: sha256_hex(b"compression_level = \"max\"\n"),
    };
    replace_config_migration_receipt(&receipt_path, &stale).unwrap();

    write_atomic_config_migration_checked(&path, migrated, Some(original.as_bytes())).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), migrated);
    assert_eq!(
        journal_receipt(&path).output_sha256,
        sha256_hex(migrated.as_bytes())
    );
    rollback_config_migration(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

/// Later normal saves changed the config, so the old journal cannot roll
/// back any more: it is archived byte-exact and a fresh journal opens.
#[test]
fn superseded_journal_is_archived_and_a_fresh_one_opens() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let original = "terse_agent = \"full\"\n";
    let edited = "compression_level = \"standard\"\noutput_density = \"terse\"\n";
    let migrated = "compression_level = \"standard\"\n";
    std::fs::write(&path, original).unwrap();
    write_atomic_config_migration(&path, "compression_level = \"standard\"\ntheme = \"a\"\n")
        .unwrap();
    let (backup, receipt_path) = config_migration_paths(&path).unwrap();
    let old_receipt = std::fs::read(&receipt_path).unwrap();
    std::fs::write(&path, edited).unwrap();

    write_atomic_config_migration_checked(&path, migrated, Some(edited.as_bytes())).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), migrated);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), edited);
    // The first journal archived beside a config is tagged as the original.
    let (archived_backup, archived_receipt) = archived_config_migration_paths(
        &backup,
        &sha256_hex(original.as_bytes()),
        &old_receipt,
        true,
    );
    assert_eq!(std::fs::read_to_string(&archived_backup).unwrap(), original);
    assert_eq!(std::fs::read(archived_receipt).unwrap(), old_receipt);
    assert_eq!(
        original_config_migration_backup(&path).map(|p| p.canonicalize().unwrap()),
        Some(archived_backup.canonicalize().unwrap())
    );
    let message = superseded_config_migration_message(&path, &archived_backup, &backup);
    let archived_name = archived_backup.file_name().unwrap().to_string_lossy();
    assert!(message.contains(archived_name.as_ref()), "{message}");
    assert!(
        message.contains("original pre-migration config"),
        "{message}"
    );
    rollback_config_migration(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    // A later archive is not the original; the original stays named.
    let later = "compression_level = \"lite\"\noutput_density = \"terse\"\n";
    write_atomic_config_migration(&path, migrated).unwrap();
    std::fs::write(&path, later).unwrap();
    write_atomic_config_migration_checked(&path, "compression_level = \"lite\"\n", None).unwrap();
    let archives = archived_config_migration_backups(&backup);
    assert_eq!(archives.len(), 2, "two archived journals");
    assert_eq!(archives.iter().filter(|a| a.original).count(), 1);
    assert_eq!(
        original_config_migration_backup(&path).map(|p| p.canonicalize().unwrap()),
        Some(archived_backup.canonicalize().unwrap())
    );
}

/// A crash between removing the receipt and the backup during archival
/// leaves an orphan backup the archive already holds: it is recognized.
#[test]
fn archived_orphan_backup_is_recognized_after_a_crash() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let original = "terse_agent = \"full\"\n";
    let edited = "output_density = \"terse\"\n";
    std::fs::write(&path, edited).unwrap();
    let (backup, _) = config_migration_paths(&path).unwrap();
    std::fs::write(&backup, original).unwrap();
    let (archived_backup, _) =
        archived_config_migration_paths(&backup, &sha256_hex(original.as_bytes()), &[], false);
    std::fs::write(&archived_backup, original).unwrap();

    write_atomic_config_migration(&path, "compression_level = \"lite\"\n").unwrap();

    assert_eq!(std::fs::read_to_string(&backup).unwrap(), edited);
    assert_eq!(std::fs::read_to_string(&archived_backup).unwrap(), original);
}

/// 3.11.x left an empty `config.toml.v4-migration.lock` beside the config;
/// the lock now lives in the data dir. The legacy file is never deleted:
/// an older process could still lock its inode (#2175 review).
#[test]
fn legacy_sibling_lock_is_left_alone() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "terse_agent = \"full\"\n").unwrap();
    let sibling = directory.path().join("config.toml.v4-migration.lock");
    assert_ne!(config_migration_lock_path(&path).unwrap(), sibling);
    std::fs::write(&sibling, "").unwrap();

    write_atomic_config_migration(&path, "compression_level = \"standard\"\n").unwrap();

    assert!(sibling.exists(), "the legacy lock file is kept");
    assert_eq!(std::fs::read(&sibling).unwrap(), b"");
}

fn with_journal_fault<T>(fault: JournalFault, run: impl FnOnce() -> T) -> T {
    JOURNAL_FAULT.with(|cell| cell.set(Some(fault)));
    let result = run();
    JOURNAL_FAULT.with(|cell| cell.set(None));
    result
}

const PRE_V4: &str = "terse_agent = \"full\"\n[cloud]\ncontribute_enabled = true\n";
const FIRST: &str = "compression_level = \"standard\"\n[cloud]\ncontribute_enabled = false\n";
const SECOND: &str = "compression_level = \"standard\"\n";
const THIRD: &str = "compression_level = \"lite\"\n";

fn completed_first_journal(directory: &Path) -> PathBuf {
    let path = directory.join("config.toml");
    std::fs::write(&path, PRE_V4).unwrap();
    write_atomic_config_migration(&path, FIRST).unwrap();
    path
}

fn assert_no_archives(path: &Path) {
    let (backup, _) = config_migration_paths(path).unwrap();
    assert!(
        archived_config_migration_backups(&backup).is_empty(),
        "the journal must not be archived"
    );
}

/// Review #1: a refused config replace while extending a completed
/// journal must leave the receipt describing the config that is on disk,
/// so the next run still classifies Completed and rollback still restores
/// the pre-v4 bytes.
#[test]
fn completed_extension_survives_a_refused_config_replace() {
    let directory = tempfile::tempdir().unwrap();
    let path = completed_first_journal(directory.path());
    let next = config_migration_next_receipt_path(&path).unwrap();

    let error = with_journal_fault(JournalFault::FailConfigReplace, || {
        write_atomic_config_migration_checked(&path, SECOND, Some(FIRST.as_bytes()))
    })
    .unwrap_err();

    assert!(error.contains("FailConfigReplace"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FIRST);
    assert!(!next.exists(), "the sidecar is removed again");
    assert_eq!(
        journal_receipt(&path).output_sha256,
        sha256_hex(FIRST.as_bytes())
    );
    assert_eq!(
        config_migration_journal_state(&path),
        ConfigMigrationJournalState::Completed
    );

    // The retry extends the same journal; rollback reaches pre-v4.
    write_atomic_config_migration_checked(&path, SECOND, Some(FIRST.as_bytes())).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SECOND);
    assert_no_archives(&path);
    rollback_config_migration(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PRE_V4);
}

/// Review #1: a crash after the sidecar receipt was written (config not
/// replaced yet) is settled by the next run without archiving.
#[test]
fn completed_extension_crash_before_replace_is_settled() {
    let directory = tempfile::tempdir().unwrap();
    let path = completed_first_journal(directory.path());
    let next = config_migration_next_receipt_path(&path).unwrap();

    with_journal_fault(JournalFault::CrashAfterNextReceipt, || {
        write_atomic_config_migration_checked(&path, SECOND, Some(FIRST.as_bytes()))
    })
    .unwrap_err();

    assert!(next.exists(), "the crash leaves the sidecar behind");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FIRST);
    assert_eq!(
        config_migration_journal_state(&path),
        ConfigMigrationJournalState::Completed
    );

    write_atomic_config_migration_checked(&path, SECOND, Some(FIRST.as_bytes())).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SECOND);
    assert!(!next.exists());
    assert_eq!(
        journal_receipt(&path).output_sha256,
        sha256_hex(SECOND.as_bytes())
    );
    assert_no_archives(&path);
    rollback_config_migration(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PRE_V4);
}

/// Review #1: a crash after the config replace but before the receipt
/// commit. The live receipt still names the previous output; the sidecar
/// names the config on disk and is promoted by rollback or the next step.
#[test]
fn completed_extension_crash_after_replace_keeps_the_pre_v4_rollback_target() {
    for settle_by_rollback in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let path = completed_first_journal(directory.path());
        let next = config_migration_next_receipt_path(&path).unwrap();

        with_journal_fault(JournalFault::CrashAfterConfigReplace, || {
            write_atomic_config_migration_checked(&path, SECOND, Some(FIRST.as_bytes()))
        })
        .unwrap_err();

        assert!(next.exists(), "the crash leaves the sidecar behind");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SECOND);
        assert_eq!(
            journal_receipt(&path).output_sha256,
            sha256_hex(FIRST.as_bytes()),
            "the live receipt was not committed yet"
        );
        assert_eq!(
            config_migration_journal_state(&path),
            ConfigMigrationJournalState::Completed
        );

        if !settle_by_rollback {
            // A later migration step extends the same journal.
            write_atomic_config_migration_checked(&path, THIRD, Some(SECOND.as_bytes())).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), THIRD);
            assert!(!next.exists());
            assert_no_archives(&path);
        }
        rollback_config_migration(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PRE_V4);
        assert!(!next.exists());
        assert_no_archives(&path);
    }
}

/// A sidecar that cannot be verified while the config matches neither
/// recorded state refuses with a stable class.
#[test]
fn unverifiable_receipt_update_refuses_with_a_stable_class() {
    let directory = tempfile::tempdir().unwrap();
    let path = completed_first_journal(directory.path());
    let next = config_migration_next_receipt_path(&path).unwrap();
    std::fs::write(&next, "{ not json").unwrap();
    std::fs::write(&path, SECOND).unwrap();

    assert_eq!(
        config_migration_journal_state(&path),
        ConfigMigrationJournalState::Refused(
            ConfigMigrationJournalRefusal::NextReceiptUnverifiable
        )
    );
    write_atomic_config_migration(&path, THIRD).unwrap_err();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SECOND);

    // Matching the live output again makes the sidecar droppable.
    std::fs::write(&path, FIRST).unwrap();
    write_atomic_config_migration(&path, SECOND).unwrap();
    assert!(!next.exists());
    rollback_config_migration(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PRE_V4);
}

#[test]
fn tampered_or_unverifiable_journal_still_refuses() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let original = "terse_agent = \"full\"\n";
    let migrated = "compression_level = \"standard\"\n";
    std::fs::write(&path, original).unwrap();
    write_atomic_config_migration(&path, migrated).unwrap();
    let (backup, receipt_path) = config_migration_paths(&path).unwrap();

    // Backup no longer matches the receipt.
    std::fs::write(&backup, "tampered = true\n").unwrap();
    assert_eq!(
        config_migration_journal_state(&path),
        ConfigMigrationJournalState::Refused(ConfigMigrationJournalRefusal::BackupMismatch)
    );
    let error = write_atomic_config_migration(&path, "theme = \"x\"\n").unwrap_err();
    assert!(error.contains("does not match its receipt"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), migrated);

    // Unparseable receipt.
    std::fs::write(&backup, original).unwrap();
    std::fs::write(&receipt_path, "{ not json").unwrap();
    let error = write_atomic_config_migration(&path, "theme = \"x\"\n").unwrap_err();
    assert!(error.contains("parse config migration receipt"), "{error}");

    // Orphan backup that is neither the config nor archived.
    std::fs::remove_file(&receipt_path).unwrap();
    assert_eq!(
        config_migration_journal_state(&path),
        ConfigMigrationJournalState::Refused(ConfigMigrationJournalRefusal::OrphanBackup)
    );
    let error = write_atomic_config_migration(&path, "theme = \"x\"\n").unwrap_err();
    assert!(error.contains("no receipt"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), migrated);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
}

/// `init --agent …` writes project files (`CLAUDE.md`, `.mcp.json`, rules)
/// through these writers; the lock must never land in the project tree,
/// where it showed up in every `git status`.
#[test]
fn writes_leave_no_lock_file_beside_the_target() {
    // Holds the env lock: the data dir (where the lock goes) stays fixed.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().unwrap();
    let file = project.path().join("CLAUDE.md");
    write_atomic_with_backup(&file, "first\n").unwrap();
    write_atomic_with_backup(&file, "second\n").unwrap();
    write_toml_preserving_minimal(&project.path().join("x.toml"), "a = 1\n", "").unwrap();
    let locks: Vec<String> = std::fs::read_dir(project.path())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.contains("migration.lock"))
        .collect();
    assert!(locks.is_empty(), "lock files in the project: {locks:?}");
    let lock = config_migration_lock_path(&file).unwrap();
    assert!(
        !lock.starts_with(project.path()),
        "lock lives in the state dir: {}",
        lock.display()
    );
    assert!(lock.exists(), "the lock still exists, in the state dir");
}
