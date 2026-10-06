use std::path::PathBuf;

use anyhow::{Result, anyhow};
use rmcp::transport::StreamableHttpServerConfig;

use crate::core::a2a::relay::RelayPeerTableV1;
use crate::core::a2a::task::TaskAuthorityConfigV1;

const MAX_ID_LEN: usize = 64;

pub(super) fn sanitize_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "default".to_string();
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(MAX_ID_LEN)
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

#[derive(Clone, Debug)]
pub struct HttpServerConfig {
    pub host: String,
    pub port: u16,
    pub project_root: PathBuf,
    pub auth_token: Option<String>,
    pub a2a_signing_key: Option<String>,
    pub a2a_recipient_id: Option<String>,
    pub a2a_tenant_id: Option<String>,
    pub a2a_project_id: Option<String>,
    pub a2a_peers: RelayPeerTableV1,
    pub a2a_quotas: super::relay_rate::RelayQuotaConfig,
    pub a2a_task_authority: TaskAuthorityConfigV1,
    pub stateful_mode: bool,
    pub json_response: bool,
    pub disable_host_check: bool,
    pub allowed_hosts: Vec<String>,
    pub max_body_bytes: usize,
    pub max_concurrency: usize,
    pub max_rps: u32,
    pub rate_burst: u32,
    pub request_timeout_ms: u64,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            project_root,
            auth_token: None,
            a2a_signing_key: None,
            a2a_recipient_id: None,
            a2a_tenant_id: None,
            a2a_project_id: None,
            a2a_peers: RelayPeerTableV1::default(),
            a2a_quotas: super::relay_rate::RelayQuotaConfig::default(),
            a2a_task_authority: TaskAuthorityConfigV1::default(),
            stateful_mode: false,
            json_response: true,
            disable_host_check: false,
            allowed_hosts: Vec::new(),
            max_body_bytes: 2 * 1024 * 1024,
            max_concurrency: 32,
            max_rps: 50,
            rate_burst: 100,
            request_timeout_ms: 30_000,
        }
    }
}

impl HttpServerConfig {
    pub(super) fn relay_quota_state(&self) -> Result<Option<super::relay_rate::RelayQuotaState>> {
        self.a2a_quotas.validate().map_err(|error| anyhow!(error))?;
        if self.a2a_peers.peers.is_empty() {
            return Ok(None);
        }
        let peers = self
            .a2a_peers
            .peers
            .iter()
            .map(|peer| peer.peer_id.clone())
            .collect::<Vec<_>>();
        super::relay_rate::RelayQuotaState::new(
            self.a2a_quotas,
            &peers,
            self.a2a_tenant_id.as_deref().unwrap_or(""),
            self.a2a_project_id.as_deref().unwrap_or(""),
            std::time::Instant::now(),
        )
        .map(Some)
        .map_err(|error| anyhow!(error))
    }

