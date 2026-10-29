// SPDX-License-Identifier: Apache-2.0
//! Vault addresses `<ws>/<vault>[@<server>]` (C0 2.0 `common.schema.json`
//! `$defs/vault_address`, `vectors/vault_address.vectors.json`), used in
//! `.lean-ctx-hive.json` (link v2), CLI vault arguments and environment
//! values. No server, or `@hive.leanctx.com`, is the hosted Hive; a
//! server with a dot or a port is self-hosted at `https://<server>`.

use std::fmt;

/// The hosted Hive.
pub const HOSTED_SERVER: &str = "hive.leanctx.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultAddress {
    pub ws: String,
    pub vault: String,
    /// Self-hosted server (`host[:port]`, lowercase); `None` = hosted.
    pub server: Option<String>,
}

/// Why an address is refused (the vectors' `reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressRefusal {
    WsInvalid,
    VaultMissing,
    VaultInvalid,
    ServerEmpty,
    ServerAmbiguous,
    ServerInvalid,
    PortInvalid,
}

impl AddressRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::WsInvalid => "ws_invalid",
            Self::VaultMissing => "vault_missing",
            Self::VaultInvalid => "vault_invalid",
            Self::ServerEmpty => "server_empty",
            Self::ServerAmbiguous => "server_ambiguous",
            Self::ServerInvalid => "server_invalid",
            Self::PortInvalid => "port_invalid",
        }
    }
}

impl fmt::Display for AddressRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hint = match self {
            Self::ServerAmbiguous => {
                "a server needs a dot or a port (e.g. hive.example.com or localhost:8443)"
            }
            Self::ServerEmpty => "nothing follows @",
            Self::PortInvalid => "the port must be 1 to 65535",
            _ => "use <workspace>/<vault>[@<server>] in lowercase",
        };
        write!(f, "{}: {hint}", self.code())
    }
}

impl std::error::Error for AddressRefusal {}

impl VaultAddress {
    pub fn parse(input: &str) -> Result<Self, AddressRefusal> {
        // The last `@` starts the server, so an `@` inside the path makes
        // the vault invalid rather than the server.
        let (path, server) = match input.rsplit_once('@') {
            Some((path, server)) => (path, Some(server)),
            None => (input, None),
        };
        let mut parts = path.split('/');
        let ws = parts.next().unwrap_or_default();
        if !is_ws(ws) {
            return Err(AddressRefusal::WsInvalid);
        }
        let vault = parts.next().ok_or(AddressRefusal::VaultMissing)?;
        if parts.next().is_some() || !is_vault(vault) {
            return Err(AddressRefusal::VaultInvalid);
        }
        let server = server.map(parse_server).transpose()?;
        Ok(Self {
            ws: ws.to_string(),
            vault: vault.to_string(),
            server: server.filter(|s| s != HOSTED_SERVER),
        })
    }

    pub fn is_hosted(&self) -> bool {
        self.server.is_none()
    }

    /// `https://<server>`.
    pub fn origin(&self) -> String {
        format!(
            "https://{}",
            self.server.as_deref().unwrap_or(HOSTED_SERVER)
        )
    }
}

impl fmt::Display for VaultAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.ws, self.vault)?;
        match &self.server {
            Some(server) => write!(f, "@{server}"),
            None => Ok(()),
        }
    }
}

/// `[a-z0-9][a-z0-9-]{0,38}`.
fn is_ws(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=39).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `[a-z0-9-]{2,40}`.
fn is_vault(s: &str) -> bool {
    (2..=40).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// DNS label `[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`.
fn is_label(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=63).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[b.len() - 1].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

/// `host-with-dot[:port] | name:port | [ipv6]:port`, lowercased.
fn parse_server(raw: &str) -> Result<String, AddressRefusal> {
    if raw.is_empty() {
        return Err(AddressRefusal::ServerEmpty);
    }
    let server = raw.to_ascii_lowercase();
    if let Some(rest) = server.strip_prefix('[') {
        let (ip, port) = rest.split_once("]:").ok_or(AddressRefusal::ServerInvalid)?;
        let ip_ok = !ip.is_empty()
            && ip
                .bytes()
                .all(|c| c.is_ascii_hexdigit() || c == b':' || c == b'.');
        if !ip_ok || ip.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(AddressRefusal::ServerInvalid);
        }
        check_port(port)?;
        return Ok(server);
    }
    let (host, port) = match server.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (server.as_str(), None),
    };
    if !host.split('.').all(is_label) {
        return Err(AddressRefusal::ServerInvalid);
    }
    match port {
        Some(port) => check_port(port)?,
        None if !host.contains('.') => return Err(AddressRefusal::ServerAmbiguous),
        None => {}
    }
    Ok(server)
}

fn check_port(port: &str) -> Result<(), AddressRefusal> {
    let valid = (1..=5).contains(&port.len())
        && port.bytes().all(|c| c.is_ascii_digit())
        && port.parse::<u32>().is_ok_and(|p| (1..=65_535).contains(&p));
    if valid {
        Ok(())
    } else {
        Err(AddressRefusal::PortInvalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &str = include_str!(
        "../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/vault_address.vectors.json"
    );

    #[test]
    fn addresses_match_the_contract_vectors() {
        let vectors: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
        let cases = vectors["cases"].as_array().unwrap();
        assert!(cases.len() >= 15);
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let want = &case["output"];
            match VaultAddress::parse(input) {
                Ok(address) => {
                    assert_eq!(want["valid"], true, "{input}");
                    assert_eq!(address.ws, want["ws"], "{input}");
                    assert_eq!(address.vault, want["vault"], "{input}");
                    assert_eq!(address.is_hosted(), want["hosted"], "{input}");
                    assert_eq!(address.origin(), want["origin"], "{input}");
                    assert_eq!(
                        address
                            .server
                            .as_deref()
                            .map_or(serde_json::Value::Null, Into::into),
                        want["server"],
                        "{input}"
                    );
                }
                Err(refusal) => {
                    assert_eq!(want["valid"], false, "{input}");
                    assert_eq!(refusal.code(), want["reason"], "{input}");
                }
            }
        }
    }

    #[test]
    fn display_round_trips_the_canonical_form() {
        for input in [
            "acme/payments",
            "acme/payments@hive.acme.example:8443",
            "acme/p2@[::1]:8443",
        ] {
            assert_eq!(VaultAddress::parse(input).unwrap().to_string(), input);
        }
        assert_eq!(
            VaultAddress::parse("acme/payments@Hive.LeanCTX.com")
                .unwrap()
                .to_string(),
            "acme/payments"
        );
        assert_eq!(
            VaultAddress::parse("acme/payments@[zz]:1"),
            Err(AddressRefusal::ServerInvalid)
        );
        assert_eq!(
            VaultAddress::parse("acme/payments@-bad.example"),
            Err(AddressRefusal::ServerInvalid)
        );
    }
}
