// SPDX-License-Identifier: Apache-2.0
//! The subscription models through the Hive models gateway (mz6,
//! `docs/model-access-subscription.md` §4): the connector's
//! [`HiveTokenSource`] for the proxy's built-in `leanctx` provider, and the
//! catalog listing of `lean-ctx models`.
//!
//! Order of every admission, each step before the next one may run:
//! Hive on, enrolled, the models origin on the Hive host (a foreign
//! `models_url` is refused before any token is read or minted), lease valid
//! or in grace and granting `models.subscription`, no hold of the models
//! path ([`throttle::Scope::Models`]), then the instance token.

use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::HiveError;
use super::auth::Session;
use super::config::HiveConfig;
use super::state::HiveStore;
use super::throttle::{self, Scope};
use lean_ctx::core::hive_models::{
    HiveTokenSource, ModelsAdmission, ModelsRefusal, models_origin, set_hive_token_source,
};
use lean_ctx_protocol::hive::lease::{HiveRight, LeaseState};

/// The models origin of the instance in `store`, or why there is none.
/// Reads only the instance record and the config, never a token.
pub fn origin_for(store: &HiveStore, config: &HiveConfig) -> Result<String, ModelsRefusal> {
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
fn held(store: &HiveStore, throttle: &throttle::Throttle, now: DateTime<Utc>) -> ModelsRefusal {
    let retry_after_s = DateTime::parse_from_rfc3339(&throttle.until)
        .ok()
        .and_then(|until| u64::try_from((until.with_timezone(&Utc) - now).num_seconds()).ok())
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
            "waiting for Hive until {}: {}",
            throttle.until, throttle.line
        ),
    }
}

/// Everything before the token: config, origin pin, lease right, holds.
/// `Ok` names the origin, whether the call is a probe, and the session.
fn admit_without_token(
    store: &HiveStore,
    config: &HiveConfig,
    now: DateTime<Utc>,
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
    ) || !session.lease.lease.grants(HiveRight::ModelsSubscription)
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
    store: &HiveStore,
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
    now: DateTime<Utc>,
) {
    if !matches!(status, 402 | 403 | 409 | 429) {
        return;
    }
    let mut value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    super::transport::honour_retry_after(status, retry_after, &mut value);
    let error = HiveError::from_response(status, &value);
    let _ = throttle::note_on(store, Some(Scope::Models), error, now);
}

struct HiveModelsSource {
    /// One token mint at a time: concurrent requests share the cache.
    mint: Mutex<()>,
}

impl HiveModelsSource {
    fn store_and_config() -> Option<(HiveStore, HiveConfig)> {
        let store = HiveStore::open().ok()?;
        let config = lean_ctx::core::config::Config::load().hive.clone();
        Some((store, config))
    }
}

impl HiveTokenSource for HiveModelsSource {
    fn admit_route(&self, fresh_token: bool) -> Result<ModelsAdmission, ModelsRefusal> {
        let (store, config) = Self::store_and_config().ok_or(ModelsRefusal::Unavailable)?;
        let (origin, probe, session) = admit_without_token(&store, &config, Utc::now())?;
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
                tracing::warn!("hive models: no instance token: {error}");
                Err(ModelsRefusal::TokenUnavailable)
            }
        }
    }

    fn note_answer(&self, status: u16, retry_after: Option<&str>, body: &[u8]) {
        if let Ok(store) = HiveStore::open() {
            note_answer_at(&store, status, retry_after, body, Utc::now());
        }
    }

    fn note_passed(&self) {
        if let Ok(store) = HiveStore::open() {
            throttle::passed(&store, Scope::Models);
        }
    }
}

/// Register the connector as this process's subscription-model token
/// source (once).
pub fn ensure_registered() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        set_hive_token_source(Arc::new(HiveModelsSource {
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

/// The rows of a catalog document (`leanctx.hive.model-catalog/v1`).
/// Display only: nothing is served or refused on these rows.
pub fn catalog_rows(document: &Value) -> Result<Vec<CatalogRow>, HiveError> {
    if document["schema"].as_str() != Some("leanctx.hive.model-catalog/v1") {
        return Err(HiveError::Protocol(
            "model catalog: unknown or missing schema".into(),
        ));
    }
    let models = document["models"]
        .as_array()
        .ok_or_else(|| HiveError::Protocol("model catalog: no models".into()))?;
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

/// `GET /v1/models/catalog` with this instance's credentials.
pub fn fetch_catalog() -> Result<Vec<CatalogRow>, HiveError> {
    let store = HiveStore::open()?;
    let config = lean_ctx::core::config::Config::load().hive.clone();
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = super::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = Session::load(store, root)?;
    let document = session.with_client(|client| client.get("/models/catalog"))?;
    catalog_rows(&document)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_enrolled_at(url: &str) -> (tempfile::TempDir, HiveStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
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
        let (_dir, store) = store_enrolled_at("https://hive.example.com");
        let config = HiveConfig {
            models_url: Some("https://models.evil.example".into()),
            ..HiveConfig::default()
        };
        let refused = admit_without_token(&store, &config, Utc::now())
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
        let refused = admit_without_token(&store, &HiveConfig::default(), Utc::now())
            .err()
            .expect("refused");
        assert_eq!(refused, ModelsRefusal::Unavailable);
    }

    /// The gateway's hard cap (429 `quota_exceeded` for the month's model
    /// credit) holds the models path until Retry-After, and only that path.
    #[test]
    fn a_credit_cap_holds_the_models_path_until_retry_after() {
        let (_dir, store) = store_enrolled_at("https://hive.example.com");
        let now = DateTime::parse_from_rfc3339("2026-11-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
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
        let later = now + chrono::Duration::seconds(121);
        assert!(
            throttle::check(&store, Scope::Models, later).is_ok(),
            "ends at Retry-After"
        );
    }
}