    pub fn validate(&self) -> Result<()> {
        self.relay_quota_state()?;
        let host = self.host.trim().to_lowercase();
        let is_loopback = host == "127.0.0.1" || host == "localhost" || host == "::1";
        if !is_loopback && self.auth_token.as_deref().unwrap_or("").is_empty() {
            return Err(anyhow!(
                "Refusing to bind to host='{host}' without auth. Provide --auth-token (or bind to 127.0.0.1)."
            ));
        }
        if self.a2a_signing_key.as_ref().is_some_and(String::is_empty) {
            return Err(anyhow!("a2a_signing_key must not be empty"));
        }
        self.a2a_peers
            .validate(false)
            .map_err(|error| anyhow!("invalid a2a peer table: {error}"))?;
        self.a2a_peers
            .reject_shared_secret()
            .map_err(|error| anyhow!("invalid a2a peer credentials: {error}"))?;
        if !self.a2a_peers.peers.is_empty() && self.a2a_recipient_id.is_none() {
            return Err(anyhow!(
                "a2a_recipient_id is required when relay peers are configured"
            ));
        }
        if self.a2a_signing_key.is_some() && self.a2a_signing_key == self.auth_token {
            return Err(anyhow!("a2a_signing_key must be distinct from auth_token"));
        }
        if self.a2a_signing_key.is_some() {
            for (name, value) in [
                ("a2a_recipient_id", self.a2a_recipient_id.as_deref()),
                ("a2a_tenant_id", self.a2a_tenant_id.as_deref()),
                ("a2a_project_id", self.a2a_project_id.as_deref()),
            ] {
                if value.is_none_or(str::is_empty) {
                    return Err(anyhow!("{name} is required when A2A signing is enabled"));
                }
            }
        }
        // Trust policy is authority: a policy that does not fully validate is
        // never served, and its Ed25519 material must not be the bearer token
        // or the HMAC channel secret.
        self.a2a_task_authority
            .validate()
            .map_err(|error| anyhow!("invalid a2a task authority policy: {error}"))?;
        for secret in [self.auth_token.as_deref(), self.a2a_signing_key.as_deref()]
            .into_iter()
            .flatten()
        {
            self.a2a_task_authority
                .reject_shared_secret(secret)
                .map_err(|error| anyhow!(error.to_string()))?;
        }
        if !self.a2a_task_authority.peers.is_empty() && self.a2a_signing_key.is_none() {
            return Err(anyhow!(
                "a2a_signing_key is required when a task authority policy is configured"
            ));
        }
        if !self.a2a_task_authority.peers.is_empty()
            && (self.a2a_recipient_id.is_none()
                || self.a2a_tenant_id.is_none()
                || self.a2a_project_id.is_none())
        {
            return Err(anyhow!(
                "a2a recipient, tenant, and project ids are required when a task authority policy is configured"
            ));
        }
        match (
            self.a2a_tenant_id.as_deref(),
            self.a2a_project_id.as_deref(),
        ) {
            (Some(tenant_id), Some(project_id)) => {
                crate::core::a2a::dlq::DlqScope::new(tenant_id, project_id)
                    .map_err(|error| anyhow!(error.to_string()))?;
            }
            (None, None) => {}
            _ => {
                return Err(anyhow!(
                    "a2a_tenant_id and a2a_project_id must be configured together"
                ));
            }
        }
        Ok(())
    }

    pub fn effective_auth_token(&self) -> Option<String> {
        if let Some(ref token) = self.auth_token
            && !token.is_empty()
        {
            return Some(token.clone());
        }
        let host = self.host.trim().to_lowercase();
        let is_loopback = host == "127.0.0.1" || host == "localhost" || host == "::1";
        if is_loopback {
            let auto_token = crate::core::session_token::generate_token();
            eprintln!(
                "[lean-ctx] Auto-generated auth token for loopback: {auto_token}\n\
                 Pass as Bearer token or set --auth-token explicitly."
            );
            Some(auto_token)
        } else {
            None
        }
    }

    pub(super) fn mcp_http_config(&self) -> StreamableHttpServerConfig {
        let mut cfg = StreamableHttpServerConfig::default()
            .with_stateful_mode(self.stateful_mode)
            .with_json_response(self.json_response);

        if self.disable_host_check {
            tracing::warn!(
                "⚠ --disable-host-check is active: DNS rebinding protection is OFF. \
                 Do NOT use this in production or on non-loopback interfaces."
            );
            cfg = cfg.disable_allowed_hosts();
            return cfg;
        }

        if !self.allowed_hosts.is_empty() {
            cfg = cfg.with_allowed_hosts(self.allowed_hosts.clone());
            return cfg;
        }

        // Keep rmcp's secure loopback defaults; also allow the configured host (if it's loopback).
        let host = self.host.trim();
        if host == "127.0.0.1" || host == "localhost" || host == "::1" {
            cfg.allowed_hosts.push(host.to_string());
        }

        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::HttpServerConfig;

    #[test]
    fn a2a_signing_key_must_be_nonempty_and_distinct_from_bearer() {
        let empty = HttpServerConfig {
            a2a_signing_key: Some(String::new()),
            ..HttpServerConfig::default()
        };
        assert!(empty.validate().is_err());

        let reused = HttpServerConfig {
            auth_token: Some("same-secret".to_string()),
            a2a_signing_key: Some("same-secret".to_string()),
            ..HttpServerConfig::default()
        };
        assert!(reused.validate().is_err());

        let missing_scope = HttpServerConfig {
            auth_token: Some("bearer-secret".to_string()),
            a2a_signing_key: Some("signing-secret".to_string()),
            ..HttpServerConfig::default()
        };
        assert!(missing_scope.validate().is_err());

        let scoped = HttpServerConfig {
            auth_token: Some("bearer-secret".to_string()),
            a2a_signing_key: Some("signing-secret".to_string()),
            a2a_recipient_id: Some("recipient".to_string()),
            a2a_tenant_id: Some("tenant-a".to_string()),
            a2a_project_id: Some("project-a".to_string()),
            ..HttpServerConfig::default()
        };
        assert!(scoped.validate().is_ok());
    }
}
