//! URL validation and SSRF protection for outbound fetches.
//!
//! `ctx_url_read` accepts arbitrary URLs supplied by an agent, so every request
//! is gated here before any socket is opened: a scheme allow-list, rejection of
//! embedded credentials, and rejection of hosts that resolve to loopback /
//! private / link-local / metadata ranges. Redirect hops are re-validated by the
//! caller using the same primitives.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

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
#[must_use]
pub fn ip_is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_is_blocked(v4),
        IpAddr::V6(v6) => {
            // Dual-stack hosts can expose internal v4 ranges via mapped addrs.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return v4_is_blocked(mapped);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || is_unique_local_v6(v6)
                || is_link_local_v6(v6)
        }
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
}

fn is_unique_local_v6(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
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
}
