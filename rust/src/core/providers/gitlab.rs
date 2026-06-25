use super::cache;
use super::config::GitLabConfig;
use super::provider_trait::{ContextProvider, ProviderParams};
use super::{ProviderItem, ProviderResult};

const DEFAULT_PER_PAGE: usize = 20;
const CACHE_TTL_SECS: u64 = 120;

pub fn list_issues(
    config: &GitLabConfig,
    state: Option<&str>,
    labels: Option<&str>,
    limit: Option<usize>,
) -> Result<ProviderResult, String> {
    let project = config
        .project_path
        .as_deref()
        .ok_or("No project path configured. Set CI_PROJECT_PATH or configure git remote.")?;
    let encoded = urlencoding::encode(project);
    let per_page = limit.unwrap_or(DEFAULT_PER_PAGE).min(100);

    let mut url =
        format!("/projects/{encoded}/issues?per_page={per_page}&order_by=updated_at&sort=desc");
    if let Some(s) = state {
        url.push_str(&format!("&state={s}"));
    }
    if let Some(l) = labels {
        url.push_str(&format!("&labels={l}"));
    }

    let cache_key = format!("gitlab:issues:{project}:{state:?}:{labels:?}:{per_page}");
    if let Some(cached) = cache::get_cached(&cache_key)
        && let Ok(result) = serde_json::from_str::<ProviderResult>(&cached)
    {
        return Ok(result);
    }

    let body = api_get(config, &url)?;
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    let result = ProviderResult {
        provider: "gitlab".to_string(),
        resource_type: "issues".to_string(),
        total_count: None,
        truncated: items.len() >= per_page,
        items: items.iter().map(parse_issue).collect(),
    };

    if let Ok(json) = serde_json::to_string(&result) {
        cache::set_cached(&cache_key, &json, CACHE_TTL_SECS);
    }
    Ok(result)
}

pub fn show_issue(config: &GitLabConfig, iid: u64) -> Result<ProviderResult, String> {
    let project = config
        .project_path
        .as_deref()
        .ok_or("No project path configured.")?;
    let encoded = urlencoding::encode(project);
    let url = format!("/projects/{encoded}/issues/{iid}");

    let body = api_get(config, &url)?;
    let issue: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    Ok(ProviderResult {
        provider: "gitlab".to_string(),
        resource_type: "issue".to_string(),
        total_count: Some(1),
        truncated: false,
        items: vec![parse_issue(&issue)],
    })
}

pub fn list_mrs(
    config: &GitLabConfig,
    state: Option<&str>,
    limit: Option<usize>,
) -> Result<ProviderResult, String> {
    let project = config
        .project_path
        .as_deref()
        .ok_or("No project path configured.")?;
    let encoded = urlencoding::encode(project);
    let per_page = limit.unwrap_or(DEFAULT_PER_PAGE).min(100);

    let mut url = format!(
        "/projects/{encoded}/merge_requests?per_page={per_page}&order_by=updated_at&sort=desc"
    );
    if let Some(s) = state {
        url.push_str(&format!("&state={s}"));
    }

    let cache_key = format!("gitlab:mrs:{project}:{state:?}:{per_page}");
    if let Some(cached) = cache::get_cached(&cache_key)
        && let Ok(result) = serde_json::from_str::<ProviderResult>(&cached)
    {
        return Ok(result);
    }

    let body = api_get(config, &url)?;
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    let result = ProviderResult {
        provider: "gitlab".to_string(),
        resource_type: "merge_requests".to_string(),
        total_count: None,
        truncated: items.len() >= per_page,
        items: items.iter().map(parse_mr).collect(),
    };

    if let Ok(json) = serde_json::to_string(&result) {
        cache::set_cached(&cache_key, &json, CACHE_TTL_SECS);
    }
    Ok(result)
}

