// SPDX-License-Identifier: Apache-2.0

//! LeanCTX Hive engine↔service wire contract, protocol major 1.
//!
//! The engine connector and the Hive service build against these types and
//! the golden fixtures in `tests/fixtures/hive/`. A contract change is a
//! fixture change that both sides review.
//!
//! Conventions shared by every module:
//! - Base URL `https://hive.leanctx.com/v1`; a self-hosted Hive may use any
//!   `https` URL (loopback may use `http`).
//! - Every HTTP body carries `schema` (`leanctx.hive.<area>/v1`). The one
//!   exception is the error body `{"error":{...}}`, which proxies may also emit.
//! - Request types refuse unknown fields (the service rejects them with
//!   `invalid_request`); response types accept unknown fields, so the service
//!   can add fields in a minor version without breaking older engines.
//! - Timestamps are `YYYY-MM-DDTHH:MM:SSZ` ([`UtcTimestamp`]); unix seconds
//!   appear only where a value is signed as text (`iat`).
//! - Keys, nonces, signatures and ciphertext are standard base64 with padding.
//! - `vault_id` is 32 lowercase hex characters: the engine API renders the
//!   vault UUID in simple form (no dashes) while the web API keeps hyphenated
//!   UUIDs.
//! - Signatures are Ed25519 over `domain || canonical_json(value)`, see
//!   [`canonical_json`]. Signed objects are verified over the raw JSON that
//!   was received, never over a re-serialized typed value, so additive fields
//!   stay covered by the signature.

pub mod anchor;
pub mod automerge;
pub mod bus;
pub mod codegraph;
pub mod compute;
pub mod ctxpkg;
pub mod effective_policy;
pub mod enroll;
pub mod error;
pub mod facts;
pub mod graph;
pub mod knowledge;
pub mod lease;
pub mod messages;
pub mod metrics_v2;
pub mod optin;
pub mod policy;
pub mod profiles;
pub mod receipts_v2;
pub mod root_cert;
pub mod sealed;
pub mod status;
pub mod swarm;
pub mod tasks;
pub mod telemetry;
pub mod vault;

pub use error::{HiveErrorBody, HiveErrorCode, HiveErrorDetailV1, HiveUploadKind};

use crate::ValidationError;
use serde_json::Value;
use std::collections::BTreeMap;

/// Major protocol version; a different major is incompatible.
pub const HIVE_PROTOCOL_MAJOR: u16 = 1;
/// Minor protocol version; minors only add optional response fields or routes.
pub const HIVE_PROTOCOL_MINOR: u16 = 9;

/// Hosted Hive origin.
pub const HIVE_DEFAULT_URL: &str = "https://hive.leanctx.com";
/// Path prefix of every engine-facing route.
pub const HIVE_API_PREFIX: &str = "/v1";
/// OAuth-style client id the engine sends in the device flow.
pub const HIVE_ENGINE_CLIENT_ID: &str = "lean-ctx-engine";

/// `major.minor` the sender speaks; sent on every request and response.
pub const HEADER_PROTOCOL: &str = "Hive-Protocol";
/// Instance id on authenticated requests.
pub const HEADER_INSTANCE: &str = "Hive-Instance";
/// Lowest engine version the service accepts; sent with `426`.
pub const HEADER_MIN_ENGINE: &str = "Hive-Min-Engine";
/// Client-chosen ULID on every `POST`; replays return the first response.
pub const HEADER_IDEMPOTENCY_KEY: &str = "Idempotency-Key";
/// 1.9 VA (until the 3.12 release day): an engine that speaks the vault
/// names sends [`NAMES_VAULT`] here on every request, the bus WebSocket
/// upgrade included. Hive then answers in the vault form (`vault_id`,
/// `vault:` channels); without it, in the earlier project form. Hive takes
/// both forms as input. From the release day only the vault form remains
/// and the header is ignored.
pub const HEADER_NAMES: &str = "Hive-Names";
/// The [`HEADER_NAMES`] value of the vault form.
pub const NAMES_VAULT: &str = "vault";

/// Domain of the lease signature (Hive signing key).
pub const LEASE_SIGNATURE_DOMAIN: &str = "leanctx.hive.lease.v1\n";
/// Domain of the key-set signature (Hive root key).
pub const KEYSET_SIGNATURE_DOMAIN: &str = "leanctx.hive.keyset.v1\n";
/// Domain of the instance token assertion (instance key).
pub const TOKEN_ASSERTION_DOMAIN: &str = "leanctx.hive.token.v1\n";
/// Domain of a filter-pack statement signature (Hive signing key).
pub const FILTER_PACK_SIGNATURE_DOMAIN: &str = "leanctx.filter-pack.v1\n";
/// Domain of the stable repository identity digest input. The bytes keep
/// their 1.0 name: a different domain would change every digest and unbind
/// every linked vault.
pub const REPO_IDENTITY_DOMAIN: &str = "leanctx.project-identity.v1\n";

