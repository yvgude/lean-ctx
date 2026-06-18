//! Typed XDG base-directory resolvers for lean-ctx (GH #408 / GL #602).
//!
//! Historically every lean-ctx file joined onto a single [`lean_ctx_data_dir`]
//! rooted at `$XDG_CONFIG_HOME/lean-ctx`, mixing `config.toml` with 30+ runtime
//! data files (sessions, vectors, graphs, events, logs, caches). That violates
//! the XDG Base Directory Spec and makes a read-only config sandbox impossible.
//!
//! This module introduces one typed resolver per XDG category so call-sites can
//! migrate to the correct base over the following phases (GL #603/#604/#606/#607).
//!
//! ## Backward compatibility (single-dir mode)
//!
//! Existing installs MUST NOT split silently. `single_dir_override` returns
//! `Some(dir)` when `LEAN_CTX_DATA_DIR` is set or a legacy/mixed install with
//! data exists; in that case every category resolves to that one directory —
//! byte-for-byte today's behavior. The real per-category split only applies to
//! fresh installs (and, later, on-demand via `lean-ctx doctor --fix`).
//!
//! ## `data_dir()` and the fresh-install flip
//!
//! [`data_dir`] delegates to [`lean_ctx_data_dir`]. Config (`config.toml` +
//! hooks) and the runtime STATE/CACHE files were migrated onto [`config_dir`],
//! [`state_dir`] and [`cache_dir`] (GL #603/#604) so that, since GL #606, the
//! data resolver defaults fresh installs to `$XDG_DATA_HOME/lean-ctx` without
//! scattering config or state into the data dir. Legacy and pre-split mixed
//! installs keep resolving every category to their existing single directory.
//!
//! Determinism (#498): every resolver is a pure function of environment + HOME;
//! no timestamps, counters or randomness.

use std::path::{Path, PathBuf};

use super::data_dir::{ensure_dir_permissions, has_data_files, lean_ctx_data_dir};

/// Reads a directory path from `name`, treating empty/whitespace as unset.
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Resolves an XDG base directory (e.g. `~/.config`), honoring the `env_name`
/// override and falling back to `$HOME/<home_fallback>`. Returns the base only;
/// callers append `lean-ctx`.
fn xdg_base(env_name: &str, home_fallback: &str) -> Result<PathBuf, String> {
    if let Some(p) = env_path(env_name) {
        return Ok(p);
    }
    dirs::home_dir()
        .map(|h| h.join(home_fallback))
        .ok_or_else(|| "Cannot determine home directory".to_string())
}

/// Pure resolution order for a category: explicit override > single-dir
/// backward-compat > XDG split default (`<base>/lean-ctx`).
fn resolve(
    category_override: Option<PathBuf>,
    single: Option<PathBuf>,
    xdg_base_dir: &Path,
) -> PathBuf {
    category_override
        .or(single)
        .unwrap_or_else(|| xdg_base_dir.join("lean-ctx"))
}

/// Returns the single directory that ALL categories must collapse onto for
/// backward compatibility, or `None` for a fresh install that may split.
///
/// `Some` when `LEAN_CTX_DATA_DIR` is set, or a legacy `~/.lean-ctx` / mixed
/// `$XDG_CONFIG_HOME/lean-ctx` install already holds data. An empty directory
/// does NOT trigger single-dir mode (matches [`lean_ctx_data_dir`] semantics).
pub(crate) fn single_dir_override() -> Option<PathBuf> {
    if let Some(p) = env_path("LEAN_CTX_DATA_DIR") {
        return Some(p);
    }
    let home = dirs::home_dir()?;
    let xdg_config_base = xdg_base("XDG_CONFIG_HOME", ".config").ok()?;
    single_dir_override_fs(&home, &xdg_config_base)
}

