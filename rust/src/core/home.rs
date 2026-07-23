use std::path::{Path, PathBuf};

/// Explicit profile override understood by lean-ctx when it is run outside
/// the Codex process that received `--profile`.
pub const LEAN_CTX_CODEX_PROFILE_ENV: &str = "LEAN_CTX_CODEX_PROFILE";
/// Compatibility with launchers that export the Codex profile name.
pub const CODEX_PROFILE_ENV: &str = "CODEX_PROFILE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConfigPaths {
    base: PathBuf,
    profile_name: Option<String>,
    profile: Option<PathBuf>,
}

impl CodexConfigPaths {
    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn profile_name(&self) -> Option<&str> {
        self.profile_name.as_deref()
    }

    pub fn profile(&self) -> Option<&Path> {
        self.profile.as_deref()
    }

    /// Path lean-ctx should write for the active Codex configuration.
    pub fn effective(&self) -> &Path {
        self.profile().unwrap_or_else(|| self.base())
    }

    /// Both layers in Codex's effective configuration, in load order.
    pub fn layers(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.base()).chain(self.profile())
    }
}

/// Resolve the user's home directory in a way that is:
/// - Override-friendly for CI/tests (HOME/USERPROFILE)
/// - Still correct in normal interactive installs (fallback to `dirs::home_dir()`)
pub fn resolve_home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }

    #[cfg(windows)]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let trimmed = profile.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }

        if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
            if !drive.trim().is_empty() && !path.trim().is_empty() {
                return Some(PathBuf::from(format!("{}{}", drive.trim(), path.trim())));
            }
        }
    }

    dirs::home_dir()
}

/// Resolve the Codex config directory.
/// Respects `CODEX_HOME` env var (official Codex CLI feature).
/// Falls back to `~/.codex` when unset or empty.
pub fn resolve_codex_dir() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("CODEX_HOME") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    resolve_home_dir().map(|h| h.join(".codex"))
}

/// Resolve the Codex profile name from an explicit env override.
fn env_codex_profile() -> Option<String> {
    [LEAN_CTX_CODEX_PROFILE_ENV, CODEX_PROFILE_ENV]
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .filter(|value| valid_codex_profile_name(value))
        })
}

fn valid_codex_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Infer a profile only when the Codex home contains exactly one named
/// profile. Multiple overlays require an explicit env selection.
fn sole_codex_profile(codex_dir: &Path) -> Option<String> {
    let mut profiles = std::fs::read_dir(codex_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            name.strip_suffix(".config.toml").map(str::to_string)
        })
        .filter(|stem| stem != "config" && valid_codex_profile_name(stem));
    let profile = profiles.next()?;
    profiles.next().is_none().then_some(profile)
}

fn codex_config_paths_at(codex_dir: &Path, profile_name: Option<String>) -> CodexConfigPaths {
    let base = codex_dir.join("config.toml");
    let profile = profile_name
        .as_deref()
        .map(|name| codex_dir.join(format!("{name}.config.toml")));
    CodexConfigPaths {
        base,
        profile_name,
        profile,
    }
}

/// Resolve both layers of the effective Codex configuration.
pub fn resolve_codex_config_paths() -> Option<CodexConfigPaths> {
    let codex_dir = resolve_codex_dir()?;
    let profile_name = env_codex_profile().or_else(|| sole_codex_profile(&codex_dir));
    Some(codex_config_paths_at(&codex_dir, profile_name))
}

