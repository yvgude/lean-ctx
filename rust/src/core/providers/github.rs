//! GitHub context provider — issues, pull requests, actions.
//!
//! Follows the same pattern as `gitlab.rs` but targets the GitHub REST API v3.
//! Implements `ContextProvider` for the registry.

use super::cache;
use super::provider_trait::{ContextProvider, ProviderParams};
use super::{ProviderItem, ProviderResult};

const DEFAULT_PER_PAGE: usize = 20;
const CACHE_TTL_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GitHubConfig {
    pub token: String,
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub api_base: String,
}

impl GitHubConfig {
    pub fn from_env() -> Result<Self, String> {
        let token = std::env::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("GH_TOKEN"))
            .or_else(|_| std::env::var("LEAN_CTX_GITHUB_TOKEN"))
            .map_err(|_| {
                "No GitHub token found. Set GITHUB_TOKEN or LEAN_CTX_GITHUB_TOKEN.".to_string()
            })?;

        let api_base = std::env::var("GITHUB_API_URL")
            .unwrap_or_else(|_| "https://api.github.com".to_string());

        let (owner, repo) = detect_owner_repo();

        Ok(Self {
            token,
            owner,
            repo,
            api_base,
        })
    }

    #[must_use]
    pub fn repo_slug(&self) -> Option<String> {
        match (&self.owner, &self.repo) {
            (Some(o), Some(r)) => Some(format!("{o}/{r}")),
            _ => None,
        }
    }

    fn api_url(&self, endpoint: &str) -> String {
        format!("{}{endpoint}", self.api_base)
    }
}

fn detect_owner_repo() -> (Option<String>, Option<String>) {
    if let Ok(full) = std::env::var("GITHUB_REPOSITORY")
        && let Some((owner, repo)) = full.split_once('/')
    {
        return (Some(owner.to_string()), Some(repo.to_string()));
    }
    if let (Ok(o), Ok(r)) = (
        std::env::var("GITHUB_REPOSITORY_OWNER"),
        std::env::var("GITHUB_REPO"),
    ) {
        return (Some(o), Some(r));
    }

    for remote in &["origin", "github", "upstream"] {
        let output = match std::process::Command::new("git")
            .args(["remote", "get-url", remote])
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => continue,
        };
        let result = parse_github_remote(&output);
        if result.0.is_some() {
            return result;
        }
    }
    (None, None)
}

fn parse_github_remote(url: &str) -> (Option<String>, Option<String>) {
    // SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let clean = rest.trim_end_matches(".git");
        if let Some((owner, repo)) = clean.split_once('/') {
            return (Some(owner.to_string()), Some(repo.to_string()));
        }
    }

    // HTTPS: https://github.com/owner/repo.git
    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    {
        let clean = rest.trim_end_matches(".git");
        if let Some((owner, repo)) = clean.split_once('/') {
            return (Some(owner.to_string()), Some(repo.to_string()));
        }
    }

    (None, None)
}

// ---------------------------------------------------------------------------
// API calls
// ---------------------------------------------------------------------------

fn api_get(config: &GitHubConfig, endpoint: &str) -> Result<String, String> {
    let url = config.api_url(endpoint);
    let res = ureq::get(&url)
        .header("Authorization", &format!("Bearer {}", config.token))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| format!("GitHub API error: {e}"))?;

    if res.status() != 200 {
        return Err(format!("GitHub API returned status {}", res.status()));
    }

    res.into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))
}

// ---------------------------------------------------------------------------
// Resource handlers
// ---------------------------------------------------------------------------

pub fn list_issues(
    config: &GitHubConfig,
    state: Option<&str>,
    limit: Option<usize>,
) -> Result<ProviderResult, String> {
    let slug = config
        .repo_slug()
        .ok_or("No GitHub repo configured. Set GITHUB_REPOSITORY or configure git remote.")?;

    let per_page = limit.unwrap_or(DEFAULT_PER_PAGE).min(100);
    let state_param = state.unwrap_or("open");

    let endpoint = format!(
        "/repos/{slug}/issues?per_page={per_page}&state={state_param}&sort=updated&direction=desc"
    );

    let cache_key = format!("github:issues:{slug}:{state_param}:{per_page}");
    if let Some(cached) = cache::get_cached(&cache_key)
        && let Ok(result) = serde_json::from_str::<ProviderResult>(&cached)
    {
        return Ok(result);
    }

    let body = api_get(config, &endpoint)?;
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    let result = ProviderResult {
        provider: "github".to_string(),
        resource_type: "issues".to_string(),
        total_count: None,
        truncated: items.len() >= per_page,
        items: items
            .iter()
            .filter(|v| v.get("pull_request").is_none_or(serde_json::Value::is_null))
            .map(parse_issue)
            .collect(),
    };

    if let Ok(json) = serde_json::to_string(&result) {
        cache::set_cached(&cache_key, &json, CACHE_TTL_SECS);
    }
    Ok(result)
}

pub fn list_pull_requests(
    config: &GitHubConfig,
    state: Option<&str>,
    limit: Option<usize>,
) -> Result<ProviderResult, String> {
    let slug = config.repo_slug().ok_or("No GitHub repo configured.")?;

    let per_page = limit.unwrap_or(DEFAULT_PER_PAGE).min(100);
    let state_param = state.unwrap_or("open");

    let endpoint = format!(
        "/repos/{slug}/pulls?per_page={per_page}&state={state_param}&sort=updated&direction=desc"
    );

    let cache_key = format!("github:prs:{slug}:{state_param}:{per_page}");
    if let Some(cached) = cache::get_cached(&cache_key)
        && let Ok(result) = serde_json::from_str::<ProviderResult>(&cached)
    {
        return Ok(result);
    }

    let body = api_get(config, &endpoint)?;
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    let result = ProviderResult {
        provider: "github".to_string(),
        resource_type: "pull_requests".to_string(),
        total_count: None,
        truncated: items.len() >= per_page,
        items: items.iter().map(parse_pr).collect(),
    };

    if let Ok(json) = serde_json::to_string(&result) {
        cache::set_cached(&cache_key, &json, CACHE_TTL_SECS);
    }
    Ok(result)
}

