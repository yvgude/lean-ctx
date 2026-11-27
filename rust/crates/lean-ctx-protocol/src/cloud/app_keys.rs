// SPDX-License-Identifier: Apache-2.0

//! f3129 app keys S2+S3: binding the app's no-prompt signing keys to an
//! instance inside the device flow (`docs/contracts/app-key-bind-v1.md`).
//!
//! - S2, `POST /v1/device/code`: the engine announces the keys
//!   ([`AppKeyAnnouncementV1`], `app_keys`) only to a Cloud whose code answer
//!   lists [`FEATURE_DEVICE_APP_KEYS`]. The approval page names each key with
//!   its fingerprint (`key_id`) before the person allows the connection.
//! - S3, `POST /v1/device/token`: the instance key signs
//!   [`app_key_bind_signing_input`] (`app_keys_bind_signature`) over the
//!   one-time `device_code`, its own public key and the announced keys. The
//!   offered keys (`chat_key`, `task_propose_key`, `optin_key`) must equal the
//!   announced set.
//!
//! Every signature here is verified strictly (`verify_strict`: canonical
//! 64-byte signature, no small-order key or `R`).
//!
//! Encoding (unlike the crate-wide standard base64): app public keys and all
//! signatures here are base64url without padding; `key_id` is
//! `hex(sha256(raw public key))[..16]`; `instance_public_key` enters every
//! statement exactly as the token request carries it (standard base64).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::signing_input;
use crate::ValidationError;

/// `DeviceCodeV1.features` entry of a Cloud that takes `app_keys` (S2) and
/// `app_keys_bind_signature` (S3). Without it the engine announces, offers
/// and binds no app key.
pub const FEATURE_DEVICE_APP_KEYS: &str = "device_app_keys";
/// Domain of the instance key's bind signature (S3).
pub const APP_KEY_BIND_DOMAIN: &str = "leanctx.cloud.app_key_bind.v1\n";
/// The one algorithm of an app key.
pub const APP_KEY_ALG: &str = "ed25519";
/// At most one key per kind.
pub const MAX_APP_KEYS: usize = 3;
/// Length of `key_id` in hex characters.
pub const APP_KEY_ID_LEN: usize = 16;
/// Error `code` of every app-key refusal (HTTP 400).
pub const APP_KEY_ERROR_CODE: &str = "invalid_field";
/// `reason`: the offered keys are not exactly the announced ones.
pub const REASON_APP_KEY_NOT_ANNOUNCED: &str = "app_key_not_announced";
/// `reason`: `app_keys_bind_signature` is missing or does not verify.
pub const REASON_APP_KEY_BIND_INVALID: &str = "app_key_bind_invalid";
/// What the engine and the app show for either reason.
pub const PAIRING_FAILED_TEXT: &str = "Pairing failed, please start connecting again.";

/// Which app key. Ordered by wire name, the order the lists are sorted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKeyKind {
    /// Signs a person's team-chat posts (`person_publish.v1`).
    Chat,
    /// Signs a person's task-pickup turn-on (`optin.v1`).
    Optin,
    /// Signs a person's task proposals (`task_propose_person.v1`).
    TaskPropose,
}

impl AppKeyKind {
    pub const ALL: [Self; 3] = [Self::Chat, Self::Optin, Self::TaskPropose];

    /// The wire name (`chat`, `optin`, `task_propose`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Optin => "optin",
            Self::TaskPropose => "task_propose",
        }
    }

    /// The device-token request member that offers this key.
    pub fn request_member(self) -> &'static str {
        match self {
            Self::Chat => "chat_key",
            Self::Optin => "optin_key",
            Self::TaskPropose => "task_propose_key",
        }
    }

    /// Domain of the key's own bind signature (`bind_signature` of its offer).
    pub fn bind_domain(self) -> &'static str {
        match self {
            Self::Chat => "leanctx.cloud.chat_key_bind.v1\n",
            Self::Optin => "leanctx.cloud.optin_key_bind.v1\n",
            Self::TaskPropose => "leanctx.cloud.task_propose_key_bind.v1\n",
        }
    }

    /// The one scope the key is bound for.
    pub fn scope(self) -> &'static str {
        match self {
            Self::Chat => "person_publish.v1",
            Self::Optin => "optin.v1",
            Self::TaskPropose => "task_propose_person.v1",
        }
    }

    /// How the approval page and LeanCTX Code name the key.
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat messages",
            Self::Optin => "Task pickup",
            Self::TaskPropose => "Task proposals",
        }
    }
}