pub fn list_pipelines(
    config: &GitLabConfig,
    status: Option<&str>,
    limit: Option<usize>,
) -> Result<ProviderResult, String> {
    let project = config
        .project_path
        .as_deref()
        .ok_or("No project path configured.")?;
    let encoded = urlencoding::encode(project);
    let per_page = limit.unwrap_or(DEFAULT_PER_PAGE).min(100);

    let mut url =
        format!("/projects/{encoded}/pipelines?per_page={per_page}&order_by=updated_at&sort=desc");
    if let Some(s) = status {
        url.push_str(&format!("&status={s}"));
    }

    let body = api_get(config, &url)?;
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    Ok(ProviderResult {
        provider: "gitlab".to_string(),
        resource_type: "pipelines".to_string(),
        total_count: None,
        truncated: items.len() >= per_page,
        items: items
            .iter()
            .map(|p| ProviderItem {
                id: p["id"].as_u64().unwrap_or(0).to_string(),
                title: p["ref"].as_str().unwrap_or("").to_string(),
                state: p["status"].as_str().map(std::string::ToString::to_string),
                author: None,
                created_at: p["created_at"]
                    .as_str()
                    .map(std::string::ToString::to_string),
                updated_at: p["updated_at"]
                    .as_str()
                    .map(std::string::ToString::to_string),
                url: p["web_url"].as_str().map(std::string::ToString::to_string),
                labels: Vec::new(),
                body: None,
                ..Default::default()
            })
            .collect(),
    })
}

pub struct GitLabProvider {
    config: Result<GitLabConfig, String>,
}

impl GitLabProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: GitLabConfig::from_env(),
        }
    }
}

impl Default for GitLabProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextProvider for GitLabProvider {
    fn id(&self) -> &'static str {
        "gitlab"
    }

    fn display_name(&self) -> &'static str {
        "GitLab"
    }

    fn supported_actions(&self) -> &[&str] {
        &["issues", "merge_requests", "pipelines"]
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        let config = self.config.as_ref().map_err(std::clone::Clone::clone)?;
        match action {
            "issues" => list_issues(config, params.state.as_deref(), None, params.limit),
            "merge_requests" | "mrs" => list_mrs(config, params.state.as_deref(), params.limit),
            "pipelines" => list_pipelines(config, params.state.as_deref(), params.limit),
            _ => Err(format!("Unknown GitLab action: {action}")),
        }
    }

    fn cache_ttl_secs(&self) -> u64 {
        CACHE_TTL_SECS
    }

    fn is_available(&self) -> bool {
        self.config.is_ok()
    }
}

fn api_get(config: &GitLabConfig, endpoint: &str) -> Result<String, String> {
    let url = config.api_url(endpoint);
    let response = ureq::get(&url)
        .header("PRIVATE-TOKEN", &config.token)
        .call()
        .map_err(|e| format!("GitLab API error: {e}"))?;

    if response.status() != 200 {
        return Err(format!("GitLab API returned status {}", response.status()));
    }

    response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))
}

fn parse_issue(v: &serde_json::Value) -> ProviderItem {
    ProviderItem {
        id: v["iid"].as_u64().unwrap_or(0).to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().map(std::string::ToString::to_string),
        author: v["author"]["username"]
            .as_str()
            .map(std::string::ToString::to_string),
        created_at: v["created_at"]
            .as_str()
            .map(std::string::ToString::to_string),
        updated_at: v["updated_at"]
            .as_str()
            .map(std::string::ToString::to_string),
        url: v["web_url"].as_str().map(std::string::ToString::to_string),
        labels: v["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l.as_str().map(std::string::ToString::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        body: v["description"]
            .as_str()
            .map(std::string::ToString::to_string),
        ..Default::default()
    }
}

fn parse_mr(v: &serde_json::Value) -> ProviderItem {
    ProviderItem {
        id: v["iid"].as_u64().unwrap_or(0).to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().map(std::string::ToString::to_string),
        author: v["author"]["username"]
            .as_str()
            .map(std::string::ToString::to_string),
        created_at: v["created_at"]
            .as_str()
            .map(std::string::ToString::to_string),
        updated_at: v["updated_at"]
            .as_str()
            .map(std::string::ToString::to_string),
        url: v["web_url"].as_str().map(std::string::ToString::to_string),
        labels: v["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l.as_str().map(std::string::ToString::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        body: v["description"]
            .as_str()
            .map(std::string::ToString::to_string),
        ..Default::default()
    }
}
