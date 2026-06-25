use std::path::PathBuf;

/// Resolve the user's home directory in a way that is:
/// - Override-friendly for CI/tests (HOME/USERPROFILE)
/// - Still correct in normal interactive installs (fallback to `dirs::home_dir()`)
#[must_use]
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
#[must_use]
pub fn resolve_codex_dir() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("CODEX_HOME") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    resolve_home_dir().map(|h| h.join(".codex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_codex_dir_respects_env_var() {
        let _guard = env_lock();
        crate::test_env::set_var("CODEX_HOME", "/tmp/custom-codex");
        let result = resolve_codex_dir();
        assert_eq!(result, Some(PathBuf::from("/tmp/custom-codex")));
        crate::test_env::remove_var("CODEX_HOME");
    }

    #[test]
    fn resolve_codex_dir_ignores_empty_env() {
        let _guard = env_lock();
        crate::test_env::set_var("CODEX_HOME", "  ");
        let result = resolve_codex_dir();
        assert!(result.is_some());
        assert!(result.unwrap().ends_with(".codex"));
        crate::test_env::remove_var("CODEX_HOME");
    }

    #[test]
    fn resolve_codex_dir_falls_back_to_home() {
        let _guard = env_lock();
        crate::test_env::remove_var("CODEX_HOME");
        let result = resolve_codex_dir();
        assert!(result.is_some());
        assert!(result.unwrap().ends_with(".codex"));
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
