// SPDX-License-Identifier: Apache-2.0
use std::fs::File;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::{
    BinaryReceipt, CURRENT_VERSION, PreparedTransaction, UPDATE_RECEIPT_SCHEMA,
    UPDATE_TRANSACTION_FILE, UPDATE_TRANSACTION_SCHEMA, UpdateReceipt, VerifiedArtifact,
    atomic_write_bytes, canonical_current_exe, canonical_state_dir, canonical_update_paths,
    constant_time_eq, ensure_no_symlink_under, load_update_receipt_for, previous_binary_path,
    receipt_digest, replace_staged_binary, sha256_hex, sign_staged_binary, staged_update_path,
    update_layout, update_lock_path, update_transaction_path, validate_receipt,
    validate_receipt_paths,
};

pub(super) struct UpdateLock {
    file: File,
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(super) fn acquire_update_lock() -> Result<UpdateLock, String> {
    let path = update_lock_path()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open updater lock {}: {e}", path.display()))?;
    file.try_lock_exclusive()
        .map_err(|e| format!("another updater transaction is active ({e})"))?;
    Ok(UpdateLock { file })
}

pub(super) fn write_update_receipt(path: &Path, receipt: &UpdateReceipt) -> Result<(), String> {
    let mut sealed = receipt.clone();
    sealed.schema_version = UPDATE_RECEIPT_SCHEMA.to_string();
    sealed.receipt_sha256 = None;
    validate_receipt(&sealed)?;
    sealed.receipt_sha256 = Some(receipt_digest(&sealed)?);
    let bytes = serde_json::to_vec_pretty(&sealed).map_err(|e| e.to_string())?;
    atomic_write_bytes(path, &[bytes.as_slice(), b"\n"].concat())
}

pub(super) fn transaction_digest(transaction: &PreparedTransaction) -> Result<String, String> {
    let mut unsigned = transaction.clone();
    unsigned.transaction_sha256 = None;
    let bytes = serde_json::to_vec(&unsigned).map_err(|e| e.to_string())?;
    Ok(sha256_hex(&bytes))
}

pub(super) fn validate_prepared_transaction(
    transaction: &PreparedTransaction,
    state_dir: &Path,
    current_exe: &Path,
) -> Result<(), String> {
    if transaction.schema_version != UPDATE_TRANSACTION_SCHEMA {
        return Err(format!(
            "unsupported update transaction schema `{}`",
            transaction.schema_version
        ));
    }
    if transaction.operation != "update" && transaction.operation != "rollback" {
        return Err(format!(
            "unsupported update transaction operation `{}`",
            transaction.operation
        ));
    }
    validate_receipt(&UpdateReceipt {
        schema_version: UPDATE_RECEIPT_SCHEMA.to_string(),
        active: transaction.target_active.clone(),
        previous: transaction.target_previous.clone(),
        receipt_sha256: None,
    })?;
    let current = canonical_current_exe(current_exe)?;
    let previous = previous_binary_path(state_dir, &current)?;
    let staged = staged_update_path(&transaction.operation, false)?;
    let backup = staged_update_path(&transaction.operation, true)?;
    if Path::new(&transaction.current_path) != current
        || Path::new(&transaction.old_active.path) != current
        || Path::new(&transaction.target_active.path) != current
        || Path::new(&transaction.previous_path) != previous
        || Path::new(&transaction.target_previous.path) != previous
        || Path::new(&transaction.staged_path) != staged
        || Path::new(&transaction.backup_path) != backup
    {
        return Err("prepared transaction contains a non-canonical path".to_string());
    }
    ensure_no_symlink_under(state_dir, &staged)?;
    ensure_no_symlink_under(state_dir, &backup)?;
    let digest = transaction
        .transaction_sha256
        .as_deref()
        .ok_or_else(|| "prepared transaction is missing its integrity digest".to_string())?;
    if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("prepared transaction has an invalid integrity digest".to_string());
    }
    let expected = transaction_digest(transaction)?;
    if !constant_time_eq(digest.as_bytes(), expected.as_bytes()) {
        return Err("prepared transaction integrity digest mismatch".to_string());
    }
    Ok(())
}

