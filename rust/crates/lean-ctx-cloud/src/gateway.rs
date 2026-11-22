// SPDX-License-Identifier: Apache-2.0
//! LeanCTX Cloud model gateway key (`POST /v1/gateway/key`): a short-lived
//! key with which this computer's sessions run on models from LeanCTX, paid
//! from the team's balance.
//!
//! Asked only when a local reader (LeanCTX Code) asks for it; the key is
//! handed to that reader and never written to disk, cached or logged
//! ([`GatewayKey`]'s `Debug` hides it). The reader keeps it in memory until
//! `expires_at`.
//!
//! The cloud answers (contract W1):
//! - `200 {schema, base_url, key, expires_at, models}` → [`GatewayAnswer::Ready`]
//!   (fields Cloud adds later are ignored; `schema`, when sent, must be
//!   [`KEY_SCHEMA`]);
//! - `429 quota_exceeded` (`model_credit_usd_micros`) → [`GatewayAnswer::Stopped`];
//! - `429 rate_limited` with `reason: gateway_keys` (this computer already
//!   holds the most live keys Cloud allows) → [`GatewayAnswer::TooManyKeys`];
//! - `403 forbidden` or `403 plan_required` (no plan or trial) →
//!   [`GatewayAnswer::Forbidden`];
//! - `503 not_configured`, or `404` from a cloud without the route →
//!   [`GatewayAnswer::NotConfigured`].
//!
//! Anything else is a [`CloudError`].

use std::fmt;

use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::CloudError;
use crate::config::CloudConfig;
use crate::state::CloudStore;

/// The cloud route (under `/v1`).
const PATH: &str = "/gateway/key";
/// Largest answer read; a key document is a few hundred bytes.
const MAX_ANSWER_BYTES: usize = 16 * 1024;
/// Longest key accepted.
const MAX_KEY_LEN: usize = 4096;
/// Longest base URL accepted.
const MAX_URL_LEN: usize = 2048;
/// The quota a stopped balance names.
pub const BALANCE_RESOURCE: &str = "model_credit_usd_micros";
/// Longest wait passed on (a day); a larger one is a broken answer.
const RETRY_AFTER_CAP_S: u64 = 86_400;
/// The `reason` of a `429 rate_limited` refused because this computer
/// already holds the most live keys.
pub const KEY_LIMIT_REASON: &str = "gateway_keys";
/// The `schema` of a key document.
pub const KEY_SCHEMA: &str = "leanctx.cloud.gateway_key.v1";

/// A key the gateway handed out. Cloud answers grow additively, so fields
/// not named here (`schema`, `models`, later ones) are not kept.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayKey {
    /// Where sessions send model requests (`https://…`).
    pub base_url: String,
    /// The bearer key; secret.
    pub key: String,
    /// When the key stops working (RFC 3339).
    pub expires_at: String,
}

impl fmt::Debug for GatewayKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GatewayKey")
            .field("base_url", &self.base_url)
            .field("key", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// What the gateway said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayAnswer {
    /// A key for sessions.
    Ready(GatewayKey),
    /// The balance is used up; nothing runs until it is topped up.
    Stopped {
        /// `next_step: top_up`: a top-up helps.
        top_up: bool,
        /// Seconds after which asking again may help.
        retry_after_s: Option<u64>,
    },
    /// This computer already holds the most live keys Cloud allows; a new
    /// one comes once one of them expires (reuse a held key until then).
    TooManyKeys {
        /// Seconds after which asking again may help.
        retry_after_s: Option<u64>,
    },
    /// This person may not use the team's balance.
    Forbidden,
    /// The cloud does not offer the gateway (not configured, or older).
    NotConfigured,
}

/// Ask the connected cloud for a gateway key.
pub fn request(store: &CloudStore, config: &CloudConfig) -> Result<GatewayAnswer, CloudError> {
    let session = crate::scheduler::connected_session(store, config)?;
    let (status, body) = session.with_client(|client| {
        let (status, body) = client.send_answer("POST", PATH, None, MAX_ANSWER_BYTES)?;
        // A refused token goes through the session's refresh.
        if status == 401 {
            return Err(CloudError::from_response(status, &body));
        }
        Ok((status, body))
    })?;
    classify(status, &body, UtcDateTime::now())
}

