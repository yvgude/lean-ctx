use std::time::Duration;

pub fn ureq_agent(config: ureq::config::Config) -> ureq::Agent {
    ureq::Agent::new_with_config(config)
}

pub fn platform_tls_config() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build()
}

pub fn ureq_agent_with_timeouts(
    timeout_resolve: Option<Duration>,
    timeout_connect: Option<Duration>,
    timeout_recv_response: Option<Duration>,
) -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(platform_tls_config())
        .timeout_resolve(timeout_resolve)
        .timeout_connect(timeout_connect)
        .timeout_recv_response(timeout_recv_response)
        .build()
        .into()
}