/// Seals `transaction` with its integrity digest, persists it, and returns the
/// sealed copy. Callers must execute the returned value: the unsealed input
/// has no digest and `validate_prepared_transaction` rejects it.
pub(super) fn write_prepared_transaction(
    transaction: &PreparedTransaction,
    current_exe: &Path,
) -> Result<PreparedTransaction, String> {
    let state = canonical_state_dir()?;
    update_layout(&state)?;
    let mut sealed = transaction.clone();
    sealed.schema_version = UPDATE_TRANSACTION_SCHEMA.to_string();
    sealed.transaction_sha256 = None;
    validate_prepared_transaction(
        &PreparedTransaction {
            transaction_sha256: Some("0".repeat(64)),
            ..sealed.clone()
        },
        &state,
        current_exe,
    )
    .or_else(|error| {
        if error.contains("integrity digest mismatch") {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    sealed.transaction_sha256 = Some(transaction_digest(&sealed)?);
    let bytes = serde_json::to_vec_pretty(&sealed).map_err(|e| e.to_string())?;
    let path = update_transaction_path()?;
    atomic_write_bytes(&path, &[bytes.as_slice(), b"\n"].concat())?;
    Ok(sealed)
}

pub(super) fn load_prepared_transaction(
    current_exe: &Path,
) -> Result<Option<PreparedTransaction>, String> {
    let path = update_transaction_path()?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let transaction: PreparedTransaction = serde_json::from_str(&raw)
        .map_err(|e| format!("invalid prepared transaction {}: {e}", path.display()))?;
    let state = canonical_state_dir()?;
    validate_prepared_transaction(&transaction, &state, current_exe)?;
    Ok(Some(transaction))
}

pub(super) fn cleanup_prepared_transaction(
    transaction: &PreparedTransaction,
) -> Result<(), String> {
    let transaction_path = update_transaction_path()?;
    for path in [
        transaction_path,
        PathBuf::from(&transaction.staged_path),
        PathBuf::from(&transaction.backup_path),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "cannot remove update state {}: {e}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn orphan_prepared_paths(state_dir: &Path) -> Vec<PathBuf> {
    let staged_dir = state_dir.join("updates").join("staged");
    let mut paths = Vec::with_capacity(9);
    for operation in ["update", "rollback"] {
        for suffix in ["target", "backup"] {
            let path = staged_dir.join(format!("{operation}-{suffix}.bin"));
            paths.push(path.clone());
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                paths.push(staged_dir.join(format!(".{name}.tmp")));
            }
        }
    }
    paths.push(state_dir.join(format!(".{UPDATE_TRANSACTION_FILE}.tmp")));
    paths
}

pub(super) fn inspect_orphan_path(path: &Path, current: &[u8]) -> Result<bool, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "cannot inspect orphaned update state {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "orphaned update state is a symlink: {}",
            path.display()
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(format!(
            "orphaned update state is not a regular file: {}",
            path.display()
        ));
    }
    let is_backup = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("-backup.bin"));
    if is_backup {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("read orphaned backup {}: {error}", path.display()))?;
        if bytes != current {
            return Err(format!(
                "orphaned backup does not match the active binary: {}",
                path.display()
            ));
        }
    }
    Ok(true)
}

