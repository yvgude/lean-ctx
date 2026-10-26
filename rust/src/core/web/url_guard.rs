//! URL validation and SSRF protection for outbound fetches.
//!
//! `ctx_url_read` accepts arbitrary URLs supplied by an agent, so every request
//! is gated here before any socket is opened: a scheme allow-list, rejection of
//! embedded credentials, and rejection of hosts that resolve to loopback /
//! private / link-local / metadata ranges. Redirect hops are re-validated by the
//! caller using the same primitives.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;

/// Reasons a URL is refused before fetching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlError {
    Empty,
    BadScheme(String),
    MissingHost,
    Credentials,
    Blocked(String),
    Unresolvable(String),
}

impl std::fmt::Display for UrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty URL"),
            Self::BadScheme(s) => {
                write!(f, "unsupported scheme '{s}' (only http/https allowed)")
            }
            Self::MissingHost => write!(f, "URL has no host"),
            Self::Credentials => write!(f, "URLs with embedded credentials are not allowed"),
            Self::Blocked(h) => {
                write!(
                    f,
                    "host '{h}' resolves to a blocked (private/loopback) address"
                )
            }
            Self::Unresolvable(h) => write!(f, "host '{h}' could not be resolved"),
        }
    }
}

impl std::error::Error for UrlError {}

/// A syntactically valid http(s) URL with its parsed authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeUrl {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub authority: String,
    pub normalized: String,
}

/// Validate URL *syntax* only (no DNS lookup). Call
/// [`SafeUrl::ensure_resolves_safely`] before opening a socket.
pub fn validate(raw: &str) -> Result<SafeUrl, UrlError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(UrlError::Empty);
    }
    let Some((scheme_raw, rest)) = trimmed.split_once("://") else {
        let head: String = trimmed.chars().take(12).collect();
        return Err(UrlError::BadScheme(head));
    };
    let scheme = scheme_raw.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(UrlError::BadScheme(scheme));
    }

    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let path = &rest[auth_end..];
    if authority.is_empty() {
        return Err(UrlError::MissingHost);
    }
    if authority.contains('@') {
        return Err(UrlError::Credentials);
    }

    let (host, port) = split_host_port(authority, &scheme)?;
    if host.is_empty() {
        return Err(UrlError::MissingHost);
    }

    Ok(SafeUrl {
        scheme: scheme.clone(),
        host,
        port,
        authority: authority.to_string(),
        normalized: format!("{scheme}://{authority}{path}"),
    })
}

fn split_host_port(authority: &str, scheme: &str) -> Result<(String, u16), UrlError> {
    let default_port = if scheme == "https" { 443 } else { 80 };

    // IPv6 literal form: `[::1]` or `[::1]:8080`.
    if let Some(stripped) = authority.strip_prefix('[') {
        let Some(end) = stripped.find(']') else {
            return Err(UrlError::MissingHost);
        };
        let host = stripped[..end].to_string();
        let port = match stripped[end + 1..].strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| UrlError::MissingHost)?,
            None => default_port,
        };
        return Ok((host, port));
    }

    match authority.rsplit_once(':') {
        Some((host, port_str))
            if !port_str.is_empty() && port_str.bytes().all(|b| b.is_ascii_digit()) =>
        {
            let port = port_str.parse().map_err(|_| UrlError::MissingHost)?;
            Ok((host.to_string(), port))
        }
        _ => Ok((authority.to_string(), default_port)),
    }
}

impl SafeUrl {
    /// Resolve the host and reject if *any* resolved address falls in a blocked
    /// range. Rejecting on a single blocked result is a conservative guard
    /// against DNS-rebinding that mixes a public and an internal address.
    pub fn ensure_resolves_safely(&self) -> Result<(), UrlError> {
        if let Ok(ip) = self.host.parse::<IpAddr>() {
            return if ip_is_blocked(ip) {
                Err(UrlError::Blocked(self.host.clone()))
            } else {
                Ok(())
            };
        }

        let addrs = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|_| UrlError::Unresolvable(self.host.clone()))?;

