// SPDX-License-Identifier: Apache-2.0

//! Device-flow enrollment and instance access tokens.
//!
//! - `POST /v1/device/code` [`DeviceCodeRequestV1`] → [`DeviceCodeV1`]
//! - `POST /v1/device/token` [`DeviceTokenRequestV1`] → [`EnrollmentV1`], or
//!   400 `authorization_pending` / `slow_down` / `expired_token` / `access_denied`,
//!   402 `plan_required`, 409 `instance_limit`
//! - `POST /v1/instances/{instance_id}/token` [`InstanceTokenRequestV1`] → [`AccessTokenV1`]
//! - `POST /v1/instances/{instance_id}/lease/renew` (empty body) → lease envelope
//! - `DELETE /v1/instances/{instance_id}` → 204, frees the seat
//!
//! There is no refresh token: the instance's Ed25519 key is its only
//! long-lived secret and mints short access tokens through a signed assertion.

use serde::{Deserialize, Serialize};
use std::fmt;

use super::lease::{HiveKeySetV1, HiveLeaseEnvelopeV1};
use super::{
    HIVE_ENGINE_CLIENT_ID, TOKEN_ASSERTION_DOMAIN, require_schema, validate_b64_len, validate_id,
    validate_text,
};
use crate::ValidationError;

pub const DEVICE_CODE_REQUEST_SCHEMA: &str = "leanctx.hive.device-code-request/v1";
pub const DEVICE_CODE_SCHEMA: &str = "leanctx.hive.device-code/v1";
pub const DEVICE_TOKEN_REQUEST_SCHEMA: &str = "leanctx.hive.device-token-request/v1";
pub const ENROLLMENT_SCHEMA: &str = "leanctx.hive.enrollment/v1";
pub const TOKEN_REQUEST_SCHEMA: &str = "leanctx.hive.token-request/v1";
pub const ACCESS_TOKEN_SCHEMA: &str = "leanctx.hive.access-token/v1";

/// Longest instance label, in characters.
pub const MAX_INSTANCE_LABEL_CHARS: usize = 64;
/// Accepted distance between `iat` and service time.
pub const MAX_ASSERTION_SKEW_SECONDS: i64 = 300;
/// Longest access-token lifetime the service issues.
pub const MAX_ACCESS_TOKEN_SECONDS: u32 = 3600;

/// Operating-system family of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineOs {
    Macos,
    Linux,
    Windows,
    Other,
}

fn validate_engine_version(value: &str) -> Result<(), ValidationError> {
    let ok = !value.is_empty()
        && value.len() <= 32
        && value.as_bytes()[0].is_ascii_digit()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    if ok {
        Ok(())
    } else {
        Err(ValidationError::new(
            "engine_version must be a semantic version of at most 32 characters",
        ))
    }
}

/// `POST /v1/device/code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCodeRequestV1 {
    pub schema: String,
    /// Always [`HIVE_ENGINE_CLIENT_ID`].
    pub client_id: String,
    pub engine_version: String,
    pub os: EngineOs,
    /// The name the user gave this engine (`--label`), shown on the approval
    /// page and prefilled as its name (Hive task #11). Like `engine_version`
    /// and `os` it is the requester's claim. Absent when the user gave none:
    /// the host-name default goes only into [`DeviceTokenRequestV1`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_label: Option<String>,
}

impl DeviceCodeRequestV1 {
    pub fn new(engine_version: impl Into<String>, os: EngineOs) -> Self {
        Self {
            schema: DEVICE_CODE_REQUEST_SCHEMA.into(),
            client_id: HIVE_ENGINE_CLIENT_ID.into(),
            engine_version: engine_version.into(),
            os,
            instance_label: None,
        }
    }

    /// The same request naming the engine (`--label`).
    #[must_use]
    pub fn with_label(mut self, label: Option<String>) -> Self {
        self.instance_label = label;
        self
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, DEVICE_CODE_REQUEST_SCHEMA)?;
        if self.client_id != HIVE_ENGINE_CLIENT_ID {
            return Err(ValidationError::new(format!(
                "client_id must be {HIVE_ENGINE_CLIENT_ID}"
            )));
        }
        if let Some(label) = &self.instance_label {
            validate_text(label, MAX_INSTANCE_LABEL_CHARS, "instance_label", false)?;
        }
        validate_engine_version(&self.engine_version)
    }
}

