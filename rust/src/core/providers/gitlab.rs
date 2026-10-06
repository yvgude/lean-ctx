// SPDX-License-Identifier: Apache-2.0

use super::config::GitLabConfig;
use super::provider_trait::{ContextProvider, ProviderParams};
use super::{ProviderItem, ProviderResult};

const DEFAULT_PER_PAGE: usize = 20;

fn push_filter(url: &mut String, key: &str, value: &str) {
    let value = urlencoding::encode(value);
    url.push_str(&format!("&{key}={value}"));
}

fn issues_endpoint(
    encoded_project: &str,
    per_page: usize,
    state: Option<&str>,
    labels: Option<&str>,
) -> String {
    let mut url = format!(
        "/projects/{encoded_project}/issues?per_page={per_page}&order_by=updated_at&sort=desc"
    );
    if let Some(s) = state {
        push_filter(&mut url, "state", s);
    }
    if let Some(l) = labels {
        push_filter(&mut url, "labels", l);
    }
    url
}

fn merge_requests_endpoint(encoded_project: &str, per_page: usize, state: Option<&str>) -> String {
    let mut url = format!(
        "/projects/{encoded_project}/merge_requests?per_page={per_page}&order_by=updated_at&sort=desc"
    );
    if let Some(s) = state {
        push_filter(&mut url, "state", s);
    }
    url
}