        let mut resolved_any = false;
        for addr in addrs {
            resolved_any = true;
            if ip_is_blocked(addr.ip()) {
                return Err(UrlError::Blocked(self.host.clone()));
            }
        }

        if resolved_any {
            Ok(())
        } else {
            Err(UrlError::Unresolvable(self.host.clone()))
        }
    }
}

/// True for addresses an outbound fetch must never reach (SSRF guard).
pub fn ip_is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_is_blocked(v4),
        IpAddr::V6(v6) => {
            // Dual-stack hosts can expose internal v4 ranges via mapped addrs.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return v4_is_blocked(mapped);
            }
            // Transition prefixes that embed an IPv4 destination (GHSA-h693-v7jv-vmv9).
            let v4 = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
            let segments = v6.segments();
            // ISATAP (RFC 5214): any /64 prefix with interface ID
            // `0:5efe:a.b.c.d`, or `200:5efe:…` with the universal/local bit
            // (the group bit is masked too). Checked independently of the
            // prefix forms below, which an ISATAP interface ID can share.
            if segments[4] & !0x0300 == 0
                && segments[5] == 0x5efe
                && v4_is_blocked(v4(segments[6], segments[7]))
            {
                return true;
            }
            match segments {
                // NAT64 well-known prefix 64:ff9b::/96 (RFC 6052), 6to4 2002::/16
                // (RFC 3056, v4 in bits 16..48), deprecated IPv4-compatible
                // ::a.b.c.d (RFC 4291; `::` and `::1` land in 0.0.0.0/8, blocked)
                // and SIIT IPv4-translated ::ffff:0:a.b.c.d (RFC 2765).
                [0x64, 0xff9b, 0, 0, 0, 0, hi, lo]
                | [0x2002, hi, lo, ..]
                | [0, 0, 0, 0, 0, 0, hi, lo]
                | [0, 0, 0, 0, 0xffff, 0, hi, lo] => return v4_is_blocked(v4(hi, lo)),
                // NAT64 local-use 64:ff9b:1::/48 (RFC 8215): the operator picks the
                // prefix length, so the embedded v4 cannot be located; never public.
                [0x64, 0xff9b, 1, ..] => return true,
                // Teredo 2001::/32 (RFC 4380): server v4 in bits 32..64, client v4
                // bit-inverted in the last 32 bits.
                [0x2001, 0, shi, slo, _, _, chi, clo] => {
                    return v4_is_blocked(v4(shi, slo)) || v4_is_blocked(v4(!chi, !clo));
                }
                _ => {}
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_unique_local_v6(v6)
                || is_link_local_v6(v6)
                || is_site_local_v6(v6)
        }
    }
}

/// A [`ureq::unversioned::resolver::Resolver`] that pins DNS resolution to the
/// exact lookup used for the SSRF check.
///
/// [`SafeUrl::ensure_resolves_safely`] and ureq's own resolver each perform an
/// independent DNS lookup, which opens a rebinding window: a hostile resolver
/// with a short TTL can answer the first (validation) query with a public
/// address and the second (connect) query with a blocked one (e.g. the cloud
/// metadata IP or loopback). Passing this resolver to the `ureq::Agent` makes
/// resolution and validation the same lookup, so there is no second query left
/// to rebind.
#[derive(Debug, Default)]
pub struct SsrfSafeResolver {
    inner: DefaultResolver,
}

impl Resolver for SsrfSafeResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let addrs = self.inner.resolve(uri, config, timeout)?;
        if addrs.iter().any(|addr| ip_is_blocked(addr.ip())) {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addrs)
    }
}

fn v4_is_blocked(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_unspecified()
        || v4.is_documentation()
        || o[0] == 0
        // 100.64.0.0/10 carrier-grade NAT.
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        // 224.0.0.0/4 multicast and 240.0.0.0/4 reserved (incl. broadcast).
        || o[0] >= 224
        // 198.18.0.0/15 benchmarking (RFC 2544), routed to internal test nets.
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
}

