// SPDX-License-Identifier: Apache-2.0
//! The subscription models through the Cloud models gateway (mz6,
//! `docs/model-access-subscription.md` §4): the connector's
//! [`CloudTokenSource`] for the proxy's built-in `leanctx` provider, and the
//! catalog of `lean-ctx models` and of the Local Read kind
//! `cloud.models.catalog` ([`read_catalog`]).
//!
//! Order of every admission, each step before the next one may run:
//! Cloud on, enrolled, the models origin on the Cloud host (a foreign
//! `models_url` is refused before any token is read or minted), lease valid
//! or in grace and granting `models.subscription`, no hold of the models
//! path ([`throttle::Scope::Models`]), then the instance token.

use std::sync::{Arc, Mutex, OnceLock};

use lean_ctx_time::UtcDateTime;
use serde_json::Value;

use super::CloudError;
use super::auth::Session;
use super::config::CloudConfig;
use super::keyset_v3::{KeySetV3, Purpose};
use super::state::CloudStore;
use super::throttle::{self, Scope};
use lean_ctx::core::cloud_models::{
    CloudTokenSource, ModelsAdmission, ModelsRefusal, models_origin, set_cloud_token_source,
};
use lean_ctx_protocol::cloud::lease::{CloudRight, LeaseState};

/// The models origin of the instance in `store`, or why there is none.
/// Reads only the instance record and the config, never a token.
pub fn origin_for(store: &CloudStore, config: &CloudConfig) -> Result<String, ModelsRefusal> {
    if config.enabled == Some(false) {
        return Err(ModelsRefusal::Unavailable);
    }
    let instance = store
        .instance()
        .ok()
        .flatten()
        .ok_or(ModelsRefusal::Unavailable)?;
    models_origin(&instance.url, config.models_url.as_deref()).map_err(ModelsRefusal::OriginRefused)
}

/// The local answer while `throttle` holds the models path: the status and
/// code the gateway gave (quota 429, plan 402, else policy 403).
fn held(store: &CloudStore, throttle: &throttle::Throttle, now: UtcDateTime) -> ModelsRefusal {
    let retry_after_s = lean_ctx_time::OffsetDateTime::parse_from_rfc3339(&throttle.until)
        .ok()
        .and_then(|until| u64::try_from((until.to_utc() - now).num_seconds()).ok())
        .unwrap_or(1)
        .max(1);
    let (status, code) = match throttle::models_hold_kind(store, now) {
        throttle::HoldKind::Quota => (429, "quota_exceeded"),
        throttle::HoldKind::Plan => (402, "plan_required"),
        throttle::HoldKind::Policy => (403, "policy_denied"),
    };
    ModelsRefusal::Held {
        status,
        code: code.into(),
        retry_after_s,
        line: format!(
            "waiting for LeanCTX Cloud until {}: {}",
            throttle.until, throttle.line
        ),
    }
}

/// Everything before the token: config, origin pin, lease right, holds.
/// `Ok` names the origin, whether the call is a probe, and the session.
fn admit_without_token(
    store: &CloudStore,
    config: &CloudConfig,
    now: UtcDateTime,
) -> Result<(String, bool, Session), ModelsRefusal> {
    let origin = origin_for(store, config)?;
    let instance = store
        .instance()
        .ok()
        .flatten()
        .ok_or(ModelsRefusal::Unavailable)?;
    let root = super::root_cert::trust_root(store, config, &instance.url)
        .map_err(|_| ModelsRefusal::Unavailable)?;
    let session = Session::load(store.clone(), root).map_err(|_| ModelsRefusal::Unavailable)?;
    if !matches!(
        session.lease.state_at(now),
        LeaseState::Valid | LeaseState::Grace
    ) || !session.lease.lease.grants(CloudRight::ModelsSubscription)
    {
        return Err(ModelsRefusal::Unavailable);
    }
    let holding = throttle::holding(store, Scope::Models, now);
    if throttle::check(store, Scope::Models, now).is_err()
        && let Some(throttle) = &holding
    {
        return Err(held(store, throttle, now));
    }
    // Held but let through: the hourly probe of a quota throttle.
    Ok((origin, holding.is_some(), session))
}

