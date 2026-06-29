use std::path::{Path, PathBuf};

fn backup_path_for(path: &Path) -> Option<PathBuf> {
    let filename = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!("{filename}.bak")))
}

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
    // #596: a user may symlink agent config (`~/.claude.json`,
    // `~/.codex/config.toml`, …) into a managed dotfiles repo. Resolve the
    // symlink to its real target and write THROUGH it (preserving the symlink)
    // instead of hard-blocking. The target must stay within `$HOME`, so a
    // planted symlink can never redirect a config write outside the user's own
    // home (preserves the GL#442 symlink-hijack protection).
    let target = resolve_write_target(path)?;

    if let Some(parent) = target.parent() {
        ensure_dir(parent)?;
    }

    // Force owner-only perms on the real config file (a symlink itself has no
    // meaningful mode); Windows ACLs are left untouched. The temp+rename
    // mechanics and the read-only-directory in-place fallback (#459) are shared
    // with the edit tools via `core::atomic_fs`.
    #[cfg(unix)]
    let perms = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let perms: Option<std::fs::Permissions> = None;

    crate::core::atomic_fs::write_bytes_with_fallback(&target, content.as_bytes(), perms.as_ref())
}

/// Resolve the real file to write, honoring a user-managed symlink (#596).
///
/// * not a symlink (or missing) → `path` unchanged.
/// * symlink whose resolved target stays within `$HOME` → the target (write
///   THROUGH, preserving the symlink) — the legitimate dotfiles pattern.
/// * symlink whose target escapes `$HOME` → refuse (preserves the GL#442
///   symlink-hijack protection).
fn resolve_write_target(path: &Path) -> Result<PathBuf, String> {
    let Ok(meta) = path.symlink_metadata() else {
        return Ok(path.to_path_buf());
    };
    if !crate::core::pathutil::is_symlink_or_reparse(&meta) {
        return Ok(path.to_path_buf());
    }

    let real_target = resolve_symlink_target(path)?;
    ensure_target_allowed(path, &real_target)?;
    Ok(real_target)
}

/// Read a symlink and resolve its target to an absolute path, resolving symlinks
/// in the existing-ancestor portion (so a symlinked *parent* is followed too)
/// while tolerating a not-yet-created target file/dir.
fn resolve_symlink_target(link_path: &Path) -> Result<PathBuf, String> {
    let link = std::fs::read_link(link_path)
        .map_err(|e| format!("cannot read symlink {}: {e}", link_path.display()))?;
    let raw_target = if link.is_absolute() {
        link
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(link)
    };
    canonicalize_existing_prefix(&raw_target)
}

/// Canonicalize `path` by resolving its deepest *existing* ancestor (following
/// symlinks) and re-appending the not-yet-created tail, so the home-only check
/// runs on a real path even when the target file/dir doesn't exist yet.
fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf, String> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(real) = crate::core::pathutil::canonicalize_secure(cur) {
            let mut out = real;
            for comp in tail.iter().rev() {
                out.push(comp);
            }
            return Ok(out);
        }
        match cur.parent() {
            Some(parent) if parent != cur => {
                if let Some(name) = cur.file_name() {
                    tail.push(name.to_os_string());
                }
                cur = parent;
            }
            _ => {
                return Err(format!(
                    "cannot resolve any existing ancestor of {}",
                    path.display()
                ));
            }
        }
    }
}