/// Response of `POST /v1/device/code`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCodeV1 {
    pub schema: String,
    /// Secret polling handle; never shown or logged.
    pub device_code: String,
    /// Short code the user confirms in the browser, e.g. `WDJB-MJHT`.
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    /// Minimum seconds between token polls.
    pub interval: u32,
    /// Seconds until `device_code` expires.
    pub expires_in: u32,
    /// C0 2.1: what this Hive offers beyond C0 1, e.g.
    /// [`FEATURE_DEVICE_SETUP`]; empty (absent) on older ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl DeviceCodeV1 {
    /// Whether this Hive takes `POST /v1/device/setup` for this code.
    pub fn offers_device_setup(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_DEVICE_SETUP)
    }
}

/// C0 2.1 `features` entry: the approval page shares the project too.
pub const FEATURE_DEVICE_SETUP: &str = "device_setup";
pub const DEVICE_SETUP_REQUEST_SCHEMA: &str = "leanctx.hive.device-setup/v1";
pub const DEVICE_SETUP_RESULT_SCHEMA: &str = "leanctx.hive.device-setup-result/v1";

/// C0 2.1 `POST /v1/device/setup` (no token; the pending `device_code`):
/// the project this engine runs in and what it shares, shown on the
/// approval page (`routes/device_setup.request.json`). Requester's claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSetupRequestV1 {
    pub schema: String,
    pub device_code: String,
    pub project: Option<DeviceSetupProjectV1>,
    /// `knowledge`, `usage`, `receipts`, `bus`, sorted.
    pub shares: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSetupProjectV1 {
    /// The checkout's folder name, 1-64 characters.
    pub name: String,
    /// The git remote as the engine sees it, for the person to recognise.
    pub remote: Option<String>,
}

/// C0 2.1 `POST /v1/device/setup/result` (instance token):
/// `routes/device_setup_result.request.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSetupResultV1 {
    pub schema: String,
    /// `shared`, `kept_local`, `no_project` or `failed`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
}

impl fmt::Debug for DeviceCodeV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCodeV1")
            .field("device_code", &"<redacted>")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("interval", &self.interval)
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// `http://localhost[:port]…` or `http://127.0.0.1[:port]…`.
fn is_loopback_http(uri: &str) -> bool {
    uri.strip_prefix("http://")
        .and_then(|rest| rest.split(['/', ':', '?']).next())
        .is_some_and(|host| host == "localhost" || host == "127.0.0.1")
}

impl DeviceCodeV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, DEVICE_CODE_SCHEMA)?;
        validate_text(&self.device_code, 256, "device_code", false)?;
        validate_text(&self.user_code, 16, "user_code", false)?;
        for uri in [&self.verification_uri, &self.verification_uri_complete] {
            if !(uri.starts_with("https://") || is_loopback_http(uri)) || uri.len() > 512 {
                return Err(ValidationError::new(
                    "verification uri must be https (http only on localhost/127.0.0.1)",
                ));
            }
        }
        if !(1..=60).contains(&self.interval) || !(60..=3600).contains(&self.expires_in) {
            return Err(ValidationError::new(
                "interval must be 1-60 s and expires_in 60-3600 s",
            ));
        }
        Ok(())
    }
}

/// `POST /v1/device/token`: poll until the user approved, then enroll the
/// instance with its public keys.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceTokenRequestV1 {
    pub schema: String,
    pub device_code: String,
    /// Ed25519 public key (32 bytes, base64) the instance signs assertions with.
    pub instance_public_key: String,
    /// X25519 public key (32 bytes, base64) sealed vault keys are wrapped to.
    pub instance_kx_public_key: String,
    /// User-visible name, e.g. the host name when labels are on.
    pub instance_label: String,
    pub engine_version: String,
    pub os: EngineOs,
}

