// SPDX-License-Identifier: Apache-2.0

//! Byte-preserving migration from the pre-v3.6.14 `.lctxpkg` name to `.ctxpkg`.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::verify::verify_package_file;

pub(crate) const PACKAGE_MIGRATION_RECEIPT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageMigrationReceipt {
    pub receipt_version: u32,
    pub source_file: String,
    pub output_file: String,
    pub source_sha256: String,
    pub output_sha256: String,
    pub byte_preserving: bool,
    pub source_retained_for_rollback: bool,
}

pub(crate) fn rollback_legacy_package(receipt_path: &Path) -> Result<PathBuf, String> {
    let _lock = lock_package_migration(receipt_path)?;
    reject_symlink(receipt_path, "migration receipt")?;
    let receipt_bytes =
        std::fs::read(receipt_path).map_err(|error| format!("read migration receipt: {error}"))?;
    let receipt: PackageMigrationReceipt = serde_json::from_slice(&receipt_bytes)
        .map_err(|error| format!("parse migration receipt: {error}"))?;
    if receipt.receipt_version != PACKAGE_MIGRATION_RECEIPT_VERSION
        || !receipt.byte_preserving
        || !receipt.source_retained_for_rollback
        || receipt.source_sha256 != receipt.output_sha256
    {
        return Err("migration receipt is not rollback-compatible".to_string());
    }
    validate_plain_filename(&receipt.source_file, "source_file")?;
    validate_plain_filename(&receipt.output_file, "output_file")?;
    let parent = receipt_path
        .parent()
        .ok_or_else(|| "migration receipt has no parent directory".to_string())?;
    let source = parent.join(&receipt.source_file);
    let output = parent.join(&receipt.output_file);
    if source == output
        || source.extension().and_then(|value| value.to_str())
            != Some(crate::core::contracts::LEGACY_PACKAGE_EXTENSION)
        || output.extension().and_then(|value| value.to_str())
            != Some(crate::core::contracts::PACKAGE_EXTENSION)
        || source.with_extension(crate::core::contracts::PACKAGE_EXTENSION) != output
    {
        return Err("migration receipt source/output package binding is invalid".to_string());
    }
    if output.with_extension("ctxpkg.migration.json") != receipt_path {
        return Err("migration receipt path does not match output package".to_string());
    }
    reject_symlink(&source, "rollback source")?;
    reject_symlink(&output, "rollback output")?;
    validate_digest(&source, &receipt.source_sha256, "rollback source")?;
    if output.exists() {
        validate_digest(&output, &receipt.output_sha256, "rollback output")?;
        std::fs::remove_file(&output)
            .map_err(|error| format!("remove migrated package {}: {error}", output.display()))?;
        sync_parent(parent)?;
    }
    std::fs::remove_file(receipt_path)
        .map_err(|error| format!("remove migration receipt: {error}"))?;
    sync_parent(parent)?;
    Ok(source)
}

fn validate_plain_filename(value: &str, field: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty() || path.file_name().and_then(|name| name.to_str()) != Some(value) {
        return Err(format!("{field} must be one plain UTF-8 filename"));
    }
    Ok(())
}

fn validate_digest(path: &Path, expected: &str, label: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read {label}: {error}"))?;
    let actual = crate::core::agent_identity::hex_encode(&Sha256::digest(&bytes));
    if actual != expected {
        return Err(format!("{label} SHA-256 mismatch"));
    }
    Ok(())
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), String> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        Err(format!("refusing symlink {label}: {}", path.display()))
    } else {
        Ok(())
    }
}

