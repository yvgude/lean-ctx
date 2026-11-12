// SPDX-License-Identifier: Apache-2.0
//! The seam through which the local proxy serves the subscription models
//! (mz6, `docs/model-access-subscription.md` §4).
//!
//! The built-in registry provider [`PROVIDER_ID`] forwards to the Hive models
//! gateway with the instance's short-lived `hive_i_` access token. The proxy
//! and the Hive connector are sibling crates, so the connector registers the
//! one process-wide [`HiveTokenSource`] (as it does the runtime
//! `LeaseSource`) and the proxy asks it, never the Hive crate directly.
//!
//! The token opens every engine route of Hive, so it may only go to the
//! models origin [`models_origin`] derives from the enrolled Hive URL.

use std::sync::{Arc, RwLock};

/// Registry id of the built-in subscription-model provider.
pub const PROVIDER_ID: &str = "leanctx";

/// Error code of the local 403 while the lease does not grant the right
/// `models.subscription` (or the models origin is refused).
pub const UNAVAILABLE_CODE: &str = "subscription_models_unavailable";

/// Error code of the local 503 when no instance token can be had.
pub const TOKEN_UNAVAILABLE_CODE: &str = "hive_token_unavailable";

/// What a request to the gateway may carry now.
pub struct ModelsAdmission {
    /// The models origin (`https://models.<hive host>`), no trailing slash.
    pub origin: String,
    /// The instance access token (`hive_i_…`).
    pub token: zeroize::Zeroizing<String>,
    /// A held route lets this one request through as a probe: its success
    /// must be reported ([`HiveTokenSource::note_passed`]) to end the hold.
    pub probe: bool,
}

// The token is a credential: debug output never shows it.
impl std::fmt::Debug for ModelsAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelsAdmission")
            .field("origin", &self.origin)
            .field("token", &"[redacted]")
            .field("probe", &self.probe)
            .finish()
    }
}

/// Why a request is answered locally instead of going to the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelsRefusal {
    /// Not connected, Hive paused, revoked, lease lapsed or without the
    /// right `models.subscription`: 403 [`UNAVAILABLE_CODE`].
    Unavailable,
    /// The configured `[hive] models_url` is not on the Hive host: 403
    /// [`UNAVAILABLE_CODE`]; checked before any token is read or minted.
    OriginRefused(String),
    /// An earlier gateway refusal (402/403/429) holds the `leanctx` routes
    /// until `retry_after_s` from now (throttle style).
    Held {
        status: u16,
        code: String,
        retry_after_s: u64,
        line: String,
    },
    /// No token could be minted: 503 [`TOKEN_UNAVAILABLE_CODE`].
    TokenUnavailable,
}

/// Where the subscription-model access comes from (the Hive connector).
/// Every method may block (file reads, a token mint); the proxy calls them
/// off the async runtime and bounds the wait.
pub trait HiveTokenSource: Send + Sync {
    /// Lease right, origin pin, holds, then the token; `fresh_token` mints
    /// a new one (the gateway answered 401 to the cached one).
    fn admit_route(&self, fresh_token: bool) -> Result<ModelsAdmission, ModelsRefusal>;
    /// The gateway answered `status` (non-2xx) with `body`: record the hold
    /// it sets, if any.
    fn note_answer(&self, status: u16, retry_after: Option<&str>, body: &[u8]);
    /// A probe passed: the holds of the `leanctx` routes end.
    fn note_passed(&self);
}

static SOURCE: RwLock<Option<Arc<dyn HiveTokenSource>>> = RwLock::new(None);

/// Registers the process-wide source. A later call replaces it.
pub fn set_hive_token_source(source: Arc<dyn HiveTokenSource>) {
    *SOURCE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(source);
}

/// The registered source; `None` without a Hive connector.
pub fn hive_token_source() -> Option<Arc<dyn HiveTokenSource>> {
    SOURCE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn origin_of(url: &reqwest::Url) -> String {
    url.origin().ascii_serialization()
}

/// The models origin for an instance enrolled at `hive_url`.
///
/// Without `models_url` it is `https://models.<hive host>` (a loopback Hive,
/// as in tests, serves the models itself). A configured `models_url` is
/// honoured only when it is a bare origin whose host equals the Hive host or
/// is a direct subdomain of it, over HTTPS; loopback is allowed for tests.
/// Anything else is refused: the token must never reach a foreign host.
pub fn models_origin(hive_url: &str, models_url: Option<&str>) -> Result<String, String> {
    let hive = reqwest::Url::parse(hive_url.trim()).map_err(|e| format!("Hive URL: {e}"))?;
    let hive_host = hive
        .host_str()
        .ok_or("Hive URL has no host")?
        .to_ascii_lowercase();
    let Some(raw) = models_url.map(str::trim).filter(|s| !s.is_empty()) else {
        if is_loopback(&hive_host) {
            return Ok(origin_of(&hive));
        }
        if hive.scheme() != "https" {
            return Err("Hive URL is not https".into());
        }
        let port = hive.port().map(|p| format!(":{p}")).unwrap_or_default();
        return Ok(format!("https://models.{hive_host}{port}"));
    };
    let url = reqwest::Url::parse(raw).map_err(|e| format!("models_url: {e}"))?;
    if !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("models_url must be a bare origin (scheme, host, port)".into());
    }
    let host = url
        .host_str()
        .ok_or("models_url has no host")?
        .to_ascii_lowercase();
    if is_loopback(&host) && matches!(url.scheme(), "http" | "https") {
        return Ok(origin_of(&url));
    }
    if url.scheme() != "https" {
        return Err("models_url must use https".into());
    }
    let direct_subdomain = host
        .strip_suffix(&hive_host)
        .and_then(|rest| rest.strip_suffix('.'))
        .is_some_and(|label| !label.is_empty() && !label.contains('.'));
    if host == hive_host || direct_subdomain {
        Ok(origin_of(&url))
    } else {
        Err(format!(
            "models_url host {host} is not the Hive host {hive_host} or a direct subdomain of it"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_origin_stays_on_the_hive_host() {
        let hive = "https://hive.example.com";
        assert_eq!(
            models_origin(hive, None).unwrap(),
            "https://models.hive.example.com"
        );
        for ok in [
            "https://models.hive.example.com",
            "https://gw.hive.example.com/",
            "https://hive.example.com:8443",
            "http://127.0.0.1:9000",
        ] {
            assert!(
                models_origin(hive, Some(ok)).is_ok(),
                "{ok} must be honoured"
            );
        }
        for refused in [
            "https://models.evil.example",
            "https://hive.example.com.evil.example",
            "https://a.b.hive.example.com",
            "https://evilhive.example.com",
            "http://models.hive.example.com",
            "https://models.hive.example.com/v1",
            "https://user@models.hive.example.com",
        ] {
            assert!(
                models_origin(hive, Some(refused)).is_err(),
                "{refused} must be refused"
            );
        }
        assert_eq!(
            models_origin("http://127.0.0.1:7000", None).unwrap(),
            "http://127.0.0.1:7000"
        );
        assert!(models_origin("http://hive.example.com", None).is_err());
    }
}
