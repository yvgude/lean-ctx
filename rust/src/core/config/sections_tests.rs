// SPDX-License-Identifier: Apache-2.0
use super::*;

fn mcp_entry(id: &str, url: &str) -> McpServerEntry {
    McpServerEntry {
        id: id.into(),
        url: url.into(),
        auth_env: None,
        enabled: None,
    }
}

#[test]
fn mcp_registry_validates_ids_urls_and_duplicates() {
    let cfg = GatewayServerConfig {
        mcp_servers: vec![
            mcp_entry("github", "https://mcp.example.com/mcp/"),
            // invalid id (uppercase) — skipped, never panics
            mcp_entry("GitHub", "https://mcp.example.com/mcp"),
            // duplicate — first occurrence wins
            mcp_entry("github", "https://other.example.com/mcp"),
            // plaintext HTTP on a non-loopback host without the opt-in — skipped
            mcp_entry("plain", "http://mcp.example.com/mcp"),
            // loopback HTTP is always fine (local/dev)
            mcp_entry("local", "http://127.0.0.1:9200/mcp"),
            McpServerEntry {
                enabled: Some(false),
                ..mcp_entry("disabled", "https://mcp.example.com/mcp")
            },
            McpServerEntry {
                auth_env: Some("  GITHUB_MCP_PAT  ".into()),
                ..mcp_entry("authed", "https://api.githubcopilot.com/mcp")
            },
        ],
        ..Default::default()
    };
    let resolved = cfg.resolve_mcp_servers(false);
    let ids: Vec<&str> = resolved.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["github", "local", "authed"]);
    // Trailing slash normalized; the duplicate kept the first URL.
    assert_eq!(resolved[0].url, "https://mcp.example.com/mcp");
    assert_eq!(resolved[2].auth_env.as_deref(), Some("GITHUB_MCP_PAT"));

    // The insecure-HTTP opt-in admits the plaintext entry (trusted LAN).
    let with_optin = cfg.resolve_mcp_servers(true);
    assert!(with_optin.iter().any(|s| s.id == "plain"));
}

#[test]
fn mcp_upstream_url_rules_match_the_proxy_posture() {
    assert!(validate_mcp_upstream_url("https://mcp.example.com/mcp", false).is_ok());
    assert!(validate_mcp_upstream_url("http://localhost:9200/mcp", false).is_ok());
    assert!(validate_mcp_upstream_url("http://mcp.example.com/mcp", false).is_err());
    assert!(validate_mcp_upstream_url("http://mcp.example.com/mcp", true).is_ok());
    assert!(validate_mcp_upstream_url("ftp://mcp.example.com", false).is_err());
    assert!(validate_mcp_upstream_url("   ", false).is_err());
}
