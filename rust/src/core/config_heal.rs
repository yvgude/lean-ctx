//! Heal a `config.toml` that got stranded in the DATA dir (#594).
//!
//! When an older lean-ctx baked `LEAN_CTX_DATA_DIR` into an editor's MCP server
//! `env`, that process ran in single-dir mode and wrote `config.toml` into the
//! data dir (`$XDG_DATA_HOME/lean-ctx`), while the terminal CLI kept config in
//! `$XDG_CONFIG_HOME/lean-ctx`. The resolver now keeps both on the config dir
//! (see `core::paths::single_dir_override`), but a `config.toml` that was already
//! written into the data dir would be silently ignored from then on.
//!
//! This module relocates it to the canonical config dir, **losslessly**:
//! - canonical config absent/empty → the stray copy is *adopted* as the real
//!   config, so the user's settings survive the switch;
//! - canonical config already present → the CLI-authored config wins and the
//!   stray copy is moved aside to `config.toml.superseded` (never deleted).
//!
//! Idempotent and safe: it only ever touches the user's own config/data dirs,
//! moves with an atomic `rename` (copy+remove fallback across filesystems), and
//! becomes a no-op once the stray file is gone.

use std::path::{Path, PathBuf};

/// What the heal pass did with the stray data-dir `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HealAction {
    /// The stray copy became the canonical config (canonical was absent/empty).
    Adopted,
    /// Canonical config already existed; the stray copy was moved aside.
    Superseded,
}

/// Outcome of a config-heal pass, surfaced through setup / `doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigHealReport {
    pub action: HealAction,
    /// The stray `config.toml` that was relocated out of the data dir.
    pub from: PathBuf,
    /// Where it landed: the canonical config (adopted) or the `.superseded` copy.
    pub to: PathBuf,
}

/// Relocate a stray data-dir `config.toml` into the canonical config dir.
/// Returns `None` when there is nothing to do (single-dir layout, or no stray
/// config in the data dir).
pub(crate) fn heal() -> Option<ConfigHealReport> {
    let config_dir = crate::core::paths::config_dir().ok()?;
    let data_dir = crate::core::paths::data_dir().ok()?;
    heal_between(&config_dir, &data_dir)
}

/// Relocate a stale `config.toml` from the legacy `~/.lean-ctx` dir to the
/// canonical config dir (GH #1421). Only acts when `~/.lean-ctx` is NOT the
/// active config dir — i.e. the install has already migrated to XDG but the
/// old config file was left behind. Uses the same adopt/supersede logic as
/// [`heal`].
pub(crate) fn heal_legacy_config() -> Option<ConfigHealReport> {
    // GH #2007: with LEAN_CTX_CONFIG_DIR pointing at a scratch dir, the real
    // ~/.lean-ctx/config.toml looked stale and was moved aside.
    if crate::core::paths::dir_override_active() {
        return None;
    }
    let config_dir = crate::core::paths::config_dir().ok()?;
    let legacy_dir = dirs::home_dir()?.join(".lean-ctx");
    if legacy_dir == config_dir {
        return None;
    }
    heal_between(&config_dir, &legacy_dir)
}

/// Relocate a stale `config.toml` from `$XDG_CONFIG_HOME/lean-ctx` when the
/// active config dir is elsewhere (e.g. a single-dir `~/.lean-ctx` install).
/// GH #1421: prevents user confusion from editing the "wrong" config file.
pub(crate) fn heal_xdg_stale_config() -> Option<ConfigHealReport> {
    // GH #2007: an override never makes the default XDG config stale.
    if crate::core::paths::dir_override_active() {
        return None;
    }
    let config_dir = crate::core::paths::config_dir().ok()?;
    let xdg_config = crate::core::paths::xdg_config_lean_ctx_dir()?;
    if xdg_config == config_dir {
        return None;
    }
    heal_between(&config_dir, &xdg_config)
}

