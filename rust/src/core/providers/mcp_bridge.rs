//! MCP Bridge provider — connects external MCP servers as data sources.
//!
//! Allows lean-ctx to query resources from other MCP servers (e.g., a
//! custom internal knowledge base MCP server) and integrate them into
//! the context pipeline.
//!
//! Configuration via lean-ctx config:
//!   [`providers.mcp_bridges`]
//!   my-kb = { url = "<http://localhost:8080>", description = "Internal KB" }

use crate::core::providers::{ContextProvider, ProviderItem, ProviderParams, ProviderResult};

/// Transport configuration for an MCP Bridge.
pub enum McpTransport {
    Http { url: String },
    Stdio { command: String, args: Vec<String> },
}

pub struct McpBridgeProvider {
    id: &'static str,
    display_name: &'static str,
    server_url: String,
    server_name: String,
    transport: McpTransport,
}

impl McpBridgeProvider {
    /// Create an HTTP-based MCP Bridge provider.
    ///
    /// Leaks `id` and `display_name` strings — acceptable because providers
    /// are registered once at startup and live for the process lifetime
    /// (same pattern as `ConfigProvider`).
    #[must_use]
    pub fn new(server_name: &str, server_url: &str) -> Self {
        let id: &'static str = crate::core::providers::intern(format!("mcp:{server_name}"));
        let display_name: &'static str =
            crate::core::providers::intern(format!("MCP Bridge ({server_name})"));
        Self {
            id,
            display_name,
            server_url: server_url.trim_end_matches('/').to_string(),
            server_name: server_name.to_string(),
            transport: McpTransport::Http {
                url: server_url.trim_end_matches('/').to_string(),
            },
        }
    }

    /// Create a stdio-based MCP Bridge provider.
    #[must_use]
    pub fn new_stdio(server_name: &str, command: &str, args: &[String]) -> Self {
        let id: &'static str = crate::core::providers::intern(format!("mcp:{server_name}"));
        let display_name: &'static str =
            crate::core::providers::intern(format!("MCP Bridge ({server_name}, stdio)"));
        Self {
            id,
            display_name,
            server_url: String::new(),
            server_name: server_name.to_string(),
            transport: McpTransport::Stdio {
                command: command.to_string(),
                args: args.to_vec(),
            },
        }
    }
}

impl ContextProvider for McpBridgeProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display_name
    }

    fn supported_actions(&self) -> &[&str] {
        &["resources", "read_resource", "tools"]
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        if matches!(self.transport, McpTransport::Stdio { .. }) {
            return Err(format!(
                "MCP bridge '{}' uses stdio transport — spawn-based execution is not yet implemented. \
                 Configure an HTTP URL via `url = \"http://...\"` for full functionality.",
                self.server_name
            ));
        }
        match action {
            "resources" => list_resources(&self.server_url, &self.server_name, params),
            "read_resource" => read_resource(&self.server_url, &self.server_name, params),
            "tools" => list_tools(&self.server_url, &self.server_name, params),
            _ => Err(format!("Unsupported action: {action}")),
        }
    }

    fn cache_ttl_secs(&self) -> u64 {
        60
    }

    fn requires_auth(&self) -> bool {
        false
    }

    fn is_available(&self) -> bool {
        match &self.transport {
            McpTransport::Http { url } => !url.is_empty(),
            McpTransport::Stdio { command, .. } => !command.is_empty(),
        }
    }
}

fn list_resources(
    server_url: &str,
    server_name: &str,
    params: &ProviderParams,
) -> Result<ProviderResult, String> {
    let limit = params.limit.unwrap_or(20);
    let url = format!("{server_url}/resources/list");

    let response = ureq::get(&url)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("MCP bridge error ({server_name}): {e}"))?;

    let text = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("MCP bridge read error: {e}"))?;
    let body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("MCP bridge JSON error: {e}"))?;

    let resources = body["resources"].as_array().cloned().unwrap_or_default();

    let items: Vec<ProviderItem> = resources
        .iter()
        .take(limit)
        .map(|r| ProviderItem {
            id: r["uri"].as_str().unwrap_or_default().to_string(),
            title: r["name"].as_str().unwrap_or_default().to_string(),
            state: Some("available".into()),
            author: None,
            created_at: None,
            updated_at: None,
            url: Some(format!("{server_url}/resources/read")),
            labels: vec![server_name.to_string()],
            body: r["description"].as_str().map(String::from),
            ..Default::default()
        })
        .collect();

    Ok(ProviderResult {
        provider: format!("mcp_bridge:{server_name}"),
        resource_type: "resources".into(),
        items,
        total_count: Some(resources.len()),
        truncated: resources.len() > limit,
    })
}

