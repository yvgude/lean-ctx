// SPDX-License-Identifier: Apache-2.0
//! The v4 config migration journal (#2175): receipt, backup, archive,
//! resume/extend/supersede classification and rollback.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    canonicalize_existing_prefix, resolve_write_target, write_atomic,
    write_toml_preserving_minimal_locked,
};

pub(super) const CONFIG_MIGRATION_RECEIPT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConfigMigrationReceipt {
    pub(super) version: u32,
    pub(super) source_file: String,
    pub(super) backup_file: String,
    pub(super) source_sha256: String,
    pub(super) output_sha256: String,
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
}

pub(super) fn config_migration_paths(path: &Path) -> Result<(PathBuf, PathBuf), String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "config path must end in a UTF-8 filename".to_string())?;
    Ok((
        path.with_file_name(format!("{filename}.v4-migration.bak")),
        path.with_file_name(format!("{filename}.v4-migration.json")),
    ))
}

pub(super) fn lock_config_migration(
    path: &Path,
) -> Result<crate::core::migration_lock::MigrationLock, String> {
    crate::core::migration_lock::acquire(&config_migration_lock_path(path)?)
}

/// Exclusive hold on the `.v4-migration.lock` for one config file, bound to
/// that file's canonical resolved path.
///
/// Existing lock, existing order: this reuses `lock_config_migration` and adds
/// no second authority. What LR-TEL-01 changed is the *span* — a holder can
/// keep the lock across a whole read-modify-write instead of only across the
/// byte write, so a load → mutate → save sequence is atomic against other
/// holders.
///
/// Scope, stated precisely: this guards holders of *this* lock file. It is not
/// a universal config-write authority — `write_toml_document` /
/// `load_toml_document` callers (`core::tool_profiles`,
/// `core::update_scheduler`) still read unlocked and take the lock only for
/// their write, and the same `write_atomic_with_backup` writer is used for many
/// non-config files.
///
/// Both fields are private on purpose: the lock cannot be separated from the
/// path it was taken for, so a guarded write cannot be aimed at a different
/// file than the one being excluded. The lock is **not reentrant** (an
/// exclusive `flock` on a fresh descriptor), so a holder must use the guard's
/// methods rather than the public, self-locking writers.
pub(crate) struct ConfigWriteGuard {
    /// Released on drop. Never handed out.
    _lock: crate::core::migration_lock::MigrationLock,
    /// Canonical resolved target, computed the same way the migration writer
    /// computes it, so both bind to one identity for a symlinked config.
    path: PathBuf,
}

impl ConfigWriteGuard {
    /// Takes the lock for `path` and binds to its canonical resolved target.
    pub(crate) fn acquire(path: &Path) -> Result<Self, String> {
        let resolved = canonicalize_existing_prefix(&resolve_write_target(path)?)?;
        let lock = lock_config_migration(&resolved)?;
        Ok(Self {
            _lock: lock,
            path: resolved,
        })
    }

    /// The canonical resolved path this guard excludes writes to. Read and
    /// write inside the critical section must both use this.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Minimal-preserving config write, bound to this guard's path.
    pub(crate) fn write_toml_preserving_minimal(
        &self,
        new_content: &str,
        default_content: &str,
    ) -> Result<(), String> {
        write_toml_preserving_minimal_locked(&self.path, new_content, default_content)
    }
}

/// The lock file for one config target. It lives in the lean-ctx state dir
/// (`<data>/locks/`), keyed by the canonical target path, never beside the
/// target: the same writer serves project files (`CLAUDE.md`, `.mcp.json` …),
/// and a sibling lock left a `*.v4-migration.lock` in every user's git status.
/// Only when no data dir resolves does it fall back to the sibling file.
pub(crate) fn config_migration_lock_path(path: &Path) -> Result<PathBuf, String> {
    let target = canonicalize_existing_prefix(&resolve_write_target(path)?)?;
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "config path must end in a UTF-8 filename".to_string())?;
    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let key = blake3::hash(target.to_string_lossy().as_bytes()).to_hex();
        return Ok(data_dir
            .join("locks")
            .join(format!("{}.{filename}.v4-migration.lock", &key[..32])));
    }
    Ok(target.with_file_name(format!("{filename}.v4-migration.lock")))
}