/// Record what a gateway refusal says about the models path at `now`.
pub fn note_answer_at(
    store: &CloudStore,
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
    now: UtcDateTime,
) {
    if !matches!(status, 402 | 403 | 409 | 429) {
        return;
    }
    let mut value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    // A 429 whose body is no JSON object (an edge page) becomes
    // `{"error":{"retry_after_s":N}}`: it decodes to `RateLimited`, which
    // sets no throttle here ([`throttle::of_error`]), exactly as the bare
    // body did. Only a decoded `quota_exceeded` holds the models path.
    super::transport::honour_retry_after(status, retry_after, &mut value);
    let error = CloudError::from_response(status, &value);
    let _ = throttle::note_on(store, Some(Scope::Models), error, now);
}

struct CloudModelsSource {
    /// One token mint at a time: concurrent requests share the cache.
    mint: Mutex<()>,
}

impl CloudModelsSource {
    fn store_and_config() -> Option<(CloudStore, CloudConfig)> {
        let store = CloudStore::open().ok()?;
        let config = lean_ctx::core::config::Config::load().cloud.clone();
        Some((store, config))
    }
}

impl CloudTokenSource for CloudModelsSource {
    fn admit_route(&self, fresh_token: bool) -> Result<ModelsAdmission, ModelsRefusal> {
        let (store, config) = Self::store_and_config().ok_or(ModelsRefusal::Unavailable)?;
        let (origin, probe, session) = admit_without_token(&store, &config, UtcDateTime::now())?;
        let token = {
            let _one = self
                .mint
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            session.bearer_token(fresh_token)
        };
        match token {
            Ok(token) => Ok(ModelsAdmission {
                origin,
                token,
                probe,
            }),
            Err(error) => {
                tracing::warn!("cloud models: no instance token: {error}");
                Err(ModelsRefusal::TokenUnavailable)
            }
        }
    }

    fn note_answer(&self, status: u16, retry_after: Option<&str>, body: &[u8]) {
        if let Ok(store) = CloudStore::open() {
            note_answer_at(&store, status, retry_after, body, UtcDateTime::now());
        }
    }

    fn note_passed(&self) {
        if let Ok(store) = CloudStore::open() {
            throttle::passed(&store, Scope::Models);
        }
    }
}

/// Register the connector as this process's subscription-model token
/// source (once).
pub fn ensure_registered() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        set_cloud_token_source(Arc::new(CloudModelsSource {
            mint: Mutex::new(()),
        }));
    });
}

/// One row of `lean-ctx models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRow {
    pub id: String,
    pub display_name: String,
    pub vendor: String,
    pub origin_country: String,
    pub status: String,
    /// `provider country[/region]`, in fallback order.
    pub hosts: Vec<String>,
}

/// Catalog text for the terminal: no control characters, at most 80 chars.
fn terminal_safe(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(80)
        .collect()
}

fn text(v: &Value, key: &str) -> String {
    terminal_safe(v[key].as_str().unwrap_or("?"))
}