fn pipelines_endpoint(encoded_project: &str, per_page: usize, status: Option<&str>) -> String {
    let mut url = format!(
        "/projects/{encoded_project}/pipelines?per_page={per_page}&order_by=updated_at&sort=desc"
    );
    if let Some(s) = status {
        push_filter(&mut url, "status", s);
    }
    url
}

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

    let url = issues_endpoint(&encoded, per_page, state, labels);

    // A cached response is not evidence of current source authorization. Ask
    // GitLab on every acquisition, including after a token or membership revoke.
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

    let url = merge_requests_endpoint(&encoded, per_page, state);

    // Use the same fresh authorization boundary as issues, not stale cache data.
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

    let url = pipelines_endpoint(&encoded, per_page, status);

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
    pub fn new() -> Self {
        Self {
            config: GitLabConfig::from_session(),
        }
    }

    /// Construct with an explicit config, bypassing env discovery. Used by the
    /// hosted team server's managed connectors so each scheduled run carries its
    /// own credential without mutating process-global env (which would race
    /// across connectors).
    #[must_use]
    pub fn with_config(config: GitLabConfig) -> Self {
        Self { config: Ok(config) }
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
        super::selected_gitlab::check_project(params.project.as_deref())?;
        let config = self.config.as_ref().map_err(std::clone::Clone::clone)?;
        match action {
            "issues" => list_issues(config, params.state.as_deref(), None, params.limit),
            "merge_requests" | "mrs" => list_mrs(config, params.state.as_deref(), params.limit),
            "pipelines" => list_pipelines(config, params.state.as_deref(), params.limit),
            _ => Err(format!("Unknown GitLab action: {action}")),
        }
    }

    fn cache_ttl_secs(&self) -> u64 {
        0
    }

    fn reuse_binding(&self, action: &str, params: &ProviderParams) -> Option<String> {
        if !matches!(action, "issues" | "merge_requests") {
            return None;
        }
        super::selected_gitlab::check_project(params.project.as_deref()).ok()?;
        super::selected_gitlab::reuse_binding(self.config.as_ref().ok()?)
    }

    fn reacquire_item(
        &self,
        action: &str,
        params: &ProviderParams,
        item_id: &str,
    ) -> Result<ProviderResult, String> {
        if self.reuse_binding(action, params).is_none() {
            return Err("selected source is unavailable for item reauthorization".into());
        }
        let id: u64 = item_id
            .parse()
            .map_err(|_| "invalid stored GitLab item ID")?;
        if id == 0 || id.to_string() != item_id {
            return Err("invalid stored GitLab item ID".into());
        }
        let config = self.config.as_ref().map_err(Clone::clone)?;
        let project = config
            .project_path
            .as_deref()
            .ok_or("selected project missing")?;
        let endpoint = format!("/projects/{}/{action}/{id}", urlencoding::encode(project));
        let body = api_get(config, &endpoint)?;
        let value = serde_json::from_str(&body).map_err(|_| "invalid GitLab item response")?;
        let item = match action {
            "issues" => parse_issue(&value),
            "merge_requests" => parse_mr(&value),
            _ => return Err("unsupported stored GitLab resource".into()),
        };
        Ok(ProviderResult {
            provider: "gitlab".into(),
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

fn api_get(config: &GitLabConfig, endpoint: &str) -> Result<String, String> {
    super::selected_gitlab::authorize(config)?;
    let url = config.api_url(endpoint);
    super::hardened_http::provider_get_with_headers(
        "gitlab",
        &url,
        &[("PRIVATE-TOKEN", config.token.as_str())],
    )
    .into_body()
    .map_err(|e| format!("GitLab API error: {e}"))
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

#[cfg(test)]
mod authorization_tests;

#[cfg(test)]
mod tests {
    use super::{issues_endpoint, merge_requests_endpoint, pipelines_endpoint};

    /// Split the query half of an endpoint into raw `key=value` pairs the way a
    /// server's query parser would: on `&`, then on the first `=`. Values are left
    /// percent-encoded so the tests can assert on the wire form.
    fn query_pairs(endpoint: &str) -> Vec<(String, String)> {
        let query = endpoint.split_once('?').expect("endpoint has a query").1;
        query
            .split('&')
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (key.to_owned(), value.to_owned())
            })
            .collect()
    }

    fn value_of(endpoint: &str, key: &str) -> Option<String> {
        query_pairs(endpoint)
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    fn keys_of(endpoint: &str) -> Vec<String> {
        query_pairs(endpoint).into_iter().map(|(k, _)| k).collect()
    }

    /// The filter value a caller passed must survive as exactly one query value.
    fn assert_round_trips(endpoint: &str, key: &str, original: &str) {
        let encoded = value_of(endpoint, key).expect("filter parameter present");
        let decoded = urlencoding::decode(&encoded).expect("value is valid percent-encoding");
        assert_eq!(
            decoded, original,
            "filter {key} must decode back to the caller's exact value"
        );
    }

    #[test]
    fn plain_filters_keep_their_literal_form() {
        let endpoint = issues_endpoint("group%2Fproject", 20, Some("opened"), Some("bug,urgent"));
        assert_eq!(value_of(&endpoint, "state").as_deref(), Some("opened"));
        // Comma is percent-encoded on the wire but still decodes to a label list.
        assert_round_trips(&endpoint, "labels", "bug,urgent");
        assert_eq!(value_of(&endpoint, "per_page").as_deref(), Some("20"));
        assert_eq!(
            value_of(&endpoint, "order_by").as_deref(),
            Some("updated_at")
        );
        assert_eq!(value_of(&endpoint, "sort").as_deref(), Some("desc"));
    }

    #[test]
    fn absent_filters_add_no_parameters() {
        assert_eq!(
            keys_of(&issues_endpoint("p", 20, None, None)),
            ["per_page", "order_by", "sort"]
        );
        assert_eq!(
            keys_of(&merge_requests_endpoint("p", 20, None)),
            ["per_page", "order_by", "sort"]
        );
        assert_eq!(
            keys_of(&pipelines_endpoint("p", 20, None)),
            ["per_page", "order_by", "sort"]
        );
    }

    #[test]
    fn ampersand_in_issue_state_cannot_inject_a_parameter() {
        let hostile = "opened&per_page=100";
        let endpoint = issues_endpoint("p", 20, Some(hostile), None);
        assert_eq!(
            keys_of(&endpoint),
            ["per_page", "order_by", "sort", "state"],
            "hostile filter must not add a query parameter"
        );
        assert_eq!(value_of(&endpoint, "per_page").as_deref(), Some("20"));
        assert_round_trips(&endpoint, "state", hostile);
    }

    #[test]
    fn ampersand_in_labels_cannot_inject_a_parameter() {
        let hostile = "bug&state=closed";
        let endpoint = issues_endpoint("p", 20, None, Some(hostile));
        assert_eq!(
            keys_of(&endpoint),
            ["per_page", "order_by", "sort", "labels"]
        );
        assert_round_trips(&endpoint, "labels", hostile);
    }

    #[test]
    fn ampersand_in_mr_state_cannot_inject_a_parameter() {
        let hostile = "opened&per_page=100";
        let endpoint = merge_requests_endpoint("p", 20, Some(hostile));
        assert_eq!(
            keys_of(&endpoint),
            ["per_page", "order_by", "sort", "state"]
        );
        assert_eq!(value_of(&endpoint, "per_page").as_deref(), Some("20"));
        assert_round_trips(&endpoint, "state", hostile);
    }

    #[test]
    fn ampersand_in_pipeline_status_cannot_inject_a_parameter() {
        let hostile = "success&ref=main";
        let endpoint = pipelines_endpoint("p", 20, Some(hostile));
        assert_eq!(
            keys_of(&endpoint),
            ["per_page", "order_by", "sort", "status"]
        );
        assert_round_trips(&endpoint, "status", hostile);
    }

    #[test]
    fn fragment_marker_cannot_truncate_the_query() {
        let hostile = "opened#frag";
        let endpoint = issues_endpoint("p", 20, Some(hostile), None);
        assert!(
            !endpoint.contains('#'),
            "a literal '#' would make the rest of the query a fragment: {endpoint}"
        );
        assert_round_trips(&endpoint, "state", hostile);
    }

    #[test]
    fn reserved_characters_stay_inside_one_value() {
        for original in [
            "opened=closed",
            "needs review",
            "rust+lang",
            "a=b&c=d#e f",
            "50%",
        ] {
            let endpoint = issues_endpoint("p", 20, Some(original), None);
            assert_eq!(
                keys_of(&endpoint),
                ["per_page", "order_by", "sort", "state"],
                "value {original:?} must stay one parameter"
            );
            assert_round_trips(&endpoint, "state", original);
        }
    }

    #[test]
    fn literal_plus_is_not_decoded_as_a_space() {
        // `+` means space in form decoding, so it must go out as %2B to survive.
        let endpoint = issues_endpoint("p", 20, Some("rust+lang"), None);
        assert_eq!(value_of(&endpoint, "state").as_deref(), Some("rust%2Blang"));
        assert_round_trips(&endpoint, "state", "rust+lang");
    }

    #[test]
    fn space_is_percent_encoded_not_left_raw() {
        let endpoint = issues_endpoint("p", 20, Some("needs review"), None);
        assert!(
            !endpoint.contains(' '),
            "a raw space would break the HTTP request line: {endpoint}"
        );
        assert_eq!(
            value_of(&endpoint, "state").as_deref(),
            Some("needs%20review")
        );
    }

    #[test]
    fn unicode_filter_values_round_trip() {
        for original in ["priorität:hoch", "标签", "naïve café", "🙂"] {
            let endpoint = issues_endpoint("p", 20, None, Some(original));
            assert!(endpoint.is_ascii(), "wire form must be ASCII: {endpoint}");
            assert_round_trips(&endpoint, "labels", original);
        }
    }

    #[test]
    fn project_path_stays_encoded_alongside_filters() {
        let endpoint = issues_endpoint(
            &urlencoding::encode("group/sub/project"),
            50,
            Some("opened"),
            None,
        );
        assert!(endpoint.starts_with("/projects/group%2Fsub%2Fproject/issues?"));
        assert_eq!(value_of(&endpoint, "per_page").as_deref(), Some("50"));
    }
}
