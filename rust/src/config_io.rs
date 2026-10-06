use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CONFIG_MIGRATION_RECEIPT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigMigrationReceipt {
    version: u32,
    source_file: String,
    backup_file: String,
    source_sha256: String,
    output_sha256: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
}

fn config_migration_paths(path: &Path) -> Result<(PathBuf, PathBuf), String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "config path must end in a UTF-8 filename".to_string())?;
    Ok((
        path.with_file_name(format!("{filename}.v4-migration.bak")),
        path.with_file_name(format!("{filename}.v4-migration.json")),
    ))
}

fn lock_config_migration(
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

fn config_migration_lock_path(path: &Path) -> Result<PathBuf, String> {
    let target = canonicalize_existing_prefix(&resolve_write_target(path)?)?;
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "config path must end in a UTF-8 filename".to_string())?;
    let lock_path = target.with_file_name(format!("{filename}.v4-migration.lock"));
    Ok(lock_path)
}

/// Atomically prepares a recoverable v3→v4 config journal before replacing the
/// config. A pre-existing journal is never overwritten.
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
    for artifact in [&backup, &receipt_path] {
        if std::fs::symlink_metadata(artifact).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err("config migration journal must not be a symlink".into());
        }
    }
    if backup.exists() && !receipt_path.exists() {
        if std::fs::symlink_metadata(&backup).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err("config migration backup must not be a symlink".to_string());
        }
        let orphan = std::fs::read(&backup)
            .map_err(|error| format!("read orphan config migration backup: {error}"))?;
        if orphan != source {
            return Err("unverifiable config migration backup already exists".to_string());
        }
        std::fs::remove_file(&backup)
            .map_err(|error| format!("remove recoverable config migration backup: {error}"))?;
    }
    if backup.exists() || receipt_path.exists() {
        return Err(format!(
            "config migration journal already exists beside {}; roll it back or remove it after verification",
            path.display()
        ));
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
        output_sha256: sha256_hex(content.as_bytes()),
    };
    std::str::from_utf8(&source).map_err(|_| "config is not UTF-8")?;
    crate::core::migration_lock::atomic_create_without_overwrite(&backup, &source)?;
    let receipt_json = serde_json::to_string_pretty(&receipt)
        .map_err(|error| format!("serialize config migration receipt: {error}"))?;
    crate::core::migration_lock::atomic_create_without_overwrite(
        &receipt_path,
        format!("{receipt_json}\n").as_bytes(),
    )?;
    write_atomic(path, content)?;
    Ok(())
}

/// Restores exact pre-migration bytes only while both journal and current
/// post-state still match their recorded SHA-256 identities.
pub fn rollback_config_migration(path: &Path) -> Result<(), String> {
    let resolved = canonicalize_existing_prefix(&resolve_write_target(path)?)?;
    let path = resolved.as_path();
    let _lock = lock_config_migration(path)?;
    let (backup, receipt_path) = config_migration_paths(path)?;
    if std::fs::symlink_metadata(&receipt_path)
        .map_err(|error| format!("stat config migration receipt: {error}"))?
        .file_type()
        .is_symlink()
    {
        return Err("config migration rollback refuses symlinks".to_string());
    }
    let receipt_bytes = std::fs::read(&receipt_path)
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
    if std::fs::symlink_metadata(path)
        .map_err(|error| format!("stat config: {error}"))?
        .file_type()
        .is_symlink()
        || std::fs::symlink_metadata(&backup)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err("config migration rollback refuses symlinks".to_string());
    }
    let current = std::fs::read(path).map_err(|error| format!("read config: {error}"))?;
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
    let guard = ConfigWriteGuard::acquire(path)?;
    guard.write_toml_preserving_minimal(new_content, default_content)
}

/// Lock-free core: the caller must already hold [`ConfigWriteGuard`].
///
/// Holding the lock around this (rather than only around the final byte write)
/// is what pulls the merge read below inside the critical section: the merge
/// decides the output from the current on-disk document, so reading it
/// unlocked let a concurrent writer's committed value be merged away.
fn write_toml_preserving_minimal_locked(
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
    write_atomic_with_backup_locked(path, &merged, None)
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
    // Resolve once: a retargeted caller alias must not redirect the locked write.
    // Backups therefore live beside the canonical target for every entry point.
    let guard = ConfigWriteGuard::acquire(path)?;
    write_atomic_with_backup_locked(guard.path(), content, expected_mtime)
}