fn sync_parent(parent: &Path) -> Result<(), String> {
    crate::core::atomic_fs::sync_directory(parent)
        .map_err(|error| format!("sync rollback directory: {error}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageMigrationResult {
    pub output_path: PathBuf,
    pub receipt_path: PathBuf,
    pub already_migrated: bool,
    pub receipt: PackageMigrationReceipt,
}

pub(crate) fn migrate_legacy_package(path: &Path) -> Result<PackageMigrationResult, String> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!("refusing to migrate symlink: {}", path.display()));
    }
    if path.extension().and_then(|value| value.to_str())
        != Some(crate::core::contracts::LEGACY_PACKAGE_EXTENSION)
    {
        return Err(format!(
            "expected a .{} package",
            crate::core::contracts::LEGACY_PACKAGE_EXTENSION
        ));
    }

    let verification = verify_package_file(path)?;
    if !verification.valid() {
        return Err(format!(
            "legacy package verification failed: {}",
            verification.errors.join("; ")
        ));
    }

    let bytes = std::fs::read(path).map_err(|error| format!("read legacy package: {error}"))?;
    let digest = crate::core::agent_identity::hex_encode(&Sha256::digest(&bytes));
    let output_path = path.with_extension(crate::core::contracts::PACKAGE_EXTENSION);
    let receipt_path = output_path.with_extension("ctxpkg.migration.json");
    let _lock = lock_package_migration(&receipt_path)?;
    let receipt = PackageMigrationReceipt {
        receipt_version: PACKAGE_MIGRATION_RECEIPT_VERSION,
        source_file: file_name(path)?,
        output_file: file_name(&output_path)?,
        source_sha256: digest.clone(),
        output_sha256: digest,
        byte_preserving: true,
        source_retained_for_rollback: true,
    };
    let receipt_bytes = serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?;
    validate_existing_file(&receipt_path, &receipt_bytes, "migration receipt")?;

    let already_migrated = if output_path.exists() {
        validate_existing_file(&output_path, &bytes, "destination")?;
        true
    } else {
        create_without_overwrite(&output_path, &bytes)?;
        false
    };

    let output_verification = verify_package_file(&output_path)?;
    if !output_verification.valid() {
        if !already_migrated {
            let _ = std::fs::remove_file(&output_path);
        }
        return Err("migrated package failed post-write verification".to_string());
    }
    if !receipt_path.exists()
        && let Err(error) = create_without_overwrite(&receipt_path, &receipt_bytes)
    {
        if !already_migrated {
            let _ = std::fs::remove_file(&output_path);
        }
        return Err(error);
    }
    Ok(PackageMigrationResult {
        output_path,
        receipt_path,
        already_migrated,
        receipt,
    })
}

fn validate_existing_file(path: &Path, expected: &[u8], label: &str) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let existing =
        std::fs::read(path).map_err(|error| format!("read existing {label}: {error}"))?;
    if existing == expected {
        Ok(())
    } else {
        Err(format!(
            "refusing to overwrite non-identical {label}: {}",
            path.display()
        ))
    }
}

fn file_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
        .ok_or_else(|| format!("package filename is not UTF-8: {}", path.display()))
}

fn create_without_overwrite(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("invalid output filename: {}", path.display()))?;
    let staged = path.with_file_name(format!(".{name}.staged"));
    reject_symlink(&staged, "staged migration file")?;
    if staged.exists() {
        std::fs::remove_file(&staged)
            .map_err(|error| format!("remove stale staged migration file: {error}"))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .map_err(|error| format!("create {}: {error}", staged.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("write {}: {error}", staged.display()));
    }
    std::fs::hard_link(&staged, path)
        .map_err(|error| format!("publish {} without overwrite: {error}", path.display()))?;
    std::fs::remove_file(&staged)
        .map_err(|error| format!("remove staged migration file: {error}"))?;
    if let Some(parent) = path.parent() {
        sync_parent(parent)?;
    }
    Ok(())
}

