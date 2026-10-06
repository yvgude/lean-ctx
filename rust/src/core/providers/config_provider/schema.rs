//! Schema definitions for user-defined provider configs.
//!
//! Users drop a `.toml` or `.json` file into `~/.config/lean-ctx/providers/`
//! (or `.lean-ctx/providers/` in a project) to register a custom data source.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Top-level provider configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_secs: u64,
    pub resources: HashMap<String, ResourceConfig>,
}

fn default_cache_ttl() -> u64 {
    120
}

/// Authentication strategy for the external API.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthConfig {
    /// `Authorization: Bearer <token>` from an env var.
    Bearer { token_env: String },
    /// API key sent as a header or query parameter.
    ApiKey {
        key_env: String,
        /// Header name (e.g. `X-Api-Key`). Mutually exclusive with `query_param`.
        #[serde(default)]
        header_name: Option<String>,
        /// Query parameter name (e.g. `api_key`).
        #[serde(default)]
        query_param: Option<String>,
    },
    /// HTTP Basic auth from two env vars.
    Basic {
        username_env: String,
        password_env: String,
    },
    /// Arbitrary header (e.g. `X-Custom-Token: <value>`).
    Header {
        header_name: String,
        value_env: String,
    },
    /// No authentication required.
    #[default]
    None,
}

/// Configuration for a single API resource/endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResourceConfig {
    /// HTTP method. Defaults to `"GET"`.
    #[serde(default = "default_method")]
    pub method: String,
    /// URL path appended to `base_url` (supports `{param}` interpolation).
    pub path: String,
    /// Query parameters (`{limit}`, `{state}` etc. are interpolated from `ProviderParams`).
    #[serde(default)]
    pub query_params: HashMap<String, String>,
    /// Extra headers for this resource.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// How to extract items from the JSON response.
    pub response: ResponseConfig,
}

fn default_method() -> String {
    "GET".into()
}

/// Describes how to map a JSON response to `ProviderItem`s.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResponseConfig {
    /// Dot-notation path to the array of items (e.g. `"data.issues"`).
    /// If `None`, the response root is treated as the array.
    #[serde(default)]
    pub root: Option<String>,
    /// Maps `ProviderItem` fields to JSON paths within each array element.
    pub mapping: FieldMapping,
}

/// Maps `ProviderItem` fields to dot-notation paths in the JSON response.
///
/// `id` and `title` are required; everything else is optional.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FieldMapping {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Path to labels array. Each element is stringified.
    #[serde(default)]
    pub labels: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl ProviderConfig {
    /// Validate that the config is well-formed.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("Provider config: 'id' must not be empty".into());
        }
        if self.base_url.is_empty() {
            return Err("Provider config: 'base_url' must not be empty".into());
        }
        if self.resources.is_empty() {
            return Err(format!(
                "Provider '{}': must define at least one resource",
                self.id
            ));
        }
        for (name, res) in &self.resources {
            if res.path.is_empty() {
                return Err(format!(
                    "Provider '{}' resource '{}': 'path' must not be empty",
                    self.id, name
                ));
            }
            let method = res.method.to_uppercase();
            if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str()) {
                return Err(format!(
                    "Provider '{}' resource '{}': unsupported method '{}'",
                    self.id, name, res.method
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_toml_bearer_config() {
        let toml_str = r#"
id = "linear"
name = "Linear"
base_url = "https://api.linear.app"

[auth]
type = "bearer"
token_env = "LINEAR_API_KEY"

[resources.issues]
method = "GET"
path = "/issues"

[resources.issues.query_params]
limit = "{limit}"
state = "{state}"

[resources.issues.response]
root = "data"

[resources.issues.response.mapping]
id = "id"
title = "title"
body = "description"
state = "state.name"
author = "creator.name"
url = "url"
labels = "labels[].name"
created_at = "createdAt"
updated_at = "updatedAt"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.id, "linear");
        assert_eq!(cfg.name, "Linear");
        assert!(matches!(cfg.auth, AuthConfig::Bearer { .. }));
        assert!(cfg.resources.contains_key("issues"));
        let issues = &cfg.resources["issues"];
        assert_eq!(issues.path, "/issues");
        assert_eq!(issues.response.mapping.id, "id");
        assert_eq!(issues.response.mapping.title, "title");
        assert_eq!(issues.response.mapping.labels, Some("labels[].name".into()));
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn parse_json_api_key_config() {
        let json_str = r#"{
            "id": "notion",
            "name": "Notion",
            "base_url": "https://api.notion.com/v1",
            "auth": {
                "type": "api_key",
                "key_env": "NOTION_TOKEN",
                "header_name": "Notion-Version"
            },
            "resources": {
                "pages": {
                    "path": "/search",
                    "method": "POST",
                    "response": {
                        "root": "results",
                        "mapping": {
                            "id": "id",
                            "title": "properties.title.title[0].text.content"
                        }
                    }
                }
            }
        }"#;
        let cfg: ProviderConfig = serde_json::from_str(json_str).unwrap();
        assert_eq!(cfg.id, "notion");
        assert!(matches!(cfg.auth, AuthConfig::ApiKey { .. }));
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn parse_no_auth_config() {
        let toml_str = r#"
id = "public-api"
name = "Public API"
base_url = "https://api.example.com"

[auth]
type = "none"

[resources.data]
path = "/data"

[resources.data.response.mapping]
id = "uuid"
title = "name"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(matches!(cfg.auth, AuthConfig::None));
        assert!(!cfg.resources["data"].query_params.contains_key("limit"));
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_catches_empty_id() {
        let cfg = ProviderConfig {
            id: String::new(),
            name: "Test".into(),
            base_url: "https://example.com".into(),
            auth: AuthConfig::None,
            cache_ttl_secs: 120,
            resources: HashMap::new(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_catches_no_resources() {
        let cfg = ProviderConfig {
            id: "test".into(),
            name: "Test".into(),
            base_url: "https://example.com".into(),
            auth: AuthConfig::None,
            cache_ttl_secs: 120,
            resources: HashMap::new(),
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("at least one resource"));
    }

    #[test]
    fn parse_basic_auth_config() {
        let toml_str = r#"
id = "jira-custom"
name = "Jira (Custom)"
base_url = "https://mycompany.atlassian.net/rest/api/3"

[auth]
type = "basic"
username_env = "JIRA_USER"
password_env = "JIRA_TOKEN"

[resources.issues]
path = "/search"
[resources.issues.query_params]
jql = "project={project} ORDER BY updated DESC"
maxResults = "{limit}"
[resources.issues.response]
root = "issues"
[resources.issues.response.mapping]
id = "key"
title = "fields.summary"
body = "fields.description"
state = "fields.status.name"
author = "fields.reporter.displayName"
labels = "fields.labels"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_str).unwrap();
        assert!(matches!(cfg.auth, AuthConfig::Basic { .. }));
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn default_method_is_get() {
        let toml_str = r#"
id = "test"
name = "Test"
base_url = "https://example.com"

[auth]
type = "none"

[resources.items]
path = "/items"
[resources.items.response.mapping]
id = "id"
title = "name"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.resources["items"].method, "GET");
    }
}
