//! Discovery of the in-IDE `JetBrains` backend via a per-project port file.
//!
//! The plugin writes `<data_dir>/jetbrains-<projecthash>.port` (JSON, 0600), where
//! `<data_dir>` = `core::data_dir::lean_ctx_data_dir()` (`LEAN_CTX_DATA_DIR` → ~/.lean-ctx → XDG).
//! `projecthash = sha256(canonical(project_root))[..16]` — Rust and Kotlin MUST
//! canonicalize identically (symlink / trailing-slash trap, spec §5.5).

use std::time::Duration;

use serde::Deserialize;

/// Contents of the per-project port file (subset Rust needs).
#[derive(Debug, Clone, Deserialize)]
pub struct PortFile {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    #[serde(default)]
    pub project_root: String,
    #[serde(default)]
    pub ide_version: String,
}

/// `sha256(canonical(project_root))[..16]` as lowercase hex (first 8 bytes → 16 chars).
#[must_use]
pub fn project_hash(project_root: &str) -> String {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};
    let canonical = std::fs::canonicalize(project_root).map_or_else(
        |_| project_root.to_string(),
        |p| p.to_string_lossy().to_string(),
    );
    let digest = Sha256::digest(canonical.as_bytes());
    let mut hex = String::with_capacity(16);
    for b in digest.iter().take(8) {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// `<data_dir>/jetbrains-<projecthash>.port` — `<data_dir>` resolved via
/// `core::data_dir::lean_ctx_data_dir()` (`LEAN_CTX_DATA_DIR` → ~/.lean-ctx → XDG),
/// NOT a hardcoded `~/.lean-ctx` (spec §5.5 / §15.5). The Kotlin side mirrors this resolution.
#[must_use]
pub fn port_file_path(project_root: &str) -> Option<std::path::PathBuf> {
    let dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    Some(dir.join(format!("jetbrains-{}.port", project_hash(project_root))))
}

/// Reads + parses the port file, or `None` if absent/unreadable/malformed.
#[must_use]
pub fn read_port_file(project_root: &str) -> Option<PortFile> {
    let path = port_file_path(project_root)?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Liveness check for the IDE process. Linux: `/proc/<pid>`. Other OSes:
/// optimistic `true` (the `/health` ping is the authoritative reachability gate).
#[must_use]
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true
    }
}

/// `GET /health` with token header and a tight timeout (~300ms, spec §4.3).
/// ureq 3.x: per-request timeout via `.config().timeout_global(..).build()`.
#[must_use]
pub fn health_ok(pf: &PortFile) -> bool {
    let url = format!("http://127.0.0.1:{}/health", pf.port);
    ureq::get(&url)
        .config()
        .timeout_global(Some(Duration::from_millis(300)))
        .build()
        .header("X-LeanCtx-Token", &pf.token)
        .call()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_hash_is_stable_and_16_hex() {
        let h1 = project_hash("/some/project");
        let h2 = project_hash("/some/project");
        assert_eq!(h1, h2, "hash must be deterministic");
        assert_eq!(h1.len(), 16, "expected 16 hex chars (8 bytes)");
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn port_file_absent_for_unlikely_root() {
        // A path that has no port file → None (never panics).
        assert!(read_port_file("/nonexistent/lean-ctx/project/xyz").is_none());
    }

    #[test]
    fn project_hash_matches_known_vector() {
        // sha256("/some/project")[..8] — canonicalize fails (path absent) → raw fallback.
        // Shared parity anchor with the Kotlin LeanCtxPaths test.
        assert_eq!(project_hash("/some/project"), "a0317725f24b01df");
    }

    #[test]
    fn port_file_path_honors_data_dir_env() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join("lc_jb_portfile_env");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        let p = port_file_path("/some/project").unwrap();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        assert_eq!(p, dir.join("jetbrains-a0317725f24b01df.port"));
    }
}