/// Largest request or response body either side accepts, except knowledge
/// documents ([`knowledge::MAX_KNOWLEDGE_BODY_BYTES`]).
pub const MAX_BODY_BYTES: usize = 1 << 20;

/// `Hive-Protocol` value this crate speaks.
pub fn protocol_header_value() -> String {
    format!("{HIVE_PROTOCOL_MAJOR}.{HIVE_PROTOCOL_MINOR}")
}

/// Parse an `Hive-Protocol` header into `(major, minor)`.
pub fn parse_protocol_header(value: &str) -> Option<(u16, u16)> {
    let (major, minor) = value.trim().split_once('.')?;
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(major) || !digits(minor) {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Whether a peer's `Hive-Protocol` header is compatible with this crate.
pub fn protocol_compatible(value: &str) -> bool {
    parse_protocol_header(value).is_some_and(|(major, _)| major == HIVE_PROTOCOL_MAJOR)
}

/// Canonical JSON encoding used under every Hive signature.
///
/// UTF-8, no insignificant whitespace, object keys sorted by their UTF-8
/// bytes at every depth, strings escaped as `serde_json` does (only `"`, `\`
/// and control characters are escaped; `\b \f \n \r \t` use the short form,
/// other control characters `\u00xx` with lowercase hex; non-ASCII stays raw
/// UTF-8), integers in plain decimal. Non-integer numbers are refused so that
/// two implementations cannot disagree on float formatting.
pub fn canonical_json(value: &Value) -> Result<Vec<u8>, ValidationError> {
    let sorted = canonical_value(value)?;
    serde_json::to_vec(&sorted)
        .map_err(|error| ValidationError::new(format!("encode canonical JSON: {error}")))
}

fn canonical_value(value: &Value) -> Result<Value, ValidationError> {
    Ok(match value {
        Value::Object(fields) => {
            let sorted = fields
                .iter()
                .map(|(key, value)| Ok((key.clone(), canonical_value(value)?)))
                .collect::<Result<BTreeMap<_, _>, ValidationError>>()?;
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(canonical_value)
                .collect::<Result<_, _>>()?,
        ),
        Value::Number(number) if !(number.is_i64() || number.is_u64()) => {
            return Err(ValidationError::new("canonical JSON carries integers only"));
        }
        scalar => scalar.clone(),
    })
}

/// `domain || canonical_json(value)`: the exact bytes an Ed25519 key signs.
pub fn signing_input(domain: &str, value: &Value) -> Result<Vec<u8>, ValidationError> {
    let mut bytes = domain.as_bytes().to_vec();
    bytes.extend(canonical_json(value)?);
    Ok(bytes)
}

/// Signing input of an object that carries its own `signature` field: the
/// canonical form of the object without that field.
pub fn signing_input_without_signature(
    domain: &str,
    value: &Value,
) -> Result<Vec<u8>, ValidationError> {
    let Value::Object(fields) = value else {
        return Err(ValidationError::new("signed value must be a JSON object"));
    };
    let mut fields = fields.clone();
    fields.remove("signature");
    signing_input(domain, &Value::Object(fields))
}

pub(crate) fn require_schema(actual: &str, expected: &str) -> Result<(), ValidationError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "schema must be {expected}, got {actual:?}"
        )))
    }
}

/// Validate standard padded base64 that decodes to exactly `decoded_len` bytes.
pub(crate) fn validate_b64_len(
    value: &str,
    decoded_len: usize,
    field: &str,
) -> Result<(), ValidationError> {
    let encoded_len = decoded_len.div_ceil(3) * 4;
    let padding = (3 - decoded_len % 3) % 3;
    if value.len() == encoded_len && b64_shape_ok(value, padding) {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "{field} must be base64 of exactly {decoded_len} bytes"
        )))
    }
}

/// Validate standard padded base64 of any length up to `max_encoded` chars.
pub(crate) fn validate_b64_bounded(
    value: &str,
    max_encoded: usize,
    field: &str,
) -> Result<(), ValidationError> {
    let padding = value.bytes().rev().take_while(|byte| *byte == b'=').count();
    if !value.is_empty()
        && value.len() <= max_encoded
        && value.len().is_multiple_of(4)
        && padding <= 2
        && b64_shape_ok(value, padding)
    {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "{field} must be non-empty padded base64 of at most {max_encoded} characters"
        )))
    }
}