pub fn list_actions(
    config: &GitHubConfig,
    status: Option<&str>,
    limit: Option<usize>,
) -> Result<ProviderResult, String> {
    let slug = config.repo_slug().ok_or("No GitHub repo configured.")?;

    let per_page = limit.unwrap_or(DEFAULT_PER_PAGE).min(30);
    let mut endpoint = format!("/repos/{slug}/actions/runs?per_page={per_page}");
    if let Some(s) = status {
        endpoint.push_str(&format!("&status={s}"));
    }

    let body = api_get(config, &endpoint)?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    let runs = json["workflow_runs"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    Ok(ProviderResult {
        provider: "github".to_string(),
        resource_type: "actions".to_string(),
        total_count: json["total_count"].as_u64().map(|n| n as usize),
        truncated: runs.len() >= per_page,
        items: runs
            .iter()
            .map(|r| ProviderItem {
                id: r["id"].as_u64().unwrap_or(0).to_string(),
                title: r["name"].as_str().unwrap_or("").to_string(),
                state: r["conclusion"]
                    .as_str()
                    .or_else(|| r["status"].as_str())
                    .map(String::from),
                author: r["actor"]["login"].as_str().map(String::from),
                created_at: r["created_at"].as_str().map(String::from),
                updated_at: r["updated_at"].as_str().map(String::from),
                url: r["html_url"].as_str().map(String::from),
                labels: Vec::new(),
                body: None,
                ..Default::default()
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

fn parse_issue(v: &serde_json::Value) -> ProviderItem {
    ProviderItem {
        id: v["number"].as_u64().unwrap_or(0).to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().map(String::from),
        author: v["user"]["login"].as_str().map(String::from),
        created_at: v["created_at"].as_str().map(String::from),
        updated_at: v["updated_at"].as_str().map(String::from),
        url: v["html_url"].as_str().map(String::from),
        labels: v["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        body: v["body"].as_str().map(String::from),
        ..Default::default()
    }
}

fn parse_pr(v: &serde_json::Value) -> ProviderItem {
    ProviderItem {
        id: v["number"].as_u64().unwrap_or(0).to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().map(String::from),
        author: v["user"]["login"].as_str().map(String::from),
        created_at: v["created_at"].as_str().map(String::from),
        updated_at: v["updated_at"].as_str().map(String::from),
        url: v["html_url"].as_str().map(String::from),
        labels: v["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        body: v["body"].as_str().map(String::from),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// ContextProvider trait impl
// ---------------------------------------------------------------------------

pub struct GitHubProvider {
    config: Result<GitHubConfig, String>,
}

impl GitHubProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: GitHubConfig::from_env(),
        }
    }
}

impl Default for GitHubProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextProvider for GitHubProvider {
    fn id(&self) -> &'static str {
        "github"
    }

    fn display_name(&self) -> &'static str {
        "GitHub"
    }

    fn supported_actions(&self) -> &[&str] {
        &["issues", "pull_requests", "actions"]
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        let config = self.config.as_ref().map_err(std::clone::Clone::clone)?;
        match action {
            "issues" => list_issues(config, params.state.as_deref(), params.limit),
            "pull_requests" => list_pull_requests(config, params.state.as_deref(), params.limit),
            "actions" => list_actions(config, params.state.as_deref(), params.limit),
            _ => Err(format!("Unknown GitHub action: {action}")),
        }
    }

    fn cache_ttl_secs(&self) -> u64 {
        CACHE_TTL_SECS
    }

    fn is_available(&self) -> bool {
        self.config.is_ok()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_remote_ssh() {
        let (owner, repo) = parse_github_remote("git@github.com:yvgude/lean-ctx.git");
        assert_eq!(owner.as_deref(), Some("yvgude"));
        assert_eq!(repo.as_deref(), Some("lean-ctx"));
    }

    #[test]
    fn parse_github_remote_https() {
        let (owner, repo) = parse_github_remote("https://github.com/yvgude/lean-ctx.git");
        assert_eq!(owner.as_deref(), Some("yvgude"));
        assert_eq!(repo.as_deref(), Some("lean-ctx"));
    }

    #[test]
    fn parse_github_remote_no_match() {
        let (owner, repo) = parse_github_remote("git@gitlab.com:foo/bar.git");
        assert!(owner.is_none());
        assert!(repo.is_none());
    }

    #[test]
    fn provider_unavailable_without_token() {
        crate::test_env::remove_var("GITHUB_TOKEN");
        crate::test_env::remove_var("GH_TOKEN");
        crate::test_env::remove_var("LEAN_CTX_GITHUB_TOKEN");
        let provider = GitHubProvider::new();
        assert!(!provider.is_available());
    }

    #[test]
    fn provider_reports_correct_id_and_actions() {
        let provider = GitHubProvider::new();
        assert_eq!(provider.id(), "github");
        assert_eq!(provider.display_name(), "GitHub");
        assert!(provider.supported_actions().contains(&"issues"));
        assert!(provider.supported_actions().contains(&"pull_requests"));
        assert!(provider.supported_actions().contains(&"actions"));
    }
}