/// One entry of `DeviceCodeRequestV1.app_keys` (S2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppKeyAnnouncementV1 {
    pub kind: AppKeyKind,
    /// Always [`APP_KEY_ALG`].
    pub alg: String,
    /// `hex(sha256(raw public key))[..16]`, the fingerprint people compare.
    pub key_id: String,
    /// base64url without padding of the raw 32 bytes.
    pub public_key: String,
}

/// `chat_key` / `task_propose_key` / `optin_key` of the device-token request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppKeyOfferV1 {
    pub alg: String,
    /// base64url without padding of the raw 32 bytes.
    pub public_key: String,
    pub key_id: String,
    /// base64url signature of the app key over [`app_key_offer_bind_input`].
    pub bind_signature: String,
}

/// `hex(sha256(public key bytes))[..16]`.
pub fn app_key_id(public_key: &[u8]) -> String {
    let mut id = lower_hex(&Sha256::digest(public_key));
    id.truncate(APP_KEY_ID_LEN);
    id
}

fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Decode canonical base64url without padding of exactly `len` bytes.
pub fn decode_b64url_exact(
    value: &str,
    len: usize,
    field: &str,
) -> Result<Vec<u8>, ValidationError> {
    let refuse = || {
        ValidationError::new(format!(
            "{field} must be base64url without padding of {len} bytes"
        ))
    };
    let bytes = B64URL.decode(value).map_err(|_| refuse())?;
    if bytes.len() != len || B64URL.encode(&bytes) != value {
        return Err(refuse());
    }
    Ok(bytes)
}

fn validate_key(
    alg: &str,
    key_id: &str,
    public_key: &str,
    field: &str,
) -> Result<(), ValidationError> {
    if alg != APP_KEY_ALG {
        return Err(ValidationError::new(format!(
            "{field}.alg must be {APP_KEY_ALG}"
        )));
    }
    let raw = decode_b64url_exact(public_key, 32, &format!("{field}.public_key"))?;
    if key_id != app_key_id(&raw) {
        return Err(ValidationError::new(format!(
            "{field}.key_id must be hex(sha256(public_key))[..16]"
        )));
    }
    Ok(())
}

impl AppKeyAnnouncementV1 {
    /// An announcement of `public_key` (raw bytes).
    pub fn new(kind: AppKeyKind, public_key: &[u8; 32]) -> Self {
        Self {
            kind,
            alg: APP_KEY_ALG.into(),
            key_id: app_key_id(public_key),
            public_key: B64URL.encode(public_key),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_key(&self.alg, &self.key_id, &self.public_key, "app_keys")
    }
}

impl AppKeyOfferV1 {
    pub fn validate(&self, kind: AppKeyKind) -> Result<(), ValidationError> {
        let field = kind.request_member();
        validate_key(&self.alg, &self.key_id, &self.public_key, field)?;
        decode_b64url_exact(&self.bind_signature, 64, &format!("{field}.bind_signature"))?;
        Ok(())
    }

