//! Jira provider — issues, sprints, and boards via the Jira REST API.
//!
//! Configuration via environment variables:
//!   - `JIRA_URL`: Base URL (e.g., `https://company.atlassian.net`)
//!   - `JIRA_EMAIL`: User email for Basic Auth
//!   - `JIRA_TOKEN`: API token
//!   - `JIRA_PROJECT`: Default project key (e.g., "PROJ")
//!   - `JIRA_DEPLOYMENT`: `cloud` (default) or `server` for Data Center/Server
//!
//! Authentication is resolved in this order:
//!   1. **OAuth 2.0 (3LO)** — used when a stored OAuth credential exists for the
//!      data source (see [`crate::core::providers::jira_oauth`]) or when
//!      `JIRA_AUTH=oauth` is set. Bearer tokens are auto-refreshed and Cloud API
//!      calls are routed through `https://api.atlassian.com/ex/jira/{cloudId}`.
//!   2. **Basic auth** — the classic `JIRA_EMAIL` + `JIRA_TOKEN` API-token flow,
//!      which remains the default and the recommended path for Jira Server / Data
//!      Center.

use crate::core::providers::jira_oauth;
use crate::core::providers::{ContextProvider, ProviderItem, ProviderParams, ProviderResult};

const B64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn simple_base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_CHARS[((n >> 18) & 63) as usize] as char);
        out.push(B64_CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_CHARS[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JiraDeployment {
    Cloud,
    Server,
}

/// How a `JiraConfig` authenticates to Jira.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JiraAuth {
    /// Classic email + API token (Basic auth). Default; works for Cloud and
    /// Server / Data Center.
    Basic { email: String, token: String },
    /// OAuth 2.0 (3LO) bearer tokens resolved from the named data source's
    /// stored credential (Jira Cloud only).
    OAuth { data_source: String },
}

pub struct JiraConfig {
    pub base_url: String,
    pub project: Option<String>,
    pub deployment: JiraDeployment,
    pub auth: JiraAuth,
}

/// The per-request resolved routing + credential.
struct ResolvedAuth {
    /// Base URL for REST calls (Cloud OAuth routes via `api.atlassian.com`).
    api_base: String,
    /// Base URL for `/browse/` item links (the user-facing site URL).
    browse_base: String,
    /// The `Authorization` header value (`Basic …` or `Bearer …`).
    auth_header: String,
}

impl JiraConfig {
    pub fn from_env() -> Result<Self, String> {
        let project = super::credentials::var("JIRA_PROJECT").ok();

        let deployment = match super::credentials::var("JIRA_DEPLOYMENT")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "server" | "dc" | "datacenter" => JiraDeployment::Server,
            _ => JiraDeployment::Cloud,
        };

        // Prefer OAuth when a credential is already stored for the data source,
        // or when explicitly forced via JIRA_AUTH=oauth.
        let data_source = super::credentials::var("JIRA_DATA_SOURCE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "jira".to_string());
        let force_oauth =
            super::credentials::var("JIRA_AUTH").is_ok_and(|v| v.eq_ignore_ascii_case("oauth"));
        // A candidate check tests the given API token, not a stored OAuth grant.
        let has_oauth_cred = !super::credentials::candidates_active()
            && jira_oauth::get_credential(&data_source).is_some();

        if force_oauth || has_oauth_cred {
            // OAuth is Jira Cloud only; JIRA_URL is optional and only used as a
            // fallback for browse links if the stored site URL is unavailable.
            let base_url = super::credentials::var("JIRA_URL")
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_string();
            return Ok(Self {
                base_url,
                project,
                deployment: JiraDeployment::Cloud,
                auth: JiraAuth::OAuth { data_source },
            });
        }

        let base_url = super::credentials::var("JIRA_URL").map_err(|_| "JIRA_URL not set")?;
        let email = super::credentials::var("JIRA_EMAIL").map_err(|_| "JIRA_EMAIL not set")?;
        let token = super::credentials::var("JIRA_TOKEN").map_err(|_| "JIRA_TOKEN not set")?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            project,
            deployment,
            auth: JiraAuth::Basic { email, token },
        })
    }

    /// Who Jira says these credentials belong to (`/myself`), for a
    /// connection check. Read-only; nothing is cached.
    pub(crate) fn whoami(&self) -> Result<String, String> {
        let auth = self.resolve()?;
        let body = super::hardened_http::provider_get_with_headers(
            "jira",
            &format!("{}/rest/api/2/myself", auth.api_base),
            &[
                ("Authorization", &auth.auth_header),
                ("Accept", "application/json"),
            ],
        )
        .into_body()
        .map_err(|e| format!("Jira refused the credentials: {e}"))?;
        let value: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| "Jira returned no account".to_string())?;
        Ok(["displayName", "emailAddress", "name"]
            .iter()
            .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
            .unwrap_or("signed in")
            .to_string())
    }

    /// Resolves the effective base URLs and `Authorization` header for a request,
    /// refreshing OAuth tokens on demand.
    fn resolve(&self) -> Result<ResolvedAuth, String> {
        match &self.auth {
            JiraAuth::Basic { email, token } => {
                let credentials = format!("{email}:{token}");
                let encoded = simple_base64(credentials.as_bytes());
                Ok(ResolvedAuth {
                    api_base: self.base_url.clone(),
                    browse_base: self.base_url.clone(),
                    auth_header: format!("Basic {encoded}"),
                })
            }
            JiraAuth::OAuth { data_source } => {
                let tok = jira_oauth::ensure_valid_access_token(data_source)?;
                let browse_base = if tok.cloud_url.is_empty() {
                    self.base_url.clone()
                } else {
                    tok.cloud_url.trim_end_matches('/').to_string()
                };
                Ok(ResolvedAuth {
                    api_base: format!("{}/{}", jira_oauth::API_BASE, tok.cloud_id),
                    browse_base,
                    auth_header: format!("Bearer {}", tok.access_token),
                })
            }
        }
    }
}

