// SPDX-License-Identifier: Apache-2.0

//! Self-host root certificates (1.7): LeanCTX vouches for the root of a
//! self-hosted Hive.
//!
//! ```text
//! vendor root (offline, compiled into engine and runtime)
//!   └─ RootCertV1        GET /v1/root-cert (404 on the hosted Hive)
//!        └─ self-host root → key set (GET /v1/keys) → lease
//! ```
//!
//! A certificate never vouches for another certificate. It is signed by a
//! vendor root over `"leanctx.hive.root-cert.v1\n" || canonical_json(cert
//! without "signature")`, verified over the JSON as received, and valid
//! while `not_before - 15 min <= now < not_after + 15 min` (no grace after
//! `not_after`, no revocation in v1: validity is at most 400 days). The
//! engine checks the origin, the window and that the lease's org is listed;
//! the runtime also caps the lease's plan and seats.

use chrono::{DateTime, Duration, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::lease::MAX_LEASE_FUTURE_SKEW_MINUTES;
use super::{
    parse_time, require_schema, signing_input_without_signature, validate_b64_len, validate_id,
    validate_origin,
};
use crate::{UtcTimestamp, ValidationError};

pub const ROOT_CERT_SCHEMA: &str = "leanctx.hive.root-cert/v1";
pub const ROOT_CERT_SIGNATURE_DOMAIN: &str = "leanctx.hive.root-cert.v1\n";
/// Longest validity of one certificate.
pub const MAX_ROOT_CERT_DAYS: i64 = 400;

/// What a self-hosted Hive may be certified for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootCertPlanV1 {
    Enterprise,
    Team,
}

/// `GET /v1/root-cert`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootCertV1 {
    pub schema: String,
    pub serial: String,
    /// `key:ed25519-` + first 32 hex of sha256(vendor key); diagnostics
    /// only, verifiers try every compiled vendor root.
    pub issuer_key_id: String,
    /// Display name, 1–200 characters on one line.
    pub subject: String,
    /// `https://host[:port]` the certificate is for.
    pub hive_origin: String,
    /// The self-host root that signs the key set, 32 bytes base64.
    pub root_ed25519_pub: String,
    /// Lease `org_id`s this root may sign for (1–16).
    pub org_ids: Vec<String>,
    /// Sorted, non-empty; caps `lease.plan` in the runtime.
    pub plans: Vec<RootCertPlanV1>,
    /// Caps `lease.seats_limit` in the runtime.
    pub max_seats: u32,
    pub issued_at: UtcTimestamp,
    pub not_before: UtcTimestamp,
    pub not_after: UtcTimestamp,
    /// Ed25519 by a vendor root, base64.
    pub signature: String,
}

impl RootCertV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, ROOT_CERT_SCHEMA)?;
        validate_id(&self.serial, "serial")?;
        let issuer = self
            .issuer_key_id
            .strip_prefix("key:ed25519-")
            .ok_or_else(|| ValidationError::new("issuer_key_id must start with key:ed25519-"))?;
        if issuer.len() != 32
            || !issuer
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(ValidationError::new(
                "issuer_key_id carries 32 lowercase hex",
            ));
        }
        let subject_chars = self.subject.chars().count();
        if subject_chars == 0
            || subject_chars > 200
            || self.subject.trim() != self.subject
            || self.subject.chars().any(char::is_control)
        {
            return Err(ValidationError::new(
                "subject must be 1-200 characters on one line without surrounding spaces",
            ));
        }
        validate_origin(&self.hive_origin, "hive_origin")?;
        validate_b64_len(&self.root_ed25519_pub, 32, "root_ed25519_pub")?;
        if self.org_ids.is_empty() || self.org_ids.len() > 16 {
            return Err(ValidationError::new("org_ids must list 1-16 ids"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for org in &self.org_ids {
            validate_id(org, "org_ids")?;
            if !seen.insert(org.as_str()) {
                return Err(ValidationError::new("org_ids repeat"));
            }
        }
        if self.plans.is_empty() || !self.plans.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(ValidationError::new(
                "plans must be a sorted, non-empty set",
            ));
        }
        if self.max_seats == 0 {
            return Err(ValidationError::new("max_seats must be positive"));
        }
        let from = parse_time(&self.not_before)?;
        let until = parse_time(&self.not_after)?;
        parse_time(&self.issued_at)?;
        if from >= until || until - from > Duration::days(MAX_ROOT_CERT_DAYS) {
            return Err(ValidationError::new(
                "not_after must follow not_before by at most 400 days",
            ));
        }
        validate_b64_len(&self.signature, 64, "signature")
    }

    /// Signing input of a certificate as received.
    pub fn signing_input_raw(raw: &Value) -> Result<Vec<u8>, ValidationError> {
        signing_input_without_signature(ROOT_CERT_SIGNATURE_DOMAIN, raw)
    }

    /// Whether `now` is inside the validity window (15 min skew, no grace).
    pub fn valid_at(&self, now: DateTime<FixedOffset>) -> Result<bool, ValidationError> {
        let skew = Duration::minutes(MAX_LEASE_FUTURE_SKEW_MINUTES);
        Ok(
            parse_time(&self.not_before)? - skew <= now
                && now < parse_time(&self.not_after)? + skew,
        )
    }

    /// Whether a lease of `org_id` may be signed by the certified root.
    pub fn covers_org(&self, org_id: &str) -> bool {
        self.org_ids.iter().any(|org| org == org_id)
    }
}