/// Atomically prepares a recoverable v3→v4 config journal before replacing the
/// config. A pre-existing journal is never silently overwritten: an
/// interrupted one is resumed, a completed one is extended (its backup stays
/// the rollback target), a superseded one is archived byte-exact under a
/// content-addressed name, and an unverifiable one refuses (#2175).
///
/// The empty `<config>.v4-migration.lock` that 3.11.x left beside the config
/// is deliberately left alone. A lock file cannot be deleted safely while an
/// older process may still open it: on Unix that process would lock the
/// unlinked inode while the next one creates and locks a fresh file, splitting
/// mutual exclusion. The empty file is harmless; the #2175 symptom was the
/// warning, not the file.
pub fn write_atomic_config_migration(path: &Path, content: &str) -> Result<(), String> {
    write_atomic_config_migration_checked(path, content, None)
}

pub fn write_atomic_config_migration_checked(
    path: &Path,
    content: &str,
    expected_source: Option<&[u8]>,
) -> Result<(), String> {
    let resolved = canonicalize_existing_prefix(&resolve_write_target(path)?)?;
    let path = resolved.as_path();
    let _lock = lock_config_migration(path)?;
    let source = std::fs::read(path).map_err(|error| format!("read config: {error}"))?;
    if expected_source.is_some_and(|expected| expected != source) {
        return Err("config changed before migration; refusing stale rewrite".into());
    }
    if source == content.as_bytes() {
        return Ok(());
    }
    let (backup, receipt_path) = config_migration_paths(path)?;
    let next_path = config_migration_next_receipt_path(path)?;
    let output_sha256 = sha256_hex(content.as_bytes());
    let (journal, fixes) =
        classify_config_migration_journal(path, &backup, &receipt_path, &next_path, &source)
            .map_err(|refusal| refusal.detail)?;
    apply_config_migration_journal_fixes(&backup, &receipt_path, &next_path, fixes)?;
    match journal {
        MigrationJournal::Absent => {}
        MigrationJournal::Interrupted(mut receipt) => {
            // The config still holds the journaled source and the backup holds
            // the same bytes: finish the replace. The receipt is updated first;
            // a crash or a refused replace leaves the config at the source, so
            // the next run classifies this same resumable state.
            std::str::from_utf8(&source).map_err(|_| "config is not UTF-8")?;
            if receipt.output_sha256 != output_sha256 {
                receipt.output_sha256 = output_sha256;
                replace_config_migration_receipt(&receipt_path, &receipt)?;
            }
            return write_atomic(path, content);
        }
        MigrationJournal::Completed(receipt) => {
            return extend_completed_config_migration(
                path,
                content,
                &receipt_path,
                &next_path,
                receipt,
                output_sha256,
            );
        }
        MigrationJournal::Superseded(receipt) => {
            // Normal saves changed the config after the journaled migration,
            // so that journal can no longer roll back (rollback refuses a
            // changed post-state). Archive it under its content address and
            // open a fresh journal for the current bytes below.
            let archived = archive_config_migration_journal(&backup, &receipt_path, &receipt)?;
            tracing::warn!(
                "{}",
                superseded_config_migration_message(path, &archived, &backup)
            );
        }
    }
    let source_file = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "invalid config filename".to_string())?;
    let backup_file = backup
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "invalid backup filename".to_string())?;
    let receipt = ConfigMigrationReceipt {
        version: CONFIG_MIGRATION_RECEIPT_VERSION,
        source_file: source_file.to_string(),
        backup_file: backup_file.to_string(),
        source_sha256: sha256_hex(&source),
        output_sha256,
    };
    std::str::from_utf8(&source).map_err(|_| "config is not UTF-8")?;
    crate::core::migration_lock::atomic_create_without_overwrite(&backup, &source)?;
    crate::core::migration_lock::atomic_create_without_overwrite(
        &receipt_path,
        config_migration_receipt_json(&receipt)?.as_bytes(),
    )?;
    write_atomic(path, content)?;
    Ok(())
}