impl fmt::Debug for DeviceTokenRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceTokenRequestV1")
            .field("device_code", &"<redacted>")
            .field("instance_public_key", &self.instance_public_key)
            .field("instance_label", &self.instance_label)
            .field("engine_version", &self.engine_version)
            .field("os", &self.os)
            .finish_non_exhaustive()
    }
}

impl DeviceTokenRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, DEVICE_TOKEN_REQUEST_SCHEMA)?;
        validate_text(&self.device_code, 256, "device_code", false)?;
        validate_b64_len(&self.instance_public_key, 32, "instance_public_key")?;
        validate_b64_len(&self.instance_kx_public_key, 32, "instance_kx_public_key")?;
        validate_text(
            &self.instance_label,
            MAX_INSTANCE_LABEL_CHARS,
            "instance_label",
            false,
        )?;
        validate_engine_version(&self.engine_version)
    }
}

/// Organization reference: the workspace the instance enrolls in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgRefV1 {
    pub id: String,
    pub name: String,
    /// `personal` | `team` (Hive C line, additive); older servers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The company a team belongs to (Hive C line, additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<CompanyRefV1>,
}

/// The company of a team: the business that holds its Enterprise plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanyRefV1 {
    pub name: String,
}

/// Successful `POST /v1/device/token`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollmentV1 {
    pub schema: String,
    pub instance_id: String,
    pub org: OrgRefV1,
    pub lease: HiveLeaseEnvelopeV1,
    /// Signed by the Hive root key; verified before the lease.
    pub keys: HiveKeySetV1,
    /// Id of the org salt the engine fetches with `GET /v1/org/salt`.
    pub org_salt_id: String,
    /// 1.3: origin of the Hive web app ("Open Hive"); may equal the API origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
}

impl EnrollmentV1 {
    /// Structural checks; signature checks are the engine's
    /// (`keys` against the root key, then `lease` against `keys`).
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, ENROLLMENT_SCHEMA)?;
        validate_id(&self.instance_id, "instance_id")?;
        validate_id(&self.org.id, "org.id")?;
        validate_text(&self.org.name, 128, "org.name", false)?;
        validate_id(&self.org_salt_id, "org_salt_id")?;
        if let Some(web_url) = &self.web_url {
            super::validate_origin(web_url, "web_url")?;
        }
        self.keys.validate()?;
        self.lease.validate_shape()
    }
}

/// `POST /v1/instances/{instance_id}/token`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceTokenRequestV1 {
    pub schema: String,
    /// Unix seconds; the service accepts ±[`MAX_ASSERTION_SKEW_SECONDS`].
    pub iat: i64,
    /// 16–64 chars of `[A-Za-z0-9_-]`, single use within the skew window.
    pub nonce: String,
    /// Ed25519 signature (base64) over [`token_assertion_input`].
    pub assertion: String,
}

impl InstanceTokenRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TOKEN_REQUEST_SCHEMA)?;
        if self.iat <= 0 {
            return Err(ValidationError::new("iat must be positive unix seconds"));
        }
        validate_nonce(&self.nonce)?;
        validate_b64_len(&self.assertion, 64, "assertion")
    }
}

fn validate_nonce(nonce: &str) -> Result<(), ValidationError> {
    if (16..=64).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(ValidationError::new(
            "nonce must be 16-64 characters of [A-Za-z0-9_-]",
        ))
    }
}

/// Bytes the instance key signs: `"leanctx.hive.token.v1\n" + instance_id +
/// "\n" + iat + "\n" + nonce` (iat in decimal unix seconds).
pub fn token_assertion_input(instance_id: &str, iat: i64, nonce: &str) -> Vec<u8> {
    format!("{TOKEN_ASSERTION_DOMAIN}{instance_id}\n{iat}\n{nonce}").into_bytes()
}

/// Response of `POST /v1/instances/{instance_id}/token`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessTokenV1 {
    pub schema: String,
    /// Bearer token; never logged, printed or persisted outside `token.json`.
    pub access_token: String,
    /// Seconds, at most [`MAX_ACCESS_TOKEN_SECONDS`].
    pub expires_in: u32,
}

