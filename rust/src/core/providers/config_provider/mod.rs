//! Config-based context providers.
//!
//! Users define providers via TOML/JSON files instead of writing Rust code.
//! Drop a file into `~/.config/lean-ctx/providers/` or `.lean-ctx/providers/`
//! to register a custom REST API as a first-class context source.
//!
//! Example TOML:
//! ```toml
//! id = "linear"
//! name = "Linear"
//! base_url = "https://api.linear.app"
//!
//! [auth]
//! type = "bearer"
//! token_env = "LINEAR_API_KEY"
//!
//! [resources.issues]
//! path = "/issues"
//! [resources.issues.response]
//! root = "data"
//! [resources.issues.response.mapping]
//! id = "id"
//! title = "title"
//! body = "description"
//! ```

pub mod discovery;
pub mod extract;
pub mod http;
pub mod schema;

use std::collections::HashMap;

use schema::ProviderConfig;

use super::ProviderResult;
use super::provider_trait::{ContextProvider, ProviderParams};
use http::ResolvedAuth;

/// A context provider dynamically created from a TOML/JSON config file.
pub struct ConfigProvider {
    id: &'static str,
    display_name: &'static str,
    actions: Vec<&'static str>,
    config: ProviderConfig,
}

impl ConfigProvider {
    /// Create a `ConfigProvider` from a parsed config.
    ///
    /// Leaks the id/name strings — acceptable because providers are registered
    /// once at startup and live for the process lifetime.
    pub fn from_config(config: ProviderConfig) -> Result<Self, String> {
        config.validate()?;

        let id: &'static str = crate::core::providers::intern(config.id.clone());
        let display_name: &'static str = crate::core::providers::intern(config.name.clone());
        let actions: Vec<&'static str> = config
            .resources
            .keys()
            .map(|k| crate::core::providers::intern(k.clone()))
            .collect();

        Ok(Self {
            id,
            display_name,
            actions,
            config,
        })
    }

    fn build_interp_params(params: &ProviderParams) -> HashMap<String, String> {
        let mut map = HashMap::new();
        if let Some(ref p) = params.project {
            map.insert("project".into(), p.clone());
        }
        if let Some(ref s) = params.state {
            map.insert("state".into(), s.clone());
        }
        if let Some(limit) = params.limit {
            map.insert("limit".into(), limit.to_string());
        }
        if let Some(ref q) = params.query {
            map.insert("query".into(), q.clone());
        }
        if let Some(ref id) = params.id {
            map.insert("id".into(), id.clone());
        }
        map
    }
}

impl ContextProvider for ConfigProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display_name
    }

    fn supported_actions(&self) -> &[&str] {
        &self.actions
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        let resource = self.config.resources.get(action).ok_or_else(|| {
            format!(
                "Provider '{}': unknown action '{}'. Available: {:?}",
                self.id,
                action,
                self.config.resources.keys().collect::<Vec<_>>()
            )
        })?;

        let auth = ResolvedAuth::from_config(&self.config.auth)?;
        let interp_params = Self::build_interp_params(params);

        let response_json =
            http::execute_request(&self.config.base_url, resource, &auth, &interp_params)?;

        let items_json =
            extract::extract_items_array(&response_json, resource.response.root.as_deref())?;

        let limit = params.limit.unwrap_or(50);
        let total_count = items_json.len();
        let truncated = total_count > limit;

        let items: Vec<_> = items_json
            .iter()
            .take(limit)
            .filter_map(|item| extract::map_item(item, &resource.response.mapping))
            .collect();

        Ok(ProviderResult {
            provider: self.id.to_string(),
            resource_type: action.to_string(),
            items,
            total_count: Some(total_count),
            truncated,
        })
    }

    fn cache_ttl_secs(&self) -> u64 {
        self.config.cache_ttl_secs
    }

    fn requires_auth(&self) -> bool {
        !matches!(self.config.auth, schema::AuthConfig::None)
    }

    fn is_available(&self) -> bool {
        ResolvedAuth::is_available(&self.config.auth)
    }
}

/// Simple base64 encoder (avoids adding a base64 crate dependency).
pub(crate) fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> ProviderConfig {
        toml::from_str(
            r#"
id = "test-api"
name = "Test API"
base_url = "https://api.example.com"
cache_ttl_secs = 60

[auth]
type = "none"

[resources.items]
path = "/items"
[resources.items.response]
root = "data"
[resources.items.response.mapping]
id = "id"
title = "name"
body = "description"
state = "status"
"#,
        )
        .unwrap()
    }

    #[test]
    fn config_provider_from_config() {
        let provider = ConfigProvider::from_config(sample_config()).unwrap();
        assert_eq!(provider.id(), "test-api");
        assert_eq!(provider.display_name(), "Test API");
        assert_eq!(provider.supported_actions(), &["items"]);
        assert!(!provider.requires_auth());
        assert!(provider.is_available());
        assert_eq!(provider.cache_ttl_secs(), 60);
    }

    #[test]
    fn config_provider_rejects_invalid() {
        let mut cfg = sample_config();
        cfg.id = String::new();
        assert!(ConfigProvider::from_config(cfg).is_err());
    }

    #[test]
    fn base64_encode_basic_auth() {
        let encoded = base64_encode(b"user:pass");
        assert_eq!(encoded, "dXNlcjpwYXNz");
    }

    #[test]
    fn base64_encode_padding() {
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    #[test]
    fn build_interp_params_maps_all_fields() {
        let params = ProviderParams {
            project: Some("myproject".into()),
            state: Some("open".into()),
            limit: Some(10),
            query: Some("search".into()),
            id: Some("42".into()),
        };
        let map = ConfigProvider::build_interp_params(&params);
        assert_eq!(map.get("project"), Some(&"myproject".into()));
        assert_eq!(map.get("state"), Some(&"open".into()));
        assert_eq!(map.get("limit"), Some(&"10".into()));
        assert_eq!(map.get("query"), Some(&"search".into()));
        assert_eq!(map.get("id"), Some(&"42".into()));
    }
}