/// Extends a completed journal to a later migration step in two phases, so
/// the live receipt never names bytes the config does not hold (#2175 review):
///
/// 1. the extended receipt is written beside the live one as
///    `<config>.v4-migration.next.json`;
/// 2. the config is replaced;
/// 3. the extended receipt replaces the live one and the sidecar goes.
///
/// A refused replace removes the sidecar again. A crash at any point is
/// settled by the next run (or by rollback): before step 2 the config still
/// equals the live receipt's output and the sidecar is dropped; after it the
/// config equals the sidecar's output and the sidecar is promoted. Either way
/// the journal stays Completed and its backup — the original pre-v4 bytes —
/// stays the rollback target, instead of looking Superseded and being archived.
pub(super) fn extend_completed_config_migration(
    path: &Path,
    content: &str,
    receipt_path: &Path,
    next_path: &Path,
    mut receipt: ConfigMigrationReceipt,
    output_sha256: String,
) -> Result<(), String> {
    receipt.output_sha256 = output_sha256;
    crate::core::migration_lock::atomic_create_without_overwrite(
        next_path,
        config_migration_receipt_json(&receipt)?.as_bytes(),
    )?;
    journal_fault(JournalFault::CrashAfterNextReceipt)?;
    let replaced =
        journal_fault(JournalFault::FailConfigReplace).and_then(|()| write_atomic(path, content));
    if let Err(error) = replaced {
        if let Err(cleanup) = std::fs::remove_file(next_path) {
            // The next run drops it: the config still equals the live output.
            tracing::debug!("config migration receipt update kept for the next run: {cleanup}");
        }
        return Err(error);
    }
    journal_fault(JournalFault::CrashAfterConfigReplace)?;
    if let Err(error) = promote_config_migration_next_receipt(receipt_path, next_path, &receipt) {
        // The config is migrated; the next run or rollback promotes the sidecar.
        tracing::debug!("config migration receipt update left for the next run: {error}");
    }
    Ok(())
}

/// Test-only fault points inside the two-phase journal extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JournalFault {
    /// Process dies after the sidecar receipt was written.
    CrashAfterNextReceipt,
    /// The config replace is refused (Windows: file held without share-delete).
    FailConfigReplace,
    /// Process dies after the config replace, before the receipt commit.
    CrashAfterConfigReplace,
}

#[cfg(test)]
thread_local! {
    pub(super) static JOURNAL_FAULT: std::cell::Cell<Option<JournalFault>> =
        const { std::cell::Cell::new(None) };
}

pub(super) fn journal_fault(point: JournalFault) -> Result<(), String> {
    #[cfg(test)]
    if JOURNAL_FAULT.with(std::cell::Cell::get) == Some(point) {
        return Err(format!("injected journal fault: {point:?}"));
    }
    #[cfg(not(test))]
    let _ = point;
    Ok(())
}

pub(super) fn config_migration_receipt_json(
    receipt: &ConfigMigrationReceipt,
) -> Result<String, String> {
    let json = serde_json::to_string_pretty(receipt)
        .map_err(|error| format!("serialize config migration receipt: {error}"))?;
    Ok(format!("{json}\n"))
}

/// Commits the sidecar receipt as the live receipt and removes the sidecar.
pub(super) fn promote_config_migration_next_receipt(
    receipt_path: &Path,
    next_path: &Path,
    next: &ConfigMigrationReceipt,
) -> Result<(), String> {
    replace_config_migration_receipt(receipt_path, next)?;
    std::fs::remove_file(next_path)
        .map_err(|error| format!("remove committed config migration receipt update: {error}"))
}

/// The sidecar of a journal extension in progress (see
/// [`extend_completed_config_migration`]).
pub(super) fn config_migration_next_receipt_path(path: &Path) -> Result<PathBuf, String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "config path must end in a UTF-8 filename".to_string())?;
    Ok(path.with_file_name(format!("{filename}.v4-migration.next.json")))
}

/// How an existing `.v4-migration` journal relates to the current config bytes.
///
/// A journal is the pair `<config>.v4-migration.bak` (pre-migration bytes) and
/// `<config>.v4-migration.json` (receipt with the SHA-256 of those bytes and of
/// the migrated output). It deliberately outlives a successful migration so
/// `lean-ctx migrate config --rollback` can restore the pre-v4 config. A later
/// release that migrates the same file again (e.g. 3.12 retiring the
/// `[cloud]` table that 3.11's migration wrote, #2175) must therefore classify
/// the journal instead of refusing forever.
pub(super) enum MigrationJournal {
    /// No journal: the migration opens a fresh one.
    Absent,
    /// Backup == source and the config still holds the source: a migration
    /// stopped after writing its journal but before replacing the config (a
    /// crash, or a replace refused because another process held the file).
    Interrupted(ConfigMigrationReceipt),
    /// Backup == source and the config is exactly the journaled output.
    Completed(ConfigMigrationReceipt),
    /// Backup == source, but normal saves changed the config since; the
    /// journal can no longer roll back and is archived before a new one opens.
    Superseded(ConfigMigrationReceipt),
}

