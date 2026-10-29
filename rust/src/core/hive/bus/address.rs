// SPDX-License-Identifier: Apache-2.0
//! Bus addresses (SPEC-ENGINE §3.7.1, C0 2.0 `bus.schema.json`
//! `$defs/address`). Only an explicitly addressed message leaves the
//! machine; no address, `local` or a local agent id stays on the local bus.

use std::fmt;

/// Where a `ctx_agent post` goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// The local engine bus only (no address, `local`).
    Local,
    /// One agent of this machine, by its local bus id.
    LocalAgent(String),
    /// `#<workspace>/<vault>`: the vault's bus.
    Vault { workspace: String, vault: String },
    /// `~<swarm>`: the swarm's bus.
    Swarm(String),
    /// `@wkr_…`: a direct message to a worker.
    Worker(String),
}

/// Why a `#`, `~` or `@` address is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidAddress;

impl fmt::Display for InvalidAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "invalid_address: use #<workspace>/<vault>, ~<swarm> or @wkr_<id> (or no address for this machine only)",
        )
    }
}

impl std::error::Error for InvalidAddress {}

impl Address {
    /// Parse a `to` argument. `#`, `~` and `@` must be well-formed; any
    /// other text names a local agent.
    pub fn parse(to: Option<&str>) -> Result<Self, InvalidAddress> {
        let Some(to) = to.map(str::trim).filter(|t| !t.is_empty()) else {
            return Ok(Self::Local);
        };
        if to == "local" {
            return Ok(Self::Local);
        }
        if let Some(rest) = to.strip_prefix('#') {
            let (workspace, vault) = rest.split_once('/').ok_or(InvalidAddress)?;
            let ws_ok = (1..=64).contains(&workspace.len())
                && workspace.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                });
            if !ws_ok || !slug(vault, 2, 40) {
                return Err(InvalidAddress);
            }
            return Ok(Self::Vault {
                workspace: workspace.to_string(),
                vault: vault.to_string(),
            });
        }
        if let Some(swarm) = to.strip_prefix('~') {
            return slug(swarm, 3, 40)
                .then(|| Self::Swarm(swarm.to_string()))
                .ok_or(InvalidAddress);
        }
        if let Some(worker) = to.strip_prefix('@') {
            return is_worker_id(worker)
                .then(|| Self::Worker(worker.to_string()))
                .ok_or(InvalidAddress);
        }
        Ok(Self::LocalAgent(to.to_string()))
    }

    /// True when the message may leave the machine.
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Vault { .. } | Self::Swarm(_) | Self::Worker(_))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => f.write_str("local"),
            Self::LocalAgent(id) => f.write_str(id),
            Self::Vault { workspace, vault } => write!(f, "#{workspace}/{vault}"),
            Self::Swarm(id) => write!(f, "~{id}"),
            Self::Worker(id) => write!(f, "@{id}"),
        }
    }
}

/// `[a-z0-9-]{min,max}`.
fn slug(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `wkr_[a-z2-7]{20}`.
pub fn is_worker_id(s: &str) -> bool {
    s.strip_prefix("wkr_").is_some_and(|rest| {
        rest.len() == 20
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_follow_the_contract_grammar() {
        assert_eq!(Address::parse(None), Ok(Address::Local));
        assert_eq!(Address::parse(Some("  ")), Ok(Address::Local));
        assert_eq!(Address::parse(Some("local")), Ok(Address::Local));
        assert_eq!(
            Address::parse(Some("mcp-1-abc")),
            Ok(Address::LocalAgent("mcp-1-abc".into()))
        );
        let vault = Address::parse(Some("#thinkery/lean-ctx")).unwrap();
        assert_eq!(
            vault,
            Address::Vault {
                workspace: "thinkery".into(),
                vault: "lean-ctx".into()
            }
        );
        assert_eq!(vault.to_string(), "#thinkery/lean-ctx");
        assert!(vault.is_remote());
        assert_eq!(
            Address::parse(Some("~launch-q4")),
            Ok(Address::Swarm("launch-q4".into()))
        );
        assert_eq!(
            Address::parse(Some("@wkr_vk4oelpv5lzjmev44hun")),
            Ok(Address::Worker("wkr_vk4oelpv5lzjmev44hun".into()))
        );
        assert!(!Address::parse(Some("mcp-1-abc")).unwrap().is_remote());
        for bad in [
            "#Thinkery/lean-ctx",
            "#thinkery",
            "#thinkery/x",
            "#thinkery/lean-ctx@hive.acme.example",
            "#/lean-ctx",
            "~ab",
            "~Launch",
            "@wkr_short",
            "@wkr_vk4oelpv5lzjmev44hu1",
            "@someone",
        ] {
            assert_eq!(Address::parse(Some(bad)), Err(InvalidAddress), "{bad}");
        }
    }
}