/// The rows of a catalog document (`leanctx.cloud.model-catalog/v1`).
/// Display only: nothing is served or refused on these rows.
pub fn catalog_rows(document: &Value) -> Result<Vec<CatalogRow>, CloudError> {
    if document["schema"].as_str() != Some("leanctx.cloud.model-catalog/v1") {
        return Err(CloudError::Protocol(
            "model catalog: unknown or missing schema".into(),
        ));
    }
    let models = document["models"]
        .as_array()
        .ok_or_else(|| CloudError::Protocol("model catalog: no models".into()))?;
    Ok(models
        .iter()
        .map(|m| CatalogRow {
            id: text(m, "id"),
            display_name: text(m, "display_name"),
            vendor: text(m, "vendor"),
            origin_country: text(m, "origin_country"),
            status: text(m, "status"),
            hosts: m["hosts"]
                .as_array()
                .map(|hosts| {
                    hosts
                        .iter()
                        .map(|h| {
                            let region = h["host_region"]
                                .as_str()
                                .map(|r| format!("/{}", terminal_safe(r)))
                                .unwrap_or_default();
                            format!(
                                "{} {}{region}",
                                text(h, "provider"),
                                text(h, "host_country")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// The connected instance's session.
fn connected_session() -> Result<Session, CloudError> {
    let store = CloudStore::open()?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let root = super::root_cert::trust_root(&store, &config, &instance.url)?;
    Session::load(store, root)
}

/// `GET /v1/models/catalog` with this instance's credentials.
pub fn fetch_catalog() -> Result<Vec<CatalogRow>, CloudError> {
    let session = connected_session()?;
    let document = session.with_client(|client| client.get("/models/catalog"))?;
    catalog_rows(&document)
}

/// Signing domain of the catalog Cloud serves (Cloud `cloud_wire::models`).
pub const CATALOG_DOMAIN: &str = "leanctx.cloud.model-catalog.v1\n";

/// Path of the LeanCTX gateway below the Cloud origin (Cloud
/// `routes::gateway::GATEWAY_BASE_PATH`): an OpenAI-compatible base URL.
pub const GATEWAY_BASE_PATH: &str = "/gateway/v1";

/// Whether `document` is signed by a key set v3 `policy` key valid at
/// `received_at` (ms): Ed25519 (`verify_strict`) over [`CATALOG_DOMAIN`] ‖
/// canonical JSON of the document without `signature` (128 hex), with the
/// key named by `key_id`, as the effective policy is verified.
///
/// `None` when the document carries no `signature` (nothing to verify);
/// `Some(false)` when it does but no stored key verifies it.
pub fn catalog_verified(
    document: &Value,
    keyset: Option<&KeySetV3>,
    received_at: u64,
) -> Option<bool> {
    let signature = document.get("signature")?;
    let verifies = || -> Option<bool> {
        let mut bytes = [0u8; 64];
        hex::decode_to_slice(signature.as_str()?, &mut bytes).ok()?;
        let key = keyset?
            .key_for(document["key_id"].as_str()?, Purpose::Policy, received_at)
            .ok()?;
        let input =
            lean_ctx_protocol::cloud::signing_input_without_signature(CATALOG_DOMAIN, document)
                .ok()?;
        Some(
            key.verify_strict(&input, &ed25519_dalek::Signature::from_bytes(&bytes))
                .is_ok(),
        )
    };
    Some(verifies() == Some(true))
}

/// The catalog as Cloud served it, with what the engine knows beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogRead {
    /// `GET /v1/models/catalog`, unchanged.
    pub document: Value,
    /// [`catalog_verified`].
    pub verified: Option<bool>,
    /// The lease is valid or in grace and grants `models.subscription`.
    pub models_subscription: bool,
    /// The LeanCTX gateway of the enrolled Cloud (`<cloud>/gateway/v1`).
    pub gateway_base_url: String,
}

/// The gateway base URL of a Cloud enrolled at `cloud_url`.
pub fn gateway_base_url(cloud_url: &str) -> String {
    format!(
        "{}{GATEWAY_BASE_PATH}",
        cloud_url.trim().trim_end_matches('/')
    )
}

/// How long a catalog `key_id` that the fetched key set still lacked is not
/// fetched again.
const UNKNOWN_KEY_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Bound on the remembered unknown keys; the oldest entries are dropped
/// first when it is reached.
const UNKNOWN_KEY_CAP: usize = 32;

/// Negative cache of catalog signing keys the key set did not hold, keyed by
/// Cloud URL and `key_id`.
#[derive(Debug, Default)]
struct UnknownKeys(std::collections::HashMap<String, std::time::Instant>);

impl UnknownKeys {
    fn recently_missed(&self, key: &str, now: std::time::Instant) -> bool {
        self.0
            .get(key)
            .is_some_and(|at| now.saturating_duration_since(*at) < UNKNOWN_KEY_TTL)
    }

    fn remember(&mut self, key: String, now: std::time::Instant) {
        self.0
            .retain(|_, at| now.saturating_duration_since(*at) < UNKNOWN_KEY_TTL);
        while self.0.len() >= UNKNOWN_KEY_CAP {
            let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.0.remove(&oldest);
        }
        self.0.insert(key, now);
    }
}

fn lock_unknown_keys() -> std::sync::MutexGuard<'static, UnknownKeys> {
    static UNKNOWN: std::sync::OnceLock<std::sync::Mutex<UnknownKeys>> = std::sync::OnceLock::new();
    UNKNOWN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `GET /v1/models/catalog` as the connected instance, verified against
/// the stored key set v3 (fetched once when it does not know the key).
pub fn read_catalog() -> Result<CatalogRead, CloudError> {
    let session = connected_session()?;
    let document = session.with_client(|client| client.get("/models/catalog"))?;
    if !document.is_object() {
        return Err(CloudError::Protocol(
            "model catalog: not a JSON object".into(),
        ));
    }
    let now = UtcDateTime::now();
    let verified = if document.get("signature").is_some() {
        // A key the stored set does not know yet (a rotation): fetch once.
        // A key the fetched set still lacks (or a failed fetch) is not
        // fetched again for UNKNOWN_KEY_TTL, so repeated reads of a catalog
        // signed with an unknown key do not hit `GET /keys` every time.
        let key_id = document["key_id"].as_str().unwrap_or_default();
        let knows = |set: &KeySetV3| set.keys.iter().any(|k| k.key_id == key_id);
        let stored = super::keyset_v3::load(&session.store, session.root_key());
        let keyset = match stored {
            Some(set) if knows(&set) => Some(set),
            stored => {
                let miss_key = format!("{}\n{key_id}", session.instance.url);
                let at = std::time::Instant::now();
                if lock_unknown_keys().recently_missed(&miss_key, at) {
                    stored
                } else if let Ok(set) = super::keyset_v3::refresh(&session) {
                    if !knows(&set) {
                        lock_unknown_keys().remember(miss_key, at);
                    }
                    Some(set)
                } else {
                    lock_unknown_keys().remember(miss_key, at);
                    stored
                }
            }
        };
        let received_at = u64::try_from(now.timestamp_millis()).unwrap_or(0);
        catalog_verified(&document, keyset.as_ref(), received_at)
    } else {
        None
    };
    let models_subscription = matches!(
        session.lease.state_at(now),
        LeaseState::Valid | LeaseState::Grace
    ) && session.lease.lease.grants(CloudRight::ModelsSubscription);
    Ok(CatalogRead {
        document,
        verified,
        models_subscription,
        gateway_base_url: gateway_base_url(&session.instance.url),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_enrolled_at(url: &str) -> (tempfile::TempDir, CloudStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        store
            .write_config(
                super::super::state::INSTANCE_FILE,
                &serde_json::json!({
                    "instance_id": "ins_test",
                    "org_id": "org_test",
                    "org_name": "Test",
                    "label": "test",
                    "url": url,
                    "org_salt_id": "salt_test",
                    "enrolled_at": "2026-11-10T00:00:00Z",
                }),
            )
            .unwrap();
        (dir, store)
    }

    /// A `models_url` on a foreign host is refused before the session, the
    /// lease or the token are touched: this store has no lease, no keys and
    /// no token, and the answer is still the origin refusal.
    #[test]
    fn a_foreign_models_url_is_refused_before_any_token() {
        let (_dir, store) = store_enrolled_at("https://cloud.example.com");
        let config = CloudConfig {
            models_url: Some("https://models.evil.example".into()),
            ..CloudConfig::default()
        };
        let refused = admit_without_token(&store, &config, UtcDateTime::now())
            .err()
            .expect("refused");
        assert!(
            matches!(refused, ModelsRefusal::OriginRefused(ref why) if why.contains("evil.example")),
            "{refused:?}"
        );
        assert!(
            store
                .read_config::<Value>(super::super::state::TOKEN_FILE)
                .unwrap()
                .is_none(),
            "no token was minted or cached"
        );
        // The default origin passes the pin and then needs the lease.
        let refused = admit_without_token(&store, &CloudConfig::default(), UtcDateTime::now())
            .err()
            .expect("refused");
        assert_eq!(refused, ModelsRefusal::Unavailable);
    }

    /// A 429 edge page (no JSON object) with Retry-After is a rate limit,
    /// not a quota: the models path is not held, as before the header was read.
    #[test]
    fn a_rate_limit_page_without_json_holds_nothing() {
        let (_dir, store) = store_enrolled_at("https://cloud.example.com");
        let now = UtcDateTime::now();
        for body in [&b"<html>Too Many Requests</html>"[..], b"[]", b"\"busy\""] {
            note_answer_at(&store, 429, Some("120"), body, now);
            assert!(throttle::holding(&store, Scope::Models, now).is_none());
            assert!(throttle::check(&store, Scope::Models, now).is_ok());
        }
    }

    /// The gateway's hard cap (429 `quota_exceeded` for the month's model
    /// credit) holds the models path until Retry-After, and only that path.
    #[test]
    fn a_credit_cap_holds_the_models_path_until_retry_after() {
        let (_dir, store) = store_enrolled_at("https://cloud.example.com");
        let now = lean_ctx_time::OffsetDateTime::parse_from_rfc3339("2026-11-10T12:00:00Z")
            .unwrap()
            .to_utc();
        let body = serde_json::json!({"error": {
            "code": "quota_exceeded",
            "message": "the month's model credit is used up",
            "resource": "model_credit_usd_micros",
            "limit": 20_000_000u64,
            "used": 20_000_000u64,
        }});
        note_answer_at(&store, 429, Some("120"), body.to_string().as_bytes(), now);
        let throttle = throttle::holding(&store, Scope::Models, now).expect("models held");
        assert_eq!(throttle.until, "2026-11-10T12:02:00Z", "Retry-After 120");
        assert_eq!(throttle.scopes, ["models"]);
        assert!(throttle::holding(&store, Scope::Sync, now).is_none());
        match held(&store, &throttle, now) {
            ModelsRefusal::Held {
                status,
                code,
                retry_after_s,
                ..
            } => {
                assert_eq!(
                    (status, code.as_str(), retry_after_s),
                    (429, "quota_exceeded", 120)
                );
            }
            other => panic!("{other:?}"),
        }
        let later = now + lean_ctx_time::Duration::seconds(121);
        assert!(
            throttle::check(&store, Scope::Models, later).is_ok(),
            "ends at Retry-After"
        );
    }

    fn policy_signer() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
    }

    /// C0 `key_id`: `hex(blake3(public key))[..16]`.
    fn key_id_of(key: &ed25519_dalek::SigningKey) -> String {
        blake3::hash(key.verifying_key().as_bytes()).to_hex()[..16].to_string()
    }

    fn keyset_with(key: &ed25519_dalek::SigningKey, purposes: Vec<Purpose>) -> KeySetV3 {
        use base64::Engine as _;
        KeySetV3 {
            schema: super::super::keyset_v3::KEYSET_V3_SCHEMA.into(),
            keys: vec![super::super::keyset_v3::KeyV3 {
                key_id: key_id_of(key),
                public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(key.verifying_key().as_bytes()),
                purposes,
                created_at: 1_000,
                revoked_at: None,
            }],
            signatures: Vec::new(),
            signature: String::new(),
        }
    }

    /// A catalog signed as Cloud signs it (`model_catalog::signed`).
    fn signed_catalog(key: &ed25519_dalek::SigningKey) -> Value {
        use ed25519_dalek::Signer as _;
        let mut doc = serde_json::json!({
            "schema": "leanctx.cloud.model-catalog/v1",
            "catalog_version": "2026-11-20.1",
            "price_version": "2026-11-20.1",
            "models": [{
                "id": "leanctx/open-model",
                "display_name": "Open Model",
                "vendor": "Example",
                "origin_country": "FR",
                "status": "unpriced",
                "plans": ["personal", "team"],
                "hosts": [{"provider": "example", "host_country": "DE"}]
            }],
            "direct": []
        });
        doc["key_id"] = key_id_of(key).into();
        let input = lean_ctx_protocol::cloud::signing_input_without_signature(CATALOG_DOMAIN, &doc)
            .unwrap();
        doc["signature"] = hex::encode(key.sign(&input).to_bytes()).into();
        doc
    }

    #[test]
    fn a_catalog_signed_by_the_policy_key_verifies() {
        let key = policy_signer();
        let doc = signed_catalog(&key);
        let set = keyset_with(&key, vec![Purpose::Policy]);
        assert_eq!(catalog_verified(&doc, Some(&set), 2_000), Some(true));
    }

    #[test]
    fn a_changed_or_foreign_catalog_does_not_verify() {
        let key = policy_signer();
        let set = keyset_with(&key, vec![Purpose::Policy]);
        let mut changed = signed_catalog(&key);
        changed["models"][0]["status"] = "available".into();
        assert_eq!(catalog_verified(&changed, Some(&set), 2_000), Some(false));
        // signed by a key the set does not hold
        let other = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        assert_eq!(
            catalog_verified(&signed_catalog(&other), Some(&set), 2_000),
            Some(false)
        );
        let doc = signed_catalog(&key);
        // a key without the policy purpose, one created after receipt, no set
        let event_only = keyset_with(&key, vec![Purpose::Event]);
        assert_eq!(
            catalog_verified(&doc, Some(&event_only), 2_000),
            Some(false)
        );
        assert_eq!(catalog_verified(&doc, Some(&set), 500), Some(false));
        assert_eq!(catalog_verified(&doc, None, 2_000), Some(false));
        let mut bad_hex = doc.clone();
        bad_hex["signature"] = "zz".into();
        assert_eq!(catalog_verified(&bad_hex, Some(&set), 2_000), Some(false));
    }

    #[test]
    fn an_unsigned_catalog_is_neither_verified_nor_refused() {
        let mut doc = signed_catalog(&policy_signer());
        doc.as_object_mut().unwrap().remove("signature");
        let set = keyset_with(&policy_signer(), vec![Purpose::Policy]);
        assert_eq!(catalog_verified(&doc, Some(&set), 2_000), None);
    }

    #[test]
    fn an_unknown_key_is_not_fetched_again_within_the_ttl() {
        let start = std::time::Instant::now();
        let mut unknown = UnknownKeys::default();
        assert!(!unknown.recently_missed("cloud\nk1", start));
        unknown.remember("cloud\nk1".into(), start);
        assert!(unknown.recently_missed("cloud\nk1", start + UNKNOWN_KEY_TTL / 2));
        assert!(!unknown.recently_missed("cloud\nk1", start + UNKNOWN_KEY_TTL));
        assert!(!unknown.recently_missed("cloud\nk2", start));
        // bounded: the oldest entry makes room
        for i in 0..UNKNOWN_KEY_CAP + 3 {
            let at = start + std::time::Duration::from_millis(u64::try_from(i).unwrap());
            unknown.remember(format!("cloud\nkey{i}"), at);
        }
        assert_eq!(unknown.0.len(), UNKNOWN_KEY_CAP);
        assert!(!unknown.0.contains_key("cloud\nkey0"));
    }

    #[test]
    fn the_gateway_base_url_is_on_the_enrolled_cloud() {
        assert_eq!(
            gateway_base_url("https://cloud.leanctx.com/"),
            "https://cloud.leanctx.com/gateway/v1"
        );
        assert_eq!(
            gateway_base_url("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080/gateway/v1"
        );
    }
}
