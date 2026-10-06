use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderHealth {
    pub provider_id: String,
    pub provider_type: String,
    pub reachable: bool,
    pub cache_fresh: bool,
    pub cache_age_secs: Option<u64>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HealthReport {
    pub providers: Vec<ProviderHealth>,
    pub all_healthy: bool,
    pub degraded_count: usize,
    pub unreachable_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityProbe {
    /// Check if provider env vars are set (fast, no network)
    EnvOnly,
    /// Check env vars + attempt lightweight HTTP probe
    WithNetwork,
}

impl ProviderHealth {
    pub fn is_healthy(&self) -> bool {
        self.reachable && self.cache_fresh
    }

    pub fn is_degraded(&self) -> bool {
        self.reachable && !self.cache_fresh
    }
}

impl HealthReport {
    pub fn summary(&self) -> String {
        format!(
            "Provider Health ({} checked, {} degraded, {} unreachable)",
            self.providers.len(),
            self.degraded_count,
            self.unreachable_count
        )
    }
}

pub fn check_provider_health(
    provider_id: &str,
    provider_type: &str,
    probe: ConnectivityProbe,
) -> ProviderHealth {
    let (cache_fresh, cache_age_secs) = cache_health(provider_id);
    let Some(env_var) = env_var_for_provider(provider_type) else {
        return health_without_network(provider_id, provider_type, cache_fresh, cache_age_secs);
    };

    if std::env::var_os(env_var).is_none() {
        return ProviderHealth {
            provider_id: provider_id.to_string(),
            provider_type: provider_type.to_string(),
            reachable: false,
            cache_fresh,
            cache_age_secs,
            latency_ms: None,
            error: Some(format!("{env_var} not set")),
        };
    }

    if probe == ConnectivityProbe::EnvOnly {
        return health_without_network(provider_id, provider_type, cache_fresh, cache_age_secs);
    }

    let Some(endpoint) = network_endpoint(provider_type) else {
        return health_without_network(provider_id, provider_type, cache_fresh, cache_age_secs);
    };

    let started = Instant::now();
    let result = probe_endpoint(provider_type, &endpoint);
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    ProviderHealth {
        provider_id: provider_id.to_string(),
        provider_type: provider_type.to_string(),
        reachable: result.is_ok(),
        cache_fresh,
        cache_age_secs,
        latency_ms: Some(latency_ms),
        error: result.err(),
    }
}

pub fn check_all_providers(probe: ConnectivityProbe) -> HealthReport {
    let providers = super::registry::global_registry()
        .discover()
        .into_iter()
        .map(|info| check_provider_health(&info.id, &info.id, probe))
        .collect();
    build_report(providers)
}

pub fn format_health_report(report: &HealthReport) -> String {
    let mut output = report.summary();
    for provider in &report.providers {
        output.push('\n');
        output.push_str(&format_provider_health(provider));
    }
    output
}

fn env_var_for_provider(provider_type: &str) -> Option<&'static str> {
    match provider_type {
        "github" => Some("GITHUB_TOKEN"),
        "gitlab" => Some("GITLAB_TOKEN"),
        "jira" => Some("JIRA_TOKEN"),
        "postgres" => Some("DATABASE_URL"),
        _ => None,
    }
}

fn api_endpoint_for_provider(provider_type: &str) -> Option<&'static str> {
    match provider_type {
        "github" => Some("https://api.github.com/rate_limit"),
        _ => None,
    }
}

fn network_endpoint(provider_type: &str) -> Option<String> {
    if let Some(endpoint) = api_endpoint_for_provider(provider_type) {
        return Some(endpoint.to_string());
    }

    match provider_type {
        "gitlab" => {
            let host = std::env::var("GITLAB_HOST")
                .or_else(|_| std::env::var("CI_SERVER_HOST"))
                .unwrap_or_else(|_| "gitlab.com".to_string());
            Some(format!("https://{host}/api/v4/version"))
        }
        "jira" => std::env::var("JIRA_URL")
            .ok()
            .map(|base| format!("{}/rest/api/2/serverInfo", base.trim_end_matches('/'))),
        _ => None,
    }
}