/// Resolve the path lean-ctx should write for the active Codex profile.
pub fn resolve_codex_config_path() -> Option<PathBuf> {
    resolve_codex_config_paths().map(|paths| paths.effective().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_codex_dir_respects_env_var() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = env_lock();
        crate::test_env::set_var("CODEX_HOME", "/tmp/custom-codex");
        crate::test_env::remove_var(LEAN_CTX_CODEX_PROFILE_ENV);
        crate::test_env::remove_var(CODEX_PROFILE_ENV);
        let result = resolve_codex_dir();
        assert_eq!(result, Some(PathBuf::from("/tmp/custom-codex")));
        crate::test_env::remove_var("CODEX_HOME");
    }

    #[test]
    fn resolve_codex_dir_ignores_empty_env() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = env_lock();
        crate::test_env::set_var("CODEX_HOME", "  ");
        crate::test_env::remove_var(LEAN_CTX_CODEX_PROFILE_ENV);
        crate::test_env::remove_var(CODEX_PROFILE_ENV);
        let result = resolve_codex_dir();
        assert!(result.is_some());
        assert!(result.unwrap().ends_with(".codex"));
        crate::test_env::remove_var("CODEX_HOME");
    }

    #[test]
    fn resolve_codex_dir_falls_back_to_home() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = env_lock();
        crate::test_env::remove_var("CODEX_HOME");
        crate::test_env::remove_var(LEAN_CTX_CODEX_PROFILE_ENV);
        crate::test_env::remove_var(CODEX_PROFILE_ENV);
        let result = resolve_codex_dir();
        assert!(result.is_some());
        assert!(result.unwrap().ends_with(".codex"));
    }

    #[test]
    fn explicit_profile_selects_overlay_for_writes_and_layers() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("CODEX_HOME", dir.path());
        crate::test_env::set_var(LEAN_CTX_CODEX_PROFILE_ENV, "cat");
        crate::test_env::remove_var(CODEX_PROFILE_ENV);

        let paths = resolve_codex_config_paths().unwrap();
        assert_eq!(paths.profile_name(), Some("cat"));
        assert_eq!(paths.effective(), dir.path().join("cat.config.toml"));
        assert_eq!(paths.layers().count(), 2);
        assert_eq!(paths.base(), &dir.path().join("config.toml"));

        crate::test_env::remove_var("CODEX_HOME");
        crate::test_env::remove_var(LEAN_CTX_CODEX_PROFILE_ENV);
    }

    #[test]
    fn legacy_profile_env_selects_overlay() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("CODEX_HOME", dir.path());
        crate::test_env::remove_var(LEAN_CTX_CODEX_PROFILE_ENV);
        crate::test_env::set_var(CODEX_PROFILE_ENV, "work");

        let paths = resolve_codex_config_paths().unwrap();
        assert_eq!(paths.effective(), dir.path().join("work.config.toml"));

        crate::test_env::remove_var("CODEX_HOME");
        crate::test_env::remove_var(CODEX_PROFILE_ENV);
    }

    #[test]
    fn sole_overlay_is_inferred_but_ambiguous_overlays_are_not() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cat.config.toml"), "").unwrap();
        crate::test_env::set_var("CODEX_HOME", dir.path());
        crate::test_env::remove_var(LEAN_CTX_CODEX_PROFILE_ENV);
        crate::test_env::remove_var(CODEX_PROFILE_ENV);
        assert_eq!(
            resolve_codex_config_paths().unwrap().profile_name(),
            Some("cat")
        );

        std::fs::write(dir.path().join("work.config.toml"), "").unwrap();
        assert_eq!(resolve_codex_config_paths().unwrap().profile_name(), None);

        crate::test_env::remove_var("CODEX_HOME");
    }

    #[test]
    fn invalid_profile_names_cannot_escape_codex_home() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("CODEX_HOME", dir.path());
        crate::test_env::set_var(LEAN_CTX_CODEX_PROFILE_ENV, "../outside");
        crate::test_env::remove_var(CODEX_PROFILE_ENV);

        let paths = resolve_codex_config_paths().unwrap();
        assert_eq!(paths.profile_name(), None);
        assert_eq!(paths.effective(), dir.path().join("config.toml"));

        crate::test_env::remove_var("CODEX_HOME");
        crate::test_env::remove_var(LEAN_CTX_CODEX_PROFILE_ENV);
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