/// SECURITY (#596 / GL#442): a resolved symlink target must stay within `$HOME`
/// or under one of the explicitly opted-in [`allowed_symlink_roots`]. Otherwise
/// a planted symlink could redirect a config write to an attacker-chosen path.
fn ensure_target_allowed(link_path: &Path, real_target: &Path) -> Result<(), String> {
    let home = crate::core::home::resolve_home_dir()
        .ok_or_else(|| "cannot determine $HOME to validate symlink target".to_string())?;
    let real_home = crate::core::pathutil::canonicalize_secure_or_self(&home);
    if real_target.starts_with(&real_home) {
        return Ok(());
    }
    if allowed_symlink_roots()
        .iter()
        .any(|root| real_target.starts_with(root))
    {
        return Ok(());
    }
    Err(format!(
        "refusing to write through a symlink whose target escapes $HOME:\n  \
         {} -> {}\n  \
         The target is outside your home directory, so lean-ctx will not follow it \
         (symlink-hijack protection). To allow this location, either:\n    \
         - point the agent at the real path (set CLAUDE_CONFIG_DIR / CODEX_HOME), or\n    \
         - move the target under $HOME, or\n    \
         - add its parent to `allow_symlink_roots` in your lean-ctx config \
         (or the LEAN_CTX_ALLOW_SYMLINK_ROOTS env var).",
        link_path.display(),
        real_target.display()
    ))
}

/// Trusted roots OUTSIDE `$HOME` the user explicitly opted into for symlinked
/// agent configs (#596). Sourced from the `LEAN_CTX_ALLOW_SYMLINK_ROOTS` env var
/// (path-list separator) and the user-level `allow_symlink_roots` config key
/// (untrusted project-local configs are stripped at load — see
/// `strip_sensitive_overrides`). Each entry is made absolute + canonicalized so
/// the boundary check compares real paths; relative/empty entries are dropped.
fn allowed_symlink_roots() -> Vec<PathBuf> {
    let mut raw: Vec<PathBuf> = Vec::new();
    if let Some(env) = std::env::var_os("LEAN_CTX_ALLOW_SYMLINK_ROOTS") {
        raw.extend(std::env::split_paths(&env));
    }
    raw.extend(
        crate::core::config::Config::load()
            .allow_symlink_roots
            .into_iter()
            .map(PathBuf::from),
    );
    raw.into_iter()
        .filter(|p| !p.as_os_str().is_empty() && p.is_absolute())
        .map(|p| crate::core::pathutil::canonicalize_secure_or_self(&p))
        .collect()
}

/// `create_dir_all` that tolerates a user-managed symlinked directory (#596):
///
/// * regular dir / missing path → `create_dir_all`.
/// * symlink to an existing directory → ok (no-op).
/// * dangling symlink whose target is within `$HOME` → create the real target.
/// * symlink to a non-directory, or a target escaping `$HOME` → clear error.
pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    match dir.symlink_metadata() {
        Ok(meta) if crate::core::pathutil::is_symlink_or_reparse(&meta) => {
            match std::fs::metadata(dir) {
                Ok(m) if m.is_dir() => Ok(()),
                Ok(_) => Err(format!(
                    "{} is a symlink to a non-directory; fix or remove the symlink",
                    dir.display()
                )),
                Err(_) => {
                    // Dangling symlink: create the intended target if it is in
                    // $HOME (or an explicitly allow-listed root, #596).
                    let real_target = resolve_symlink_target(dir)?;
                    ensure_target_allowed(dir, &real_target)?;
                    std::fs::create_dir_all(&real_target).map_err(|e| {
                        format!(
                            "cannot create symlink target dir {}: {e}",
                            real_target.display()
                        )
                    })
                }
            }
        }
        _ => std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create directory {}: {e}", dir.display())),
    }
}

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