/// Filesystem half of [`single_dir_override`], parameterized for hermetic tests.
fn single_dir_override_fs(home: &Path, xdg_config_base: &Path) -> Option<PathBuf> {
    // A committed XDG install is the single source of truth: never re-collapse
    // it onto a stray legacy/mixed data marker (GL #623). The pin lives next to
    // the mixed probe below, so the two reads always agree on `xdg_config_base`.
    if crate::core::layout_pin::is_xdg_pinned_in(xdg_config_base) {
        return None;
    }
    let legacy = home.join(".lean-ctx");
    if legacy.exists() && has_data_files(&legacy) {
        return Some(legacy);
    }
    let mixed = xdg_config_base.join("lean-ctx");
    if mixed.exists() && has_data_files(&mixed) {
        return Some(mixed);
    }
    None
}

/// Shared resolver for the config/state/cache categories.
// Under `#[cfg(test)]` the body always succeeds (returns the sandbox); the
// fallible XDG resolution only runs in real builds.
#[cfg_attr(test, allow(clippy::unnecessary_wraps))]
fn category_dir(cat_env: &str, xdg_env: &str, home_fallback: &str) -> Result<PathBuf, String> {
    let category_override = env_path(cat_env);

    // A category override always wins, even under #[cfg(test)] — this lets the
    // RO-config sandbox integration test point each category at a temp dir.
    #[cfg(test)]
    {
        if let Some(p) = category_override {
            ensure_dir_permissions(&p);
            return Ok(p);
        }
        // Unit tests share one per-process sandbox so stray store writes can't
        // escape to a developer's real dirs. The branch logic itself is covered
        // by the pure `resolve` / `single_dir_override_fs` tests below.
        let _ = (xdg_env, home_fallback);
        Ok(super::data_dir::test_sandbox_dir())
    }
    #[cfg(not(test))]
    {
        let base = xdg_base(xdg_env, home_fallback)?;
        let dir = resolve(category_override, single_dir_override(), &base);
        ensure_dir_permissions(&dir);
        Ok(dir)
    }
}

/// Config directory — `config.toml`, shell hooks, `env.sh`. RO-safe.
/// Override: `LEAN_CTX_CONFIG_DIR`; default `$XDG_CONFIG_HOME/lean-ctx`.
pub fn config_dir() -> Result<PathBuf, String> {
    category_dir("LEAN_CTX_CONFIG_DIR", "XDG_CONFIG_HOME", ".config")
}

/// Data directory — sessions, vectors, graphs, knowledge, archives, memory.
///
/// Delegates to [`lean_ctx_data_dir`], which since GL #606 defaults fresh
/// installs to `$XDG_DATA_HOME/lean-ctx`. Legacy `~/.lean-ctx` and pre-split
/// mixed `$XDG_CONFIG_HOME/lean-ctx` installs (and an explicit
/// `LEAN_CTX_DATA_DIR`) continue to resolve in place for backward compatibility.
pub fn data_dir() -> Result<PathBuf, String> {
    lean_ctx_data_dir()
}

/// State directory — events, stats, logs, journals, ledgers, captured keys.
/// Override: `LEAN_CTX_STATE_DIR`; default `$XDG_STATE_HOME/lean-ctx`.
pub fn state_dir() -> Result<PathBuf, String> {
    category_dir("LEAN_CTX_STATE_DIR", "XDG_STATE_HOME", ".local/state")
}

/// Cache directory — semantic cache, models, learned patterns. tmpfs-safe.
/// Override: `LEAN_CTX_CACHE_DIR`; default `$XDG_CACHE_HOME/lean-ctx`.
pub fn cache_dir() -> Result<PathBuf, String> {
    category_dir("LEAN_CTX_CACHE_DIR", "XDG_CACHE_HOME", ".cache")
}

/// Runtime directory — `daemon.pid`, `daemon.sock`. `$XDG_RUNTIME_DIR/lean-ctx`.
///
/// When `XDG_RUNTIME_DIR` is unset (common on macOS), falls back to
/// [`state_dir`] so runtime files stay in a private, writable, non-config path
/// rather than a world-readable temp location.
pub fn runtime_dir() -> Result<PathBuf, String> {
    if let Some(base) = env_path("XDG_RUNTIME_DIR") {
        return Ok(base.join("lean-ctx"));
    }
    state_dir()
}