/// The answer of `status` and `body`, checked against `now`.
fn classify(status: u16, body: &Value, now: UtcDateTime) -> Result<GatewayAnswer, CloudError> {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let retry_after_s = || {
        body["error"]["retry_after_s"]
            .as_u64()
            .map(|s| s.min(RETRY_AFTER_CAP_S))
    };
    match status {
        200 | 201 => ready(body, now).map(GatewayAnswer::Ready),
        429 if code == "quota_exceeded" => Ok(GatewayAnswer::Stopped {
            top_up: body["error"]["next_step"].as_str() == Some("top_up"),
            retry_after_s: retry_after_s(),
        }),
        429 if code == "rate_limited"
            && body["error"]["reason"].as_str() == Some(KEY_LIMIT_REASON) =>
        {
            Ok(GatewayAnswer::TooManyKeys {
                retry_after_s: retry_after_s(),
            })
        }
        403 if code == "forbidden" || code == "plan_required" => Ok(GatewayAnswer::Forbidden),
        503 if code == "not_configured" => Ok(GatewayAnswer::NotConfigured),
        404 if code.is_empty() || code == "not_found" => Ok(GatewayAnswer::NotConfigured),
        _ => Err(CloudError::from_response(status, body)),
    }
}

fn ready(body: &Value, now: UtcDateTime) -> Result<GatewayKey, CloudError> {
    let bad = |why: &str| CloudError::Protocol(format!("gateway key: {why}"));
    match body.get("schema") {
        None => {}
        Some(Value::String(schema)) if schema == KEY_SCHEMA => {}
        Some(_) => return Err(bad("unknown schema")),
    }
    if body.get("models").is_some_and(|m| !m.is_array()) {
        return Err(bad("models is not a list"));
    }
    let key: GatewayKey =
        serde_json::from_value(body.clone()).map_err(|_| bad("not a key document"))?;
    if !key.base_url.starts_with("https://")
        || key.base_url.len() > MAX_URL_LEN
        || key.base_url.chars().any(char::is_whitespace)
    {
        return Err(bad("base_url is not an https URL"));
    }
    if key.key.is_empty()
        || key.key.len() > MAX_KEY_LEN
        || !key.key.chars().all(|c| c.is_ascii_graphic())
    {
        return Err(bad("key is malformed"));
    }
    let expires = UtcDateTime::parse_from_rfc3339(&key.expires_at)
        .map_err(|_| bad("expires_at is not RFC 3339"))?;
    if expires <= now {
        return Err(bad("key already expired"));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> UtcDateTime {
        UtcDateTime::parse_from_rfc3339("2026-11-18T12:00:00Z").expect("time")
    }

    fn key_doc() -> Value {
        json!({
            "base_url": "https://gateway.example/v1",
            "key": "gk_abc123",
            "expires_at": "2026-11-18T13:00:00Z"
        })
    }

    #[test]
    fn a_key_is_handed_on_and_never_shown_in_debug() {
        let answer = classify(200, &key_doc(), now()).expect("ready");
        let GatewayAnswer::Ready(key) = &answer else {
            panic!("not ready: {answer:?}");
        };
        assert_eq!(key.base_url, "https://gateway.example/v1");
        assert_eq!(key.key, "gk_abc123");
        let shown = format!("{answer:?}");
        assert!(!shown.contains("gk_abc123"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn a_used_up_balance_stops_with_the_top_up_step() {
        let body = json!({"error": {
            "code": "quota_exceeded", "resource": BALANCE_RESOURCE,
            "next_step": "top_up", "retry_after_s": 999_999_999u64
        }});
        assert_eq!(
            classify(429, &body, now()).expect("stopped"),
            GatewayAnswer::Stopped {
                top_up: true,
                retry_after_s: Some(RETRY_AFTER_CAP_S)
            }
        );
    }

    #[test]
    fn the_key_limit_is_its_own_answer_not_a_used_up_balance() {
        let body = json!({"error": {
            "code": "rate_limited", "reason": KEY_LIMIT_REASON,
            "message": "this Host already holds 32 live gateway keys; reuse one until it expires",
            "retry_after_s": 30
        }});
        assert_eq!(
            classify(429, &body, now()).expect("too many keys"),
            GatewayAnswer::TooManyKeys {
                retry_after_s: Some(30)
            }
        );
        // any wait Cloud names passes through unchanged (Cloud sends 30 today)
        for wait in [1u64, 1_800, 3_600] {
            let body = json!({"error": {
                "code": "rate_limited", "reason": KEY_LIMIT_REASON, "retry_after_s": wait
            }});
            assert_eq!(
                classify(429, &body, now()).expect("too many keys"),
                GatewayAnswer::TooManyKeys {
                    retry_after_s: Some(wait)
                }
            );
        }
        let no_wait = json!({"error": {"code": "rate_limited", "reason": KEY_LIMIT_REASON}});
        assert_eq!(
            classify(429, &no_wait, now()).expect("too many keys"),
            GatewayAnswer::TooManyKeys {
                retry_after_s: None
            }
        );
        let huge = json!({"error": {
            "code": "rate_limited", "reason": KEY_LIMIT_REASON,
            "retry_after_s": 999_999_999u64
        }});
        assert_eq!(
            classify(429, &huge, now()).expect("capped"),
            GatewayAnswer::TooManyKeys {
                retry_after_s: Some(RETRY_AFTER_CAP_S)
            }
        );
    }

    #[test]
    fn other_rate_limits_stay_errors() {
        let limited = |reason: Value| json!({"error": {"code": "rate_limited", "reason": reason, "retry_after_s": 5}});
        assert!(classify(429, &limited(Value::Null), now()).is_err());
        assert!(classify(429, &limited(json!("daily_jobs")), now()).is_err());
        assert!(classify(429, &limited(json!(KEY_LIMIT_REASON.to_uppercase())), now()).is_err());
        // the reason alone, under another code or status, is not the key limit
        let other_code = json!({"error": {"code": "busy", "reason": KEY_LIMIT_REASON}});
        assert!(classify(429, &other_code, now()).is_err());
        assert!(classify(503, &limited(json!(KEY_LIMIT_REASON)), now()).is_err());
    }

    #[test]
    fn refusals_and_missing_gateways_are_answers_not_errors() {
        let err = |code: &str| json!({"error": {"code": code}});
        assert_eq!(
            classify(403, &err("forbidden"), now()).expect("forbidden"),
            GatewayAnswer::Forbidden
        );
        // no plan or trial: the same next step for the reader
        assert_eq!(
            classify(403, &err("plan_required"), now()).expect("no plan"),
            GatewayAnswer::Forbidden
        );
        assert!(classify(403, &err("csrf"), now()).is_err());
        assert_eq!(
            classify(503, &err("not_configured"), now()).expect("off"),
            GatewayAnswer::NotConfigured
        );
        assert_eq!(
            classify(404, &Value::Null, now()).expect("older cloud"),
            GatewayAnswer::NotConfigured
        );
    }

    #[test]
    fn other_failures_stay_errors() {
        // a real outage is not "not configured"
        assert!(classify(503, &Value::Null, now()).is_err());
        assert!(classify(500, &json!({"error": {"code": "internal"}}), now()).is_err());
    }

    #[test]
    fn a_broken_or_stale_key_is_refused() {
        let with = |field: &str, value: Value| {
            let mut doc = key_doc();
            doc[field] = value;
            classify(200, &doc, now())
        };
        assert!(with("base_url", json!("http://gateway.example")).is_err());
        assert!(with("key", json!("")).is_err());
        assert!(with("key", json!("has space")).is_err());
        assert!(with("expires_at", json!("tomorrow")).is_err());
        assert!(with("expires_at", json!("2026-11-18T11:59:59Z")).is_err());
        assert!(with("schema", json!("leanctx.cloud.gateway_key.v2")).is_err());
        assert!(with("schema", json!("leanctx.cloud.gateway_key-v1")).is_err());
        assert!(with("schema", json!(1)).is_err());
        assert!(with("models", json!("gpt")).is_err());
        let mut no_key = key_doc();
        no_key.as_object_mut().expect("object").remove("key");
        assert!(classify(200, &no_key, now()).is_err());
        // a field Cloud adds later does not break the key
        assert!(with("extra", json!(1)).is_ok());
    }

    #[test]
    fn the_cloud_key_document_is_ready() {
        // the exact 200 answer of LeanCTX Cloud's `POST /v1/gateway/key`
        let body = json!({
            "schema": "leanctx.cloud.gateway_key.v1",
            "base_url": "https://cloud.leanctx.com/gateway/v1",
            "key": "cloud_g_k1",
            "expires_at": "2026-11-18T13:00:00Z",
            "models": ["claude-sonnet-4-5", "gpt-5-mini"]
        });
        assert_eq!(
            classify(200, &body, now()).expect("ready"),
            GatewayAnswer::Ready(GatewayKey {
                base_url: "https://cloud.leanctx.com/gateway/v1".into(),
                key: "cloud_g_k1".into(),
                expires_at: "2026-11-18T13:00:00Z".into(),
            })
        );
        let mut no_models = body.clone();
        no_models["models"] = json!([]);
        assert!(classify(200, &no_models, now()).is_ok());
    }
}