fn read_resource(
    server_url: &str,
    server_name: &str,
    params: &ProviderParams,
) -> Result<ProviderResult, String> {
    let uri = params
        .id
        .as_deref()
        .or(params.query.as_deref())
        .ok_or_else(|| {
            format!("read_resource requires 'id' (resource URI). Use action=resources to discover URIs for {server_name}")
        })?;

    let url = format!("{server_url}/resources/read");

    let body = serde_json::json!({ "uri": uri });
    let response = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON encode error: {e}"))?)
        .map_err(|e| format!("MCP bridge read error ({server_name}): {e}"))?;

    let text = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("MCP bridge read body error: {e}"))?;
    let body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("MCP bridge JSON error: {e}"))?;

    let contents = body["contents"].as_array().cloned().unwrap_or_default();
    let content_text = contents
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let items = vec![ProviderItem {
        id: uri.to_string(),
        title: uri.rsplit('/').next().unwrap_or(uri).to_string(),
        state: Some("available".into()),
        author: None,
        created_at: None,
        updated_at: None,
        url: Some(format!("{server_url}/resources/read")),
        labels: vec![server_name.to_string()],
        body: Some(content_text),
        ..Default::default()
    }];

    Ok(ProviderResult {
        provider: format!("mcp_bridge:{server_name}"),
        resource_type: "resource_content".into(),
        items,
        total_count: Some(1),
        truncated: false,
    })
}

fn list_tools(
    server_url: &str,
    server_name: &str,
    params: &ProviderParams,
) -> Result<ProviderResult, String> {
    let limit = params.limit.unwrap_or(20);
    let url = format!("{server_url}/tools/list");

    let response = ureq::get(&url)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("MCP bridge error ({server_name}): {e}"))?;

    let text = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("MCP bridge read error: {e}"))?;
    let body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("MCP bridge JSON error: {e}"))?;

    let tools = body["tools"].as_array().cloned().unwrap_or_default();

    let items: Vec<ProviderItem> = tools
        .iter()
        .take(limit)
        .map(|t| ProviderItem {
            id: t["name"].as_str().unwrap_or_default().to_string(),
            title: t["name"].as_str().unwrap_or_default().to_string(),
            state: Some("available".into()),
            author: None,
            created_at: None,
            updated_at: None,
            url: Some(format!("{server_url}/tools/call")),
            labels: vec![server_name.to_string()],
            body: t["description"].as_str().map(String::from),
            ..Default::default()
        })
        .collect();

    Ok(ProviderResult {
        provider: format!("mcp_bridge:{server_name}"),
        resource_type: "tools".into(),
        items,
        total_count: Some(tools.len()),
        truncated: tools.len() > limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_bridge_unavailable_when_empty_url() {
        let provider = McpBridgeProvider::new("test", "");
        assert!(!provider.is_available());
    }

    #[test]
    fn mcp_bridge_available_with_url() {
        let provider = McpBridgeProvider::new("kb", "http://localhost:8080");
        assert!(provider.is_available());
        assert_eq!(provider.id(), "mcp:kb");
    }

    #[test]
    fn mcp_bridge_unique_ids_per_instance() {
        let a = McpBridgeProvider::new("knowledge-base", "http://localhost:8080");
        let b = McpBridgeProvider::new("github-issues", "http://localhost:9090");
        assert_eq!(a.id(), "mcp:knowledge-base");
        assert_eq!(b.id(), "mcp:github-issues");
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn mcp_bridge_supported_actions() {
        let provider = McpBridgeProvider::new("test", "http://localhost");
        assert!(provider.supported_actions().contains(&"resources"));
        assert!(provider.supported_actions().contains(&"tools"));
    }
}