/// Lock-free core: the caller must already hold [`ConfigWriteGuard`].
fn write_atomic_with_backup_locked(
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
            std::fs::copy(path, &bak).map_err(|error| {
                format!(
                    "cannot create config backup {} before updating {}: {error}",
                    bak.display(),
                    path.display()
                )
            })?;
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
    fn config_lock_key_resolves_symlink_alias_to_target() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().expect("home");
        let _home = HomeGuard::set(home.path());
        let dotfiles = home.path().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).expect("dotfiles");
        let target = dotfiles.join("agent.json");
        std::fs::write(&target, "{}\n").expect("target");
        let alias = home.path().join(".agent.json");
        symlink(&target, &alias).expect("alias");

        assert_eq!(
            config_migration_lock_path(&alias).expect("alias lock"),
            config_migration_lock_path(&target).expect("target lock")
        );
    }

    #[test]
    fn guarded_minimal_write_stays_bound_when_the_alias_is_retargeted() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());
        let target = home.path().join("target.toml");
        let other = home.path().join("other.toml");
        let alias = home.path().join("alias.toml");
        std::fs::write(&target, "value = 1\n").unwrap();
        std::fs::write(&other, "value = 99\n").unwrap();
        symlink(&target, &alias).unwrap();
        let guard = ConfigWriteGuard::acquire(&alias).unwrap();
        let replacement = home.path().join("replacement.toml");
        symlink(&other, &replacement).unwrap();
        std::fs::rename(&replacement, &alias).unwrap();

        guard
            .write_toml_preserving_minimal("value = 2\n", "")
            .unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "value = 2\n");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "value = 99\n");
        assert_eq!(std::fs::read_link(&alias).unwrap(), other);
        assert_eq!(
            std::fs::read_to_string(backup_path_for(&target).unwrap()).unwrap(),
            "value = 1\n"
        );
    }

    #[test]
    fn config_entry_points_share_the_canonical_target_and_backup() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());
        let target = home.path().join("target.toml");
        let alias = home.path().join("alias.toml");
        std::fs::write(&target, "max_ram_percent = 10\n").unwrap();
        symlink(&target, &alias).unwrap();
        let backup = backup_path_for(&target).unwrap();

        write_toml_preserving_minimal(&alias, "max_ram_percent = 20\n", "").unwrap();
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "max_ram_percent = 10\n"
        );
        crate::core::config::Config::update_global_at(&alias, |cfg| cfg.max_ram_percent = 30)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "max_ram_percent = 20\n"
        );
        let before_atomic = std::fs::read_to_string(&target).unwrap();
        write_atomic_with_backup(&alias, "max_ram_percent = 40\n").unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), before_atomic);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "max_ram_percent = 40\n"
        );
        assert!(
            std::fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!backup_path_for(&alias).unwrap().exists());
    }

    #[test]
    fn migration_and_rollback_share_one_journal_across_aliases() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().expect("home");
        let _home = HomeGuard::set(home.path());
        let target = home.path().join("agent.toml");
        let alias = home.path().join(".agent.toml");
        std::fs::write(&target, "terse_agent = \"full\"\n").expect("source");
        symlink(&target, &alias).expect("alias");
        let migrated = "compression_level = \"standard\"\n";
        write_atomic_config_migration(&alias, migrated).expect("migrate alias");
        write_atomic_config_migration(&target, migrated).expect("idempotent target");
        assert!(
            !alias
                .with_file_name(".agent.toml.v4-migration.json")
                .exists()
        );
        rollback_config_migration(&target).expect("rollback target");
        assert_eq!(
            std::fs::read_to_string(&target).expect("restored"),
            "terse_agent = \"full\"\n"
        );
        assert!(
            std::fs::symlink_metadata(alias)
                .expect("alias metadata")
                .file_type()
                .is_symlink()
        );
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