fn b64_shape_ok(value: &str, padding: usize) -> bool {
    let (body, pad) = value.split_at(value.len() - padding);
    pad.bytes().all(|byte| byte == b'=')
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
}

/// 1.9 R3-3e-1: characters that make a text read differently from what it
/// is, or hide text a person reviewing it cannot see: bidi marks,
/// embeddings, overrides and isolates, zero-width space, the invisible
/// operators, a byte-order mark, tag characters and the variation selector
/// supplement; and (1.9 erratum) the soft hyphen, the Mongolian vowel
/// separator, the combining grapheme joiner, the Hangul fillers, the Khmer
/// inherent vowels and the line and paragraph separators (not controls, so
/// they would break a one-line text), the whole block U+2060–U+206F (with
/// the deprecated format controls), the interlinear annotation characters
/// (they hide annotation text) and the invisible format controls of
/// Egyptian hieroglyphs, shorthand and musical notation. Every text field
/// refuses them; engines strip them from foreign content. The joiners
/// U+200C/U+200D and U+FE00–FE0F stay: emoji and Persian need them; the
/// visible Arabic and Syriac signs (U+0600–0605, U+06DD, U+070F, U+08E2) too.
#[must_use]
pub fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// Validate a bounded, printable text field (`max` in characters).
pub(crate) fn validate_text(
    value: &str,
    max: usize,
    field: &str,
    allow_newlines: bool,
) -> Result<(), ValidationError> {
    let count = value.chars().count();
    if count == 0 || count > max || value.trim().is_empty() {
        return Err(ValidationError::new(format!(
            "{field} must be non-empty text of at most {max} characters"
        )));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && !(allow_newlines && matches!(ch, '\n' | '\t' | '\r')))
    {
        return Err(ValidationError::new(format!(
            "{field} contains control characters"
        )));
    }
    if value.chars().any(invisible) {
        return Err(ValidationError::new(format!(
            "{field} contains invisible or direction characters"
        )));
    }
    Ok(())
}

/// Validate a server- or client-assigned opaque id: 1–128 chars of
/// `[A-Za-z0-9._:-]`, starting alphanumeric.
pub(crate) fn validate_id(value: &str, field: &str) -> Result<(), ValidationError> {
    let bytes = value.as_bytes();
    if !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "{field} must be 1-128 characters of [A-Za-z0-9._:-] starting alphanumeric"
        )))
    }
}

/// Validate a Hive `vault_id`: 32 lowercase hex characters (128 random
/// bits), the same form `leanctx.cloud-metrics/v1` requires of `vault.id`.
/// The engine API renders the vault UUID in simple form (no dashes) while the
/// web API keeps hyphenated UUIDs.
pub fn validate_vault_id(value: &str) -> Result<(), ValidationError> {
    if value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(ValidationError::new(
            "vault_id must be 32 lowercase hexadecimal characters",
        ))
    }
}

/// Validate an origin: `https://host[:port]`, or `http://` for
/// localhost/127.0.0.1/[::1]; no path, query, credentials or trailing slash.
pub fn validate_origin(value: &str, field: &str) -> Result<(), ValidationError> {
    let refuse = || {
        ValidationError::new(format!(
            "{field} must be an https origin (http only on loopback)"
        ))
    };
    let (scheme, rest) = value.split_once("://").ok_or_else(refuse)?;
    if rest.is_empty()
        || value.len() > 256
        || rest.contains(['/', '?', '#', '@'])
        || rest.chars().any(char::is_whitespace)
    {
        return Err(refuse());
    }
    let host = rest
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(rest, |(host, _)| host);
    match scheme {
        "https" => Ok(()),
        "http" if matches!(host, "localhost" | "127.0.0.1" | "[::1]") => Ok(()),
        _ => Err(refuse()),
    }
}

/// Validate a lowercase hexadecimal SHA-256 digest without prefix.
pub(crate) fn validate_hex64(value: &str, field: &str) -> Result<(), ValidationError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "{field} must be 64 lowercase hexadecimal characters"
        )))
    }
}

