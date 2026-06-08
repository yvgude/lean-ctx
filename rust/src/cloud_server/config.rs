#[derive(Clone, Debug)]
pub(super) struct Config {
    pub bind_host: String,
    pub bind_port: u16,
    pub public_base_url: String,
    pub api_base_url: String,
    pub database_url: String,
    /// Salt for the abuse-only `ip_hash` (Wrapped permalink rate limiting). Never used for
    /// tracking; only to bound publishes per source. Set `LEANCTX_CLOUD_IP_SALT` in prod.
    pub ip_hash_salt: String,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub smtp_from: Option<String>,
    /// Base URL of the private commercial control-plane (`lean-ctx-cloud`). When
    /// unset, the edge resolves every account to [`Plan::Free`](crate::core::billing::Plan)
    /// — i.e. the open backend runs fully without the paid plane (Local-Free).
    pub billing_base_url: Option<String>,
    /// Shared `X-Internal-Key` secret for calling the billing service.
    pub billing_internal_key: Option<String>,
}

impl Config {
    pub(super) fn from_env() -> anyhow::Result<Self> {
        let bind_host =
            std::env::var("LEANCTX_CLOUD_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let bind_port = std::env::var("LEANCTX_CLOUD_BIND_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(8088);
        let public_base_url = std::env::var("LEANCTX_CLOUD_PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "https://leanctx.com".into());
        let api_base_url = std::env::var("LEANCTX_CLOUD_API_BASE_URL")
            .unwrap_or_else(|_| "https://api.leanctx.com".into());
        let database_url = std::env::var("LEANCTX_CLOUD_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .map_err(|_| {
                anyhow::anyhow!("Missing env: LEANCTX_CLOUD_DATABASE_URL (or DATABASE_URL)")
            })?;
        let ip_hash_salt = std::env::var("LEANCTX_CLOUD_IP_SALT")
            .unwrap_or_else(|_| "lean-ctx-wrapped-ip-salt-v1".into());
        let smtp_host = std::env::var("LEANCTX_CLOUD_SMTP_HOST").ok();
        let smtp_port = std::env::var("LEANCTX_CLOUD_SMTP_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok());
        let smtp_username = std::env::var("LEANCTX_CLOUD_SMTP_USERNAME").ok();
        let smtp_password = std::env::var("LEANCTX_CLOUD_SMTP_PASSWORD").ok();
        let smtp_from = std::env::var("LEANCTX_CLOUD_SMTP_FROM").ok();
        let billing_base_url = std::env::var("LEANCTX_CLOUD_BILLING_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());
        let billing_internal_key = std::env::var("LEANCTX_CLOUD_BILLING_INTERNAL_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty());

        Ok(Self {
            bind_host,
            bind_port,
            public_base_url,
            api_base_url,
            database_url,
            ip_hash_salt,
            smtp_host,
            smtp_port,
            smtp_username,
            smtp_password,
            smtp_from,
            billing_base_url,
            billing_internal_key,
        })
    }

    pub(super) fn bind_addr(&self) -> String {
        format!("{}:{}", self.bind_host, self.bind_port)
    }

    pub(super) fn smtp_enabled(&self) -> bool {
        self.smtp_host.is_some()
            && self.smtp_port.is_some()
            && self.smtp_username.is_some()
            && self.smtp_password.is_some()
            && self.smtp_from.is_some()
    }
}
