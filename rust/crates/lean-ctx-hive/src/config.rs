// SPDX-License-Identifier: Apache-2.0
//! `[hive]` configuration: the section's types come from
//! [`lean_ctx::core::config::hive`]; this module adds the Hive's origin and
//! root-key rules.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use super::HiveError;
use lean_ctx_protocol::hive::HIVE_DEFAULT_URL;

pub use lean_ctx::core::config::hive::{
    DEFAULT_PULL_INTERVAL_MINUTES, HiveBusMode, HiveConfig, PULL_INTERVAL_BOUNDS, schema_section,
};

/// Root public keys of the hosted Hive (`hive.leanctx.com`), base64
/// Ed25519, the one that signs today first (C0 `root_keys.vectors.json`
/// `hosted`, RESOLUTIONS R55). Any other Hive needs `[hive] root_key`.
pub const HOSTED_ROOT_KEYS: &[&str] = &["gu/tH0HqqDVcjm+fbHoS05NodwN7eWqQ5N4qAxq+cV8="];

/// Hosted roots that no longer sign a key set (`retired`): a set signed
/// only by one of them is refused as `retired_root`.
pub const RETIRED_ROOT_KEYS: &[&str] = &[];

/// Key id (`hex(blake3(pk))[..16]`) of the hosted root while the offline
/// rotation V15.O9 is open (`pre_rotation`); `None` once it is done.
pub const PRE_ROTATION_ROOT_KEY_ID: Option<&str> = Some("b5406e1db468082e");

/// The compiled keys of `list`, decoded (a malformed entry fails the
/// vector test, never a connect).
pub fn compiled_root_keys(list: &[&str]) -> Vec<[u8; 32]> {
    list.iter()
        .filter_map(|k| decode_key(k, "root_key").ok())
        .collect()
}

/// The Hive's origin and root key of a `[hive]` section (the section type
/// lives in the engine; these rules are the connector's).
pub trait HiveConfigExt {
    /// The configured origin, normalized and checked.
    fn origin(&self) -> Result<String, HiveError>;

    /// The root key that signs this Hive's key set.
    fn root_key(&self, origin: &str) -> Result<[u8; 32], HiveError>;
}

impl HiveConfigExt for HiveConfig {
    /// The managed `hive.url` already overlays `url` on every config load
    /// (`lean_ctx::core::config::managed`); a refused managed carrier refuses here.
    fn origin(&self) -> Result<String, HiveError> {
        managed_carrier_ok()?;
        normalize_origin(self.url.as_deref().unwrap_or(HIVE_DEFAULT_URL))
    }

    fn root_key(&self, origin: &str) -> Result<[u8; 32], HiveError> {
        managed_carrier_ok()?;
        let encoded = match (&self.root_key, origin == HIVE_DEFAULT_URL) {
            (Some(key), _) => key.as_str(),
            (None, true) => HOSTED_ROOT_KEYS.first().copied().ok_or_else(|| {
                HiveError::Config(
                    "this build has no hosted Hive root key; set `[hive] root_key`".into(),
                )
            })?,
            (None, false) => {
                return Err(HiveError::Config(format!(
                    "set `[hive] root_key` to the root public key of {origin}"
                )));
            }
        };
        decode_key(encoded.trim(), "root_key")
    }
}

/// A managed carrier that exists but was refused stops every Hive call.
fn managed_carrier_ok() -> Result<(), HiveError> {
    match lean_ctx::core::config::managed::current().as_deref() {
        Some(managed) => match &managed.config {
            Ok(_) => Ok(()),
            Err(error) => Err(HiveError::Config(format!(
                "the managed configuration ({}) is refused: {error}",
                managed.location
            ))),
        },
        None => Ok(()),
    }
}

/// Why this machine's managed configuration forbids an enrollment with
/// `org_id` at `origin` (`hive.org`, `hive.url`), if it does.
pub fn managed_enrollment_refusal(org_id: &str, origin: &str) -> Option<HiveError> {
    let managed = lean_ctx::core::config::managed::current()?;
    enrollment_refusal(&managed, org_id, origin)
}

pub(crate) fn enrollment_refusal(
    managed: &lean_ctx::core::config::managed::Managed,
    org_id: &str,
    origin: &str,
) -> Option<HiveError> {
    if let Some(refusal) = managed.org_refusal(org_id) {
        return Some(HiveError::Config(refusal));
    }
    let url = managed.config.as_ref().ok()?.hive_url.as_deref()?;
    match normalize_origin(url) {
        Ok(managed_origin) if managed_origin == origin => None,
        Ok(managed_origin) => Some(HiveError::Config(format!(
            "{}: it connects only to {managed_origin}, not {origin}. Run `lean-ctx hive connect` again",
            managed.manager()
        ))),
        Err(error) => Some(error),
    }
}

/// Decode a base64 Ed25519 public key.
pub(crate) fn decode_key(encoded: &str, what: &str) -> Result<[u8; 32], HiveError> {
    B64.decode(encoded)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| HiveError::Config(format!("{what} must be base64 of 32 bytes")))
}