/// Heal ALL known stale config locations in a single pass (GH #1421).
/// Called from startup heal paths to ensure at most one `config.toml` exists.
pub(crate) fn heal_all() -> Vec<ConfigHealReport> {
    let mut reports = Vec::new();
    if let Some(r) = heal() {
        reports.push(r);
    }
    if let Some(r) = heal_legacy_config() {
        reports.push(r);
    }
    if let Some(r) = heal_xdg_stale_config() {
        reports.push(r);
    }
    reports
}

/// Read-only check used by `doctor`: returns the stray data-dir `config.toml`
/// that [`heal`] would relocate, or `None` when the CLI and the MCP server
/// already resolve the same config (no divergence).
pub(crate) fn pending() -> Option<PathBuf> {
    let config_dir = crate::core::paths::config_dir().ok()?;
    let data_dir = crate::core::paths::data_dir().ok()?;
    if config_dir == data_dir {
        return None;
    }
    let stray = data_dir.join("config.toml");
    file_has_content(&stray).then_some(stray)
}

/// Pure core of [`heal`], parameterized for hermetic tests.
fn heal_between(config_dir: &Path, data_dir: &Path) -> Option<ConfigHealReport> {
    // Single-dir layout (legacy/mixed/explicit pin): config legitimately lives
    // in that one directory and is not stranded.
    if config_dir == data_dir {
        return None;
    }

    let stray = data_dir.join("config.toml");
    if !file_has_content(&stray) {
        return None;
    }

    std::fs::create_dir_all(config_dir).ok()?;
    crate::core::data_dir::ensure_dir_permissions(config_dir);
    let canonical = config_dir.join("config.toml");

    if file_has_content(&canonical) {
        // The CLI-authored config is the source of truth; preserve the stray
        // copy next to it (lossless) instead of dropping it.
        let aside = data_dir.join("config.toml.superseded");
        move_overwrite(&stray, &aside).ok()?;
        return Some(ConfigHealReport {
            action: HealAction::Superseded,
            from: stray,
            to: aside,
        });
    }

    // Canonical config is absent/empty → adopt the stray copy so the user's
    // settings keep working after config resolves to the config dir.
    move_overwrite(&stray, &canonical).ok()?;
    Some(ConfigHealReport {
        action: HealAction::Adopted,
        from: stray,
        to: canonical,
    })
}

/// True when `p` is a readable file with non-whitespace content.
fn file_has_content(p: &Path) -> bool {
    std::fs::read_to_string(p).is_ok_and(|s| !s.trim().is_empty())
}