/// Why a journal cannot be continued automatically. Stable: the labels feed
/// the once-per-state warning fingerprint and `lean-ctx doctor`, so they
/// never carry volatile text such as temporary file names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMigrationJournalRefusal {
    /// A journal file is a symbolic link.
    Symlink,
    /// A journal file exists but cannot be read.
    Unreadable,
    /// The receipt does not parse, has an unknown version or names other files.
    ReceiptUnverifiable,
    /// The backup bytes do not match the receipt's source hash.
    BackupMismatch,
    /// A backup without receipt that matches neither the config nor an archive.
    OrphanBackup,
    /// A receipt without backup whose source is not the current config.
    OrphanReceipt,
    /// An unfinished receipt update that cannot be verified while the config
    /// matches neither recorded state.
    NextReceiptUnverifiable,
}

impl ConfigMigrationJournalRefusal {
    pub fn label(self) -> &'static str {
        match self {
            Self::Symlink => "journal-symlink",
            Self::Unreadable => "journal-unreadable",
            Self::ReceiptUnverifiable => "receipt-unverifiable",
            Self::BackupMismatch => "backup-mismatch",
            Self::OrphanBackup => "orphan-backup",
            Self::OrphanReceipt => "orphan-receipt",
            Self::NextReceiptUnverifiable => "next-receipt-unverifiable",
        }
    }
}

/// Read-only view of the journal beside a config, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMigrationJournalState {
    Absent,
    Interrupted,
    Completed,
    Superseded,
    Refused(ConfigMigrationJournalRefusal),
}

impl ConfigMigrationJournalState {
    /// Whether `lean-ctx migrate config --rollback` can act in this state.
    pub fn rollback_available(self) -> bool {
        matches!(self, Self::Interrupted | Self::Completed)
    }
}

pub(super) struct JournalRefused {
    refusal: ConfigMigrationJournalRefusal,
    detail: String,
}

pub(super) fn refused(
    refusal: ConfigMigrationJournalRefusal,
    detail: impl Into<String>,
) -> JournalRefused {
    JournalRefused {
        refusal,
        detail: detail.into(),
    }
}

/// Recoverable leftovers the classification found; applied under the lock.
pub(super) enum JournalFix {
    RemoveBackup,
    RemoveReceipt,
    RemoveNextReceipt,
    PromoteNextReceipt(ConfigMigrationReceipt),
}

/// Reads and validates the receipt beside `path`; the bytes are returned for
/// byte-exact archival.
pub(super) fn read_config_migration_receipt(
    path: &Path,
    backup: &Path,
    receipt_path: &Path,
) -> Result<(ConfigMigrationReceipt, Vec<u8>), String> {
    let receipt_bytes = std::fs::read(receipt_path)
        .map_err(|error| format!("read config migration receipt: {error}"))?;
    let receipt: ConfigMigrationReceipt = serde_json::from_slice(&receipt_bytes)
        .map_err(|error| format!("parse config migration receipt: {error}"))?;
    if receipt.version != CONFIG_MIGRATION_RECEIPT_VERSION {
        return Err(format!(
            "unsupported config migration receipt version {}",
            receipt.version
        ));
    }
    let expected_source = path.file_name().and_then(|name| name.to_str());
    let expected_backup = backup.file_name().and_then(|name| name.to_str());
    if Some(receipt.source_file.as_str()) != expected_source
        || Some(receipt.backup_file.as_str()) != expected_backup
    {
        return Err("config migration receipt references unexpected files".to_string());
    }
    Ok((receipt, receipt_bytes))
}

/// An archived journal backup beside the config.
pub(super) struct ArchivedBackup {
    pub(super) path: PathBuf,
    pub(super) key: String,
    /// The first journal ever archived beside this config: its backup is the
    /// config from before the first recorded migration (the pre-v4 original).
    pub(super) original: bool,
}

