// SPDX-License-Identifier: Apache-2.0

//! `leanctx.managed-config/v1`: what an organisation preconfigures on a
//! machine through MDM (contract: `docs/contracts/managed-config-v1.md`).
//!
//! The carrier (a macOS profile, the Windows policy key, or
//! `/etc/lean-ctx/managed.toml`) delivers members either nested
//! (`{"hive": {"url": …}}`) or dotted (`{"hive.url": …}`, the natural form of
//! a registry value name); [`ManagedConfigV1::from_members`] accepts both.
//! The configuration names the Hive, the org, the IdP hint and the update
//! mode. It carries no secret and no policy content. Every member is
//! optional; an unknown member, a wrong type or a value out of shape refuses
//! the whole configuration, and the engine then fails closed.
//!
//! Precedence: managed > Hive floor > the user's `config.toml` > defaults.
//! `telemetry.*` members only tighten: `false` turns an upload off, `true`
//! changes nothing.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::effective_policy::policy_digest;
use crate::ValidationError;

pub const MANAGED_CONFIG_SCHEMA: &str = "leanctx.managed-config/v1";

/// The update channel an organisation chose (`updates.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedUpdateModeV1 {
    /// The MDM ships updates; the engine never updates itself.
    Mdm,
    /// The Hive names the version.
    Hive,
    /// The user decides (the unmanaged behaviour).
    User,
}

/// A validated managed configuration. `None`/empty: not managed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedConfigV1 {
    /// Hive web origin; wins over `[hive] url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive_url: Option<String>,
    /// Base64 Ed25519 root key; wins over `[hive] root_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive_root_key: Option<String>,
    /// The only org this machine may connect to (org wire id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive_org: Option<String>,
    /// Identity-provider hint for sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive_idp_hint: Option<String>,
    /// Tools refuse until this machine is signed in with a live lease and
    /// a policy floor is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive_require: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_vaults: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updates_mode: Option<ManagedUpdateModeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updates_feed_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry_metrics_upload: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry_receipts_upload: Option<bool>,
}

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Origin,
    Https,
    RootKey,
    Id,
    Bool,
    List,
    Mode,
}

/// Every member of v1, by its dotted name.
const MEMBERS: &[(&str, Kind)] = &[
    ("hive.url", Kind::Origin),
    ("hive.root_key", Kind::RootKey),
    ("hive.org", Kind::Id),
    ("hive.idp_hint", Kind::Text),
    ("hive.require", Kind::Bool),
    ("workspace.vaults", Kind::List),
    ("updates.mode", Kind::Mode),
    ("updates.feed_base", Kind::Https),
    ("telemetry.metrics_upload", Kind::Bool),
    ("telemetry.receipts_upload", Kind::Bool),
];

impl ManagedConfigV1 {
    /// Parse the members a carrier holds. `schema`, when present, must be
    /// [`MANAGED_CONFIG_SCHEMA`].
    pub fn from_members(members: &Map<String, Value>) -> Result<Self, ValidationError> {
        let mut flat = Map::new();
        for (key, value) in members {
            if key == "schema" {
                if value.as_str() != Some(MANAGED_CONFIG_SCHEMA) {
                    return Err(ValidationError::new(format!(
                        "schema must be \"{MANAGED_CONFIG_SCHEMA}\""
                    )));
                }
                continue;
            }
            flatten(key, value, &mut flat)?;
        }
        let mut out = Map::new();
        for (name, value) in flat {
            let Some((_, kind)) = MEMBERS.iter().find(|(member, _)| *member == name) else {
                return Err(ValidationError::new(format!("unknown member `{name}`")));
            };
            let value = coerce(&name, *kind, value)?;
            out.insert(name.replace('.', "_"), value);
        }
        serde_json::from_value(Value::Object(out))
            .map_err(|error| ValidationError::new(format!("managed configuration: {error}")))
    }

    /// No member is set.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// The members that are set, by dotted name, with their value as text
    /// (lists joined by `, `), in schema order.
    pub fn members(&self) -> Vec<(&'static str, String)> {
        let Ok(Value::Object(map)) = serde_json::to_value(self) else {
            return Vec::new();
        };
        MEMBERS
            .iter()
            .filter_map(|(name, _)| {
                let value = map.get(&name.replace('.', "_"))?;
                let text = match value {
                    Value::String(text) => text.clone(),
                    Value::Array(items) => items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                    other => other.to_string(),
                };
                Some((*name, text))
            })
            .collect()
    }

    /// Whether `member` (dotted, e.g. `hive.url`) is set.
    pub fn sets(&self, member: &str) -> bool {
        self.members().iter().any(|(name, _)| *name == member)
    }

    /// `managed_digest`: `"sha256:" + hex(sha256(canonical_json))` of the
    /// configuration with its schema, for the audit trail.
    pub fn digest(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert("schema".into(), Value::from(MANAGED_CONFIG_SCHEMA));
        }
        policy_digest(&value).unwrap_or_default()
    }
}

/// Nested objects become dotted names; a name may already be dotted.
fn flatten(
    prefix: &str,
    value: &Value,
    out: &mut Map<String, Value>,
) -> Result<(), ValidationError> {
    match value {
        Value::Object(inner) => {
            for (key, value) in inner {
                flatten(&format!("{prefix}.{key}"), value, out)?;
            }
            Ok(())
        }
        other => {
            if out.insert(prefix.to_string(), other.clone()).is_some() {
                return Err(ValidationError::new(format!(
                    "member `{prefix}` is set twice"
                )));
            }
            Ok(())
        }
    }
}

