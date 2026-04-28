use std::path::PathBuf;

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
