// SPDX-License-Identifier: Apache-2.0
//! #596: write THROUGH a user-managed symlink to its real (in-`$HOME`) target,
//! reject targets that escape `$HOME`, and make `ensure_dir` tolerant of
//! symlinked directories. Unix-only (POSIX symlinks + `$HOME` override).

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
    crate::core::config::Config::update_global_at(&alias, |cfg| cfg.max_ram_percent = 30).unwrap();
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