    /// Whether this offer names exactly the announced key.
    pub fn matches(&self, announced: &AppKeyAnnouncementV1) -> bool {
        self.alg == announced.alg
            && self.key_id == announced.key_id
            && self.public_key == announced.public_key
    }
}

/// `DeviceCodeRequestV1.app_keys`: each entry valid, sorted by kind, at most
/// one per kind (so at most [`MAX_APP_KEYS`]). The Cloud answers any failure
/// with 400 `invalid_field`, `field: "app_keys"`.
pub fn validate_app_keys(keys: &[AppKeyAnnouncementV1]) -> Result<(), ValidationError> {
    if keys.len() > MAX_APP_KEYS {
        return Err(ValidationError::new(format!(
            "app_keys carries at most {MAX_APP_KEYS} keys"
        )));
    }
    for pair in keys.windows(2) {
        if pair[0].kind == pair[1].kind {
            return Err(ValidationError::new("app_keys carries one key per kind"));
        }
        if pair[0].kind > pair[1].kind {
            return Err(ValidationError::new("app_keys must be sorted by kind"));
        }
    }
    keys.iter().try_for_each(AppKeyAnnouncementV1::validate)
}

/// The bytes the instance key signs for S3 (`app_keys_bind_signature`):
/// [`APP_KEY_BIND_DOMAIN`] ‖ canonical_json({device_code_sha256,
/// instance_public_key, keys: [{alg, kind, key_id, public_key}] sorted by
/// kind}).
/// `keys` are the announced keys; the caller validated them.
pub fn app_key_bind_signing_input(
    device_code: &str,
    instance_public_key: &str,
    keys: &[AppKeyAnnouncementV1],
) -> Result<Vec<u8>, ValidationError> {
    signing_input(
        APP_KEY_BIND_DOMAIN,
        &app_key_bind_statement(device_code, instance_public_key, keys),
    )
}

/// The JSON value under [`app_key_bind_signing_input`].
pub fn app_key_bind_statement(
    device_code: &str,
    instance_public_key: &str,
    keys: &[AppKeyAnnouncementV1],
) -> Value {
    let mut keys = keys.iter().collect::<Vec<_>>();
    keys.sort_by_key(|key| key.kind);
    json!({
        "device_code_sha256": lower_hex(&Sha256::digest(device_code.as_bytes())),
        "instance_public_key": instance_public_key,
        "keys": keys
            .iter()
            .map(|key| json!({"alg": key.alg, "kind": key.kind, "key_id": key.key_id, "public_key": key.public_key}))
            .collect::<Vec<_>>(),
    })
}

/// The bytes an app key signs for its own offer (`bind_signature`):
/// the kind's bind domain ‖ canonical_json({alg, instance_public_key,
/// key_id, public_key, scope}), as !622/!627/!628 define them.
pub fn app_key_offer_bind_input(
    kind: AppKeyKind,
    instance_public_key: &str,
    key_id: &str,
    public_key: &str,
) -> Result<Vec<u8>, ValidationError> {
    signing_input(
        kind.bind_domain(),
        &json!({
            "alg": APP_KEY_ALG,
            "instance_public_key": instance_public_key,
            "key_id": key_id,
            "public_key": public_key,
            "scope": kind.scope(),
        }),
    )
}

/// Announced-set equality (check 4 of the Cloud's order): every announced
/// key is offered with identical `alg`, `key_id` and `public_key`, nothing
/// else is offered, and no kind is offered twice. `Err` names the first
/// differing (or repeated) request member;
/// the Cloud answers 400 `invalid_field`, reason
/// [`REASON_APP_KEY_NOT_ANNOUNCED`], with that `field`.
pub fn offered_equals_announced(
    announced: &[AppKeyAnnouncementV1],
    offered: &[(AppKeyKind, &AppKeyOfferV1)],
) -> Result<(), &'static str> {
    for kind in AppKeyKind::ALL {
        if offered.iter().filter(|(k, _)| *k == kind).count() > 1 {
            return Err(kind.request_member());
        }
        let a = announced.iter().find(|key| key.kind == kind);
        let o = offered
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, offer)| *offer);
        match (a, o) {
            (None, None) => {}
            (Some(a), Some(o)) if o.matches(a) => {}
            _ => return Err(kind.request_member()),
        }
    }
    Ok(())
}