pub struct JiraProvider {
    config: Result<JiraConfig, String>,
}

impl Default for JiraProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl JiraProvider {
    pub fn new() -> Self {
        Self {
            config: JiraConfig::from_env(),
        }
    }

    /// Construct with an explicit config, bypassing env discovery.
    #[must_use]
    pub fn with_config(config: JiraConfig) -> Self {
        Self { config: Ok(config) }
    }
}

/// A Jira project key as Jira defines it: an uppercase letter, then uppercase
/// letters, digits or underscores. Anything else is refused, never quoted into
/// JQL, so a caller value cannot become a JQL clause.
fn valid_project_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    (2..=255).contains(&value.len())
        && bytes.next().is_some_and(|b| b.is_ascii_uppercase())
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// An issue key `PROJECT-123` with a valid project key and a positive number.
fn valid_issue_key(value: &str) -> bool {
    value.rsplit_once('-').is_some_and(|(project, number)| {
        valid_project_key(project)
            && !number.is_empty()
            && !number.starts_with('0')
            && number.bytes().all(|b| b.is_ascii_digit())
    })
}

impl ContextProvider for JiraProvider {
    fn id(&self) -> &'static str {
        "jira"
    }

    fn display_name(&self) -> &'static str {
        "Jira"
    }

    fn supported_actions(&self) -> &[&str] {
        &["issues", "sprints"]
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        let config = self.config.as_ref().map_err(std::clone::Clone::clone)?;
        match action {
            "issues" => list_issues(config, params),
            "sprints" => list_sprints(config, params),
            _ => Err(format!("Unsupported action: {action}")),
        }
    }

    fn cache_ttl_secs(&self) -> u64 {
        0
    }

    fn descriptor(&self) -> super::provider_trait::ConnectorDescriptor {
        descriptor()
    }

    /// The source a stored issue came from: site, deployment and the identity
    /// that read it (API-token user or OAuth data source). Another account or
    /// site is another source.
    fn reuse_binding(&self, action: &str, _params: &ProviderParams) -> Option<String> {
        if action != "issues" {
            return None;
        }
        let config = self.config.as_ref().ok()?;
        let identity = match &config.auth {
            JiraAuth::Basic { email, token } => {
                serde_json::json!({"basic": email, "credential": token})
            }
            JiraAuth::OAuth { data_source } => serde_json::json!({"oauth": data_source}),
        };
        super::provenance::digest(&serde_json::json!({
            "version": 1, "provider": "jira", "site": config.base_url,
            "deployment": matches!(config.deployment, JiraDeployment::Server), "identity": identity,
        }))
    }

    fn reacquire_item(
        &self,
        action: &str,
        params: &ProviderParams,
        item_id: &str,
    ) -> Result<ProviderResult, String> {
        if self.reuse_binding(action, params).is_none() {
            return Err("Jira source is unavailable for item reauthorization".into());
        }
        if !valid_issue_key(item_id) {
            return Err("invalid stored Jira issue key".into());
        }
        let config = self.config.as_ref().map_err(Clone::clone)?;
        let resolved = config.resolve()?;
        let version = match config.deployment {
            JiraDeployment::Cloud => 3,
            JiraDeployment::Server => 2,
        };
        // Jira answers 404 for an issue the identity may not see, including
        // issues behind an issue-security level: that is "no longer accessible".
        let url = format!(
            "{}/rest/api/{version}/issue/{item_id}?fields=summary,status,reporter,created,updated,labels,description",
            resolved.api_base
        );
        let text = jira_request(&resolved.auth_header, "GET", &url, None)?;
        let issue: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| "invalid Jira issue response")?;
        if issue["key"].as_str() != Some(item_id) {
            return Err("stored Jira issue moved or was renamed".into());
        }
        Ok(ProviderResult {
            provider: "jira".into(),
            resource_type: "issues".into(),
            items: vec![parse_issue(&issue, &resolved.browse_base)],
            total_count: Some(1),
            truncated: false,
        })
    }

    fn requires_auth(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.config.is_ok()
    }
}