pub(super) fn cleanup_orphaned_prepared_files(
    state_dir: &Path,
    current: &[u8],
) -> Result<bool, String> {
    let mut present = Vec::new();
    for path in orphan_prepared_paths(state_dir) {
        if inspect_orphan_path(&path, current)? {
            present.push(path);
        }
    }
    if present.is_empty() {
        return Ok(false);
    }
    for path in &present {
        std::fs::remove_file(path)
            .map_err(|error| format!("remove orphaned update state {}: {error}", path.display()))?;
    }
    #[cfg(unix)]
    std::fs::File::open(state_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync update state directory after orphan cleanup: {error}"))?;
    Ok(true)
}

pub(super) fn binary_matches(bytes: &[u8], receipt: &BinaryReceipt) -> bool {
    !bytes.is_empty()
        && bytes.len() as u64 == receipt.size
        && constant_time_eq(sha256_hex(bytes).as_bytes(), receipt.sha256.as_bytes())
}

pub(super) fn prepare_update_transaction(
    current_exe: &Path,
    asset_name: &str,
    target_version: &str,
    verified: &VerifiedArtifact,
    binary: &[u8],
) -> Result<PreparedTransaction, String> {
    let (_state, _receipt_path, previous_path) = canonical_update_paths(current_exe)?;
    let current_path = canonical_current_exe(current_exe)?;
    let current = std::fs::read(&current_path)
        .map_err(|e| format!("cannot read current binary {}: {e}", current_exe.display()))?;
    if current.is_empty() {
        return Err("current binary is empty".to_string());
    }
    let current_sha = sha256_hex(&current);
    let mut existing = load_update_receipt_for(current_exe)?;
    if let Some(receipt) = &existing {
        if receipt.active.size != current.len() as u64
            || !constant_time_eq(receipt.active.sha256.as_bytes(), current_sha.as_bytes())
        {
            if !receipt_outdated_by_external_install(receipt) {
                return Err(
                    "current binary differs from the active receipt; refusing update".to_string(),
                );
            }
            // Reinstalled from outside the updater: start a new receipt chain
            // from the running binary instead of refusing forever.
            existing = None;
        }
    }
    let old_active = existing
        .as_ref()
        .map(|receipt| receipt.active.clone())
        .unwrap_or_else(|| BinaryReceipt {
            version: CURRENT_VERSION.to_string(),
            asset: current_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("lean-ctx")
                .to_string(),
            sha256: current_sha,
            size: current.len() as u64,
            path: current_path.to_string_lossy().into_owned(),
            manifest_sha256: None,
            archive_sha256: None,
            release_commit: None,
        });
    let target_previous = BinaryReceipt {
        path: previous_path.to_string_lossy().into_owned(),
        ..old_active.clone()
    };
    let staged_path = staged_update_path("update", false)?;
    let backup_path = staged_update_path("update", true)?;
    for path in [&staged_path, &backup_path] {
        if path.exists() {
            return Err(format!(
                "stale prepared update file exists: {}",
                path.display()
            ));
        }
    }
    atomic_write_bytes(&staged_path, binary)?;
    // The verified release binary, signed for this machine where the platform
    // needs it: the receipt records exactly the bytes that will be installed,
    // while the manifest and archive digests keep the release provenance.
    let installed = sign_staged_binary(&staged_path, binary)?;
    atomic_write_bytes(&backup_path, &current)?;
    let target_active = BinaryReceipt {
        version: target_version.to_string(),
        asset: asset_name.to_string(),
        sha256: sha256_hex(&installed),
        size: installed.len() as u64,
        path: current_path.to_string_lossy().into_owned(),
        manifest_sha256: Some(verified.manifest_sha256.clone()),
        archive_sha256: Some(verified.archive_sha256.clone()),
        release_commit: Some(verified.release_commit.clone()),
    };
    let transaction = PreparedTransaction {
        schema_version: UPDATE_TRANSACTION_SCHEMA.to_string(),
        operation: "update".to_string(),
        current_path: current_path.to_string_lossy().into_owned(),
        previous_path: previous_path.to_string_lossy().into_owned(),
        staged_path: staged_path.to_string_lossy().into_owned(),
        backup_path: backup_path.to_string_lossy().into_owned(),
        old_active,
        target_active,
        target_previous,
        transaction_sha256: None,
    };
    write_prepared_transaction(&transaction, &current_path)
}

pub(super) fn commit_prepared_transaction(
    transaction: &PreparedTransaction,
    current_exe: &Path,
) -> Result<(), String> {
    let (state, receipt_path, previous_path) = canonical_update_paths(current_exe)?;
    validate_prepared_transaction(transaction, &state, current_exe)?;
    let current = std::fs::read(current_exe)
        .map_err(|e| format!("cannot read active binary after swap: {e}"))?;
    if !binary_matches(&current, &transaction.target_active) {
        return Err("active binary does not match prepared target".to_string());
    }
    let backup = std::fs::read(&transaction.backup_path)
        .map_err(|e| format!("cannot read prepared rollback backup: {e}"))?;
    if !binary_matches(&backup, &transaction.old_active) {
        return Err("prepared rollback backup failed verification".to_string());
    }
    atomic_write_bytes(&previous_path, &backup)?;
    let receipt = UpdateReceipt {
        schema_version: UPDATE_RECEIPT_SCHEMA.to_string(),
        active: transaction.target_active.clone(),
        previous: transaction.target_previous.clone(),
        receipt_sha256: None,
    };
    validate_receipt_paths(&receipt, &state, current_exe)?;
    write_update_receipt(&receipt_path, &receipt)?;
    let cleanup = cleanup_prepared_transaction(transaction);
    // Succeeds when a helper finished the swap; a process still running from
    // the sidecar cannot delete it, and the next swap reclaims it (#2048).
    #[cfg(windows)]
    if cleanup.is_ok() {
        let _ = std::fs::remove_file(super::windows_sidecar::sidecar_path(current_exe));
    }
    cleanup
}

pub(super) fn execute_prepared_transaction(
    transaction: &PreparedTransaction,
    current_exe: &Path,
) -> Result<(), String> {
    let state = canonical_state_dir()?;
    validate_prepared_transaction(transaction, &state, current_exe)?;
    let current = std::fs::read(current_exe)
        .map_err(|e| format!("cannot read active binary before swap: {e}"))?;
    if !binary_matches(&current, &transaction.old_active) {
        return Err("active binary changed after transaction preparation".to_string());
    }
    let staged = Path::new(&transaction.staged_path);
    let target = std::fs::read(staged).map_err(|e| format!("cannot read staged binary: {e}"))?;
    if !binary_matches(&target, &transaction.target_active) {
        return Err("staged binary failed prepared-target verification".to_string());
    }
    replace_staged_binary(staged, current_exe)?;
    commit_prepared_transaction(transaction, current_exe)
}

pub(super) fn recover_pending_transaction(current_exe: &Path) -> Result<bool, String> {
    let Some(transaction) = load_prepared_transaction(current_exe)? else {
        let state = canonical_state_dir()?;
        update_layout(&state)?;
        let current_path = canonical_current_exe(current_exe)?;
        let current = std::fs::read(&current_path)
            .map_err(|e| format!("cannot read active binary during recovery: {e}"))?;
        if current.is_empty() {
            return Err("active binary is empty; refusing orphan cleanup".to_string());
        }
        cleanup_orphaned_prepared_files(&state, &current)?;
        return Ok(false);
    };
    let backup = std::fs::read(&transaction.backup_path)
        .map_err(|e| format!("cannot read prepared rollback backup: {e}"))?;
    if !binary_matches(&backup, &transaction.old_active) {
        return Err("prepared rollback backup failed verification".to_string());
    }
    let current = match std::fs::read(current_exe) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot inspect active binary during recovery: {e}")),
    };
    if current
        .as_deref()
        .is_some_and(|bytes| binary_matches(bytes, &transaction.target_active))
    {
        commit_prepared_transaction(&transaction, current_exe)?;
        return Ok(true);
    }
    if current
        .as_deref()
        .is_some_and(|bytes| binary_matches(bytes, &transaction.old_active))
    {
        cleanup_prepared_transaction(&transaction)?;
        return Ok(false);
    }
    if current.is_none() {
        atomic_write_bytes(current_exe, &backup)?;
        cleanup_prepared_transaction(&transaction)?;
        return Ok(false);
    }
    if superseded_by_external_install(&transaction) {
        cleanup_prepared_transaction(&transaction)?;
        return Ok(false);
    }
    Err("active binary matches neither prepared state; refusing recovery".to_string())
}