fn probe_endpoint(provider_type: &str, endpoint: &str) -> Result<(), String> {
    let agent = super::hardened_http::hardened_agent();
    let request = match provider_type {
        "github" => agent
            .get(endpoint)
            .header(
                "Authorization",
                &format!(
                    "Bearer {}",
                    std::env::var("GITHUB_TOKEN").unwrap_or_default()
                ),
            )
            .header("Accept", "application/vnd.github+json"),
        "gitlab" => agent.get(endpoint).header(
            "PRIVATE-TOKEN",
            &std::env::var("GITLAB_TOKEN").unwrap_or_default(),
        ),
        "jira" => agent.get(endpoint).header(
            "Authorization",
            &format!("Bearer {}", std::env::var("JIRA_TOKEN").unwrap_or_default()),
        ),
        _ => agent.get(endpoint),
    };
    let response = request
        .call()
        .map_err(|error| format!("{provider_type} connectivity probe failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "{provider_type} connectivity probe failed: HTTP {}",
            response.status().as_u16()
        ));
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn health_probe_rejects_redirects_without_contacting_the_target() {
    use super::hardened_http::redirect_tests::Server;
    let target = Server::new(200, None, "[]");
    let origin = Server::new(302, Some(&target.url), "synthetic-private-error");
    let error = probe_endpoint("fixture", &origin.url).unwrap_err();
    assert!(error.contains("302"));
    assert!(!error.contains("synthetic-private-error"));
    assert_eq!(origin.count(), 1);
    assert_eq!(target.count(), 0);
    assert!(probe_endpoint("fixture", &target.url).is_ok());
}

fn cache_health(provider_id: &str) -> (bool, Option<u64>) {
    let metrics = super::cache::cache_metrics();
    let Some(stats) = metrics
        .provider_stats
        .iter()
        .find(|stats| stats.provider_id == provider_id)
    else {
        return (true, None);
    };

    let age = stats
        .last_fetch
        .and_then(|fetched| SystemTime::now().duration_since(fetched).ok())
        .map(|elapsed| elapsed.as_secs());
    (stats.entry_count > 0, age)
}

fn health_without_network(
    provider_id: &str,
    provider_type: &str,
    cache_fresh: bool,
    cache_age_secs: Option<u64>,
) -> ProviderHealth {
    ProviderHealth {
        provider_id: provider_id.to_string(),
        provider_type: provider_type.to_string(),
        reachable: true,
        cache_fresh,
        cache_age_secs,
        latency_ms: None,
        error: None,
    }
}

fn build_report(providers: Vec<ProviderHealth>) -> HealthReport {
    let degraded_count = providers
        .iter()
        .filter(|provider| provider.is_degraded())
        .count();
    let unreachable_count = providers
        .iter()
        .filter(|provider| !provider.reachable)
        .count();
    let all_healthy = providers.iter().all(ProviderHealth::is_healthy);

    HealthReport {
        providers,
        all_healthy,
        degraded_count,
        unreachable_count,
    }
}

fn format_provider_health(provider: &ProviderHealth) -> String {
    let cache = match (provider.cache_fresh, provider.cache_age_secs) {
        (true, Some(age)) => format!("fresh ({age}s ago)"),
        (false, Some(age)) => format!("stale ({age}s ago)"),
        (true, None) => "fresh".to_string(),
        (false, None) => "stale".to_string(),
    };
    let latency = provider
        .latency_ms
        .map(|value| format!("  latency: {value}ms"))
        .unwrap_or_default();

    if provider.reachable {
        format!(
            "  {:<10} ✓ reachable  cache: {cache}{latency}",
            provider.provider_id
        )
    } else {
        format!(
            "  {:<10} ✗ unreachable: {}",
            provider.provider_id,
            provider.error.as_deref().unwrap_or("unknown error")
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::{
        ConnectivityProbe, ProviderHealth, build_report, check_provider_health,
        env_var_for_provider, format_health_report,
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn health(reachable: bool, cache_fresh: bool) -> ProviderHealth {
        ProviderHealth {
            provider_id: "test".to_string(),
            provider_type: "test".to_string(),
            reachable,
            cache_fresh,
            cache_age_secs: None,
            latency_ms: None,
            error: None,
        }
    }

    #[test]
    fn test_env_var_mapping_github() {
        assert_eq!(env_var_for_provider("github"), Some("GITHUB_TOKEN"));
    }

    #[test]
    fn test_env_var_mapping_unknown() {
        assert_eq!(env_var_for_provider("unknown"), None);
    }

    #[test]
    fn test_provider_health_env_only_available() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original = std::env::var_os("GITHUB_TOKEN");
        // SAFETY: test-only, serialized by ENV_LOCK
        unsafe {
            std::env::set_var("GITHUB_TOKEN", "test-token");
        }

        let result = check_provider_health("github-test", "github", ConnectivityProbe::EnvOnly);

        match original {
            // SAFETY: test-only, serialized by ENV_LOCK
            Some(value) => unsafe { std::env::set_var("GITHUB_TOKEN", value) },
            // SAFETY: test-only, serialized by ENV_LOCK
            None => unsafe { std::env::remove_var("GITHUB_TOKEN") },
        }
        assert!(result.reachable);
        assert!(result.latency_ms.is_none());
        assert!(result.error.is_none());
    }

    #[test]
    fn test_provider_health_env_only_unavailable() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original = std::env::var_os("GITHUB_TOKEN");
        // SAFETY: test-only, serialized by ENV_LOCK
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
        }

        let result = check_provider_health("github-test", "github", ConnectivityProbe::EnvOnly);

        if let Some(value) = original {
            // SAFETY: test-only, serialized by ENV_LOCK
            unsafe {
                std::env::set_var("GITHUB_TOKEN", value);
            }
        }
        assert!(!result.reachable);
        assert_eq!(result.error.as_deref(), Some("GITHUB_TOKEN not set"));
    }

    #[test]
    fn test_health_report_all_healthy() {
        let report = build_report(vec![health(true, true), health(true, true)]);
        assert!(report.all_healthy);
        assert_eq!(report.degraded_count, 0);
        assert_eq!(report.unreachable_count, 0);
    }

    #[test]
    fn test_health_report_with_unreachable() {
        let report = build_report(vec![health(true, true), health(false, true)]);
        assert!(!report.all_healthy);
        assert_eq!(report.unreachable_count, 1);
    }

    #[test]
    fn test_format_report_contains_status() {
        let healthy = format_health_report(&build_report(vec![health(true, true)]));
        let unhealthy = format_health_report(&build_report(vec![health(false, true)]));
        assert!(healthy.contains('✓'));
        assert!(unhealthy.contains('✗'));
    }

    #[test]
    fn test_is_degraded_logic() {
        assert!(health(true, false).is_degraded());
        assert!(!health(true, true).is_degraded());
        assert!(!health(false, false).is_degraded());
    }

    #[test]
    fn test_empty_provider_list() {
        let report = build_report(Vec::new());
        assert!(report.providers.is_empty());
        assert!(report.all_healthy);
        assert_eq!(report.degraded_count, 0);
        assert_eq!(report.unreachable_count, 0);
    }
}