/// `https://host[:port]` (or `http://localhost|127.0.0.1[:port]`), no path,
/// query, fragment or credentials, no trailing slash.
pub fn normalize_origin(url: &str) -> Result<String, HiveError> {
    let url = url.trim().trim_end_matches('/');
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| HiveError::Config(format!("{url:?} is not a URL")))?;
    if rest.is_empty()
        || rest.contains(['/', '?', '#', '@'])
        || rest.chars().any(char::is_whitespace)
    {
        return Err(HiveError::Config(
            "the Hive URL is an origin only: scheme, host and optional port".into(),
        ));
    }
    let host = rest.rsplit_once(':').map_or(rest, |(host, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) {
            host
        } else {
            rest
        }
    });
    match scheme.to_ascii_lowercase().as_str() {
        "https" => Ok(format!("https://{rest}")),
        "http" if matches!(host, "localhost" | "127.0.0.1" | "[::1]") => {
            Ok(format!("http://{rest}"))
        }
        _ => Err(HiveError::Config(
            "Hive is reached over https (http only on localhost)".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_https_or_loopback_http() {
        assert_eq!(
            normalize_origin("https://hive.example.com/").unwrap(),
            "https://hive.example.com"
        );
        assert_eq!(
            normalize_origin("http://127.0.0.1:8100").unwrap(),
            "http://127.0.0.1:8100"
        );
        assert!(normalize_origin("http://hive.example.com").is_err());
        assert!(normalize_origin("http://localhost.evil.com").is_err());
        assert!(normalize_origin("https://user:pw@hive.example.com").is_err());
        assert!(normalize_origin("https://hive.example.com/v1").is_err());
        assert!(normalize_origin("ftp://hive.example.com").is_err());
    }

    #[test]
    fn a_managed_machine_refuses_a_foreign_org_or_hive() {
        use lean_ctx::core::config::managed::Managed;
        use lean_ctx_protocol::hive::managed::{ManagedCarrierKindV1, ManagedConfigV1};
        let managed = |members: serde_json::Value| Managed {
            carrier: ManagedCarrierKindV1::LinuxFile,
            location: "/etc/lean-ctx/managed.toml".into(),
            config: ManagedConfigV1::from_members(members.as_object().expect("object"))
                .map_err(|error| error.to_string()),
        };
        let refused = |managed: &Managed, org: &str, origin: &str| {
            enrollment_refusal(managed, org, origin).map(|error| error.to_string())
        };
        let acme = managed(serde_json::json!({
            "hive": {"org": "org_acme", "url": "https://hive.acme.example"}
        }));
        assert!(refused(&acme, "org_acme", "https://hive.acme.example").is_none());
        let org = refused(&acme, "org_other", "https://hive.acme.example").expect("other org");
        assert!(org.contains("managed by org_acme"), "{org}");
        let hive = refused(&acme, "org_acme", "https://hive.other.example").expect("other Hive");
        assert!(
            hive.contains("connects only to https://hive.acme.example"),
            "{hive}"
        );

        // Only the org is managed: any Hive of that org.
        let org_only = managed(serde_json::json!({"hive": {"org": "org_acme"}}));
        assert!(refused(&org_only, "org_acme", "https://hive.other.example").is_none());

        // A refused carrier refuses every enrollment.
        let broken = Managed {
            config: Err("hive.require: not a boolean".into()),
            ..org_only
        };
        let error = refused(&broken, "org_acme", "https://hive.acme.example").expect("refused");
        assert!(error.contains("is refused"), "{error}");
    }

    #[test]
    fn self_hosted_hive_needs_its_root_key() {
        let config = HiveConfig {
            url: Some("https://hive.acme.internal".into()),
            ..HiveConfig::default()
        };
        let origin = config.origin().unwrap();
        assert!(config.root_key(&origin).is_err());
        let config = HiveConfig {
            root_key: Some(B64.encode([7u8; 32])),
            ..config
        };
        assert_eq!(config.root_key(&origin).unwrap(), [7u8; 32]);
    }

    #[test]
    fn compiled_roots_match_the_contract_vectors() {
        let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/root_keys.vectors.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let listed = |name: &str| -> Vec<(String, String)> {
            v[name]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    let (id, pk) = (&e["key_id"], &e["public_key"]);
                    (id.as_str().unwrap().into(), pk.as_str().unwrap().into())
                })
                .collect()
        };
        let compiled = |list: &[&str]| -> Vec<(String, String)> {
            let keys = compiled_root_keys(list);
            assert_eq!(keys.len(), list.len(), "every compiled root decodes");
            keys.iter()
                .map(|k| {
                    let id = blake3::hash(k).to_hex()[..16].to_string();
                    (id, B64.encode(k))
                })
                .collect()
        };
        assert!(!HOSTED_ROOT_KEYS.is_empty(), "3.12.0 refuses an empty list");
        assert_eq!(compiled(HOSTED_ROOT_KEYS), listed("hosted"));
        assert_eq!(compiled(RETIRED_ROOT_KEYS), listed("retired"));
        assert_eq!(PRE_ROTATION_ROOT_KEY_ID, v["pre_rotation"].as_str());
        let origin = HiveConfig::default().origin().unwrap();
        assert_eq!(
            HiveConfig::default().root_key(&origin).unwrap(),
            compiled_root_keys(HOSTED_ROOT_KEYS)[0]
        );
    }

    #[test]
    fn defaults_match_the_documented_section() {
        let parsed: HiveConfig = toml::from_str("").unwrap();
        assert_eq!(parsed, HiveConfig::default());
        assert_eq!(parsed.pull_interval_minutes, 5);
        assert!(parsed.auto_capture);
        assert_eq!(parsed.bus, HiveBusMode::Auto);
    }

    #[test]
    fn the_pull_interval_is_clamped_to_its_bounds() {
        let at = |minutes| {
            HiveConfig {
                pull_interval_minutes: minutes,
                ..HiveConfig::default()
            }
            .pull_interval()
        };
        assert_eq!(at(0), 1);
        assert_eq!(at(15), 15);
        assert_eq!(at(600), 60);
    }
}