/// #596: write THROUGH a user-managed symlink to its real (in-`$HOME`) target,
/// reject targets that escape `$HOME`, and make `ensure_dir` tolerant of
/// symlinked directories. Unix-only (POSIX symlinks + `$HOME` override).
#[cfg(all(test, unix))]
mod symlink_596_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// RAII override of `$HOME` that restores the previous value on drop (even on
    /// panic). Pair with `test_env_lock()` so env access stays serialized.
    struct HomeGuard(Option<std::ffi::OsString>);
    impl HomeGuard {
        fn set(home: &Path) -> Self {
            let prev = std::env::var_os("HOME");
            crate::test_env::set_var("HOME", home);
            HomeGuard(prev)
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => crate::test_env::set_var("HOME", v),
                None => crate::test_env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn write_through_symlink_in_home_updates_target_and_keeps_link() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());

        let dotfiles = home.path().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        let target = dotfiles.join("agent.json");
        std::fs::write(&target, "{}\n").unwrap();
        let link = home.path().join(".agent.json");
        symlink(&target, &link).unwrap();

        write_atomic(&link, "{\"k\":1}\n").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the user symlink must be preserved (write-through, not replace)"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"k\":1}\n");

        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600,
            "owner-only perms must land on the real config file"
        );
    }

    #[test]
    fn refuses_symlink_whose_target_escapes_home() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());

        let target = outside.path().join("escape.json");
        std::fs::write(&target, "{}").unwrap();
        let link = home.path().join(".agent.json");
        symlink(&target, &link).unwrap();

        let err = write_atomic(&link, "x").unwrap_err();
        assert!(err.contains("escapes $HOME"), "got: {err}");
        assert!(
            err.contains("allow_symlink_roots"),
            "error must point at the opt-in escape hatch, got: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "{}",
            "an escaping target must be left untouched"
        );
    }

    /// RAII override of `LEAN_CTX_ALLOW_SYMLINK_ROOTS` (restores on drop).
    struct AllowRootsGuard(Option<std::ffi::OsString>);
    impl AllowRootsGuard {
        fn set(value: &std::ffi::OsStr) -> Self {
            let prev = std::env::var_os("LEAN_CTX_ALLOW_SYMLINK_ROOTS");
            crate::test_env::set_var("LEAN_CTX_ALLOW_SYMLINK_ROOTS", value);
            AllowRootsGuard(prev)
        }
    }
    impl Drop for AllowRootsGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => crate::test_env::set_var("LEAN_CTX_ALLOW_SYMLINK_ROOTS", v),
                None => crate::test_env::remove_var("LEAN_CTX_ALLOW_SYMLINK_ROOTS"),
            }
        }
    }

    #[test]
    fn allows_symlink_escape_when_target_root_is_allowlisted() {
        // #596 premium: an out-of-$HOME target IS written through once its root
        // is explicitly opted into via LEAN_CTX_ALLOW_SYMLINK_ROOTS.
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());

        // Canonical root (macOS tempdirs live under /var → /private/var).
        let real_outside = std::fs::canonicalize(outside.path()).unwrap();
        let target = real_outside.join("agent.json");
        std::fs::write(&target, "{}\n").unwrap();
        let link = home.path().join(".agent.json");
        symlink(&target, &link).unwrap();

        let _roots = AllowRootsGuard::set(real_outside.as_os_str());
        write_atomic(&link, "{\"k\":1}\n").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"k\":1}\n");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the user symlink must be preserved (write-through, not replace)"
        );
    }

    #[test]
    fn ensure_dir_accepts_symlink_to_dir_rejects_symlink_to_file() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());

        let real_dir = home.path().join("real_dir");
        std::fs::create_dir_all(&real_dir).unwrap();
        let dir_link = home.path().join(".agentdir");
        symlink(&real_dir, &dir_link).unwrap();
        assert!(
            ensure_dir(&dir_link).is_ok(),
            "a healthy dir symlink must be accepted"
        );

        let real_file = home.path().join("real_file");
        std::fs::write(&real_file, "x").unwrap();
        let file_link = home.path().join(".agentfile");
        symlink(&real_file, &file_link).unwrap();
        let err = ensure_dir(&file_link).unwrap_err();
        assert!(err.contains("non-directory"), "got: {err}");
    }

    #[test]
    fn ensure_dir_creates_dangling_symlink_target_in_home() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());

        // Dangling: link → home/dotfiles/.codex, neither exists yet.
        let target = home.path().join("dotfiles/.codex");
        let link = home.path().join(".codex");
        symlink(&target, &link).unwrap();

        ensure_dir(&link).unwrap();

        assert!(target.is_dir(), "dangling symlink target must be created");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must remain"
        );
        assert!(std::fs::metadata(&link).unwrap().is_dir());
    }
}