// ---------------------------------------------------------------------------
// HTTP helper with status-code-aware error messages
// ---------------------------------------------------------------------------

fn jira_request(
    auth_header: &str,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
) -> Result<String, String> {
    let agent = super::hardened_http::hardened_agent();
    let resp = match method {
        "POST" => agent
            .post(url)
            .header("Authorization", auth_header)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send(body.unwrap_or(&[]))
            .map_err(|ref e| jira_error_with_hint(e))?,
        _ => agent
            .get(url)
            .header("Authorization", auth_header)
            .header("Accept", "application/json")
            .call()
            .map_err(|ref e| jira_error_with_hint(e))?,
    };

    if !resp.status().is_success() {
        return Err(format!("Jira API error: HTTP {}", resp.status().as_u16()));
    }

    resp.into_body()
        .into_with_config()
        .limit(super::hardened_http::MAX_RESPONSE_BODY_BYTES)
        .read_to_string()
        .map_err(|e| format!("Jira read error: {e}"))
}

fn jira_error_with_hint(e: &ureq::Error) -> String {
    let hint = match e {
        ureq::Error::StatusCode(410) => {
            " (endpoint removed — update lean-ctx or check Jira Cloud API version)"
        }
        ureq::Error::StatusCode(401) => " (check JIRA_EMAIL + JIRA_TOKEN credentials)",
        ureq::Error::StatusCode(403) => " (insufficient permissions for this resource)",
        ureq::Error::StatusCode(404) => {
            " (endpoint not found — check JIRA_URL and JIRA_DEPLOYMENT setting)"
        }
        _ => "",
    };
    format!("Jira API error: {e}{hint}")
}

#[cfg(test)]
#[test]
fn jira_requests_keep_credentials_and_body_at_selected_origin() {
    use super::hardened_http::redirect_tests::Server;
    let target = Server::new(200, None, "{}");
    let body = "synthetic-private-jira-query";
    for status in [301, 302, 303, 307, 308] {
        let origin = Server::new(status, Some(&target.url), body);
        for method in ["GET", "POST"] {
            let error = jira_request(
                "Bearer synthetic-token",
                method,
                &origin.url,
                Some(body.as_bytes()),
            )
            .unwrap_err();
            assert!(error.contains(&status.to_string()));
            assert!(!error.contains(body));
        }
        assert_eq!(origin.count(), 2);
        assert!(origin.received_body(body));
        assert_eq!(target.count(), 0);
    }
    assert_eq!(
        jira_request(
            "Bearer synthetic-token",
            "POST",
            &target.url,
            Some(body.as_bytes())
        )
        .unwrap(),
        "{}"
    );
    assert!(target.received_body(body));
}

