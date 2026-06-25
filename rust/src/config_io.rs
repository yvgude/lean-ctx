use std::path::{Path, PathBuf};

fn backup_path_for(path: &Path) -> Option<PathBuf> {
    let filename = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!("{filename}.bak")))
}

#[must_use]
pub fn snapshot_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

pub fn write_atomic_with_backup(path: &Path, content: &str) -> Result<(), String> {
    write_atomic_with_backup_checked(path, content, None)
}

/// Writes TOML config while preserving comments, formatting, key ordering, and
/// any keys present on disk but absent from `new_content` (user customizations,
/// unknown/future keys). Values from `new_content` are merged onto the existing
/// document. Falls back to a plain atomic write when there is nothing to merge
/// or the existing file cannot be parsed.
pub fn write_toml_preserving(path: &Path, new_content: &str) -> Result<(), String> {
    let merged = match std::fs::read_to_string(path) {
        Ok(existing) if !existing.trim().is_empty() => {
            merge_toml(&existing, new_content).unwrap_or_else(|_| new_content.to_string())
        }
        _ => new_content.to_string(),
    };
    write_atomic_with_backup(path, &merged)
}

/// Loads a TOML file into an editable document, preserving comments and
/// formatting. Returns an empty document when the file is missing or invalid.
#[must_use]
pub fn load_toml_document(path: &Path) -> toml_edit::DocumentMut {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| c.parse::<toml_edit::DocumentMut>().ok())
        .unwrap_or_default()
}

/// Persists an edited document via the atomic-with-backup path.
pub fn write_toml_document(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), String> {
    write_atomic_with_backup(path, &doc.to_string())
}

/// Like `write_toml_preserving`, but keeps the config minimal: keys whose value
/// equals the type's default AND are not already present on disk are skipped,
/// so a hand-written config is not bloated with every default key. Existing
/// keys are always updated (preserving comments), and non-default values are
/// always written. `default_content` is `toml::to_string_pretty(&T::default())`.
pub fn write_toml_preserving_minimal(
    path: &Path,
    new_content: &str,
    default_content: &str,
) -> Result<(), String> {
    let merged = match std::fs::read_to_string(path) {
        Ok(existing) if !existing.trim().is_empty() => {
            // Refuse to overwrite a non-empty file we cannot parse. `new_content`
            // and `default_content` come from our own serializer (always valid),
            // so a merge failure means the on-disk config is corrupt — clobbering
            // it with defaults would silently wipe customizations (#443). We
            // propagate the error and leave the file untouched instead.
            merge_toml_inner(&existing, new_content, Some(default_content)).map_err(|e| {
                format!(
                    "refusing to overwrite an unparseable config at {}: {e}",
                    path.display()
                )
            })?
        }
        // No existing file: write a fresh minimal document (drop defaults).
        _ => merge_toml_inner("", new_content, Some(default_content))
            .unwrap_or_else(|_| new_content.to_string()),
    };
    write_atomic_with_backup(path, &merged)
}

/// Merges `incoming` TOML values onto the `existing` document, retaining the
/// existing document's comments, whitespace, and unknown keys.
fn merge_toml(existing: &str, incoming: &str) -> Result<String, String> {
    merge_toml_inner(existing, incoming, None)
}

