//! Shared proxy-setup constants and helpers.

pub(crate) const PROXY_ENV_START: &str = "# >>> lean-ctx proxy env >>>";
pub(crate) const PROXY_ENV_END: &str = "# <<< lean-ctx proxy env <<<";

use crate::core::proxy_endpoint::{
    DEFAULT_PROXY_PORT, default_port, proxy_timeout, uid_based_port,
};

/// Comment written in place of the `ANTHROPIC_BASE_URL` export when no Anthropic API
/// key is detectable. Anthropic does not permit third-party tools to route requests
/// through a Claude Pro/Max subscription login, so we must not route it through the
/// proxy.
pub(crate) const ANTHROPIC_OMITTED_NOTE: &str = "ANTHROPIC_BASE_URL omitted: a Claude Pro/Max subscription login stays on api.anthropic.com (set ANTHROPIC_API_KEY to route Claude through the proxy)";

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

/// True when `url` is the lean-ctx proxy itself: a loopback base URL on a port
/// lean-ctx owns. Callers use this to decide what lean-ctx may overwrite or
/// remove, so "any localhost URL" is not enough — another local gateway
/// (Omniroute, LiteLLM, a corporate relay) belongs to the user (#1972).
pub fn is_local_lean_ctx_url(url: &str) -> bool {
    is_owned_proxy_url(url, &owned_proxy_ports())
}

/// [`is_local_lean_ctx_url`], also owning `install_port` — the port an install
/// pass is wiring right now, which may differ from the configured one.
pub(crate) fn is_lean_ctx_url_on(url: &str, install_port: Option<u16>) -> bool {
    is_local_lean_ctx_url(url) || install_port.is_some_and(|port| is_owned_proxy_url(url, &[port]))
}

/// The ports a lean-ctx proxy listens on: the configured or UID-derived port,
/// plus the historical default that older installs wrote.
fn owned_proxy_ports() -> [u16; 3] {
    [default_port(), uid_based_port(), DEFAULT_PROXY_PORT]
}

/// [`is_local_lean_ctx_url`] with the owned ports passed in, so the rule is
/// testable without reading config or the environment.
pub(crate) fn is_owned_proxy_url(url: &str, ports: &[u16]) -> bool {
    loopback_port(url).is_some_and(|port| ports.contains(&port))
}

/// Port of a plain-HTTP loopback URL (`127.0.0.1`, `localhost`, `[::1]`),
/// path allowed; `None` for anything else.
fn loopback_port(url: &str) -> Option<u16> {
    let url = url.trim();
    let rest = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .find_map(|prefix| url.strip_prefix(prefix))?;
    let port = rest.split('/').next()?;
    port.parse().ok()
}

pub(crate) fn is_proxy_reachable(port: u16) -> bool {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, proxy_timeout()).is_ok()
}
