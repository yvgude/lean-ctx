//! HTTP client with pluggable authentication for config-based providers.
//!
//! Builds and executes authenticated HTTP requests using ureq based on
//! the `AuthConfig` from the provider's TOML/JSON definition.

use std::collections::HashMap;

use super::schema::{AuthConfig, ResourceConfig};

/// Resolved auth credentials (env vars already read).
#[derive(Debug, Clone)]
pub enum ResolvedAuth {
    Bearer(String),
    ApiKeyHeader { header: String, value: String },
    ApiKeyQuery { param: String, value: String },
    Basic { username: String, password: String },
    CustomHeader { header: String, value: String },
    None,
}

impl ResolvedAuth {
    /// Resolve auth credentials from environment variables.
    pub fn from_config(auth: &AuthConfig) -> Result<Self, String> {
        match auth {
            AuthConfig::Bearer { token_env } => {
                let token = read_env(token_env)?;
                Ok(Self::Bearer(token))
            }
            AuthConfig::ApiKey {
                key_env,
                header_name,
                query_param,
            } => {
                let key = read_env(key_env)?;
                if let Some(header) = header_name {
                    Ok(Self::ApiKeyHeader {
                        header: header.clone(),
                        value: key,
                    })
                } else if let Some(param) = query_param {
                    Ok(Self::ApiKeyQuery {
                        param: param.clone(),
                        value: key,
                    })
                } else {
                    Ok(Self::ApiKeyHeader {
                        header: "X-Api-Key".into(),
                        value: key,
                    })
                }
            }
            AuthConfig::Basic {
                username_env,
                password_env,
            } => {
                let username = read_env(username_env)?;
                let password = read_env(password_env)?;
                Ok(Self::Basic { username, password })
            }
            AuthConfig::Header {
                header_name,
                value_env,
            } => {
                let value = read_env(value_env)?;
                Ok(Self::CustomHeader {
                    header: header_name.clone(),
                    value,
                })
            }
            AuthConfig::None => Ok(Self::None),
        }
    }

    /// Whether auth credentials could be resolved (provider is usable).
    #[must_use]
    pub fn is_available(auth: &AuthConfig) -> bool {
        match auth {
            AuthConfig::Bearer { token_env } => std::env::var(token_env).is_ok(),
            AuthConfig::ApiKey { key_env, .. } => std::env::var(key_env).is_ok(),
            AuthConfig::Basic {
                username_env,
                password_env,
            } => std::env::var(username_env).is_ok() && std::env::var(password_env).is_ok(),
            AuthConfig::Header { value_env, .. } => std::env::var(value_env).is_ok(),
            AuthConfig::None => true,
        }
    }
}

fn read_env(var: &str) -> Result<String, String> {
    std::env::var(var).map_err(|_| format!("Environment variable '{var}' not set"))
}

/// Interpolate `{param}` placeholders in a string with actual values.
#[must_use]
pub fn interpolate(template: &str, params: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in params {
        result = result.replace(&format!("{{{key}}}"), value);
    }
    // Remove unresolved placeholders (optional params not provided)
    let re = regex::Regex::new(r"\{[a-zA-Z_][a-zA-Z0-9_]*\}").unwrap();
    re.replace_all(&result, "").to_string()
}

/// Build the full URL with query parameters.
fn build_url(
    base_url: &str,
    resource: &ResourceConfig,
    interp_params: &HashMap<String, String>,
    auth: &ResolvedAuth,
) -> String {
    let path = interpolate(&resource.path, interp_params);
    let base = base_url.trim_end_matches('/');
    let mut url = format!("{base}{path}");

    let mut query_parts: Vec<String> = Vec::new();
    for (key, val_template) in &resource.query_params {
        let val = interpolate(val_template, interp_params);
        if !val.is_empty() {
            query_parts.push(format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(&val)
            ));
        }
    }

    if let ResolvedAuth::ApiKeyQuery { param, value } = auth {
        query_parts.push(format!(
            "{}={}",
            urlencoding::encode(param),
            urlencoding::encode(value)
        ));
    }

    if !query_parts.is_empty() {
        url.push('?');
        url.push_str(&query_parts.join("&"));
    }

    url
}

/// Collect all headers (auth + resource-specific + Accept).
fn collect_headers(
    auth: &ResolvedAuth,
    resource_headers: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();

    match auth {
        ResolvedAuth::Bearer(token) => {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
        }
        ResolvedAuth::ApiKeyHeader { header, value }
        | ResolvedAuth::CustomHeader { header, value } => {
            headers.push((header.clone(), value.clone()));
        }
        ResolvedAuth::Basic { username, password } => {
            let encoded = crate::core::providers::config_provider::base64_encode(
                format!("{username}:{password}").as_bytes(),
            );
            headers.push(("Authorization".into(), format!("Basic {encoded}")));
        }
        ResolvedAuth::ApiKeyQuery { .. } | ResolvedAuth::None => {}
    }

    for (key, value) in resource_headers {
        headers.push((key.clone(), value.clone()));
    }

    headers.push(("Accept".into(), "application/json".into()));
    headers
}

