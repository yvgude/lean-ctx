use std::path::PathBuf;

const DATA_MARKERS: &[&str] = &["stats.json", "config.toml", "sessions"];

/// Resolve the lean-ctx data directory.
///
/// Priority order (backward-compatible XDG migration):
/// 1. `LEAN_CTX_DATA_DIR` env var (explicit override)
/// 2. `~/.lean-ctx` if it has actual data (stats.json/config.toml/sessions)
/// 3. `$XDG_CONFIG_HOME/lean-ctx` (XDG compliant, default `~/.config/lean-ctx`)
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
        static TEST_SANDBOX: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let dir = TEST_SANDBOX.get_or_init(|| {
            let d = std::env::temp_dir().join(format!("lean-ctx-testdata-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&d);
            d
        });
        Ok(dir.clone())
    }

    #[cfg(not(test))]
    {
        resolve_home_data_dir()
    }
}

/// Home-based resolution (legacy `~/.lean-ctx` vs XDG). Split out so the
/// priority rules stay unit-testable despite the test sandbox above.
fn resolve_home_data_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;

    let legacy = home.join(".lean-ctx");
    if legacy.exists() && has_data_files(&legacy) {
        ensure_dir_permissions(&legacy);
        return Ok(legacy);
    }

    let xdg_config = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map_or_else(|| home.join(".config"), PathBuf::from);

    let xdg_dir = xdg_config.join("lean-ctx");

    if xdg_dir.exists() && has_data_files(&xdg_dir) {
        ensure_dir_permissions(&xdg_dir);
        return Ok(xdg_dir);
    }

    if legacy.exists() {
        ensure_dir_permissions(&legacy);
        return Ok(legacy);
    }

    ensure_dir_permissions(&xdg_dir);
    Ok(xdg_dir)
}

fn has_data_files(dir: &std::path::Path) -> bool {
    DATA_MARKERS.iter().any(|f| dir.join(f).exists())
}

/// Returns all known data directories that contain stats data.
/// Used for migration and doctor diagnostics.
pub fn all_data_dirs_with_stats() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let legacy = home.join(".lean-ctx");
        if legacy.join("stats.json").exists() {
            dirs.push(legacy);
        }
        let xdg = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map_or_else(|| home.join(".config"), PathBuf::from)
            .join("lean-ctx");
        if xdg.join("stats.json").exists() && !dirs.contains(&xdg) {
            dirs.push(xdg);
        }
    }
    dirs
}

/// Detect and repair a data directory split.
/// Returns the number of tokens migrated, or None if no split detected.
pub fn migrate_if_split() -> Option<u64> {
    let dirs = all_data_dirs_with_stats();
    if dirs.len() < 2 {
        return None;
    }

    let primary = lean_ctx_data_dir().ok()?;
    let secondary = dirs.iter().find(|d| **d != primary)?;

    let sec_content = std::fs::read_to_string(secondary.join("stats.json")).ok()?;
    let sec_store: serde_json::Value = serde_json::from_str(&sec_content).ok()?;
    let sec_commands = sec_store["total_commands"].as_u64().unwrap_or(0);
    if sec_commands == 0 {
        return None;
    }

    let primary_path = primary.join("stats.json");
    if !primary_path.exists() {
        let _ = std::fs::create_dir_all(&primary);
        let _ = std::fs::copy(secondary.join("stats.json"), &primary_path);
        let _ = std::fs::remove_file(secondary.join("stats.json"));
        let tokens = sec_store["total_input_tokens"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(sec_store["total_output_tokens"].as_u64().unwrap_or(0));
        return Some(tokens);
    }

    None
}

#[cfg(unix)]
fn ensure_dir_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if path.is_dir() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
}

#[cfg(not(unix))]
fn ensure_dir_permissions(_path: &std::path::Path) {}

pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let mutex = LOCK.get_or_init(|| Mutex::new(()));
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    fn has_data_files_with_config() {
        let dir = std::env::temp_dir().join("test_data_dir_config");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("config.toml"), "").unwrap();
        assert!(has_data_files(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn has_data_files_with_sessions() {
        let dir = std::env::temp_dir().join("test_data_dir_sessions");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("sessions"));
        assert!(has_data_files(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lean_ctx_data_dir_env_override() {
        let _lock = test_env_lock();
        let dir = std::env::temp_dir().join("test_data_dir_env");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        let result = lean_ctx_data_dir().unwrap();
        assert_eq!(result, dir);
        std::env::remove_var("LEAN_CTX_DATA_DIR");
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

        std::env::set_var("LEAN_CTX_DATA_DIR", "");
        std::env::set_var("XDG_CONFIG_HOME", xdg_base.to_str().unwrap());

        // Calls the home resolver directly: lean_ctx_data_dir() is sandboxed
        // under cfg(test) (GL #512) and would short-circuit before XDG logic.
        let result = resolve_home_data_dir().unwrap();

        std::env::remove_var("LEAN_CTX_DATA_DIR");
        std::env::remove_var("XDG_CONFIG_HOME");

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
}