/// Raw per-category target dir for the four XDG categories, **bypassing**
/// single-dir back-compat and the test sandbox. Honors an explicit
/// `LEAN_CTX_<CAT>_DIR` override, otherwise `<XDG base>/lean-ctx`.
///
/// `category_dir`/[`data_dir`] deliberately collapse onto one directory for a
/// legacy/mixed install; the `doctor --fix` migration (GH #408) needs to know
/// where each category SHOULD live *after* a split, which is what this returns.
fn raw_category_dir(cat_env: &str, xdg_env: &str, home_fallback: &str) -> Result<PathBuf, String> {
    if let Some(p) = env_path(cat_env) {
        return Ok(p);
    }
    Ok(xdg_base(xdg_env, home_fallback)?.join("lean-ctx"))
}

/// Split target for the config category (`$XDG_CONFIG_HOME/lean-ctx`).
pub(crate) fn config_split_target() -> Result<PathBuf, String> {
    raw_category_dir("LEAN_CTX_CONFIG_DIR", "XDG_CONFIG_HOME", ".config")
}

/// `$XDG_CONFIG_HOME/lean-ctx` (or `~/.config/lean-ctx`) — where `config.toml`
/// and the layout pin (`layout.toml`) live. Resolved through the XDG config base
/// only, bypassing single-dir collapse, so the pin that governs that collapse
/// never depends on it (GL #623). `None` only when HOME cannot be determined.
pub(crate) fn xdg_config_lean_ctx_dir() -> Option<PathBuf> {
    xdg_base("XDG_CONFIG_HOME", ".config")
        .ok()
        .map(|b| b.join("lean-ctx"))
}

/// Split target for the data category (`$XDG_DATA_HOME/lean-ctx`).
pub(crate) fn data_split_target() -> Result<PathBuf, String> {
    raw_category_dir("LEAN_CTX_DATA_DIR", "XDG_DATA_HOME", ".local/share")
}

/// Split target for the state category (`$XDG_STATE_HOME/lean-ctx`).
pub(crate) fn state_split_target() -> Result<PathBuf, String> {
    raw_category_dir("LEAN_CTX_STATE_DIR", "XDG_STATE_HOME", ".local/state")
}

