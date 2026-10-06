use std::time::Duration;

/// Shared TLS config for every ureq client so OS/enterprise root CAs are honored
/// (#643). ureq's default `RootCerts::WebPki` ignores the system store, so requests
/// fail with `UnknownIssuer` behind TLS-intercepting corporate proxies. Inject this
/// into a `ureq::config::Config::builder().tls_config(platform_tls_config())` at each
/// call site — ureq's builder scope typestate is private, so a shared *builder*
/// cannot be returned from a function; the shared piece is this `TlsConfig`.
pub(crate) fn platform_tls_config() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build()
}

/// Builds a ureq agent from an already-assembled config (kept as a thin, nameable
/// wrapper so call sites read uniformly alongside `platform_tls_config`).
pub(crate) fn ureq_agent(config: ureq::config::Config) -> ureq::Agent {
    ureq::Agent::new_with_config(config)
}

/// Agent that honors platform roots and bounds only the connection-setup phases
/// (DNS/connect/first-byte) — a large but progressing download stays uncapped.
pub(crate) fn ureq_agent_with_timeouts(
    timeout_resolve: Option<Duration>,
    timeout_connect: Option<Duration>,
    timeout_recv_response: Option<Duration>,
) -> ureq::Agent {
    agent_with_redirect_policy(
        timeout_resolve,
        timeout_connect,
        timeout_recv_response,
        false,
    )
}

/// Source credentials and request bodies must stay at the selected endpoint.
pub(crate) fn ureq_agent_without_redirects(
    timeout_resolve: Option<Duration>,
    timeout_connect: Option<Duration>,
    timeout_recv_response: Option<Duration>,
) -> ureq::Agent {
    agent_with_redirect_policy(
        timeout_resolve,
        timeout_connect,
        timeout_recv_response,
        true,
    )
}

fn agent_with_redirect_policy(
    timeout_resolve: Option<Duration>,
    timeout_connect: Option<Duration>,
    timeout_recv_response: Option<Duration>,
    deny_redirects: bool,
) -> ureq::Agent {
    let mut config = ureq::config::Config::builder()
        .tls_config(platform_tls_config())
        .timeout_resolve(timeout_resolve)
        .timeout_connect(timeout_connect)
        .timeout_recv_response(timeout_recv_response);
    if deny_redirects {
        config = config.max_redirects(0);
    }
    config.build().into()
}