/// Archived backups beside `backup`, sorted by name (deterministic).
pub(super) fn archived_config_migration_backups(backup: &Path) -> Vec<ArchivedBackup> {
    let (Some(parent), Some(backup_name)) = (
        backup.parent(),
        backup.file_name().and_then(|name| name.to_str()),
    ) else {
        return Vec::new();
    };
    let stem = backup_name.strip_suffix(".bak").unwrap_or(backup_name);
    let prefix = format!("{stem}.");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut archived: Vec<ArchivedBackup> = entries
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let rest = name.strip_prefix(&prefix)?.strip_suffix(".bak")?;
            let (original, key) = match rest.strip_prefix("original.") {
                Some(key) => (true, key),
                None => (false, rest),
            };
            (key.len() == 16 && key.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| {
                ArchivedBackup {
                    path: parent.join(&name),
                    key: key.to_string(),
                    original,
                }
            })
        })
        .collect();
    archived.sort_by(|left, right| left.path.cmp(&right.path));
    archived
}

/// Content-addressed archive names for a retired journal. The backup is keyed
/// by its own SHA-256 (= the receipt's `source_sha256`), the receipt by that
/// key plus its own digest, so re-archiving the same journal is idempotent and
/// two different journals never collide. The first journal archived beside a
/// config carries an `original.` tag: it holds the pre-v4 original. Names
/// carry no time or counter.
pub(super) fn archived_config_migration_paths(
    backup: &Path,
    source_sha256: &str,
    receipt_bytes: &[u8],
    original: bool,
) -> (PathBuf, PathBuf) {
    let key = &source_sha256[..source_sha256.len().min(16)];
    let tag = if original {
        format!("original.{key}")
    } else {
        key.to_string()
    };
    let receipt_sha256 = sha256_hex(receipt_bytes);
    let backup_name = backup
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml.v4-migration.bak");
    let stem = backup_name.strip_suffix(".bak").unwrap_or(backup_name);
    (
        backup.with_file_name(format!("{stem}.{tag}.bak")),
        backup.with_file_name(format!("{stem}.{tag}.{}.json", &receipt_sha256[..8])),
    )
}

/// Creates `target` with `bytes`, or accepts an existing byte-identical file.
pub(super) fn create_or_verify(target: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() {
            return Err(format!("{} must not be a symlink", target.display()));
        }
        let existing =
            std::fs::read(target).map_err(|error| format!("read {}: {error}", target.display()))?;
        return if existing == bytes {
            Ok(())
        } else {
            Err(format!(
                "{} exists with different content",
                target.display()
            ))
        };
    }
    crate::core::migration_lock::atomic_create_without_overwrite(target, bytes)
}

/// Moves a superseded journal aside, byte-exact, under its content address,
/// and returns the archived backup. Order matters for crash safety: the
/// archive copies exist before anything is removed, and the receipt goes
/// before the backup, so an interruption leaves either the full journal or an
/// orphan backup that [`classify_config_migration_journal`] recognizes as
/// already archived. Re-archiving after a crash reuses the `original.` tag the
/// first attempt chose, because the archive with this key already exists.
pub(super) fn archive_config_migration_journal(
    backup: &Path,
    receipt_path: &Path,
    receipt: &ConfigMigrationReceipt,
) -> Result<PathBuf, String> {
    let backup_bytes =
        std::fs::read(backup).map_err(|error| format!("read config migration backup: {error}"))?;
    let receipt_bytes = std::fs::read(receipt_path)
        .map_err(|error| format!("read config migration receipt: {error}"))?;
    let key = &receipt.source_sha256[..receipt.source_sha256.len().min(16)];
    let existing = archived_config_migration_backups(backup);
    let original = existing
        .iter()
        .find(|archived| archived.key == key)
        .map_or(existing.is_empty(), |archived| archived.original);
    let (archived_backup, archived_receipt) =
        archived_config_migration_paths(backup, &receipt.source_sha256, &receipt_bytes, original);
    create_or_verify(&archived_backup, &backup_bytes)?;
    create_or_verify(&archived_receipt, &receipt_bytes)?;
    std::fs::remove_file(receipt_path)
        .map_err(|error| format!("retire config migration receipt: {error}"))?;
    std::fs::remove_file(backup)
        .map_err(|error| format!("retire config migration backup: {error}"))?;
    Ok(archived_backup)
}

/// One-time notice when a superseded journal was archived; names the archive
/// that keeps the original pre-v4 config.
pub(super) fn superseded_config_migration_message(
    path: &Path,
    archived: &Path,
    backup: &Path,
) -> String {
    let mut message = format!(
        "config migration journal for {} no longer matched the config (it was edited after \
         the recorded migration) and was archived as {}; `lean-ctx migrate config --rollback` \
         now undoes only the newest migration.",
        path.display(),
        archived.display()
    );
    if let Some(original) = original_backup_beside(backup) {
        message.push_str(&original_config_sentence(&original));
    }
    message
}