/// Split target for the cache category (`$XDG_CACHE_HOME/lean-ctx`).
pub(crate) fn cache_split_target() -> Result<PathBuf, String> {
    raw_category_dir("LEAN_CTX_CACHE_DIR", "XDG_CACHE_HOME", ".cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_override_then_single_then_xdg() {
        let over = PathBuf::from("/over/ride");
        let single = PathBuf::from("/single/dir");
        let base = PathBuf::from("/xdg/base");

        assert_eq!(
            resolve(Some(over.clone()), Some(single.clone()), &base),
            over
        );
        assert_eq!(resolve(None, Some(single.clone()), &base), single);
        assert_eq!(
            resolve(None, None, &base),
            PathBuf::from("/xdg/base/lean-ctx")
        );
    }

    #[test]
    fn single_dir_fs_detects_legacy_with_data() {
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let legacy = home.path().join(".lean-ctx");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("stats.json"), "{}").unwrap();

        assert_eq!(
            single_dir_override_fs(home.path(), xdg.path()),
            Some(legacy)
        );
    }

    #[test]
    fn single_dir_fs_detects_mixed_with_data() {
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let mixed = xdg.path().join("lean-ctx");
        std::fs::create_dir_all(&mixed).unwrap();
        // A real data marker (stats.json) — NOT config.toml, which post-split
        // lives alone in the config dir and must not trigger single-dir mode.
        std::fs::write(mixed.join("stats.json"), "{}").unwrap();

        assert_eq!(single_dir_override_fs(home.path(), xdg.path()), Some(mixed));
    }

    #[test]
    fn single_dir_fs_ignores_config_only_dir() {
        // GH #408: a clean post-split config dir (only config.toml + hooks) must
        // NOT collapse the four-dir layout.
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let mixed = xdg.path().join("lean-ctx");
        std::fs::create_dir_all(&mixed).unwrap();
        std::fs::write(mixed.join("config.toml"), "").unwrap();
        std::fs::write(mixed.join("shell-hook.zsh"), "").unwrap();

        assert_eq!(single_dir_override_fs(home.path(), xdg.path()), None);
    }

    #[test]
    fn single_dir_fs_prefers_legacy_over_mixed() {
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let legacy = home.path().join(".lean-ctx");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("sessions"), "x").unwrap();
        let mixed = xdg.path().join("lean-ctx");
        std::fs::create_dir_all(&mixed).unwrap();
        std::fs::write(mixed.join("stats.json"), "{}").unwrap();

        assert_eq!(
            single_dir_override_fs(home.path(), xdg.path()),
            Some(legacy)
        );
    }

    #[test]
    fn xdg_pinned_install_ignores_stray_legacy_marker() {
        // GL #623: once committed to XDG (pin in the config dir), a stray
        // `~/.lean-ctx/stats.json` (legacy residue, restored backup, concurrent
        // old binary) must NOT re-collapse the layout onto the legacy dir.
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        crate::core::layout_pin::write_xdg_pin_in(xdg.path()).unwrap();

        let legacy = home.path().join(".lean-ctx");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("stats.json"), "{}").unwrap();

        assert_eq!(single_dir_override_fs(home.path(), xdg.path()), None);
    }

    #[test]
    fn xdg_pinned_install_ignores_stray_mixed_marker() {
        // GL #623: same protection for a stray data marker that lands in the
        // mixed `$XDG_CONFIG_HOME/lean-ctx` dir after the install committed.
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        crate::core::layout_pin::write_xdg_pin_in(xdg.path()).unwrap();

        let mixed = xdg.path().join("lean-ctx");
        std::fs::write(mixed.join("stats.json"), "{}").unwrap();

        assert_eq!(single_dir_override_fs(home.path(), xdg.path()), None);
    }

    #[test]
    fn single_dir_fs_ignores_empty_dirs() {
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".lean-ctx")).unwrap();
        std::fs::create_dir_all(xdg.path().join("lean-ctx")).unwrap();

        assert_eq!(single_dir_override_fs(home.path(), xdg.path()), None);
    }

    #[test]
    fn single_dir_fs_ignores_non_marker_files() {
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let mixed = xdg.path().join("lean-ctx");
        std::fs::create_dir_all(&mixed).unwrap();
        std::fs::write(mixed.join("random.txt"), "x").unwrap();

        assert_eq!(single_dir_override_fs(home.path(), xdg.path()), None);
    }

    #[test]
    fn xdg_base_honors_env_then_home_fallback() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("XDG_CONFIG_HOME", tmp.path());
        let from_env = xdg_base("XDG_CONFIG_HOME", ".config").unwrap();
        crate::test_env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(from_env, tmp.path());

        // Unset var → falls back to $HOME/<home_fallback>.
        let fallback = xdg_base("LEAN_CTX_NONEXISTENT_XDG_VAR", ".cache").unwrap();
        assert!(fallback.ends_with(".cache"), "got: {}", fallback.display());
    }

    #[test]
    fn single_dir_override_honors_data_dir_env() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());
        let got = single_dir_override();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        assert_eq!(got, Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn config_dir_honors_explicit_override() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", tmp.path());
        let got = config_dir().unwrap();
        crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR");
        assert_eq!(got, tmp.path());
    }

    #[test]
    fn state_and_cache_dirs_honor_explicit_overrides() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_STATE_DIR", state.path());
        crate::test_env::set_var("LEAN_CTX_CACHE_DIR", cache.path());
        let got_state = state_dir().unwrap();
        let got_cache = cache_dir().unwrap();
        crate::test_env::remove_var("LEAN_CTX_STATE_DIR");
        crate::test_env::remove_var("LEAN_CTX_CACHE_DIR");
        assert_eq!(got_state, state.path());
        assert_eq!(got_cache, cache.path());
    }

    #[test]
    fn data_dir_matches_lean_ctx_data_dir() {
        let _guard = crate::core::data_dir::isolated_data_dir();
        assert_eq!(
            data_dir().unwrap(),
            crate::core::data_dir::lean_ctx_data_dir().unwrap()
        );
    }

    #[test]
    fn runtime_dir_honors_xdg_runtime_dir() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("XDG_RUNTIME_DIR", tmp.path());
        let got = runtime_dir().unwrap();
        crate::test_env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(got, tmp.path().join("lean-ctx"));
    }
}