/// The text the engine shows for an app-key refusal, `None` for any other
/// `reason`.
pub fn app_key_refusal_text(reason: Option<&str>) -> Option<&'static str> {
    match reason? {
        REASON_APP_KEY_NOT_ANNOUNCED | REASON_APP_KEY_BIND_INVALID => Some(PAIRING_FAILED_TEXT),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(&[byte; 32])
            .verifying_key()
            .to_bytes()
    }

    /// The first test key whose announcement satisfies `wanted`.
    fn chat_key_where(wanted: impl Fn(&AppKeyAnnouncementV1) -> bool) -> AppKeyAnnouncementV1 {
        (1..=u8::MAX)
            .map(|byte| AppKeyAnnouncementV1::new(AppKeyKind::Chat, &key(byte)))
            .find(wanted)
            .expect("a test key with the wanted shape")
    }

    fn three() -> Vec<AppKeyAnnouncementV1> {
        vec![
            AppKeyAnnouncementV1::new(AppKeyKind::Chat, &key(1)),
            AppKeyAnnouncementV1::new(AppKeyKind::Optin, &key(2)),
            AppKeyAnnouncementV1::new(AppKeyKind::TaskPropose, &key(3)),
        ]
    }

    #[test]
    fn kinds_sort_by_wire_name() {
        let mut names = AppKeyKind::ALL.map(AppKeyKind::as_str);
        names.sort_unstable();
        assert_eq!(names, AppKeyKind::ALL.map(AppKeyKind::as_str));
        for kind in AppKeyKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }

    #[test]
    fn valid_lists_pass() {
        validate_app_keys(&[]).unwrap();
        validate_app_keys(&three()).unwrap();
        validate_app_keys(&three()[1..]).unwrap();
    }

    #[test]
    fn the_validator_refuses_every_malformed_list() {
        let keys = three();
        let mut unsorted = keys.clone();
        unsorted.swap(0, 2);
        let duplicate = vec![keys[0].clone(), keys[0].clone()];
        let mut four = keys.clone();
        four.push(keys[2].clone());
        let mut alg = keys.clone();
        alg[0].alg = "ed448".into();
        let mut padded = keys.clone();
        padded[0].public_key.push('=');
        // A key whose base64url has `-` or `_`, in the standard alphabet
        // without padding: only the alphabet is wrong.
        let mut standard = keys.clone();
        standard[0] = chat_key_where(|a| a.public_key.contains(['-', '_']));
        standard[0].public_key = standard[0].public_key.replace('-', "+").replace('_', "/");
        // Non-canonical trailing bits: 32 bytes leave 2 unused low bits in
        // the last character; setting one decodes to the same bytes.
        let mut trailing = keys.clone();
        let last = trailing[0].public_key.pop().unwrap();
        let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let index = alphabet.find(last).unwrap() | 1;
        trailing[0]
            .public_key
            .push(alphabet.as_bytes()[index] as char);
        let mut short = keys.clone();
        short[0].public_key = B64URL.encode([7u8; 31]);
        let mut id = keys.clone();
        id[0].key_id = keys[1].key_id.clone();
        let mut upper = keys.clone();
        upper[0] = chat_key_where(|a| a.key_id.bytes().any(|b| b.is_ascii_lowercase()));
        upper[0].key_id = upper[0].key_id.to_ascii_uppercase();
        for (name, list) in [
            ("unsorted", unsorted),
            ("duplicate", duplicate),
            ("four", four),
            ("alg", alg),
            ("padded", padded),
            ("short", short),
            ("key_id", id),
            ("standard", standard),
            ("trailing_bits", trailing),
            ("upper", upper),
        ] {
            assert!(validate_app_keys(&list).is_err(), "{name} passed");
        }
        let unknown = serde_json::json!({"kind": "share", "alg": "ed25519", "key_id": "0", "public_key": "x"});
        assert!(serde_json::from_value::<AppKeyAnnouncementV1>(unknown).is_err());
    }

    #[test]
    fn the_bind_input_is_deterministic_jcs_over_the_sorted_keys() {
        let keys = three();
        let mut reversed = keys.clone();
        reversed.reverse();
        let ipk = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        let one = app_key_bind_signing_input("dc_x", ipk, &keys).unwrap();
        assert_eq!(
            one,
            app_key_bind_signing_input("dc_x", ipk, &reversed).unwrap()
        );
        assert_ne!(one, app_key_bind_signing_input("dc_y", ipk, &keys).unwrap());
        let text = String::from_utf8(one).unwrap();
        let expected = format!(
            "leanctx.cloud.app_key_bind.v1\n{{\"device_code_sha256\":\"{}\",\"instance_public_key\":\"{ipk}\",\"keys\":[{}]}}",
            lower_hex(&Sha256::digest(b"dc_x")),
            keys.iter()
                .map(|k| format!(
                    "{{\"alg\":\"ed25519\",\"key_id\":\"{}\",\"kind\":\"{}\",\"public_key\":\"{}\"}}",
                    k.key_id,
                    k.kind.as_str(),
                    k.public_key
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn offered_set_must_equal_the_announced_set() {
        let keys = three();
        let offer = |a: &AppKeyAnnouncementV1| AppKeyOfferV1 {
            alg: a.alg.clone(),
            public_key: a.public_key.clone(),
            key_id: a.key_id.clone(),
            bind_signature: B64URL.encode([0u8; 64]),
        };
        let offers = keys.iter().map(offer).collect::<Vec<_>>();
        let all = keys
            .iter()
            .map(|k| k.kind)
            .zip(offers.iter())
            .collect::<Vec<_>>();
        offered_equals_announced(&keys, &all).unwrap();
        assert_eq!(
            offered_equals_announced(&keys, &all[..2]),
            Err("task_propose_key")
        );
        assert_eq!(offered_equals_announced(&keys[..1], &all), Err("optin_key"));
        let other = offer(&AppKeyAnnouncementV1::new(AppKeyKind::Chat, &key(9)));
        assert_eq!(
            offered_equals_announced(&keys[..1], &[(AppKeyKind::Chat, &other)]),
            Err("chat_key")
        );
        offered_equals_announced(&[], &[]).unwrap();
        let twice = [all[0], all[0], all[1], all[2]];
        assert_eq!(offered_equals_announced(&keys, &twice), Err("chat_key"));
    }

    #[test]
    fn both_refusals_read_the_same_and_others_are_left_alone() {
        assert_eq!(
            app_key_refusal_text(Some(REASON_APP_KEY_NOT_ANNOUNCED)),
            Some(PAIRING_FAILED_TEXT)
        );
        assert_eq!(
            app_key_refusal_text(Some(REASON_APP_KEY_BIND_INVALID)),
            Some(PAIRING_FAILED_TEXT)
        );
        assert_eq!(app_key_refusal_text(Some("required")), None);
        assert_eq!(app_key_refusal_text(None), None);
        let banned = [
            "hive",
            "this mac",
            "workspace",
            "engine",
            "lease",
            "instance",
            "weave",
            "merge queue",
            "retry",
            "excl. vat",
            "leanctx cloud",
            "queen",
            "pro",
            "leanctx app",
            "instances",
        ];
        // Whole words only: "please" does not contain the word "lease".
        let words = |text: &str| {
            let spaced: String = text
                .to_ascii_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
                .collect();
            format!(
                " {} ",
                spaced.split_whitespace().collect::<Vec<_>>().join(" ")
            )
        };
        let clean = |text: &str| {
            let text = words(text);
            banned.iter().all(|word| !text.contains(&words(word)))
        };
        assert!(clean(PAIRING_FAILED_TEXT));
        assert!(!clean("Lease expired"), "the check finds a banned word");
        for kind in AppKeyKind::ALL {
            assert!(clean(kind.label()), "{}", kind.label());
        }
    }
}
