// SPDX-License-Identifier: Apache-2.0

//! GitHub context provider — issues, pull requests, actions.
//!
//! Follows the same pattern as `gitlab.rs` but targets the GitHub REST API v3.
//! Implements `ContextProvider` for the registry.

use super::provider_trait::{ContextProvider, ProviderParams};
use super::{ProviderItem, ProviderResult};

const DEFAULT_PER_PAGE: usize = 20;

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
        let token = super::credentials::var("GITHUB_TOKEN")
            .or_else(|_| super::credentials::var("GH_TOKEN"))
            .or_else(|_| super::credentials::var("LEAN_CTX_GITHUB_TOKEN"))
            .map_err(|_| {
                "No GitHub token found. Set GITHUB_TOKEN or LEAN_CTX_GITHUB_TOKEN.".to_string()
            })?;

        let api_base = super::credentials::var("GITHUB_API_URL")
            .unwrap_or_else(|_| "https://api.github.com".to_string());

        let (owner, repo) = detect_owner_repo();

        Ok(Self {
            token,
            owner,
            repo,
            api_base,
        })
    }

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
    if let Ok(full) = super::credentials::var("GITHUB_REPOSITORY")
        && let Some((owner, repo)) = full.split_once('/')
    {
        return (Some(owner.to_string()), Some(repo.to_string()));
    }
    if let (Ok(o), Ok(r)) = (
        super::credentials::var("GITHUB_REPOSITORY_OWNER"),
        super::credentials::var("GITHUB_REPO"),
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
    let token = format!("Bearer {}", config.token);
    super::hardened_http::provider_get_with_headers(
        "github",
        &url,
        &[
            ("Authorization", &token),
            ("Accept", "application/vnd.github+json"),
            ("X-GitHub-Api-Version", "2022-11-28"),
        ],
    )
    .into_body()
    .map_err(|e| format!("GitHub API error: {e}"))
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
    let endpoint = list_endpoint(&slug, "issues", per_page, state.unwrap_or("open"));

    // No response cache: a list answer is the principal's current access, and
    // a cached one would keep serving objects after access was revoked.
    let body = api_get(config, &endpoint)?;
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    Ok(ProviderResult {
        provider: "github".to_string(),
        resource_type: "issues".to_string(),
        total_count: None,
        truncated: items.len() >= per_page,
        items: items
            .iter()
            .filter(|v| v.get("pull_request").is_none_or(serde_json::Value::is_null))
            .map(parse_issue)
            .collect(),
    })
}

/// A list endpoint whose filter value is one query value: it cannot add or
/// replace a parameter, whatever the caller passes.
fn list_endpoint(slug: &str, resource: &str, per_page: usize, state: &str) -> String {
    format!(
        "/repos/{slug}/{resource}?per_page={per_page}&state={}&sort=updated&direction=desc",
        urlencoding::encode(state)
    )
}

pub fn list_pull_requests(
    config: &GitHubConfig,
    state: Option<&str>,
    limit: Option<usize>,
) -> Result<ProviderResult, String> {
    let slug = config.repo_slug().ok_or("No GitHub repo configured.")?;

    let per_page = limit.unwrap_or(DEFAULT_PER_PAGE).min(100);
    let endpoint = list_endpoint(&slug, "pulls", per_page, state.unwrap_or("open"));

    let body = api_get(config, &endpoint)?;
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    Ok(ProviderResult {
        provider: "github".to_string(),
        resource_type: "pull_requests".to_string(),
        total_count: None,
        truncated: items.len() >= per_page,
        items: items.iter().map(parse_pr).collect(),
    })
}