/// A prepared update the binary has already outgrown: something other than
/// the updater (install script, package manager, `cargo install`) replaced
/// it with a build at least as new as the transaction's target. The
/// 3.11.0/3.11.1 updater left such transactions behind on every run, because
/// it executed the unsealed copy. Dropping the staged files is safe; the
/// active binary is not touched.
fn superseded_by_external_install(transaction: &PreparedTransaction) -> bool {
    transaction.operation == "update"
        && !crate::core::version_check::is_newer(
            &transaction.target_active.version,
            CURRENT_VERSION,
        )
}

/// Whether `receipt` describes an earlier install that something other than
/// the updater replaced with a different version. Its rollback chain no
/// longer applies; a same-version mismatch stays a refusal.
fn receipt_outdated_by_external_install(receipt: &UpdateReceipt) -> bool {
    receipt.active.version.trim_start_matches('v') != CURRENT_VERSION
}

pub(super) fn rollback_to_previous() -> Result<(), String> {
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    rollback_installation(&current_exe)
}

/// Restores the retained previous binary over `current_exe`.
pub(super) fn rollback_installation(current_exe: &Path) -> Result<(), String> {
    // The transaction validator requires canonical paths, like an update does;
    // an executable reached through a symlinked directory (`/var` on macOS)
    // would otherwise fail with "non-canonical path".
    let current_exe = canonical_current_exe(current_exe)?;
    let _lock = acquire_update_lock()?;
    if recover_pending_transaction(&current_exe)? {
        return Ok(());
    }
    let receipt = load_update_receipt_for(&current_exe)?
        .ok_or_else(|| "no update receipt is recorded".to_string())?;
    let (state, _receipt_path, previous_path) = canonical_update_paths(&current_exe)?;
    validate_receipt_paths(&receipt, &state, &current_exe)?;
    let previous_bytes =
        std::fs::read(&previous_path).map_err(|e| format!("cannot read retained binary: {e}"))?;
    if !binary_matches(&previous_bytes, &receipt.previous) {
        return Err("retained previous binary failed receipt verification".to_string());
    }
    let old_active = receipt.active.clone();
    let current_bytes = std::fs::read(&current_exe).map_err(|e| e.to_string())?;
    if !binary_matches(&current_bytes, &old_active) {
        return Err("active binary differs from the committed receipt".to_string());
    }
    let target_active = BinaryReceipt {
        path: current_exe.to_string_lossy().into_owned(),
        ..receipt.previous.clone()
    };
    let target_previous = BinaryReceipt {
        path: previous_path.to_string_lossy().into_owned(),
        ..old_active
    };
    let staged_path = staged_update_path("rollback", false)?;
    let backup_path = staged_update_path("rollback", true)?;
    for path in [&staged_path, &backup_path] {
        if path.exists() {
            return Err(format!(
                "stale prepared rollback file exists: {}",
                path.display()
            ));
        }
    }
    atomic_write_bytes(&staged_path, &previous_bytes)?;
    atomic_write_bytes(&backup_path, &current_bytes)?;
    let transaction = PreparedTransaction {
        schema_version: UPDATE_TRANSACTION_SCHEMA.to_string(),
        operation: "rollback".to_string(),
        current_path: current_exe.to_string_lossy().into_owned(),
        previous_path: previous_path.to_string_lossy().into_owned(),
        staged_path: staged_path.to_string_lossy().into_owned(),
        backup_path: backup_path.to_string_lossy().into_owned(),
        old_active: receipt.active,
        target_active,
        target_previous,
        transaction_sha256: None,
    };
    let sealed = write_prepared_transaction(&transaction, &current_exe)?;
    execute_prepared_transaction(&sealed, &current_exe)
}
