use std::path::PathBuf;

/// Markers that identify a legacy / pre-split install whose categories must
/// stay collapsed onto one directory (GH #408).
///
/// Deliberately excludes `config.toml`: after the XDG split it legitimately
/// lives alone in the config dir, so treating it as a data marker would
/// re-collapse a clean four-dir install back onto the config dir. These are all
/// real data/state artifacts that only exist in a pre-split (mixed) install.
const DATA_MARKERS: &[&str] = &["stats.json", "sessions", "vectors", "graphs", "knowledge"];

/// Resolve the lean-ctx data directory.
///
/// Priority order (backward-compatible XDG split, GH #408):
/// 1. `LEAN_CTX_DATA_DIR` env var (explicit override)
/// 2. `~/.lean-ctx` if it has actual data (legacy installs)
/// 3. `$XDG_CONFIG_HOME/lean-ctx` if it has actual data (pre-split installs that
///    mixed data into the config dir — kept in place, never silently moved)
/// 4. `$XDG_DATA_HOME/lean-ctx` (default `~/.local/share/lean-ctx`) for fresh
///    installs, so the config dir holds only config and stays RO-sandbox-safe.
///
/// An empty `~/.lean-ctx/` directory does NOT trigger legacy mode — this prevents
/// data directory splits when setup creates the dir before MCP writes stats.
pub fn lean_ctx_data_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("LEAN_CTX_DATA_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            ensure_dir_permissions(&p);
            return Ok(p);
        }
    }

    // Test sandbox (GL #512): without this, any unit test that triggers a
    // store write (stats, savings ledger, context ledger, heatmap, ...)
    // silently pollutes the developer's real ~/.lean-ctx — bounce events from
    // test fixtures showed up as "today 61%" on the user dashboard. Tests that
    // set LEAN_CTX_DATA_DIR keep full control (handled above); everyone else
    // lands in a per-process temp dir and physically cannot touch real data.
    #[cfg(test)]
    {
        Ok(test_sandbox_dir())
    }

    #[cfg(not(test))]
    {
        resolve_home_data_dir()
    }
}

/// Per-process temp sandbox used as the default data dir under `#[cfg(test)]`,
/// so any store write from a test fixture lands in a throwaway dir instead of
/// the developer's real `~/.lean-ctx` (GL #512). Tests that need an *empty*,
/// private dir should use [`isolated_data_dir`] instead (the shared sandbox is
/// not empty: parallel tests write into it).
#[cfg(test)]
pub(crate) fn test_sandbox_dir() -> PathBuf {
    static TEST_SANDBOX: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    TEST_SANDBOX
        .get_or_init(|| {
            let d = std::env::temp_dir().join(format!("lean-ctx-testdata-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&d);
            d
        })
        .clone()
}

/// Home-based resolution (legacy `~/.lean-ctx` / mixed vs XDG). Split out so the
/// priority rules stay unit-testable despite the test sandbox above.
///
/// The legacy/mixed back-compat decision lives in exactly ONE place,
/// [`crate::core::paths::single_dir_override`], so the data dir can never
/// disagree with config/state/cache (which all resolve through it): a legacy
/// `~/.lean-ctx` or mixed `$XDG_CONFIG_HOME/lean-ctx` install wins only while it
/// still holds data markers. Once `doctor --fix` has split it out, every
/// category — data included — flips to its typed XDG dir, and a leftover
/// (marker-free) `~/.lean-ctx` is no longer silently re-adopted (GH #436).
fn resolve_home_data_dir() -> Result<PathBuf, String> {
    // 1./2. Legacy or mixed single-dir install that still holds data → keep it
    //       in place (back-compat). `LEAN_CTX_DATA_DIR` is handled by the caller,
    //       and `single_dir_override` honors it too.
    if let Some(dir) = crate::core::paths::single_dir_override() {
        ensure_dir_permissions(&dir);
        return Ok(dir);
    }

    // 3. Fresh / fully-split install: default DATA to `$XDG_DATA_HOME/lean-ctx`
    //    (GH #408) so the config dir (`$XDG_CONFIG_HOME`) holds only config and
    //    stays read-only-sandbox-safe.
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let xdg_data = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map_or_else(|| home.join(".local").join("share"), PathBuf::from);
    let data_dir = xdg_data.join("lean-ctx");
    ensure_dir_permissions(&data_dir);
    Ok(data_dir)
}

pub(crate) fn has_data_files(dir: &std::path::Path) -> bool {
    DATA_MARKERS.iter().any(|f| marker_has_data(&dir.join(f)))
}

/// A marker counts only when it actually carries data: a non-empty file, or a
/// directory with at least one entry. An empty `sessions/` (a stray `mkdir`, a
/// half-removed residue, a backup-restore artifact) must NOT collapse the whole
/// layout onto a directory that holds no real data (GL #623 / #625).
fn marker_has_data(path: &std::path::Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => std::fs::read_dir(path).is_ok_and(|mut it| it.next().is_some()),
        Ok(m) => m.len() > 0,
        Err(_) => false,
    }
}

/// Returns all known data directories that contain stats data, in resolution
/// priority order (legacy → mixed config → XDG data). Used by the dual-dir
/// consolidation ([`crate::core::data_consolidate`]) and doctor diagnostics.
///
/// `$XDG_DATA_HOME/lean-ctx` is included (GH #414): after the #408 default flip
/// a fresh install writes stats there, so a user who *also* has a legacy/mixed
/// tree has the split that the consolidation must detect and merge.
pub fn all_data_dirs_with_stats() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let legacy = home.join(".lean-ctx");
        if legacy.join("stats.json").exists() {
            dirs.push(legacy);
        }
        let xdg_config = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map_or_else(|| home.join(".config"), PathBuf::from)
            .join("lean-ctx");
        if xdg_config.join("stats.json").exists() && !dirs.contains(&xdg_config) {
            dirs.push(xdg_config);
        }
        let xdg_data = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map_or_else(|| home.join(".local").join("share"), PathBuf::from)
            .join("lean-ctx");
        if xdg_data.join("stats.json").exists() && !dirs.contains(&xdg_data) {
            dirs.push(xdg_data);
        }
    }
    dirs
}