/// Move `from` onto `to`, replacing any existing file. Atomic `rename` first,
/// with a copy+remove fallback across filesystems; the source is removed only
/// after a successful copy, so an interrupted move never loses data.
fn move_overwrite(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn adopts_stray_when_canonical_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        write(&data_dir.join("config.toml"), "path_jail = false\n");

        let report = heal_between(&config_dir, &data_dir).expect("should adopt");

        assert_eq!(report.action, HealAction::Adopted);
        assert_eq!(
            std::fs::read_to_string(config_dir.join("config.toml")).unwrap(),
            "path_jail = false\n"
        );
        assert!(
            !data_dir.join("config.toml").exists(),
            "stray copy must be relocated out of the data dir"
        );
    }

    #[test]
    fn supersedes_stray_when_canonical_present() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        write(&config_dir.join("config.toml"), "ultra_compact = true\n");
        write(&data_dir.join("config.toml"), "STALE\n");

        let report = heal_between(&config_dir, &data_dir).expect("should supersede");

        assert_eq!(report.action, HealAction::Superseded);
        // The CLI-authored canonical config is untouched.
        assert_eq!(
            std::fs::read_to_string(config_dir.join("config.toml")).unwrap(),
            "ultra_compact = true\n"
        );
        // The stray copy is preserved aside (lossless), not deleted.
        assert!(!data_dir.join("config.toml").exists());
        assert_eq!(
            std::fs::read_to_string(data_dir.join("config.toml.superseded")).unwrap(),
            "STALE\n"
        );
    }

    #[test]
    fn noop_when_no_stray_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        assert_eq!(heal_between(&config_dir, &data_dir), None);
    }

    #[test]
    fn noop_for_single_dir_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lean-ctx");
        write(&dir.join("config.toml"), "x = 1\n");
        // Same dir for config and data → nothing is stranded.
        assert_eq!(heal_between(&dir, &dir), None);
    }

    #[test]
    fn heal_legacy_adopts_when_canonical_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let legacy_dir = tmp.path().join("legacy");
        write(&legacy_dir.join("config.toml"), "legacy = true\n");

        let report = heal_between(&config_dir, &legacy_dir).expect("should adopt legacy");
        assert_eq!(report.action, HealAction::Adopted);
        assert_eq!(
            std::fs::read_to_string(config_dir.join("config.toml")).unwrap(),
            "legacy = true\n"
        );
        assert!(!legacy_dir.join("config.toml").exists());
    }

    #[test]
    fn heal_legacy_supersedes_when_canonical_present() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let legacy_dir = tmp.path().join("legacy");
        write(&config_dir.join("config.toml"), "canonical = true\n");
        write(&legacy_dir.join("config.toml"), "stale_legacy = true\n");

        let report = heal_between(&config_dir, &legacy_dir).expect("should supersede");
        assert_eq!(report.action, HealAction::Superseded);
        assert_eq!(
            std::fs::read_to_string(config_dir.join("config.toml")).unwrap(),
            "canonical = true\n"
        );
        assert!(
            legacy_dir.join("config.toml.superseded").exists(),
            "stale legacy copy must be preserved as .superseded"
        );
    }

    // GH #2007: a process started with LEAN_CTX_CONFIG_DIR / _DATA_DIR pointing
    // at a scratch dir renamed the real ~/.lean-ctx/config.toml to
    // config.toml.superseded. An override applies to that process only.
    #[cfg(unix)]
    #[test]
    fn heal_all_leaves_default_locations_alone_under_a_dir_override() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let xdg_config = tmp.path().join("xc");
        let scratch = tmp.path().join("scratch");
        write(&home.join(".lean-ctx/config.toml"), "real_legacy = true\n");
        write(
            &xdg_config.join("lean-ctx/config.toml"),
            "real_xdg = true\n",
        );
        write(&scratch.join("config/config.toml"), "scratch = true\n");
        let names = [
            "HOME",
            "XDG_CONFIG_HOME",
            "LEAN_CTX_CONFIG_DIR",
            "LEAN_CTX_DATA_DIR",
            "LEAN_CTX_STATE_DIR",
            "LEAN_CTX_CACHE_DIR",
        ];
        let saved: Vec<_> = names.iter().map(|k| (*k, std::env::var_os(k))).collect();
        crate::test_env::set_var("HOME", &home);
        crate::test_env::set_var("XDG_CONFIG_HOME", &xdg_config);
        crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", scratch.join("config"));
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", scratch.join("data"));
        crate::test_env::remove_var("LEAN_CTX_STATE_DIR");
        crate::test_env::remove_var("LEAN_CTX_CACHE_DIR");

        let legacy = heal_legacy_config();
        let xdg = heal_xdg_stale_config();

        for (k, v) in saved {
            match v {
                Some(value) => crate::test_env::set_var(k, value),
                None => crate::test_env::remove_var(k),
            }
        }
        assert_eq!(legacy, None);
        assert_eq!(xdg, None);
        assert_eq!(
            std::fs::read_to_string(home.join(".lean-ctx/config.toml")).unwrap(),
            "real_legacy = true\n"
        );
        assert!(!home.join(".lean-ctx/config.toml.superseded").exists());
        assert_eq!(
            std::fs::read_to_string(xdg_config.join("lean-ctx/config.toml")).unwrap(),
            "real_xdg = true\n"
        );
    }

    #[test]
    fn is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        write(&data_dir.join("config.toml"), "k = 1\n");

        assert!(heal_between(&config_dir, &data_dir).is_some());
        // Second run: stray is gone → no-op.
        assert_eq!(heal_between(&config_dir, &data_dir), None);
    }
}
