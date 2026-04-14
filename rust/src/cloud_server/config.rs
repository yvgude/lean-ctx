#[derive(Clone, Debug)]
pub struct Config {
    pub bind_host: String,
    pub bind_port: u16,
    pub public_base_url: String,
    pub api_base_url: String,
    pub database_url: String,
    pub jwt_secret: String,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub smtp_from: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind_host =
            std::env::var("LEANCTX_CLOUD_BIND_HOST").unwrap_or_else(|_| "0.0.0.0".into());
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
        let jwt_secret = std::env::var("LEANCTX_CLOUD_JWT_SECRET")
            .map_err(|_| anyhow::anyhow!("Missing env: LEANCTX_CLOUD_JWT_SECRET"))?;

        let smtp_host = std::env::var("LEANCTX_CLOUD_SMTP_HOST").ok();
        let smtp_port = std::env::var("LEANCTX_CLOUD_SMTP_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok());
        let smtp_username = std::env::var("LEANCTX_CLOUD_SMTP_USERNAME").ok();
        let smtp_password = std::env::var("LEANCTX_CLOUD_SMTP_PASSWORD").ok();
        let smtp_from = std::env::var("LEANCTX_CLOUD_SMTP_FROM").ok();

        Ok(Self {
            bind_host,
            bind_port,
            public_base_url,
            api_base_url,
            database_url,
            jwt_secret,
            smtp_host,
            smtp_port,
            smtp_username,
            smtp_password,
            smtp_from,
        })
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.bind_host, self.bind_port)
    }

    pub fn smtp_enabled(&self) -> bool {
        self.smtp_host.is_some()
            && self.smtp_port.is_some()
            && self.smtp_username.is_some()
            && self.smtp_password.is_some()
            && self.smtp_from.is_some()
    }
}
