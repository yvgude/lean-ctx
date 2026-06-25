//! Legacy artifact migration — detect old postcard+zstd index files and migrate
//! them to the new SQLite `code_index.db`.
//!
//! # Flow
//!
//! 1. Check for old artifacts (`project_index.bin.zst`, `bm25_index.bin.zst`,
//!    `bm25_index.bin`, `bm25_index.json`) in `vectors_dir(project_root)`.
//! 2. If any exist, run the full [`IndexPipeline`] to produce `code_index.db`.
//! 3. After a successful run, delete the old artifacts.
//! 4. Return `true` if migration happened, `false` if nothing was needed.

use std::path::Path;

use anyhow::{Context, Result};

use crate::core::index_namespace;
use crate::core::index_pipeline::pipeline::IndexPipeline;

const OLD_ARTIFACTS: &[&str] = &[
    "project_index.bin.zst",
    "bm25_index.bin.zst",
    "bm25_index.bin",
    "bm25_index.json",
];

/// Detect legacy postcard+zstd artifacts and migrate to SQLite.
///
/// Returns `true` if migration was performed, `false` if no artifacts were found
/// or `code_index.db` already exists and no old artifacts remain.
pub fn migrate_if_needed(project_root: &Path) -> Result<bool> {
    let vectors_dir = index_namespace::vectors_dir(project_root);

    // No vectors directory — nothing to migrate.
    if !vectors_dir.exists() {
        return Ok(false);
    }

    let db_path = vectors_dir.join("code_index.db");
    let db_exists = db_path.exists();

    // Check for old artifacts.
    let has_old_artifacts: bool = OLD_ARTIFACTS
        .iter()
        .any(|name| vectors_dir.join(name).exists());

    // Nothing to do.
    if !has_old_artifacts {
        return Ok(false);
    }

    // Old artifacts present but code_index.db already exists — just clean up.
    if db_exists {
        delete_old_artifacts(&vectors_dir);
        return Ok(true);
    }

    // Run the full pipeline to produce a fresh code_index.db.
    IndexPipeline::new(project_root.to_path_buf())
        .build()
        .context("migration: pipeline build failed")?
        .run()
        .context("migration: pipeline run failed")?;

    // Only delete old artifacts after a successful pipeline run.
    delete_old_artifacts(&vectors_dir);

    Ok(true)
}

/// Remove all known old artifact files from `vectors_dir`. Silently skips
/// files that no longer exist.
fn delete_old_artifacts(vectors_dir: &Path) {
    for name in OLD_ARTIFACTS {
        let path = vectors_dir.join(name);
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::data_dir::isolated_data_dir;

    /// Create a minimal source tree so the pipeline has something to index.
    fn create_minimal_tree(root: &Path) {
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() { println!(\"hello\"); }").unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn helper() -> u32 { 42 }").unwrap();
    }

    /// Convenience: write a fake old artifact file.
    fn touch_artifact(vectors_dir: &Path, name: &str) {
        std::fs::create_dir_all(vectors_dir).unwrap();
        std::fs::write(vectors_dir.join(name), b"fake legacy data").unwrap();
    }

    #[test]
    fn migrate_from_legacy_artifacts() {
        let _iso = isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        create_minimal_tree(project.path());

        let vec_dir = index_namespace::vectors_dir(project.path());
        for name in OLD_ARTIFACTS {
            touch_artifact(&vec_dir, name);
        }

        // All old artifacts present before migration.
        for name in OLD_ARTIFACTS {
            assert!(vec_dir.join(name).exists(), "{name} should exist before migration");
        }

        let migrated = migrate_if_needed(project.path()).unwrap();
        assert!(migrated, "migration should return true");

        // Old artifacts removed.
        for name in OLD_ARTIFACTS {
            assert!(!vec_dir.join(name).exists(), "{name} should be deleted after migration");
        }

        // SQLite database created.
        assert!(
            vec_dir.join("code_index.db").exists(),
            "code_index.db should exist after migration"
        );
    }

    #[test]
    fn migrate_idempotent() {
        let _iso = isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        create_minimal_tree(project.path());

        let vec_dir = index_namespace::vectors_dir(project.path());
        for name in OLD_ARTIFACTS {
            touch_artifact(&vec_dir, name);
        }

        // First call — performs migration.
        let first = migrate_if_needed(project.path()).unwrap();
        assert!(first, "first call should migrate");
        assert!(vec_dir.join("code_index.db").exists());

        // Second call — nothing to do.
        let second = migrate_if_needed(project.path()).unwrap();
        assert!(!second, "second call should return false");
    }

    #[test]
    fn migrate_noop_when_no_artifacts() {
        let _iso = isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        create_minimal_tree(project.path());

        // No old artifacts — migration should be a no-op.
        let result = migrate_if_needed(project.path()).unwrap();
        assert!(!result, "should return false when no old artifacts exist");
    }

    #[test]
    fn migrate_handles_absent_vectors_dir() {
        let _iso = isolated_data_dir();
        let project = tempfile::tempdir().unwrap();

        // Project has no vectors dir yet (pipeline never ran).
        let result = migrate_if_needed(project.path()).unwrap();
        assert!(!result, "should return false when vectors_dir does not exist");
    }

    #[test]
    fn migrate_already_migrated_cleans_old_artifacts() {
        let _iso = isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        create_minimal_tree(project.path());

        let vec_dir = index_namespace::vectors_dir(project.path());

        // Create code_index.db (simulates already migrated state).
        std::fs::create_dir_all(&vec_dir).unwrap();
        std::fs::write(vec_dir.join("code_index.db"), b"fake db").unwrap();

        // Also leave old artifacts around.
        for name in OLD_ARTIFACTS {
            touch_artifact(&vec_dir, name);
        }

        let migrated = migrate_if_needed(project.path()).unwrap();
        assert!(migrated, "should return true because old artifacts were cleaned");

        // Old artifacts removed.
        for name in OLD_ARTIFACTS {
            assert!(!vec_dir.join(name).exists(), "{name} should be deleted");
        }
    }
}