/// The sentence every journal message uses to name the original pre-v4 config.
pub fn original_config_sentence(original: &Path) -> String {
    format!(
        " The original pre-migration config (from before the first recorded migration) is kept \
         in {}.",
        original.display()
    )
}

pub(super) fn original_backup_beside(backup: &Path) -> Option<PathBuf> {
    archived_config_migration_backups(backup)
        .into_iter()
        .find(|archived| archived.original)
        .map(|archived| archived.path)
}

/// The archive that keeps the config from before the first recorded
/// migration (the pre-v4 original), when a journal was ever archived beside
/// `path`. `None` while the live journal is still the first one: then its
/// backup is the original and `migrate config --rollback` restores it.
pub fn original_config_migration_backup(path: &Path) -> Option<PathBuf> {
    let resolved = canonicalize_existing_prefix(&resolve_write_target(path).ok()?).ok()?;
    let (backup, _) = config_migration_paths(&resolved).ok()?;
    original_backup_beside(&backup)
}

/// Atomically replaces the live receipt (temp file + rename).
pub(super) fn replace_config_migration_receipt(
    receipt_path: &Path,
    receipt: &ConfigMigrationReceipt,
) -> Result<(), String> {
    write_atomic(receipt_path, &config_migration_receipt_json(receipt)?)
}

/// Decides what to do with a `.next.json` sidecar left by an interrupted
/// journal extension, given the live receipt and the current config hash.
pub(super) fn settle_config_migration_next_receipt(
    path: &Path,
    backup: &Path,
    next_path: &Path,
    receipt: &ConfigMigrationReceipt,
    current_sha256: &str,
) -> Result<JournalFix, JournalRefused> {
    let next = read_config_migration_receipt(path, backup, next_path)
        .ok()
        .map(|(next, _)| next)
        .filter(|next| next.source_sha256 == receipt.source_sha256);
    match next {
        // The replace happened, the receipt commit did not.
        Some(next)
            if current_sha256 == next.output_sha256 && current_sha256 != receipt.output_sha256 =>
        {
            Ok(JournalFix::PromoteNextReceipt(next))
        }
        // The replace never happened (or later saves changed the config;
        // then the journal is superseded either way).
        Some(_) => Ok(JournalFix::RemoveNextReceipt),
        None if current_sha256 == receipt.source_sha256
            || current_sha256 == receipt.output_sha256 =>
        {
            Ok(JournalFix::RemoveNextReceipt)
        }
        None => Err(refused(
            ConfigMigrationJournalRefusal::NextReceiptUnverifiable,
            "unfinished config migration receipt update cannot be verified",
        )),
    }
}