fn is_unique_local_v6(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

/// fec0::/10, deprecated site-local (RFC 3879) — still routed internally.
fn is_site_local_v6(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfec0
}

fn is_link_local_v6(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_https_with_path() {
        let u = validate("https://example.com/foo/bar?x=1").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 443);
        assert_eq!(u.authority, "example.com");
        assert_eq!(u.normalized, "https://example.com/foo/bar?x=1");
    }

    #[test]
    fn validates_http_with_explicit_port() {
        let u = validate("http://example.com:8080/p").unwrap();
        assert_eq!(u.port, 8080);
        assert_eq!(u.authority, "example.com:8080");
    }

    #[test]
    fn validates_ipv6_literal_with_port() {
        let u = validate("https://[2606:4700::1111]:8443/p").unwrap();
        assert_eq!(u.host, "2606:4700::1111");
        assert_eq!(u.port, 8443);
    }

    #[test]
    fn rejects_non_http_scheme() {
        assert!(matches!(
            validate("ftp://example.com"),
            Err(UrlError::BadScheme(_))
        ));
        assert!(matches!(
            validate("file:///etc/passwd"),
            Err(UrlError::BadScheme(_))
        ));
    }

    #[test]
    fn rejects_empty_and_credentials() {
        assert_eq!(validate("   "), Err(UrlError::Empty));
        assert_eq!(
            validate("https://user:pass@example.com"),
            Err(UrlError::Credentials)
        );
    }

    #[test]
    fn blocks_loopback_and_private_v4() {
        for ip in ["127.0.0.1", "10.0.0.1", "192.168.1.1", "172.16.0.1"] {
            assert!(ip_is_blocked(ip.parse().unwrap()), "{ip} must be blocked");
        }
    }

    #[test]
    fn blocks_metadata_and_cgnat() {
        assert!(ip_is_blocked("169.254.169.254".parse().unwrap()));
        assert!(ip_is_blocked("100.64.0.1".parse().unwrap()));
        assert!(ip_is_blocked("0.0.0.0".parse().unwrap()));
    }

    #[test]
    fn allows_public_v4_and_v6() {
        assert!(!ip_is_blocked("8.8.8.8".parse().unwrap()));
        assert!(!ip_is_blocked("1.1.1.1".parse().unwrap()));
        assert!(!ip_is_blocked("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn blocks_v6_internal_ranges() {
        assert!(ip_is_blocked("::1".parse().unwrap()));
        assert!(ip_is_blocked("fe80::1".parse().unwrap()));
        assert!(ip_is_blocked("fc00::1".parse().unwrap()));
        assert!(ip_is_blocked("::ffff:127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn blocks_v6_transition_addrs_embedding_internal_v4() {
        // GHSA-h693-v7jv-vmv9: 169.254.169.254, 127.0.0.1 and 10.0.0.1 via
        // NAT64, 6to4, Teredo (server and client) and IPv4-compatible forms.
        for ip in [
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            "64:ff9b:1::a9fe:a9fe",
            "2002:a9fe:a9fe::1",
            "2002:7f00:1::1",
            "2002:a00:1::1",
            "2001:0:a9fe:a9fe::1",
            "2001:0:808:808:0:0:5601:5601",
            "2001:0:808:808:0:0:80ff:fffe",
            "2001:0:808:808:0:0:f5ff:fffe",
            "::a9fe:a9fe",
            "::7f00:1",
            "::a00:1",
            "::",
        ] {
            assert!(ip_is_blocked(ip.parse().unwrap()), "{ip} must be blocked");
            let u = validate(&format!("http://[{ip}]/")).unwrap();
            assert!(
                matches!(u.ensure_resolves_safely(), Err(UrlError::Blocked(_))),
                "{ip} must be refused by ensure_resolves_safely"
            );
        }
    }

    #[test]
    fn allows_v6_transition_addrs_embedding_public_v4() {
        for ip in [
            "64:ff9b::808:808",
            "2002:808:808::1",
            // Teredo: server 8.8.8.8, client 1.1.1.1 (inverted fefe:fefe).
            "2001:0:808:808:0:0:fefe:fefe",
            "::808:808",
        ] {
            assert!(!ip_is_blocked(ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    /// GHSA-h693-v7jv-vmv9 residuals: SIIT, ISATAP, IPv6 multicast and
    /// site-local, IPv4 multicast/reserved and benchmarking ranges.
    #[test]
    fn blocks_siit_isatap_multicast_site_local_and_benchmarking() {
        for ip in [
            // SIIT IPv4-translated ::ffff:0:a.b.c.d.
            "::ffff:0:a9fe:a9fe",
            "::ffff:0:7f00:1",
            "::ffff:0:a00:1",
            // ISATAP interface IDs (local and universal) embedding internal v4.
            "2001:db8::5efe:a9fe:a9fe",
            "2606:4700::200:5efe:7f00:1",
            "2606:4700::5efe:a00:1",
            "fe80::200:5efe:c0a8:101",
            // IPv6 multicast ff00::/8 and site-local fec0::/10.
            "ff02::1",
            "ff05::2",
            "ff0e::1",
            "fec0::1",
            "feff::1",
            // IPv4 multicast, reserved and benchmarking.
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "198.18.0.1",
            "198.19.255.255",
        ] {
            assert!(ip_is_blocked(ip.parse().unwrap()), "{ip} must be blocked");
            let u = validate(&format!(
                "http://{}/",
                if ip.contains(':') {
                    format!("[{ip}]")
                } else {
                    ip.to_string()
                }
            ))
            .unwrap();
            assert!(
                matches!(u.ensure_resolves_safely(), Err(UrlError::Blocked(_))),
                "{ip} must be refused by ensure_resolves_safely"
            );
        }
    }

    #[test]
    fn allows_public_neighbours_of_the_new_blocked_ranges() {
        for ip in [
            // SIIT and ISATAP embedding public v4 stay allowed.
            "::ffff:0:808:808",
            "2606:4700::5efe:808:808",
            "2606:4700::200:5efe:101:101",
            // Global unicast neighbours of fec0::/10 and ff00::/8.
            "2001:4860:4860::8888",
            "fe00::1",
            // Public v4 next to the benchmarking and multicast ranges.
            "198.17.255.255",
            "198.20.0.1",
            "223.255.255.254",
        ] {
            assert!(!ip_is_blocked(ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    #[test]
    fn ensure_resolves_safely_rejects_literal_loopback() {
        let u = validate("http://127.0.0.1/").unwrap();
        assert!(matches!(
            u.ensure_resolves_safely(),
            Err(UrlError::Blocked(_))
        ));
    }

    #[test]
    fn ensure_resolves_safely_allows_literal_public_ip() {
        let u = validate("http://8.8.8.8/").unwrap();
        assert!(u.ensure_resolves_safely().is_ok());
    }

    // --- SSRF-rebinding: resolver pinning ---

    fn no_timeout() -> NextTimeout {
        use ureq::unversioned::transport::time::Duration;
        NextTimeout {
            after: Duration::NotHappening,
            reason: ureq::Timeout::Global,
        }
    }

    #[test]
    fn ssrf_safe_resolver_blocks_loopback_hostname() {
        // `localhost` resolves via the OS hosts file to 127.0.0.1 with no
        // network access, so this exercises the real DefaultResolver lookup
        // path — a stand-in for a DNS-rebinding response that only appears
        // blocked at the moment of connection, not at the earlier
        // `ensure_resolves_safely` check.
        let resolver = SsrfSafeResolver::default();
        let uri: Uri = "http://localhost:80/".parse().unwrap();
        let config = Config::default();
        assert!(resolver.resolve(&uri, &config, no_timeout()).is_err());
    }

    #[test]
    fn ssrf_safe_resolver_allows_literal_public_ip() {
        let resolver = SsrfSafeResolver::default();
        let uri: Uri = "http://8.8.8.8:80/".parse().unwrap();
        let config = Config::default();
        assert!(resolver.resolve(&uri, &config, no_timeout()).is_ok());
    }
}