impl fmt::Debug for AccessTokenV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccessTokenV1")
            .field("access_token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

impl AccessTokenV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, ACCESS_TOKEN_SCHEMA)?;
        validate_text(&self.access_token, 4096, "access_token", false)?;
        if !(1..=MAX_ACCESS_TOKEN_SECONDS).contains(&self.expires_in) {
            return Err(ValidationError::new("expires_in must be 1-3600 s"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_code_label_follows_the_text_rule() {
        let request = |label: Option<&str>| {
            DeviceCodeRequestV1::new("3.12.0", EngineOs::Macos)
                .with_label(label.map(str::to_string))
        };
        assert!(request(None).validate().is_ok());
        assert!(request(Some("build-box")).validate().is_ok());
        let long = "x".repeat(MAX_INSTANCE_LABEL_CHARS + 1);
        for bad in [
            "",
            "line\nbreak",
            "zero\u{200b}width",
            "a\u{202e}b",
            long.as_str(),
        ] {
            assert!(request(Some(bad)).validate().is_err(), "{bad:?}");
        }
        let unlabelled = serde_json::to_value(request(None)).unwrap();
        assert!(
            unlabelled.get("instance_label").is_none(),
            "older services refuse unknown fields"
        );
    }

    #[test]
    fn assertion_input_is_the_documented_text() {
        assert_eq!(
            token_assertion_input("inst_1", 1_792_500_000, "n0nce-n0nce-n0nce"),
            b"leanctx.hive.token.v1\ninst_1\n1792500000\nn0nce-n0nce-n0nce".to_vec()
        );
    }

    #[test]
    fn secrets_stay_out_of_debug() {
        let token = AccessTokenV1 {
            schema: ACCESS_TOKEN_SCHEMA.into(),
            access_token: "atl_secret_value".into(),
            expires_in: 3600,
        };
        assert!(!format!("{token:?}").contains("atl_secret_value"));
        let code = DeviceCodeV1 {
            schema: DEVICE_CODE_SCHEMA.into(),
            device_code: "dc_secret_value".into(),
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://hive.leanctx.com/device".into(),
            verification_uri_complete: "https://hive.leanctx.com/device?code=WDJB-MJHT".into(),
            interval: 5,
            expires_in: 900,
            features: Vec::new(),
        };
        assert!(!format!("{code:?}").contains("dc_secret_value"));
        assert!(!code.offers_device_setup());
    }

    /// C0 2.1 device setup: the contract fixtures round-trip byte for byte
    /// through the engine's types, and the features list is optional.
    #[test]
    fn device_setup_documents_are_the_contract_fixtures() {
        fn round<T: serde::de::DeserializeOwned + Serialize>(raw: &str) {
            let value: serde_json::Value = serde_json::from_str(raw).unwrap();
            let typed: T = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(), value);
        }
        round::<DeviceSetupRequestV1>(include_str!(
            "../../tests/fixtures/hive-v2/routes/device_setup.request.json"
        ));
        round::<DeviceSetupResultV1>(include_str!(
            "../../tests/fixtures/hive-v2/routes/device_setup_result.request.json"
        ));
        let mut answer = serde_json::json!({
            "schema": DEVICE_CODE_SCHEMA, "device_code": "dc_x", "user_code": "WDJB-MJHT",
            "verification_uri": "https://hive.leanctx.com/device",
            "verification_uri_complete": "https://hive.leanctx.com/device?user_code=WDJB-MJHT",
            "interval": 5, "expires_in": 900,
        });
        let old: DeviceCodeV1 = serde_json::from_value(answer.clone()).unwrap();
        assert!(!old.offers_device_setup(), "a C0 2.0 Hive lists nothing");
        answer["features"] = serde_json::json!([FEATURE_DEVICE_SETUP, "later"]);
        let new: DeviceCodeV1 = serde_json::from_value(answer).unwrap();
        assert!(new.offers_device_setup());
    }

    #[test]
    fn requests_refuse_unknown_fields() {
        let json = r#"{"schema":"leanctx.hive.device-code-request/v1","client_id":"lean-ctx-engine","engine_version":"3.12.0","os":"macos","extra":true}"#;
        assert!(serde_json::from_str::<DeviceCodeRequestV1>(json).is_err());
    }
}