/// Classifies the journal beside `path` against the current config bytes,
/// without touching the disk. Recoverable leftovers of an interrupted
/// rollback, archival or extension come back as fixes for the caller to apply
/// under the lock; anything that cannot be verified is a refusal (the
/// migration pauses).
pub(super) fn classify_config_migration_journal(
    path: &Path,
    backup: &Path,
    receipt_path: &Path,
    next_path: &Path,
    current: &[u8],
) -> Result<(MigrationJournal, Vec<JournalFix>), JournalRefused> {
    use ConfigMigrationJournalRefusal as Refusal;
    for artifact in [backup, receipt_path, next_path] {
        if std::fs::symlink_metadata(artifact).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(refused(
                Refusal::Symlink,
                "config migration journal must not be a symlink",
            ));
        }
    }
    let has_backup = std::fs::symlink_metadata(backup).is_ok();
    let has_receipt = std::fs::symlink_metadata(receipt_path).is_ok();
    let has_next = std::fs::symlink_metadata(next_path).is_ok();
    let current_sha256 = sha256_hex(current);
    let mut fixes = Vec::new();
    let journal = match (has_backup, has_receipt) {
        (false, false) => MigrationJournal::Absent,
        (true, false) => {
            let orphan = std::fs::read(backup).map_err(|error| {
                refused(
                    Refusal::Unreadable,
                    format!("read orphan config migration backup: {error}"),
                )
            })?;
            // Either the pre-journal step of a migration (backup == config) or
            // the last step of an archival (the archive holds these bytes).
            let orphan_sha256 = sha256_hex(&orphan);
            let already_archived =
                archived_config_migration_backups(backup)
                    .iter()
                    .any(|archived| {
                        orphan_sha256.starts_with(&archived.key)
                            && std::fs::read(&archived.path).is_ok_and(|bytes| bytes == orphan)
                    });
            if orphan != current && !already_archived {
                return Err(refused(
                    Refusal::OrphanBackup,
                    "config migration backup has no receipt and does not match the config",
                ));
            }
            fixes.push(JournalFix::RemoveBackup);
            MigrationJournal::Absent
        }
        (false, true) => {
            // A rollback that restored the config and removed the backup, then
            // stopped before removing the receipt.
            let (receipt, _) = read_config_migration_receipt(path, backup, receipt_path)
                .map_err(|detail| refused(Refusal::ReceiptUnverifiable, detail))?;
            if current_sha256 != receipt.source_sha256 {
                return Err(refused(
                    Refusal::OrphanReceipt,
                    "config migration receipt has no backup and the config is not its source",
                ));
            }
            fixes.push(JournalFix::RemoveReceipt);
            MigrationJournal::Absent
        }
        (true, true) => {
            let (mut receipt, _) = read_config_migration_receipt(path, backup, receipt_path)
                .map_err(|detail| refused(Refusal::ReceiptUnverifiable, detail))?;
            let backup_bytes = std::fs::read(backup).map_err(|error| {
                refused(
                    Refusal::Unreadable,
                    format!("read config migration backup: {error}"),
                )
            })?;
            if sha256_hex(&backup_bytes) != receipt.source_sha256 {
                return Err(refused(
                    Refusal::BackupMismatch,
                    "config migration backup does not match its receipt",
                ));
            }
            if has_next {
                let fix = settle_config_migration_next_receipt(
                    path,
                    backup,
                    next_path,
                    &receipt,
                    &current_sha256,
                )?;
                if let JournalFix::PromoteNextReceipt(next) = &fix {
                    receipt.output_sha256.clone_from(&next.output_sha256);
                }
                fixes.push(fix);
            }
            if current_sha256 == receipt.source_sha256 {
                MigrationJournal::Interrupted(receipt)
            } else if current_sha256 == receipt.output_sha256 {
                MigrationJournal::Completed(receipt)
            } else {
                MigrationJournal::Superseded(receipt)
            }
        }
    };
    // A sidecar without a live journal holds only a hash, never config bytes.
    if has_next && !(has_backup && has_receipt) {
        fixes.push(JournalFix::RemoveNextReceipt);
    }
    Ok((journal, fixes))
}

pub(super) fn apply_config_migration_journal_fixes(
    backup: &Path,
    receipt_path: &Path,
    next_path: &Path,
    fixes: Vec<JournalFix>,
) -> Result<(), String> {
    for fix in fixes {
        match fix {
            JournalFix::RemoveBackup => std::fs::remove_file(backup)
                .map_err(|error| format!("remove recoverable config migration backup: {error}"))?,
            JournalFix::RemoveReceipt => std::fs::remove_file(receipt_path)
                .map_err(|error| format!("remove completed config migration receipt: {error}"))?,
            JournalFix::RemoveNextReceipt => std::fs::remove_file(next_path).map_err(|error| {
                format!("remove stale config migration receipt update: {error}")
            })?,
            JournalFix::PromoteNextReceipt(next) => {
                promote_config_migration_next_receipt(receipt_path, next_path, &next)?;
            }
        }
    }
    Ok(())
}

/// Read-only classification of the journal beside `path` (no lock, no
/// cleanup), for warnings and `lean-ctx doctor`.
pub fn config_migration_journal_state(path: &Path) -> ConfigMigrationJournalState {
    let Some(resolved) = resolve_write_target(path)
        .ok()
        .and_then(|target| canonicalize_existing_prefix(&target).ok())
    else {
        return ConfigMigrationJournalState::Absent;
    };
    let (Ok((backup, receipt_path)), Ok(next_path)) = (
        config_migration_paths(&resolved),
        config_migration_next_receipt_path(&resolved),
    ) else {
        return ConfigMigrationJournalState::Absent;
    };
    let current = std::fs::read(&resolved).unwrap_or_default();
    match classify_config_migration_journal(&resolved, &backup, &receipt_path, &next_path, &current)
    {
        Ok((MigrationJournal::Absent, _)) => ConfigMigrationJournalState::Absent,
        Ok((MigrationJournal::Interrupted(_), _)) => ConfigMigrationJournalState::Interrupted,
        Ok((MigrationJournal::Completed(_), _)) => ConfigMigrationJournalState::Completed,
        Ok((MigrationJournal::Superseded(_), _)) => ConfigMigrationJournalState::Superseded,
        Err(refusal) => ConfigMigrationJournalState::Refused(refusal.refusal),
    }
}

