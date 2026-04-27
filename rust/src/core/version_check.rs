use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION_URL: &str = "https://leanctx.com/version.txt";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
struct VersionCache {
    latest: String,
    checked_at: u64,
}

fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".lean-ctx/latest-version.json"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
        .get(VERSION_URL)
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .call()
        .map_err(|e| e.to_string())?
        .into_body()
        .read_to_string()
        .map_err(|e| e.to_string())?;

    let version = body.trim().trim_start_matches('v').to_string();
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

/// Spawn a background thread to fetch latest version from leanctx.com/version.txt
/// and write the result to ~/.lean-ctx/latest-version.json.
/// Non-blocking, fire-and-forget. Skips if cache is fresh (<24h).
/// Respects `update_check_disabled` config and `LEAN_CTX_NO_UPDATE_CHECK` env var.
pub fn check_background() {
    let cfg = super::config::Config::load();
    if cfg.update_check_disabled_effective() {
        return;
    }

    let cache = read_cache();
    if let Some(ref c) = cache {
        if !is_cache_stale(c) {
            return;
        }
    }

    std::thread::spawn(|| {
        if let Ok(latest) = fetch_latest_version() {
            write_cache(&latest);
        }
    });
}

/// Returns a formatted yellow update banner if a newer version is available.
/// Reads only the local cache file — zero network calls, zero delay.
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
pub fn version_info_json() -> String {
    let cache = read_cache();
    let (latest, update_available) = match cache {
        Some(c) => {
            let newer = is_newer(&c.latest, CURRENT_VERSION);
            (c.latest, newer)
        }
        None => (CURRENT_VERSION.to_string(), false),
    };

    format!(
        r#"{{"current":"{CURRENT_VERSION}","latest":"{latest}","update_available":{update_available}}}"#
    )
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
    }

    #[test]
    fn banner_none_for_current_version() {
        assert!(!is_newer(CURRENT_VERSION, CURRENT_VERSION));
    }
}