// ---------------------------------------------------------------------------
// Issues — Cloud: POST /rest/api/3/search/jql  |  Server: GET /rest/api/2/search
// ---------------------------------------------------------------------------

fn list_issues(config: &JiraConfig, params: &ProviderParams) -> Result<ProviderResult, String> {
    build_jql(config, params)?;
    match config.deployment {
        JiraDeployment::Cloud => list_issues_cloud(config, params),
        JiraDeployment::Server => list_issues_server(config, params),
    }
}

fn build_jql(config: &JiraConfig, params: &ProviderParams) -> Result<String, String> {
    let project = params
        .state
        .as_deref()
        .or(config.project.as_deref())
        .unwrap_or("*");

    if project == "*" {
        Ok("ORDER BY updated DESC".to_string())
    } else if valid_project_key(project) {
        Ok(format!("project={project} ORDER BY updated DESC"))
    } else {
        Err("invalid Jira project key".into())
    }
}

fn list_issues_cloud(
    config: &JiraConfig,
    params: &ProviderParams,
) -> Result<ProviderResult, String> {
    let resolved = config.resolve()?;
    let limit = params.limit.unwrap_or(20);
    let jql = build_jql(config, params)?;
    let url = format!("{}/rest/api/3/search/jql", resolved.api_base);

    let mut all_items = Vec::new();
    let mut next_page_token: Option<String> = None;
    loop {
        let page_size = (limit - all_items.len()).min(100);
        let mut body = serde_json::json!({
            "jql": jql,
            "maxResults": page_size,
            "fields": ["summary", "status", "reporter", "created", "updated", "labels", "description"]
        });
        if let Some(ref token) = next_page_token {
            body["nextPageToken"] = serde_json::json!(token);
        }

        let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
        let text = jira_request(&resolved.auth_header, "POST", &url, Some(&body_bytes))?;
        let resp: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Jira JSON parse error: {e}"))?;

        let issues = resp["issues"].as_array().cloned().unwrap_or_default();
        all_items.extend(
            issues
                .iter()
                .map(|issue| parse_issue(issue, &resolved.browse_base)),
        );

        next_page_token = resp["nextPageToken"].as_str().map(String::from);

        if next_page_token.is_none() || all_items.len() >= limit {
            break;
        }
    }

    let truncated = next_page_token.is_some();
    all_items.truncate(limit);

    Ok(ProviderResult {
        provider: "jira".into(),
        resource_type: "issues".into(),
        total_count: Some(all_items.len()),
        truncated,
        items: all_items,
    })
}

fn list_issues_server(
    config: &JiraConfig,
    params: &ProviderParams,
) -> Result<ProviderResult, String> {
    let resolved = config.resolve()?;
    let limit = params.limit.unwrap_or(20);
    let jql = build_jql(config, params)?;

    let url = format!(
        "{}/rest/api/2/search?jql={}&maxResults={limit}",
        resolved.api_base,
        urlencoding::encode(&jql)
    );

    let text = jira_request(&resolved.auth_header, "GET", &url, None)?;
    let body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Jira JSON parse error: {e}"))?;

    let total = body["total"].as_u64().unwrap_or(0) as usize;
    let issues = body["issues"].as_array().cloned().unwrap_or_default();

    let items: Vec<ProviderItem> = issues
        .iter()
        .map(|issue| parse_issue(issue, &resolved.browse_base))
        .collect();

    Ok(ProviderResult {
        provider: "jira".into(),
        resource_type: "issues".into(),
        items,
        total_count: Some(total),
        truncated: total > limit,
    })
}