#[cfg(unix)]
pub(crate) fn ensure_dir_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if path.is_dir() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
}

#[cfg(not(unix))]
pub(crate) fn ensure_dir_permissions(_path: &std::path::Path) {}

pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let mutex = LOCK.get_or_init(|| Mutex::new(()));
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII data-dir isolation for tests (GL #556): holds `test_env_lock` for
/// the guard's lifetime, points `LEAN_CTX_DATA_DIR` at a fresh temp dir and
/// restores the env on drop — even on panic, so a failing test cannot leak
/// the override into others. Use this instead of hand-rolled
/// `set_var`/`remove_var` pairs whenever a test needs an empty, private
/// data dir (the shared per-process sandbox is NOT empty: parallel tests
/// write stores like feedback, bandit and sessions into it).
#[cfg(test)]
pub struct IsolatedDataDir {
    tmp: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl IsolatedDataDir {
    pub fn path(&self) -> &std::path::Path {
        self.tmp.path()
    }
}

/// Category env vars pointed at the isolated temp dir so all four XDG
/// categories (config/data/state/cache) collapse onto it in tests (GH #408).
#[cfg(test)]
const ISOLATED_ENV_VARS: &[&str] = &[
    "LEAN_CTX_DATA_DIR",
    "LEAN_CTX_CONFIG_DIR",
    "LEAN_CTX_STATE_DIR",
    "LEAN_CTX_CACHE_DIR",
];

#[cfg(test)]
impl Drop for IsolatedDataDir {
    fn drop(&mut self) {
        // Struct Drop runs before field drops, so the env is restored while
        // the lock is still held.
        for var in ISOLATED_ENV_VARS {
            crate::test_env::remove_var(var);
        }
    }
}

#[cfg(test)]
pub fn isolated_data_dir() -> IsolatedDataDir {
    let guard = test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir for isolated data dir");
    for var in ISOLATED_ENV_VARS {
        crate::test_env::set_var(var, tmp.path());
    }
    IsolatedDataDir { tmp, _guard: guard }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_data_files_empty_dir() {
        let dir = std::env::temp_dir().join("test_data_dir_empty");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        assert!(!has_data_files(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn has_data_files_with_stats() {
        let dir = std::env::temp_dir().join("test_data_dir_stats");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("stats.json"), "{}").unwrap();
        assert!(has_data_files(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn has_data_files_ignores_config_only() {
        // GH #408: config.toml (+ hooks) alone must NOT mark a dir as "has data",
        // otherwise a clean post-split config dir would re-collapse the four-dir
        // layout back onto itself via single_dir_override.
        let dir = std::env::temp_dir().join("test_data_dir_config_only");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("config.toml"), "").unwrap();
        std::fs::write(dir.join("env.sh"), "").unwrap();
        assert!(!has_data_files(&dir), "config-only dir is not a data dir");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_install_defaults_data_to_xdg_data_home() {
        // GH #408 flip: with no legacy/mixed data, a fresh install resolves DATA
        // to $XDG_DATA_HOME/lean-ctx (not the config dir).
        let _lock = test_env_lock();
        let xdg_config = tempfile::tempdir().unwrap();
        let xdg_data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", "");
        crate::test_env::set_var("XDG_CONFIG_HOME", xdg_config.path());
        crate::test_env::set_var("XDG_DATA_HOME", xdg_data.path());

        let result = resolve_home_data_dir().unwrap();

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        crate::test_env::remove_var("XDG_CONFIG_HOME");
        crate::test_env::remove_var("XDG_DATA_HOME");

        // A real `~/.lean-ctx` (legacy) would correctly take precedence; only
        // assert the fresh default when it is absent (always true on CI).
        let legacy = dirs::home_dir().unwrap().join(".lean-ctx");
        if !legacy.exists() {
            assert_eq!(result, xdg_data.path().join("lean-ctx"));
        }
    }

    #[test]
    fn empty_marker_dir_does_not_count() {
        // GL #623/#625: an empty `sessions/` (a stray mkdir, a half-removed
        // residue) must not flip the whole layout — only a marker carrying real
        // data counts.
        let dir = std::env::temp_dir().join("test_data_dir_empty_marker");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("sessions"));
        assert!(!has_data_files(&dir), "empty marker dir must not count");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_marker_file_does_not_count() {
        // A zero-byte `stats.json` is not real data either (GL #625).
        let dir = std::env::temp_dir().join("test_data_dir_empty_marker_file");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("stats.json"), "").unwrap();
        assert!(!has_data_files(&dir), "empty marker file must not count");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn has_data_files_with_sessions() {
        let dir = std::env::temp_dir().join("test_data_dir_sessions");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("sessions"));
        // A non-empty sessions/ is real data (GL #625: empty dirs no longer count).
        std::fs::write(dir.join("sessions").join("s1.json"), "{}").unwrap();
        assert!(has_data_files(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lean_ctx_data_dir_env_override() {
        let _lock = test_env_lock();
        let dir = std::env::temp_dir().join("test_data_dir_env");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        let result = lean_ctx_data_dir().unwrap();
        assert_eq!(result, dir);
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn has_data_files_is_false_for_empty_dir() {
        let dir = std::env::temp_dir().join("test_data_dir_no_data");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("random.txt"), "not a marker").unwrap();
        assert!(!has_data_files(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn xdg_override_with_data_wins() {
        let _lock = test_env_lock();

        let xdg_base = std::env::temp_dir().join("test_xdg_override_wins");
        let _ = std::fs::remove_dir_all(&xdg_base);
        let xdg_dir = xdg_base.join("lean-ctx");
        let _ = std::fs::create_dir_all(&xdg_dir);
        std::fs::write(xdg_dir.join("stats.json"), r#"{"total_commands":1}"#).unwrap();

        crate::test_env::set_var("LEAN_CTX_DATA_DIR", "");
        crate::test_env::set_var("XDG_CONFIG_HOME", xdg_base.to_str().unwrap());

        // Calls the home resolver directly: lean_ctx_data_dir() is sandboxed
        // under cfg(test) (GL #512) and would short-circuit before XDG logic.
        let result = resolve_home_data_dir().unwrap();

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        crate::test_env::remove_var("XDG_CONFIG_HOME");

        let home = dirs::home_dir().unwrap();
        let legacy = home.join(".lean-ctx");
        if !has_data_files(&legacy) {
            assert_eq!(
                result, xdg_dir,
                "XDG with data should win when legacy has no data"
            );
        }

        let _ = std::fs::remove_dir_all(&xdg_base);
    }

    #[cfg(unix)]
    fn restore_env(key: &str, val: Option<std::ffi::OsString>) {
        match val {
            Some(v) => crate::test_env::set_var(key, v),
            None => crate::test_env::remove_var(key),
        }
    }

    #[cfg(unix)]
    #[test]
    fn markerless_legacy_dir_does_not_win() {
        // GH #436: after `doctor --fix` moves data to XDG, `~/.lean-ctx` lingers
        // (runtime leftovers) but holds no data markers. It must NOT keep being
        // re-adopted as the data dir — data must flip to $XDG_DATA_HOME/lean-ctx,
        // exactly like config/state/cache already do via single_dir_override.
        let _lock = test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let xdg_data = tmp.path().join("xdg-data");
        let legacy = home.join(".lean-ctx");
        std::fs::create_dir_all(&legacy).unwrap();
        // A runtime leftover (daemon.pid) is not a data marker.
        std::fs::write(legacy.join("daemon.pid"), "123").unwrap();

        let saved_home = std::env::var_os("HOME");
        let saved_config = std::env::var_os("XDG_CONFIG_HOME");
        let saved_data = std::env::var_os("XDG_DATA_HOME");
        crate::test_env::set_var("HOME", &home);
        crate::test_env::remove_var("XDG_CONFIG_HOME");
        crate::test_env::set_var("XDG_DATA_HOME", &xdg_data);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", "");

        let result = resolve_home_data_dir().unwrap();

        // Restore before asserting so a failure can't leak env into other tests.
        restore_env("HOME", saved_home);
        restore_env("XDG_CONFIG_HOME", saved_config);
        restore_env("XDG_DATA_HOME", saved_data);
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");

        assert_eq!(
            result,
            xdg_data.join("lean-ctx"),
            "marker-free legacy dir must not be re-adopted as the data dir"
        );
    }
}