fn lock_package_migration(
    receipt_path: &Path,
) -> Result<crate::core::migration_lock::MigrationLock, String> {
    let lock_path = receipt_path.with_extension("json.lock");
    reject_symlink(&lock_path, "migration lock")?;
    crate::core::migration_lock::acquire(&lock_path)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::core::context_package::content::PackageContent;
    use crate::core::context_package::manifest::{
        CompatibilitySpec, PackageIntegrity, PackageLayer, PackageManifest, PackageProvenance,
        PackageStats,
    };

    #[derive(Serialize)]
    struct Bundle {
        manifest: PackageManifest,
        content: PackageContent,
    }

    fn legacy_package(path: &Path, signed: bool) -> Vec<u8> {
        let content = PackageContent::default();
        let content_json = serde_json::to_string(&content).unwrap();
        let content_hash =
            crate::core::agent_identity::hex_encode(&Sha256::digest(content_json.as_bytes()));
        let composite = format!("legacy-fixture:1.0.0:{content_hash}");
        let mut manifest = PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            kind: Default::default(),
            name: "legacy-fixture".to_string(),
            version: "1.0.0".to_string(),
            description: "v3 package migration fixture".to_string(),
            author: None,
            scope: None,
            created_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            updated_at: None,
            layers: vec![PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: PackageIntegrity {
                sha256: crate::core::agent_identity::hex_encode(&Sha256::digest(
                    composite.as_bytes(),
                )),
                content_hash,
                byte_size: content_json.len() as u64,
            },
            provenance: PackageProvenance {
                tool: "lean-ctx".to_string(),
                tool_version: "3.10.1".to_string(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
        };
        if signed {
            let key = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
            crate::core::context_package::signing::sign_package(&mut manifest, &content, &key);
        }
        let bytes = serde_json::to_vec_pretty(&Bundle { manifest, content }).unwrap();
        std::fs::write(path, &bytes).unwrap();
        bytes
    }

    #[test]
    fn signed_legacy_package_migrates_byte_for_byte_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        let original = legacy_package(&source, true);

        let first = migrate_legacy_package(&source).unwrap();
        assert!(!first.already_migrated);
        assert_eq!(std::fs::read(&first.output_path).unwrap(), original);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(first.receipt.source_sha256, first.receipt.output_sha256);

        let second = migrate_legacy_package(&source).unwrap();
        assert!(second.already_migrated);
        assert_eq!(first.receipt, second.receipt);
    }

    #[test]
    fn rollback_removes_only_verified_generated_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        let original = legacy_package(&source, true);
        let migrated = migrate_legacy_package(&source).unwrap();

        let restored = rollback_legacy_package(&migrated.receipt_path).unwrap();
        assert_eq!(restored, source);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(!migrated.output_path.exists());
        assert!(!migrated.receipt_path.exists());
    }

    #[test]
    fn rollback_refuses_changed_output_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        legacy_package(&source, false);
        let migrated = migrate_legacy_package(&source).unwrap();
        std::fs::write(&migrated.output_path, b"changed").unwrap();

        assert!(rollback_legacy_package(&migrated.receipt_path).is_err());
        assert_eq!(std::fs::read(&migrated.output_path).unwrap(), b"changed");
        assert!(migrated.receipt_path.exists());
    }

    #[test]
    fn rollback_recovers_after_output_was_already_removed() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        legacy_package(&source, false);
        let migrated = migrate_legacy_package(&source).unwrap();
        std::fs::remove_file(&migrated.output_path).unwrap();

        assert_eq!(
            rollback_legacy_package(&migrated.receipt_path).unwrap(),
            source
        );
        assert!(!migrated.receipt_path.exists());
    }

    #[test]
    fn rollback_rejects_receipt_moved_away_from_bound_output() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        legacy_package(&source, false);
        let migrated = migrate_legacy_package(&source).unwrap();
        let moved = dir.path().join("unbound.migration.json");
        std::fs::rename(&migrated.receipt_path, &moved).unwrap();

        assert!(rollback_legacy_package(&moved).is_err());
        assert!(migrated.output_path.exists());
    }

    #[test]
    fn rollback_rejects_receipt_that_aliases_source_and_output() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        legacy_package(&source, false);
        let migrated = migrate_legacy_package(&source).unwrap();
        let mut receipt = migrated.receipt.clone();
        receipt.output_file = receipt.source_file.clone();
        std::fs::write(
            &migrated.receipt_path,
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();

        assert!(rollback_legacy_package(&migrated.receipt_path).is_err());
        assert!(source.exists());
        assert!(migrated.output_path.exists());
        assert!(migrated.receipt_path.exists());
    }

    #[test]
    fn tampered_signed_package_is_rejected_without_output() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        let mut bytes = legacy_package(&source, true);
        let index = bytes.iter().position(|byte| *byte == b'v').unwrap();
        bytes[index] = b'x';
        std::fs::write(&source, bytes).unwrap();

        assert!(migrate_legacy_package(&source).is_err());
        assert!(!source.with_extension("ctxpkg").exists());
    }

    #[test]
    fn conflicting_destination_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        legacy_package(&source, false);
        let output = source.with_extension("ctxpkg");
        std::fs::write(&output, b"owned").unwrap();

        assert!(migrate_legacy_package(&source).is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"owned");
    }

    #[test]
    fn conflicting_receipt_does_not_create_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.lctxpkg");
        legacy_package(&source, false);
        let output = source.with_extension("ctxpkg");
        let receipt = output.with_extension("ctxpkg.migration.json");
        std::fs::write(&receipt, b"owned").unwrap();

        assert!(migrate_legacy_package(&source).is_err());
        assert!(!output.exists());
        assert_eq!(std::fs::read(receipt).unwrap(), b"owned");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_source_is_rejected() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.lctxpkg");
        legacy_package(&target, false);
        let source = dir.path().join("fixture.lctxpkg");
        symlink(&target, &source).unwrap();

        assert!(migrate_legacy_package(&source).is_err());
        assert!(!source.with_extension("ctxpkg").exists());
    }
}