fn parse_issue(issue: &serde_json::Value, browse_base: &str) -> ProviderItem {
    let fields = &issue["fields"];
    ProviderItem {
        id: issue["key"].as_str().unwrap_or_default().to_string(),
        title: fields["summary"].as_str().unwrap_or_default().to_string(),
        state: fields["status"]["name"].as_str().map(String::from),
        author: fields["reporter"]["displayName"].as_str().map(String::from),
        created_at: fields["created"].as_str().map(String::from),
        updated_at: fields["updated"].as_str().map(String::from),
        url: Some(format!(
            "{}/browse/{}",
            browse_base,
            issue["key"].as_str().unwrap_or_default()
        )),
        labels: fields["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        body: fields["description"]
            .as_str()
            .map(String::from)
            .or_else(|| {
                fields["description"]["content"]
                    .as_array()
                    .map(|_| "[Jira rich text — see web UI]".to_string())
            }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Sprints — /rest/agile/1.0/board/{id}/sprint
// ---------------------------------------------------------------------------

fn list_sprints(config: &JiraConfig, params: &ProviderParams) -> Result<ProviderResult, String> {
    let board_id = params
        .state
        .as_deref()
        .ok_or("Sprint listing requires a board ID via the 'state' parameter")?;
    if board_id.is_empty() || board_id.len() > 19 || !board_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Jira board ID must be a number".into());
    }

    let resolved = config.resolve()?;
    let limit = params.limit.unwrap_or(5);
    let url = format!(
        "{}/rest/agile/1.0/board/{board_id}/sprint?state=active,future&maxResults={limit}",
        resolved.api_base
    );

    let text = jira_request(&resolved.auth_header, "GET", &url, None)?;
    let body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Jira JSON parse error: {e}"))?;

    let sprints = body["values"].as_array().cloned().unwrap_or_default();
    let items: Vec<ProviderItem> = sprints
        .iter()
        .map(|s| ProviderItem {
            id: s["id"].as_u64().map_or_else(String::new, |n| n.to_string()),
            title: s["name"].as_str().unwrap_or_default().to_string(),
            state: s["state"].as_str().map(String::from),
            author: None,
            created_at: s["startDate"].as_str().map(String::from),
            updated_at: s["endDate"].as_str().map(String::from),
            url: None,
            labels: vec![],
            body: s["goal"].as_str().map(String::from),
            ..Default::default()
        })
        .collect();

    Ok(ProviderResult {
        provider: "jira".into(),
        resource_type: "sprints".into(),
        items,
        total_count: Some(sprints.len()),
        truncated: false,
    })
}

pub(crate) fn descriptor() -> super::provider_trait::ConnectorDescriptor {
    use super::provider_trait::{CapabilityLevel as L, ConnectorDescriptor as C};
    C {
        principal_binding: "API-token user (Basic) or OAuth 3LO data source, bound with site and deployment",
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
        notes: "search = JQL by validated project key only; issues re-checked per item with the principal's \
                identity (404 incl. issue security = no longer accessible, moved key refused); comments and \
                attachments not fetched; sprints are list-only; rich-text bodies are not rendered",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that mutate the process-global `JIRA_*` environment.
    /// Without this, parallel tests race on shared env and intermittently fail.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Clears every Jira-related env var so a test starts from a known state.
    /// Pins a data source that has no stored OAuth credential so the OAuth
    /// auto-detection in `from_env` is deterministic across machines.
    fn reset_jira_env() {
        for var in [
            "JIRA_URL",
            "JIRA_EMAIL",
            "JIRA_TOKEN",
            "JIRA_PROJECT",
            "JIRA_DEPLOYMENT",
            "JIRA_AUTH",
        ] {
            crate::test_env::remove_var(var);
        }
        crate::test_env::set_var("JIRA_DATA_SOURCE", "lean-ctx-test-no-such-source");
    }

    #[test]
    fn jira_provider_is_unavailable_without_env() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_jira_env();

        let provider = JiraProvider::new();
        assert!(!provider.is_available());
        assert_eq!(provider.id(), "jira");
        assert!(provider.requires_auth());
        crate::test_env::remove_var("JIRA_DATA_SOURCE");
    }

    #[test]
    fn jira_provider_supported_actions() {
        let provider = JiraProvider::new();
        assert!(provider.supported_actions().contains(&"issues"));
        assert!(provider.supported_actions().contains(&"sprints"));
    }

    #[test]
    fn deployment_defaults_to_cloud() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_jira_env();
        crate::test_env::set_var("JIRA_URL", "https://test.atlassian.net");
        crate::test_env::set_var("JIRA_EMAIL", "test@test.com");
        crate::test_env::set_var("JIRA_TOKEN", "token");
        let cfg = JiraConfig::from_env().unwrap();
        assert_eq!(cfg.deployment, JiraDeployment::Cloud);
        assert!(matches!(cfg.auth, JiraAuth::Basic { .. }));
        reset_jira_env();
        crate::test_env::remove_var("JIRA_DATA_SOURCE");
    }

    #[test]
    fn deployment_server_variants() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for val in &["server", "dc", "datacenter", "SERVER", "DC"] {
            reset_jira_env();
            crate::test_env::set_var("JIRA_URL", "https://jira.internal");
            crate::test_env::set_var("JIRA_EMAIL", "u@e.com");
            crate::test_env::set_var("JIRA_TOKEN", "t");
            crate::test_env::set_var("JIRA_DEPLOYMENT", val);
            let cfg = JiraConfig::from_env().unwrap();
            assert_eq!(cfg.deployment, JiraDeployment::Server, "failed for {val}");
        }
        reset_jira_env();
        crate::test_env::remove_var("JIRA_DATA_SOURCE");
    }

    #[test]
    fn oauth_is_selected_when_forced() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_jira_env();
        crate::test_env::set_var("JIRA_AUTH", "oauth");
        let cfg = JiraConfig::from_env().unwrap();
        assert!(matches!(cfg.auth, JiraAuth::OAuth { .. }));
        assert_eq!(cfg.deployment, JiraDeployment::Cloud);
        reset_jira_env();
        crate::test_env::remove_var("JIRA_DATA_SOURCE");
    }

    fn basic_cfg(base_url: &str, project: Option<&str>) -> JiraConfig {
        JiraConfig {
            base_url: base_url.into(),
            project: project.map(String::from),
            deployment: JiraDeployment::Cloud,
            auth: JiraAuth::Basic {
                email: String::new(),
                token: String::new(),
            },
        }
    }

    #[test]
    fn build_jql_with_project() {
        let cfg = basic_cfg("https://x.atlassian.net", Some("PROJ"));
        let params = ProviderParams::default();
        assert_eq!(
            build_jql(&cfg, &params).unwrap(),
            "project=PROJ ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_wildcard() {
        let cfg = basic_cfg("", None);
        let params = ProviderParams::default();
        assert_eq!(build_jql(&cfg, &params).unwrap(), "ORDER BY updated DESC");
    }

    #[test]
    fn a_project_value_cannot_become_a_jql_clause() {
        let cfg = basic_cfg("https://x.atlassian.net", None);
        for injected in [
            "PROJ OR project=SECRET",
            "PROJ ORDER BY created",
            "proj",
            "PROJ\" OR 1=1",
            "P",
            "PROJ)",
            "PROJ-1",
        ] {
            let params = ProviderParams {
                state: Some(injected.into()),
                ..ProviderParams::default()
            };
            assert!(build_jql(&cfg, &params).is_err(), "{injected}");
        }
        assert!(valid_issue_key("PROJ-12") && valid_issue_key("A_1-9"));
        for key in [
            "PROJ-0",
            "PROJ-012",
            "proj-1",
            "PROJ-",
            "PROJ-1/../2",
            "PROJ-1?x=1",
        ] {
            assert!(!valid_issue_key(key), "{key}");
        }
    }

    fn reachable_cfg(url: &str) -> JiraConfig {
        JiraConfig {
            base_url: url.into(),
            project: Some("PROJ".into()),
            deployment: JiraDeployment::Server,
            auth: JiraAuth::Basic {
                email: "reader@example.test".into(),
                token: "synthetic-jira-token".into(),
            },
        }
    }

    #[test]
    fn an_issue_is_reacquired_now_and_a_lost_permission_or_move_is_refused() {
        use crate::core::providers::hardened_http::redirect_tests::Server;
        let issue = |key: &str, updated: &str| {
            serde_json::json!({"key": key, "fields": {"summary": "s", "status": {"name": "Open"},
                "updated": updated, "description": "body"}})
            .to_string()
        };
        let server = Server::new(200, None, &issue("PROJ-7", "2026-10-14T10:00:00Z"));
        let provider = JiraProvider::with_config(reachable_cfg(&server.url));
        let params = ProviderParams::default();
        let binding = provider.reuse_binding("issues", &params).unwrap();
        assert!(provider.reuse_binding("sprints", &params).is_none());

        let current = provider
            .reacquire_item("issues", &params, "PROJ-7")
            .unwrap();
        assert_eq!(current.items[0].id, "PROJ-7");
        assert_eq!(
            server.last_request_line().unwrap(),
            "GET /rest/api/2/issue/PROJ-7?fields=summary,status,reporter,created,updated,labels,description HTTP/1.1"
        );

        // Issue security or a removed project: Jira answers 404.
        server.respond(404, "{}");
        assert!(
            provider
                .reacquire_item("issues", &params, "PROJ-7")
                .is_err()
        );
        // Moved to another project: the stored key no longer names it.
        server.respond(200, &issue("OTHER-1", "2026-10-14T11:00:00Z"));
        assert!(
            provider
                .reacquire_item("issues", &params, "PROJ-7")
                .is_err()
        );
        // A malformed stored key never reaches the network.
        let before = server.count();
        assert!(
            provider
                .reacquire_item("issues", &params, "PROJ-7/../x")
                .is_err()
        );
        assert_eq!(server.count(), before);

        // Another account on the same site is another source.
        let mut other = reachable_cfg(&server.url);
        other.auth = JiraAuth::Basic {
            email: "other@example.test".into(),
            token: "synthetic-jira-token".into(),
        };
        assert_ne!(
            JiraProvider::with_config(other).reuse_binding("issues", &params),
            Some(binding)
        );
    }

    #[test]
    fn a_board_id_is_a_number() {
        let provider = JiraProvider::with_config(reachable_cfg("http://127.0.0.1:9"));
        let params = ProviderParams {
            state: Some("1/../../rest/api/2/myself".into()),
            ..ProviderParams::default()
        };
        assert_eq!(
            provider.execute("sprints", &params).unwrap_err(),
            "Jira board ID must be a number"
        );
    }

    #[test]
    fn error_hint_410() {
        let msg = jira_error_with_hint(&ureq::Error::StatusCode(410));
        assert!(msg.contains("endpoint removed"), "{msg}");
    }

    #[test]
    fn error_hint_401() {
        let msg = jira_error_with_hint(&ureq::Error::StatusCode(401));
        assert!(msg.contains("JIRA_EMAIL"), "{msg}");
    }

    #[test]
    fn error_hint_403() {
        let msg = jira_error_with_hint(&ureq::Error::StatusCode(403));
        assert!(msg.contains("permissions"), "{msg}");
    }

    #[test]
    fn error_hint_404() {
        let msg = jira_error_with_hint(&ureq::Error::StatusCode(404));
        assert!(msg.contains("JIRA_DEPLOYMENT"), "{msg}");
    }

    #[test]
    fn parse_issue_extracts_fields() {
        let issue = serde_json::json!({
            "key": "PROJ-123",
            "fields": {
                "summary": "Test issue",
                "status": { "name": "Open" },
                "reporter": { "displayName": "Alice" },
                "created": "2026-01-01T00:00:00Z",
                "updated": "2026-05-01T00:00:00Z",
                "labels": ["bug", "urgent"],
                "description": "Fix the thing"
            }
        });
        let item = parse_issue(&issue, "https://x.atlassian.net");
        assert_eq!(item.id, "PROJ-123");
        assert_eq!(item.title, "Test issue");
        assert_eq!(item.state.as_deref(), Some("Open"));
        assert_eq!(item.author.as_deref(), Some("Alice"));
        assert_eq!(item.labels, vec!["bug", "urgent"]);
        assert_eq!(item.body.as_deref(), Some("Fix the thing"));
        assert!(item.url.as_deref().unwrap().contains("/browse/PROJ-123"));
    }

    #[test]
    fn base64_encoding() {
        assert_eq!(simple_base64(b"user:token"), "dXNlcjp0b2tlbg==");
        assert_eq!(simple_base64(b"a"), "YQ==");
        assert_eq!(simple_base64(b"ab"), "YWI=");
        assert_eq!(simple_base64(b"abc"), "YWJj");
    }
}
