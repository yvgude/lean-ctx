//! Client for the hosted ctxpkg registry (GL #406) — publish, resolve, fetch.
//!
//! Trust model: the registry is the authenticity gate, this client is the
//! integrity gate. Every download is verified locally — artifact SHA-256
//! against the package index AND the embedded ed25519 manifest signature —
//! so a compromised registry cannot hand us altered content undetected.

use sha2::{Digest, Sha256};

use super::manifest::PackageManifest;

/// Default public registry, served via ctxpkg.com (nginx → control plane).
pub const DEFAULT_REGISTRY: &str = "https://ctxpkg.com/api";

/// Resolve the registry base URL: explicit flag > `CTXPKG_REGISTRY` env >
/// the public default. Trailing slashes are trimmed for clean joins.
pub fn registry_base(flag: Option<&str>) -> String {
    flag.map(str::to_string)
        .or_else(|| std::env::var("CTXPKG_REGISTRY").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Resolve the registry token: explicit flag > `CTXPKG_TOKEN` env. Used for
/// publish (`ctxp_…`) and for installing private packages (`ctxp_…` or the
/// read-only `ctxr_…`, GL #524).
pub fn publish_token(flag: Option<&str>) -> Option<String> {
    flag.map(str::to_string)
        .or_else(|| std::env::var("CTXPKG_TOKEN").ok())
        .filter(|s| !s.trim().is_empty())
}

/// A remote package reference: `@ns/name` or `ns/name`, optional `@version`
/// pin after the name (`acme/auth-context@1.2.0`).
#[derive(Debug, PartialEq, Eq)]
pub struct RemoteRef {
    pub namespace: String,
    pub name: String,
    pub version: Option<String>,
}

/// Parse a remote reference. Returns `None` for plain local names (no `/`).
#[must_use]
pub fn parse_remote_ref(input: &str) -> Option<RemoteRef> {
    let trimmed = input.strip_prefix('@').unwrap_or(input);
    let (ns, rest) = trimmed.split_once('/')?;
    let (name, version) = match rest.split_once('@') {
        Some((n, v)) => (n, Some(v.to_string())),
        None => (rest, None),
    };
    if ns.is_empty() || name.is_empty() {
        return None;
    }
    Some(RemoteRef {
        namespace: ns.to_string(),
        name: name.to_string(),
        version,
    })
}

/// One version entry from the package index.
#[derive(Debug)]
pub struct VersionInfo {
    pub version: String,
    pub artifact_sha256: String,
    pub yanked: bool,
}

/// `GET {base}/v1/packages/{ns}/{name}/index.json` → all versions.
/// `token` unlocks private packages; public ones need none.
pub fn fetch_versions(
    base: &str,
    ns: &str,
    name: &str,
    token: Option<&str>,
) -> Result<Vec<VersionInfo>, String> {
    let url = format!("{base}/v1/packages/{ns}/{name}/index.json");
    let body = http_get(&url, token)?;
    let doc: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("registry returned non-JSON: {e}"))?;
    let versions = doc
        .get("versions")
        .and_then(|v| v.as_array())
        .ok_or("registry index has no versions array")?;
    Ok(versions
        .iter()
        .filter_map(|v| {
            Some(VersionInfo {
                version: v.get("version")?.as_str()?.to_string(),
                artifact_sha256: v.get("artifact_sha256")?.as_str()?.to_string(),
                yanked: v
                    .get("yanked")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect())
}

/// Pick the version to install: an explicit pin (yanked allowed, warned by
/// the caller) or the newest non-yanked version.
pub fn select_version<'a>(
    versions: &'a [VersionInfo],
    pin: Option<&str>,
) -> Result<&'a VersionInfo, String> {
    match pin {
        Some(want) => versions
            .iter()
            .find(|v| v.version == want)
            .ok_or(format!("version {want} not found in the registry")),
        None => versions
            .iter()
            .find(|v| !v.yanked)
            .ok_or("no installable (non-yanked) version found".to_string()),
    }
}

/// Download an artifact and verify its SHA-256 against the index entry.
pub fn download_verified(
    base: &str,
    ns: &str,
    name: &str,
    info: &VersionInfo,
    token: Option<&str>,
) -> Result<Vec<u8>, String> {
    let url = format!("{base}/v1/packages/{ns}/{name}/{}/download", info.version);
    let bytes = http_get_bytes(&url, token)?;
    let actual = sha256_hex(&bytes);
    if actual != info.artifact_sha256 {
        return Err(format!(
            "artifact checksum mismatch — registry index says {}, downloaded bytes hash to {actual}; \
             refusing to install",
            info.artifact_sha256
        ));
    }
    Ok(bytes)
}

/// Publish receipt as returned by the registry.
#[derive(Debug)]
pub struct PublishReceipt {
    pub published: String,
    pub artifact_sha256: String,
}

/// `PUT {base}/v1/packages/{ns}/{name}/{version}` with the artifact bytes.
pub fn publish(
    base: &str,
    token: &str,
    ns: &str,
    name: &str,
    version: &str,
    bytes: &[u8],
) -> Result<PublishReceipt, String> {
    let url = format!("{base}/v1/packages/{ns}/{name}/{version}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let resp = agent
        .put(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/octet-stream")
        .send(bytes)
        .map_err(|e| format!("registry unreachable: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read registry response: {e}"))?;

    if status == 201 {
        let doc: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("registry returned non-JSON: {e}"))?;
        return Ok(PublishReceipt {
            published: doc
                .get("published")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)")
                .to_string(),
            artifact_sha256: doc
                .get("artifact_sha256")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    // Error bodies are JSON {"error": …} or plain text — surface either.
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or(body);
    Err(format!(
        "registry rejected the publish (HTTP {status}): {detail}"
    ))
}

/// Parse + verify a local bundle before any network call: must be a valid
/// manifest with a verifying ed25519 signature, and the scoped name must
/// match the publish target. Returns `(namespace, name, version)`.
pub fn preflight_bundle(bytes: &[u8]) -> Result<(String, String, String), String> {
    #[derive(serde::Deserialize)]
    struct BundleProbe {
        manifest: PackageManifest,
    }
    let probe: BundleProbe =
        serde_json::from_slice(bytes).map_err(|e| format!("not a ctxpkg bundle: {e}"))?;
    let manifest = probe.manifest;

    let signed = super::signing::verify_signature(&manifest)?;
    if !signed {
        return Err(
            "package is unsigned — the hosted registry requires ed25519 signatures \
             (re-export with `lean-ctx pack export <name> --sign`)"
                .to_string(),
        );
    }

    let scoped = manifest.name.clone();
    let stripped = scoped.strip_prefix('@').ok_or(format!(
        "manifest.name '{scoped}' is not scoped — hosted packages need '@namespace/name'"
    ))?;
    let (ns, name) = stripped
        .split_once('/')
        .ok_or(format!("manifest.name '{scoped}' is not '@namespace/name'"))?;
    Ok((ns.to_string(), name.to_string(), manifest.version))
}

/// Private packages return 404 for outsiders — hint at the token when none
/// was sent, so `install` failures stay actionable.
fn not_found_hint(token: Option<&str>) -> &'static str {
    if token.is_some() {
        "package not found in the registry (or your token's namespace does not own it)"
    } else {
        "package not found in the registry — private packages need CTXPKG_TOKEN"
    }
}

/// Paid packs answer 402 with an actionable message in `{"error": …}`
/// (where to buy, how to install) — surface it verbatim (GL #529).
fn payment_hint(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| "this is a paid package — purchase required".to_string())
}

fn http_get(url: &str, token: Option<&str>) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent.get(url);
    if let Some(t) = token {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    let resp = req
        .call()
        .map_err(|e| format!("registry unreachable: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read registry response: {e}"))?;
    if status == 404 {
        return Err(not_found_hint(token).to_string());
    }
    if status == 402 {
        return Err(payment_hint(&body));
    }
    if status >= 400 {
        return Err(format!("registry error (HTTP {status})"));
    }
    Ok(body)
}

fn http_get_bytes(url: &str, token: Option<&str>) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent.get(url);
    if let Some(t) = token {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    let resp = req
        .call()
        .map_err(|e| format!("registry unreachable: {e}"))?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Err(not_found_hint(token).to_string());
    }
    let mut reader = resp.into_body().into_reader();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf).map_err(|e| format!("read artifact: {e}"))?;
    if status == 402 {
        return Err(payment_hint(&String::from_utf8_lossy(&buf)));
    }
    if status >= 400 {
        return Err(format!("registry error (HTTP {status})"));
    }
    Ok(buf)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    crate::core::agent_identity::hex_encode(&h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_ref_parsing() {
        assert_eq!(
            parse_remote_ref("acme/auth-context"),
            Some(RemoteRef {
                namespace: "acme".into(),
                name: "auth-context".into(),
                version: None
            })
        );
        assert_eq!(
            parse_remote_ref("@acme/auth-context@1.2.0"),
            Some(RemoteRef {
                namespace: "acme".into(),
                name: "auth-context".into(),
                version: Some("1.2.0".into())
            })
        );
        assert_eq!(parse_remote_ref("local-package"), None);
        assert_eq!(parse_remote_ref("/x"), None);
        assert_eq!(parse_remote_ref("ns/"), None);
    }

    #[test]
    fn version_selection_skips_yanked_unless_pinned() {
        let versions = vec![
            VersionInfo {
                version: "2.0.0".into(),
                artifact_sha256: "b".into(),
                yanked: true,
            },
            VersionInfo {
                version: "1.0.0".into(),
                artifact_sha256: "a".into(),
                yanked: false,
            },
        ];
        assert_eq!(
            select_version(&versions, None).expect("latest").version,
            "1.0.0"
        );
        assert_eq!(
            select_version(&versions, Some("2.0.0"))
                .expect("pinned")
                .version,
            "2.0.0"
        );
        assert!(select_version(&versions, Some("3.0.0")).is_err());
    }

    #[test]
    fn registry_base_resolution_order() {
        assert_eq!(
            registry_base(Some("https://r.example/api/")),
            "https://r.example/api"
        );
        // No flag, no env (tests run without CTXPKG_REGISTRY) → default.
        if std::env::var("CTXPKG_REGISTRY").is_err() {
            assert_eq!(registry_base(None), DEFAULT_REGISTRY);
        }
    }

    #[test]
    fn preflight_rejects_garbage_and_unscoped() {
        assert!(preflight_bundle(b"not json").is_err());
    }
}