fn coerce(name: &str, kind: Kind, value: Value) -> Result<Value, ValidationError> {
    let refuse = |what: &str| ValidationError::new(format!("member `{name}` must be {what}"));
    let text = |value: &Value| -> Result<String, ValidationError> {
        match value.as_str().map(str::trim) {
            Some(text) if !text.is_empty() && text.len() <= 512 => Ok(text.to_string()),
            _ => Err(refuse("a non-empty string")),
        }
    };
    Ok(match kind {
        Kind::Text => Value::from(text(&value)?),
        Kind::Origin => {
            let url = text(&value)?;
            if !is_origin(&url) {
                return Err(refuse(
                    "an https origin (http only for loopback), without path or query",
                ));
            }
            Value::from(url)
        }
        Kind::Https => {
            let url = text(&value)?;
            if !url.starts_with("https://") || url.len() <= "https://".len() {
                return Err(refuse("an https URL"));
            }
            Value::from(url)
        }
        Kind::RootKey => {
            let key = text(&value)?;
            let base64 = |c: char| c.is_ascii_alphanumeric() || c == '+' || c == '/';
            if key.len() != 44 || !key.ends_with('=') || !key[..43].chars().all(base64) {
                return Err(refuse(
                    "a standard base64 Ed25519 public key (44 characters)",
                ));
            }
            Value::from(key)
        }
        Kind::Id => {
            let id = text(&value)?;
            if id.len() > 128 || id.chars().any(char::is_whitespace) {
                return Err(refuse("an org id without whitespace"));
            }
            Value::from(id)
        }
        Kind::Bool => Value::from(match &value {
            Value::Bool(flag) => *flag,
            Value::Number(number) if number.as_u64() == Some(0) => false,
            Value::Number(number) if number.as_u64() == Some(1) => true,
            Value::String(text) if matches!(text.trim(), "true" | "1") => true,
            Value::String(text) if matches!(text.trim(), "false" | "0") => false,
            _ => return Err(refuse("true or false (or 1/0)")),
        }),
        Kind::List => {
            let items: Vec<Value> = match &value {
                Value::Array(items) => items.clone(),
                Value::String(joined) => joined
                    .split([',', '\n'])
                    .filter(|part| !part.trim().is_empty())
                    .map(|part| Value::from(part.trim()))
                    .collect(),
                _ => return Err(refuse("a list of strings")),
            };
            let items = items.iter().map(text).collect::<Result<Vec<_>, _>>()?;
            Value::from(items)
        }
        Kind::Mode => {
            let mode = text(&value)?;
            if !matches!(mode.as_str(), "mdm" | "hive" | "user") {
                return Err(refuse("one of mdm, hive, user"));
            }
            Value::from(mode)
        }
    })
}

/// `https://host[:port]` (or `http://` for loopback), an optional trailing
/// `/`, nothing else.
fn is_origin(url: &str) -> bool {
    let (scheme, rest) = match url.split_once("://") {
        Some(parts) => parts,
        None => return false,
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() || authority.contains(['/', '?', '#', '@', ' ']) {
        return false;
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()) && !authority.ends_with(']'))
        .map_or(authority, |(host, _)| host);
    match scheme {
        "https" => true,
        "http" => matches!(host, "localhost" | "127.0.0.1" | "[::1]"),
        _ => false,
    }
}

/// Where a managed configuration came from (`hive status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedCarrierKindV1 {
    MacosProfile,
    WindowsPolicy,
    LinuxFile,
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: Value) -> Result<ManagedConfigV1, ValidationError> {
        match value {
            Value::Object(map) => ManagedConfigV1::from_members(&map),
            _ => unreachable!(),
        }
    }

    const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    #[test]
    fn nested_and_dotted_carriers_mean_the_same_configuration() {
        let nested = parse(json!({
            "schema": MANAGED_CONFIG_SCHEMA,
            "hive": {"url": "https://hive.acme.example", "root_key": KEY, "org": "org_1", "require": true},
            "workspace": {"vaults": ["acme/main"]},
            "updates": {"mode": "mdm"},
            "telemetry": {"metrics_upload": false},
        }))
        .expect("valid");
        // Registry shape: dotted names, DWORD booleans, a joined list.
        let dotted = parse(json!({
            "hive.url": "https://hive.acme.example",
            "hive.root_key": KEY,
            "hive.org": "org_1",
            "hive.require": 1,
            "workspace.vaults": "acme/main",
            "updates.mode": "mdm",
            "telemetry.metrics_upload": 0,
        }))
        .expect("valid");
        assert_eq!(nested, dotted);
        assert_eq!(nested.digest(), dotted.digest());
        assert!(nested.digest().starts_with("sha256:"));
        assert!(nested.sets("hive.require") && !nested.sets("hive.idp_hint"));
        assert_eq!(
            nested.members()[0],
            ("hive.url", "https://hive.acme.example".into())
        );
    }

    #[test]
    fn a_configuration_out_of_shape_is_refused_whole() {
        for bad in [
            json!({"schema": "leanctx.managed-config/v2"}),
            json!({"hive": {"rquire": true}}),
            json!({"hive.url": "http://hive.acme.example"}),
            json!({"hive.url": "https://hive.acme.example/v1"}),
            json!({"hive.root_key": "not-a-key"}),
            json!({"hive.require": "yes"}),
            json!({"hive": {"org": "a"}, "hive.org": "b"}),
            json!({"updates.mode": "auto"}),
            json!({"updates.feed_base": "http://feed.example"}),
            json!({"hive.org": "org 1"}),
        ] {
            assert!(parse(bad.clone()).is_err(), "accepted {bad}");
        }
        assert!(parse(json!({"hive.url": "http://127.0.0.1:8080"})).is_ok());
        assert!(parse(json!({})).expect("empty").is_empty());
    }
}
