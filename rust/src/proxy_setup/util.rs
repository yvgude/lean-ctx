//! Shared proxy-setup constants and helpers.

pub(crate) const PROXY_ENV_START: &str = "# >>> lean-ctx proxy env >>>";
pub(crate) const PROXY_ENV_END: &str = "# <<< lean-ctx proxy env <<<";
pub(crate) const DEFAULT_PROXY_PORT: u16 = 4444;

/// Comment written in place of the `ANTHROPIC_BASE_URL` export when no Anthropic API
/// key is detectable. A Claude Pro/Max subscription authenticates via OAuth against
/// `api.anthropic.com` directly and is rejected by any custom base URL, so we must not
/// route it through the proxy.
pub(crate) const ANTHROPIC_OMITTED_NOTE: &str = "ANTHROPIC_BASE_URL omitted: Claude Pro/Max subscription authenticates against api.anthropic.com directly (set ANTHROPIC_API_KEY to route Claude through the proxy)";

/// Comment written when Grok is not routable through the proxy (no session and no API key).
pub(crate) const GROK_OMITTED_NOTE: &str = "Grok proxy env omitted: run `grok login` (subscription) or set XAI_API_KEY to route Grok through lean-ctx";

/// Comment written when Command Code is not routable through the proxy (no session and no API key).
pub(crate) const COMMANDCODE_OMITTED_NOTE: &str =
    "Command Code omitted (no ~/.commandcode auth — run `cmd login` or set COMMAND_CODE_API_KEY)";

/// Comment written in place of the `OPENAI_BASE_URL` export when Codex is signed
/// in with a ChatGPT subscription (#1685).
///
/// `api.openai.com` accepts API keys only; a subscription OAuth token sent to its
/// `/v1` rail comes back as `401 … Missing scopes: api.responses.write`, which
/// reads like an organization-permission problem and is not one.
pub(crate) const OPENAI_OMITTED_NOTE: &str = "OPENAI_BASE_URL omitted: Codex is signed in with a ChatGPT subscription, whose token only authenticates against chatgpt.com (set OPENAI_API_KEY to route OpenAI through the proxy)";

pub fn is_local_lean_ctx_url(url: &str) -> bool {
    url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")
}

/// Proxy reachability timeout. Priority: env var > config.toml > 200ms default.
pub fn proxy_timeout() -> std::time::Duration {
    if let Ok(val) = std::env::var("LEAN_CTX_PROXY_TIMEOUT_MS")
        && let Ok(ms) = val.parse::<u64>()
    {
        return std::time::Duration::from_millis(ms);
    }
    if let Some(ms) = crate::core::config::Config::load().proxy_timeout_ms {
        return std::time::Duration::from_millis(ms);
    }
    std::time::Duration::from_millis(200)
}

pub(crate) fn is_proxy_reachable(port: u16) -> bool {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, proxy_timeout()).is_ok()
}

pub fn default_port() -> u16 {
    if let Ok(val) = std::env::var("LEAN_CTX_PROXY_PORT")
        && let Ok(port) = val.parse::<u16>()
    {
        return port;
    }
    let cfg = crate::core::config::Config::load();
    if let Some(port) = cfg.proxy_port {
        return port;
    }
    uid_based_port()
}

/// Derives a deterministic port from the user's UID to avoid collisions
/// on multi-user systems. uid 1000 → 4444, uid 1001 → 4445, etc.
/// System accounts (uid < 1000) and root always get the base port 4444.
pub(crate) fn uid_based_port() -> u16 {
    #[cfg(unix)]
    {
        // SAFETY: `getuid` takes no arguments, always succeeds, and only reads
        // the calling process's real UID — no preconditions, no UB.
        let uid = unsafe { libc::getuid() } as u16;
        let offset = uid.saturating_sub(1000) % 1000;
        DEFAULT_PROXY_PORT + offset
    }
    #[cfg(not(unix))]
    {
        DEFAULT_PROXY_PORT
    }
}