/// Parse the response body into JSON, checking status first.
fn parse_response(status: u16, body: &str, url: &str) -> Result<serde_json::Value, String> {
    if !(200..300).contains(&status) {
        return Err(format!(
            "API returned status {status} for {}",
            url.split('?').next().unwrap_or(url)
        ));
    }
    serde_json::from_str(body).map_err(|e| format!("Invalid JSON response: {e}"))
}

fn status_to_u16(status: ureq::http::StatusCode) -> u16 {
    status.as_u16()
}

/// Execute an HTTP request to the external API.
///
/// Handles GET/DELETE (no body) and POST/PUT/PATCH (with empty body) separately
/// because ureq 3 uses different types for body vs bodyless requests.
pub fn execute_request(
    base_url: &str,
    resource: &ResourceConfig,
    auth: &ResolvedAuth,
    interp_params: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let url = build_url(base_url, resource, interp_params, auth);
    let headers = collect_headers(auth, &resource.headers);
    let method = resource.method.to_uppercase();

    let (status, body) = match method.as_str() {
        "POST" | "PUT" | "PATCH" => {
            let mut req = match method.as_str() {
                "PUT" => ureq::put(&url),
                "PATCH" => ureq::patch(&url),
                _ => ureq::post(&url),
            };
            for (k, v) in &headers {
                req = req.header(k, v);
            }
            let res = req
                .send_empty()
                .map_err(|e| format!("HTTP request failed: {e}"))?;
            let st = status_to_u16(res.status());
            let b = res
                .into_body()
                .read_to_string()
                .map_err(|e| format!("Failed to read response body: {e}"))?;
            (st, b)
        }
        _ => {
            let mut req = if method == "DELETE" {
                ureq::delete(&url)
            } else {
                ureq::get(&url)
            };
            for (k, v) in &headers {
                req = req.header(k, v);
            }
            let res = req
                .call()
                .map_err(|e| format!("HTTP request failed: {e}"))?;
            let st = status_to_u16(res.status());
            let b = res
                .into_body()
                .read_to_string()
                .map_err(|e| format!("Failed to read response body: {e}"))?;
            (st, b)
        }
    };

    parse_response(status, &body, &url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_replaces_known_params() {
        let mut params = HashMap::new();
        params.insert("limit".into(), "10".into());
        params.insert("state".into(), "open".into());
        params.insert("owner".into(), "acme".into());
        assert_eq!(
            interpolate("/repos/{owner}/issues?limit={limit}&state={state}", &params),
            "/repos/acme/issues?limit=10&state=open"
        );
    }

    #[test]
    fn interpolate_removes_unresolved_placeholders() {
        let params = HashMap::new();
        assert_eq!(
            interpolate("/items?filter={filter}", &params),
            "/items?filter="
        );
    }

    #[test]
    fn build_url_with_query_params() {
        let resource = ResourceConfig {
            method: "GET".into(),
            path: "/issues".into(),
            query_params: {
                let mut m = HashMap::new();
                m.insert("state".into(), "{state}".into());
                m.insert("per_page".into(), "{limit}".into());
                m
            },
            headers: HashMap::new(),
            response: super::super::schema::ResponseConfig {
                root: None,
                mapping: super::super::schema::FieldMapping {
                    id: "id".into(),
                    title: "title".into(),
                    body: None,
                    state: None,
                    author: None,
                    url: None,
                    labels: None,
                    created_at: None,
                    updated_at: None,
                },
            },
        };
        let mut params = HashMap::new();
        params.insert("state".into(), "open".into());
        params.insert("limit".into(), "25".into());

        let url = build_url(
            "https://api.example.com",
            &resource,
            &params,
            &ResolvedAuth::None,
        );
        assert!(url.starts_with("https://api.example.com/issues?"));
        assert!(url.contains("state=open"));
        assert!(url.contains("per_page=25"));
    }

    #[test]
    fn build_url_with_api_key_query() {
        let resource = ResourceConfig {
            method: "GET".into(),
            path: "/data".into(),
            query_params: HashMap::new(),
            headers: HashMap::new(),
            response: super::super::schema::ResponseConfig {
                root: None,
                mapping: super::super::schema::FieldMapping {
                    id: "id".into(),
                    title: "name".into(),
                    body: None,
                    state: None,
                    author: None,
                    url: None,
                    labels: None,
                    created_at: None,
                    updated_at: None,
                },
            },
        };
        let auth = ResolvedAuth::ApiKeyQuery {
            param: "api_key".into(),
            value: "secret123".into(),
        };
        let url = build_url("https://api.example.com", &resource, &HashMap::new(), &auth);
        assert!(url.contains("api_key=secret123"));
    }

    #[test]
    fn resolved_auth_none_always_available() {
        assert!(ResolvedAuth::is_available(&AuthConfig::None));
    }

    #[test]
    fn resolved_auth_bearer_unavailable_without_env() {
        let auth = AuthConfig::Bearer {
            token_env: "LEAN_CTX_TEST_NONEXISTENT_TOKEN_12345".into(),
        };
        assert!(!ResolvedAuth::is_available(&auth));
    }
}