fn merge_toml_inner(
    existing: &str,
    incoming: &str,
    defaults: Option<&str>,
) -> Result<String, String> {
    let mut existing_doc = existing
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| e.to_string())?;
    let incoming_doc = incoming
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| e.to_string())?;
    let default_doc = match defaults {
        Some(d) => Some(
            d.parse::<toml_edit::DocumentMut>()
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    merge_table(
        existing_doc.as_table_mut(),
        incoming_doc.as_table(),
        default_doc.as_ref().map(toml_edit::DocumentMut::as_table),
    );
    Ok(existing_doc.to_string())
}

/// Recursively merges `source` keys into `target`, updating values in place so
/// surrounding comments (key decor) survive, recursing into nested tables, and
/// preserving inline value decor (trailing comments) on updated leaves.
///
/// When `defaults` is `Some`, a key that is absent from `target` and whose value
/// equals the corresponding default is skipped (minimal-config mode).
fn merge_table(
    target: &mut toml_edit::Table,
    source: &toml_edit::Table,
    defaults: Option<&toml_edit::Table>,
) {
    use toml_edit::Item;
    for (key, source_item) in source {
        let default_item = defaults.and_then(|d| d.get(key));
        match (source_item, target.get_mut(key)) {
            (Item::Table(source_tbl), Some(Item::Table(target_tbl))) => {
                merge_table(
                    target_tbl,
                    source_tbl,
                    default_item.and_then(Item::as_table),
                );
            }
            (Item::Value(source_val), Some(Item::Value(target_val))) => {
                let prefix = target_val.decor().prefix().cloned();
                let suffix = target_val.decor().suffix().cloned();
                let mut new_val = source_val.clone();
                if let Some(p) = prefix {
                    new_val.decor_mut().set_prefix(p);
                }
                if let Some(s) = suffix {
                    new_val.decor_mut().set_suffix(s);
                }
                *target_val = new_val;
            }
            (_, Some(target_item)) => {
                *target_item = source_item.clone();
            }
            (Item::Table(source_tbl), None) if defaults.is_some() => {
                // New table in minimal mode: build it from non-default leaves
                // only and skip it entirely if nothing meaningful remains.
                let mut fresh = toml_edit::Table::new();
                merge_table(
                    &mut fresh,
                    source_tbl,
                    default_item.and_then(Item::as_table),
                );
                if !fresh.is_empty() {
                    target.insert(key, Item::Table(fresh));
                }
            }
            (_, None) => {
                if defaults.is_none() || !item_equals_default(source_item, default_item) {
                    target.insert(key, source_item.clone());
                }
            }
        }
    }
}

/// Compares a serialized item against its default, ignoring decor. Both sides
/// originate from the same serializer, so their normalized string form matches
/// exactly when the underlying values are equal.
fn item_equals_default(item: &toml_edit::Item, default: Option<&toml_edit::Item>) -> bool {
    match default {
        Some(d) => item.to_string().trim() == d.to_string().trim(),
        None => false,
    }
}

/// Remove stale timestamped `.bak` files left by the old backup scheme.
/// Called once at startup to clean up the accumulated backups.
pub fn cleanup_legacy_backups(data_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.contains(".lean-ctx.") && name.ends_with(".bak") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub fn write_atomic_with_backup_checked(
    path: &Path,
    content: &str,
    expected_mtime: Option<std::time::SystemTime>,
) -> Result<(), String> {
    if path.exists() {
        if let Some(expected) = expected_mtime {
            let current = snapshot_mtime(path);
            if current != Some(expected) {
                return Err(format!(
                    "file was modified externally since last read: {}",
                    path.display()
                ));
            }
        }
        if let Some(bak) = backup_path_for(path) {
            let _ = std::fs::copy(path, &bak);
        }
    }

    write_atomic(path, content)
}

pub fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    reject_symlink(path)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let parent = path
        .parent()
        .ok_or_else(|| "invalid path (no parent directory)".to_string())?;
    let filename = path
        .file_name()
        .ok_or_else(|| "invalid path (no filename)".to_string())?
        .to_string_lossy();

    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());

    let tmp = parent.join(format!(".{filename}.lean-ctx.tmp.{pid}.{nanos}"));
    std::fs::write(&tmp, content).map_err(|e| e.to_string())?;

    #[cfg(windows)]
    {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        format!(
            "atomic write failed: {} (tmp: {})",
            e,
            tmp.to_string_lossy()
        )
    })?;

    restrict_file_permissions(path);

    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    // `is_symlink_or_reparse`: on Windows this also rejects NTFS junctions,
    // which `FileType::is_symlink` misses (GL#442).
    if path.exists()
        && path
            .symlink_metadata()
            .is_ok_and(|m| crate::core::pathutil::is_symlink_or_reparse(&m))
    {
        return Err(format!(
            "refusing to write through symlink: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
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

        let result = write_toml_preserving_minimal(
            &path,
            "ultra_compact = false\n",
            "ultra_compact = false\n",
        );

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
}