pub(crate) fn parse_time(
    value: &crate::UtcTimestamp,
) -> Result<chrono::DateTime<chrono::FixedOffset>, ValidationError> {
    chrono::DateTime::parse_from_rfc3339(value.as_str())
        .map_err(|_| ValidationError::new(format!("invalid timestamp {:?}", value.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_refuses_what_a_reviewer_cannot_see() {
        // Tag characters spell "hi" invisibly after a harmless word.
        assert!(validate_text("fine\u{E0068}\u{E0069}", 100, "t", false).is_err());
        assert!(validate_text("a\u{E0100}", 100, "t", false).is_err());
        assert!(validate_text("safe \u{202E}cod.exe", 100, "t", false).is_err());
        assert!(validate_text("a\u{200B}b", 100, "t", true).is_err());
        // Emoji sequences and Persian joiners stay.
        assert!(
            validate_text(
                "dev \u{1F469}\u{200D}\u{1F4BB} \u{2764}\u{FE0F}",
                100,
                "t",
                false
            )
            .is_ok()
        );
        assert!(
            validate_text(
                "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
                100,
                "t",
                false
            )
            .is_ok()
        );
    }

    /// 1.9 erratum: every character of the set, one by one, in a one-line
    /// and a multi-line text and in an anchor path; the joiners stay in
    /// text only.
    #[test]
    fn every_invisible_character_is_refused() {
        let set = [
            '\u{00AD}',
            '\u{034F}',
            '\u{061C}',
            '\u{115F}',
            '\u{1160}',
            '\u{17B4}',
            '\u{17B5}',
            '\u{180E}',
            '\u{200B}',
            '\u{200E}',
            '\u{200F}',
            '\u{2028}',
            '\u{2029}',
            '\u{202A}',
            '\u{202E}',
            '\u{2060}',
            '\u{2064}',
            '\u{2066}',
            '\u{2069}',
            '\u{3164}',
            '\u{FEFF}',
            '\u{FFA0}',
            '\u{E0000}',
            '\u{E0041}',
            '\u{E007F}',
            '\u{E0100}',
            '\u{E01EF}',
            '\u{2065}',
            '\u{206A}',
            '\u{206F}',
            '\u{FFF9}',
            '\u{FFFB}',
            '\u{13430}',
            '\u{1343F}',
            '\u{1BCA0}',
            '\u{1BCA3}',
            '\u{1D173}',
            '\u{1D17A}',
        ];
        for c in set {
            let text = format!("a{c}b");
            assert!(invisible(c), "{c:?}");
            assert!(validate_text(&text, 100, "t", false).is_err(), "{c:?}");
            assert!(validate_text(&text, 100, "t", true).is_err(), "{c:?}");
            assert!(
                anchor::validate_anchor_path(&format!("src/{text}.rs")).is_err(),
                "{c:?}"
            );
        }
        // The joiners, emoji selectors and visible Arabic and Syriac signs stay.
        for c in [
            '\u{200C}', '\u{200D}', '\u{FE0F}', '\u{0600}', '\u{06DD}', '\u{070F}', '\u{08E2}',
            'a', ' ', 'ü',
        ] {
            assert!(!invisible(c), "{c:?}");
        }
        assert!(anchor::validate_anchor_path("src/a\u{200D}b.rs").is_err());
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let value = json!({"b": 1, "a": {"z": [ {"y": 2, "x": 1} ], "c": "ü\n"}});
        assert_eq!(
            String::from_utf8(canonical_json(&value).unwrap()).unwrap(),
            r#"{"a":{"c":"ü\n","z":[{"x":1,"y":2}]},"b":1}"#
        );
    }

    #[test]
    fn canonical_json_refuses_floats() {
        assert!(canonical_json(&json!({"a": 1.5})).is_err());
        assert!(canonical_json(&json!({"a": -3})).is_ok());
    }

    #[test]
    fn signing_input_without_signature_drops_only_the_top_level_field() {
        let value = json!({"signature": "x", "keys": [{"signature": "kept"}]});
        let input = signing_input_without_signature("d\n", &value).unwrap();
        assert_eq!(input, b"d\n{\"keys\":[{\"signature\":\"kept\"}]}".to_vec());
    }

    #[test]
    fn protocol_header_parses_and_checks_major() {
        assert_eq!(parse_protocol_header("1.8"), Some((1, 8)));
        assert!(protocol_compatible("1.0"));
        assert!(!protocol_compatible("2.0"));
        assert!(!protocol_compatible("1"));
        assert!(!protocol_compatible("1.x"));
        assert_eq!(protocol_header_value(), "1.9");
    }

    #[test]
    fn base64_validation_checks_length_and_padding() {
        assert!(validate_b64_len(&"A".repeat(44), 32, "k").is_err());
        assert!(validate_b64_len(&format!("{}=", "A".repeat(43)), 32, "k").is_ok());
        assert!(validate_b64_len(&format!("{}==", "A".repeat(86)), 64, "s").is_ok());
        assert!(validate_b64_len(&format!("{}=", "-".repeat(43)), 32, "k").is_err());
        assert!(validate_b64_bounded("AAAA", 8, "c").is_ok());
        assert!(validate_b64_bounded("AAA", 8, "c").is_err());
    }
}