/// Fetch one issue or pull request now, with this config's credential. GitHub
/// answers 404 for an object the credential may not read.
fn show_item(config: &GitHubConfig, resource: &str, number: u64) -> Result<ProviderItem, String> {
    let slug = config.repo_slug().ok_or("No GitHub repo configured.")?;
    let (path, parse): (&str, fn(&serde_json::Value) -> ProviderItem) = match resource {
        "issues" => ("issues", parse_issue),
        "pull_requests" => ("pulls", parse_pr),
        _ => return Err("unsupported stored GitHub resource".into()),
    };
    let body = api_get(config, &format!("/repos/{slug}/{path}/{number}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| "invalid GitHub item response")?;
    // The issues endpoint also returns pull requests; an issue origin must not
    // be satisfied by a pull request of the same number, or the reverse.
    let is_pull =
        value.get("pull_request").is_some_and(|v| !v.is_null()) || resource == "pull_requests";
    if (resource == "issues") == is_pull {
        return Err("stored GitHub resource changed kind".into());
    }
    Ok(parse(&value))
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
        endpoint.push_str(&format!("&status={}", urlencoding::encode(s)));
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
    pub fn new() -> Self {
        Self {
            config: GitHubConfig::from_env(),
        }
    }

    /// Construct with an explicit config, bypassing env discovery. Used by the
    /// hosted team server's managed connectors so each scheduled run carries its
    /// own credential without mutating process-global env (which would race
    /// across connectors).
    #[must_use]
    pub fn with_config(config: GitHubConfig) -> Self {
        Self { config: Ok(config) }
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
        0
    }

    fn descriptor(&self) -> super::provider_trait::ConnectorDescriptor {
        descriptor()
    }

    /// The source a stored item came from: API origin, repository and the
    /// credential that read it. Another token or repository is another source.
    fn reuse_binding(&self, action: &str, _params: &ProviderParams) -> Option<String> {
        if !matches!(action, "issues" | "pull_requests") {
            return None;
        }
        let config = self.config.as_ref().ok()?;
        super::provenance::digest(&serde_json::json!({
            "version": 1, "provider": "github", "api_base": config.api_base,
            "repository": config.repo_slug()?, "credential": config.token,
        }))
    }

    fn reacquire_item(
        &self,
        action: &str,
        params: &ProviderParams,
        item_id: &str,
    ) -> Result<ProviderResult, String> {
        if self.reuse_binding(action, params).is_none() {
            return Err("GitHub source is unavailable for item reauthorization".into());
        }
        let number: u64 = item_id
            .parse()
            .map_err(|_| "invalid stored GitHub item number")?;
        if number == 0 || number.to_string() != item_id {
            return Err("invalid stored GitHub item number".into());
        }
        let config = self.config.as_ref().map_err(Clone::clone)?;
        let item = show_item(config, action, number)?;
        Ok(ProviderResult {
            provider: "github".into(),
            resource_type: action.into(),
            items: vec![item],
            total_count: Some(1),
            truncated: false,
        })
    }

    fn is_available(&self) -> bool {
        self.config.is_ok()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

pub(crate) fn descriptor() -> super::provider_trait::ConnectorDescriptor {
    use super::provider_trait::{CapabilityLevel as L, ConnectorDescriptor as C};
    C {
        principal_binding: "token (GITHUB_TOKEN / GH_TOKEN / LEAN_CTX_GITHUB_TOKEN), bound with API origin and repository",
        search: L::Partial,
        list: L::Supported,
        get: L::Supported,
        delta: L::Unsupported,
        revalidate_acl: L::Supported,
        revalidate_revision: L::Supported,
        delete_event: L::Partial,
        content_trust: C::UNTRUSTED,
        write_surface: C::NO_WRITES,
        failure_policy: C::FAIL_CLOSED,
        notes: "search = state filter, no full text; issues and pull requests re-checked per item with the \
                principal's token (404 = no longer accessible, kind checked); actions runs are list-only and \
                never stored with an origin; no response cache",
    }
}

#[cfg(test)]
mod tests {
    use super::{GitHubProvider, parse_github_remote};
    use crate::core::providers::provider_trait::ContextProvider;

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
        let _env_lock = crate::core::data_dir::test_env_lock();
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

    fn config(url: &str, token: &str, repo: &str) -> super::GitHubConfig {
        super::GitHubConfig {
            token: token.into(),
            owner: Some("acme".into()),
            repo: Some(repo.into()),
            api_base: url.into(),
        }
    }

    #[test]
    fn a_revoked_list_is_asked_again_and_never_served_from_a_cache() {
        use crate::core::providers::hardened_http::redirect_tests::Server;
        let issue = r#"[{"number":3,"title":"canary","state":"open","updated_at":"t1"}]"#;
        let server = Server::new(200, None, issue);
        let provider = GitHubProvider::with_config(config(&server.url, "reader-token", "repo"));
        let params = super::ProviderParams::default();
        assert_eq!(
            provider.execute("issues", &params).unwrap().items[0].title,
            "canary"
        );
        server.respond(404, r#"{"message":"Not Found"}"#);
        assert!(provider.execute("issues", &params).is_err());
        assert_eq!(server.count(), 2);
    }

    #[test]
    fn a_filter_value_stays_one_query_value() {
        use crate::core::providers::hardened_http::redirect_tests::Server;
        let server = Server::new(200, None, "[]");
        let provider = GitHubProvider::with_config(config(&server.url, "reader-token", "repo"));
        let params = super::ProviderParams {
            state: Some("all&per_page=100&sort=created#x".into()),
            ..super::ProviderParams::default()
        };
        provider.execute("pull_requests", &params).unwrap();
        assert_eq!(
            server.last_request_line().unwrap(),
            "GET /repos/acme/repo/pulls?per_page=20&state=all%26per_page%3D100%26sort%3Dcreated%23x&sort=updated&direction=desc HTTP/1.1"
        );
    }

    #[test]
    fn an_item_is_reacquired_now_with_its_kind_and_credential() {
        use crate::core::providers::hardened_http::redirect_tests::Server;
        let server = Server::new(
            200,
            None,
            r#"{"number":5,"title":"t","state":"open","updated_at":"t1"}"#,
        );
        let provider = GitHubProvider::with_config(config(&server.url, "reader-token", "repo"));
        let params = super::ProviderParams::default();
        let binding = provider.reuse_binding("issues", &params).unwrap();
        assert!(provider.reuse_binding("actions", &params).is_none());

        assert_eq!(
            provider
                .reacquire_item("issues", &params, "5")
                .unwrap()
                .items[0]
                .id,
            "5"
        );
        assert_eq!(
            server.last_request_line().unwrap(),
            "GET /repos/acme/repo/issues/5 HTTP/1.1"
        );
        // The issues endpoint answering with a pull request is not the stored issue.
        server.respond(
            200,
            r#"{"number":5,"title":"t","pull_request":{"url":"u"}}"#,
        );
        assert!(provider.reacquire_item("issues", &params, "5").is_err());
        // Lost access: GitHub answers 404.
        server.respond(404, r#"{"message":"Not Found"}"#);
        assert!(provider.reacquire_item("issues", &params, "5").is_err());
        let before = server.count();
        for bad in ["0", "05", "5/../6", "x"] {
            assert!(
                provider.reacquire_item("issues", &params, bad).is_err(),
                "{bad}"
            );
        }
        assert_eq!(server.count(), before);
        // Another token or repository is another source.
        for other in [
            config(&server.url, "other-token", "repo"),
            config(&server.url, "reader-token", "other"),
        ] {
            assert_ne!(
                GitHubProvider::with_config(other).reuse_binding("issues", &params),
                Some(binding.clone())
            );
        }
    }
}
