//! Per-person gateway keys + request identity tags (enterprise#11).
//!
//! `gateway-keys.toml` maps SHA-256 hashes of bearer keys to an identity
//! (person, optional team, optional default project), so an org gateway can
//! meter usage per person/project without the clients sharing one token:
//!
//! ```toml
//! [[keys]]
//! sha256_hex = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
//! person = "yves"
//! team = "platform"
//! default_project = "ai-gateway"
//! ```
//!
//! Only the hash is ever stored (same rule as `TeamTokenConfig` /
//! `cloud_server::auth`); the plaintext key lives with the person. The file
//! path resolves via `LEAN_CTX_GATEWAY_KEYS`, falling back to
//! `<config_dir>/gateway-keys.toml` — deployments mount it as a secret.
//!
//! A caller may override the project per request with the `x-leanctx-project`
//! header (an internal gateway header: it is deliberately not on
//! `ALLOWED_REQUEST_HEADERS`, so it never leaks upstream).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The identity tags attached to an authenticated gateway request. Inserted as
/// a request extension by the auth guard and stamped onto the usage record by
/// the forward path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayTags {
    pub person: Option<String>,
    pub team: Option<String>,
    pub project: Option<String>,
}

/// Internal trust marker set only after the proxy auth guard accepts a
/// gateway-owned credential (or explicit loopback-open mode). Provider API-key
/// fallback must not be able to manufacture managed OCLA lineage headers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TrustedGatewayRequest;

impl GatewayTags {
    /// True when there is anything worth stamping.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.person.is_none() && self.team.is_none() && self.project.is_none()
    }
}

#[derive(Debug, Deserialize)]
struct GatewayKeysFile {
    #[serde(default)]
    keys: Vec<GatewayKeyEntry>,
}

#[derive(Debug, Deserialize)]
struct GatewayKeyEntry {
    /// Lowercase hex SHA-256 of the bearer key (never the key itself).
    sha256_hex: String,
    person: String,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    default_project: Option<String>,
}

/// Loaded, lookup-ready key set. One instance per proxy process, loaded at
/// startup (key rotation = redeploy/restart, the standard secret-mount flow).
#[derive(Debug, Default)]
pub struct GatewayKeys {
    by_sha: HashMap<String, GatewayTags>,
}

impl GatewayKeys {
    /// Resolve the keys file path: `LEAN_CTX_GATEWAY_KEYS` env wins, else
    /// `<config_dir>/gateway-keys.toml` (next to `config.toml`).
    #[must_use]
    pub fn default_path() -> PathBuf {
        std::env::var("LEAN_CTX_GATEWAY_KEYS").ok().map_or_else(
            || {
                crate::core::paths::config_dir().map_or_else(
                    |_| PathBuf::from("gateway-keys.toml"),
                    |d| d.join("gateway-keys.toml"),
                )
            },
            PathBuf::from,
        )
    }

    /// Load from the default path; a missing file is an empty key set (the
    /// common local case), a malformed file is a loud startup error.
    pub fn load_default() -> anyhow::Result<Self> {
        Self::load(&Self::default_path())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        Self::parse(&raw, path)
    }

    /// Parses a key-file body without touching disk. `origin` is only used in
    /// error messages. Callers that assemble a file body by hand validate it
    /// through this BEFORE the atomic write, so an invalid assembly can never
    /// replace a good file on disk (#716).
    pub fn parse(raw: &str, origin: &Path) -> anyhow::Result<Self> {
        let file: GatewayKeysFile =
            toml::from_str(raw).map_err(|e| anyhow::anyhow!("parse {}: {e}", origin.display()))?;
        let mut by_sha = HashMap::new();
        for entry in file.keys {
            let sha = entry.sha256_hex.trim().to_ascii_lowercase();
            if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                anyhow::bail!(
                    "{}: key for '{}' has invalid sha256_hex (expected 64 hex chars)",
                    origin.display(),
                    entry.person
                );
            }
            let person = entry.person.trim();
            if person.is_empty() {
                anyhow::bail!("{}: entry with empty person", origin.display());
            }
            by_sha.insert(
                sha,
                GatewayTags {
                    person: Some(person.to_string()),
                    team: entry
                        .team
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(str::to_string),
                    project: entry
                        .default_project
                        .as_deref()
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(str::to_string),
                },
            );
        }
        Ok(Self { by_sha })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_sha.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_sha.len()
    }

    /// Authenticate a bearer key: SHA-256 it and look the hash up. Returns the
    /// identity tags on a match.
    #[must_use]
    pub fn lookup(&self, bearer_key: &str) -> Option<GatewayTags> {
        self.by_sha.get(&sha256_hex(bearer_key)).cloned()
    }
}

/// Lowercase hex SHA-256 (the storage form of every gateway key).
#[must_use]
pub fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_keys(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("gateway-keys.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn lookup_maps_key_hash_to_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let sha = sha256_hex("gk-yves-secret");
        let path = write_keys(
            tmp.path(),
            &format!(
                r#"
                [[keys]]
                sha256_hex = "{sha}"
                person = "yves"
                team = "platform"
                default_project = "ai-gateway"

                [[keys]]
                sha256_hex = "{}"
                person = "mara"
                "#,
                sha256_hex("gk-mara-secret")
            ),
        );
        let keys = GatewayKeys::load(&path).unwrap();
        assert_eq!(keys.len(), 2);

        let yves = keys.lookup("gk-yves-secret").expect("known key");
        assert_eq!(yves.person.as_deref(), Some("yves"));
        assert_eq!(yves.team.as_deref(), Some("platform"));
        assert_eq!(yves.project.as_deref(), Some("ai-gateway"));

        let mara = keys.lookup("gk-mara-secret").expect("known key");
        assert_eq!(mara.person.as_deref(), Some("mara"));
        assert_eq!(mara.team, None);
        assert_eq!(mara.project, None);

        assert!(keys.lookup("gk-unknown").is_none());
    }

    #[test]
    fn missing_file_is_empty_but_malformed_is_loud() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = GatewayKeys::load(&tmp.path().join("nope.toml")).unwrap();
        assert!(missing.is_empty());

        let bad_hash = write_keys(
            tmp.path(),
            r#"
            [[keys]]
            sha256_hex = "not-a-hash"
            person = "yves"
            "#,
        );
        assert!(
            GatewayKeys::load(&bad_hash).is_err(),
            "an invalid sha256_hex must fail loudly, not silently drop the key"
        );
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("abc") — the FIPS 180-2 test vector.
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
