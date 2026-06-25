use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const GITHUB_API_RELEASES: &str = "https://api.github.com/repos/yvgude/lean-ctx/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
struct VersionCache {
    latest: String,
    checked_at: u64,
}

fn cache_path() -> Option<PathBuf> {
    crate::core::paths::cache_dir()
        .ok()
        .map(|d| d.join("latest-version.json"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn read_cache() -> Option<VersionCache> {
    let path = cache_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_cache(latest: &str) {
    if let Some(path) = cache_path() {
        let cache = VersionCache {
            latest: latest.to_string(),
            checked_at: now_secs(),
        };
        if let Ok(json) = serde_json::to_string(&cache) {
            let _ = std::fs::write(path, json);
        }
    }
}

fn is_cache_stale(cache: &VersionCache) -> bool {
    let age = now_secs().saturating_sub(cache.checked_at);
    age > CACHE_TTL_SECS
}

fn fetch_latest_version() -> Result<String, String> {
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(5)))
            .build(),
    );

    let body = agent
        .get(GITHUB_API_RELEASES)
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| e.to_string())?
        .into_body()
        .read_to_string()
        .map_err(|e| e.to_string())?;

    let release: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| "missing tag_name in GitHub releases response".to_string())?;

    let version = tag.trim().trim_start_matches('v').to_string();
    if version.is_empty() || !version.contains('.') {
        return Err("invalid version format".to_string());
    }
    Ok(version)
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse =
        |v: &str| -> Vec<u32> { v.split('.').filter_map(|p| p.parse::<u32>().ok()).collect() };
    parse(latest) > parse(current)
}

/// Spawn a background thread to fetch latest version from GitHub Releases
/// and write the result to the lean-ctx data dir (`latest-version.json`).
/// Non-blocking, fire-and-forget. Skips if cache is fresh (<24h).
/// Respects `update_check_disabled` config and `LEAN_CTX_NO_UPDATE_CHECK` env var.
pub fn check_background() {
    let cfg = super::config::Config::load();
    if cfg.update_check_disabled_effective() {
        return;
    }

    let cache = read_cache();
    if let Some(ref c) = cache
        && !is_cache_stale(c)
    {
        return;
    }

    std::thread::spawn(|| {
        if let Ok(latest) = fetch_latest_version() {
            write_cache(&latest);
        }
    });
}

/// Returns a formatted yellow update banner if a newer version is available.
/// Reads only the local cache file — zero network calls, zero delay.
#[must_use]
pub fn get_update_banner() -> Option<String> {
    let cache = read_cache()?;
    if is_newer(&cache.latest, CURRENT_VERSION) {
        Some(format!(
            "  \x1b[33m\x1b[1m\u{27F3} Update available: v{CURRENT_VERSION} \u{2192} v{}\x1b[0m  \x1b[2m\u{2014} run:\x1b[0m \x1b[1mlean-ctx update\x1b[0m",
            cache.latest
        ))
    } else {
        None
    }
}

/// Returns version info as JSON for the dashboard /api/version endpoint.
/// Includes the cache age so the UI can be honest about staleness (#563).
#[must_use]
pub fn version_info_json() -> String {
    let cache = read_cache();
    let (latest, update_available, age_secs) = match cache {
        Some(c) => {
            let newer = is_newer(&c.latest, CURRENT_VERSION);
            let age = now_secs().saturating_sub(c.checked_at);
            (c.latest, newer, Some(age))
        }
        None => (CURRENT_VERSION.to_string(), false, None),
    };

    let age_json = age_secs.map_or("null".to_string(), |a| a.to_string());
    format!(
        r#"{{"current":"{CURRENT_VERSION}","latest":"{latest}","update_available":{update_available},"checked_age_secs":{age_json}}}"#
    )
}

use std::sync::atomic::{AtomicBool, Ordering};

static NOTIFIED_THIS_SESSION: AtomicBool = AtomicBool::new(false);

/// Returns a one-line update notification if available, exactly once per session.
/// Safe to call from any tool — returns None after first notification.
pub fn session_update_hint() -> Option<String> {
    if NOTIFIED_THIS_SESSION.swap(true, Ordering::Relaxed) {
        return None;
    }

    let cache = read_cache()?;
    if !is_newer(&cache.latest, CURRENT_VERSION) {
        return None;
    }

    Some(format!(
        "[lean-ctx] Update available: v{CURRENT_VERSION} → v{} (run: lean-ctx update)",
        cache.latest
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_version_detected() {
        assert!(is_newer("2.9.14", "2.9.13"));
        assert!(is_newer("3.0.0", "2.9.99"));
        assert!(is_newer("2.10.0", "2.9.14"));
    }

    #[test]
    fn same_or_older_not_newer() {
        assert!(!is_newer("2.9.13", "2.9.13"));
        assert!(!is_newer("2.9.12", "2.9.13"));
        assert!(!is_newer("1.0.0", "2.9.13"));
    }

    #[test]
    fn cache_fresh_within_ttl() {
        let fresh = VersionCache {
            latest: "2.9.14".to_string(),
            checked_at: now_secs(),
        };
        assert!(!is_cache_stale(&fresh));
    }

    #[test]
    fn cache_stale_after_ttl() {
        let old = VersionCache {
            latest: "2.9.14".to_string(),
            checked_at: now_secs() - CACHE_TTL_SECS - 1,
        };
        assert!(is_cache_stale(&old));
    }

    #[test]
    fn version_json_has_required_fields() {
        let json = version_info_json();
        assert!(json.contains("current"));
        assert!(json.contains("latest"));
        assert!(json.contains("update_available"));
        assert!(json.contains("checked_age_secs"));
    }

    #[test]
    fn banner_none_for_current_version() {
        assert!(!is_newer(CURRENT_VERSION, CURRENT_VERSION));
    }

    #[test]
    fn session_hint_returns_once() {
        NOTIFIED_THIS_SESSION.store(false, Ordering::Relaxed);
        // No cache file in test env, so we verify the atomic gate directly
        NOTIFIED_THIS_SESSION.store(false, Ordering::Relaxed);
        let first_swap = NOTIFIED_THIS_SESSION.swap(true, Ordering::Relaxed);
        assert!(
            !first_swap,
            "First call should get false (not yet notified)"
        );
        let second_swap = NOTIFIED_THIS_SESSION.swap(true, Ordering::Relaxed);
        assert!(
            second_swap,
            "Second call should get true (already notified)"
        );
    }
}