/// Paths of the live journal beside `path` (for messages), after resolving
/// the config's write target the same way the writer does.
pub fn config_migration_journal_files(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let resolved = config_migration_target(path)?;
    config_migration_paths(&resolved).ok()
}

/// The sidecar of an unfinished journal extension beside `path`.
pub fn config_migration_next_receipt_file(path: &Path) -> Option<PathBuf> {
    config_migration_next_receipt_path(&config_migration_target(path)?).ok()
}

/// The canonical write target the migration journal is bound to.
pub fn config_migration_target(path: &Path) -> Option<PathBuf> {
    canonicalize_existing_prefix(&resolve_write_target(path).ok()?).ok()
}

/// Restores exact pre-migration bytes only while both journal and current
/// post-state still match their recorded SHA-256 identities. After a journal
/// was extended by a later release's migration step (#2175) this still
/// restores the original pre-v4 bytes; an unfinished extension
/// (`.v4-migration.next.json`) is settled first. Archived journals
/// (`*.v4-migration.[original.]<sha16>.{bak,json}`) are kept for manual
/// recovery only; they live beside the config, so `lean-ctx uninstall`
/// removes them together with the config directory and the live journal.
pub fn rollback_config_migration(path: &Path) -> Result<(), String> {
    let resolved = canonicalize_existing_prefix(&resolve_write_target(path)?)?;
    let path = resolved.as_path();
    let _lock = lock_config_migration(path)?;
    let (backup, receipt_path) = config_migration_paths(path)?;
    let next_path = config_migration_next_receipt_path(path)?;
    if std::fs::symlink_metadata(&receipt_path)
        .map_err(|error| format!("stat config migration receipt: {error}"))?
        .file_type()
        .is_symlink()
    {
        return Err("config migration rollback refuses symlinks".to_string());
    }
    let (mut receipt, _) = read_config_migration_receipt(path, &backup, &receipt_path)?;
    if std::fs::symlink_metadata(path)
        .map_err(|error| format!("stat config: {error}"))?
        .file_type()
        .is_symlink()
        || [&backup, &next_path].iter().any(|artifact| {
            std::fs::symlink_metadata(artifact)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        })
    {
        return Err("config migration rollback refuses symlinks".to_string());
    }
    let current = std::fs::read(path).map_err(|error| format!("read config: {error}"))?;
    if std::fs::symlink_metadata(&next_path).is_ok() {
        let fix = settle_config_migration_next_receipt(
            path,
            &backup,
            &next_path,
            &receipt,
            &sha256_hex(&current),
        )
        .map_err(|refusal| refusal.detail)?;
        if let JournalFix::PromoteNextReceipt(next) = &fix {
            receipt.output_sha256.clone_from(&next.output_sha256);
        }
        apply_config_migration_journal_fixes(&backup, &receipt_path, &next_path, vec![fix])?;
    }
    if !backup.exists() {
        if sha256_hex(&current) != receipt.source_sha256 {
            return Err("config migration backup missing before rollback completed".to_string());
        }
        std::fs::remove_file(&receipt_path)
            .map_err(|error| format!("remove completed config migration receipt: {error}"))?;
        return Ok(());
    }
    let source =
        std::fs::read(&backup).map_err(|error| format!("read config migration backup: {error}"))?;
    let current_sha256 = sha256_hex(&current);
    if sha256_hex(&source) != receipt.source_sha256 {
        return Err("config migration state changed; refusing rollback".to_string());
    }
    if current_sha256 == receipt.output_sha256 {
        let source = std::str::from_utf8(&source).map_err(|_| "config backup is not UTF-8")?;
        write_atomic(path, source)?;
    } else if current_sha256 != receipt.source_sha256 {
        return Err("config migration state changed; refusing rollback".to_string());
    }
    std::fs::remove_file(&backup)
        .map_err(|error| format!("remove config migration backup: {error}"))?;
    std::fs::remove_file(&receipt_path)
        .map_err(|error| format!("remove config migration receipt: {error}"))?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync config directory after rollback: {error}"))?;
    }
    Ok(())
}
