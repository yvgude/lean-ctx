// SPDX-License-Identifier: Apache-2.0

//! Instance lease and the Hive signing key set.
//!
//! - `GET /v1/keys` → [`HiveKeySetV1`], signed by the Hive root key that is
//!   compiled into the engine (self-host: `[hive] root_key`).
//! - The lease arrives in enrollment and from `lease/renew` as
//!   [`HiveLeaseEnvelopeV1`], signed by a key of the key set.
//!
//! A lease is `Valid` until `expires_at`, `Grace` until `grace_until`, then
//! `Lapsed`. Local tools never depend on the lease; only Hive traffic does.

use chrono::{DateTime, Duration, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    KEYSET_SIGNATURE_DOMAIN, LEASE_SIGNATURE_DOMAIN, parse_time, require_schema, signing_input,
    signing_input_without_signature, validate_b64_len, validate_id,
};
use crate::{UtcTimestamp, ValidationError};

pub const LEASE_SCHEMA: &str = "leanctx.hive.lease/v1";
pub const LEASE_ENVELOPE_SCHEMA: &str = "leanctx.hive.lease-envelope/v1";
pub const KEYSET_SCHEMA: &str = "leanctx.hive.keyset/v1";

/// Longest `expires_at - issued_at`.
pub const MAX_LEASE_VALIDITY_HOURS: i64 = 24;
/// Longest `grace_until - expires_at`.
pub const MAX_LEASE_GRACE_DAYS: i64 = 7;
/// A lease issued further than this in the future (after offset correction) is refused.
pub const MAX_LEASE_FUTURE_SKEW_MINUTES: i64 = 15;
/// The engine renews when less than this remains before `expires_at`.
pub const LEASE_RENEW_BEFORE_HOURS: i64 = 6;

/// Plan of the organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HivePlan {
    /// A workspace without a paid plan: Hive sends `none`.
    #[serde(rename = "none")]
    NoPlan,
    Personal,
    Team,
    Enterprise,
    #[serde(other)]
    Unknown,
}

impl HivePlan {
    /// The plan's name as people read it: Personal, Team and Enterprise.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::NoPlan => "No plan",
            Self::Personal => "Personal",
            Self::Team => "Team",
            Self::Enterprise => "Enterprise",
            Self::Unknown => "unknown",
        }
    }

    /// The plan's product name: Personal, Team, Enterprise.
    pub fn product_name(self) -> &'static str {
        match self {
            Self::NoPlan => "No plan",
            Self::Personal => "Personal",
            Self::Team => "Team",
            Self::Enterprise => "Enterprise",
            Self::Unknown => "Unknown",
        }
    }
}

/// A right granted to this instance. Unknown rights decode as `Unknown` and grant nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HiveRight {
    #[serde(rename = "knowledge.read")]
    KnowledgeRead,
    #[serde(rename = "knowledge.draft")]
    KnowledgeDraft,
    #[serde(rename = "knowledge.merge")]
    KnowledgeMerge,
    #[serde(rename = "metrics.upload")]
    MetricsUpload,
    #[serde(rename = "receipts.upload")]
    ReceiptsUpload,
    #[serde(rename = "policy.pull")]
    PolicyPull,
    #[serde(rename = "bus.relay")]
    BusRelay,
    #[serde(rename = "bus.control")]
    BusControl,
    #[serde(rename = "sealed")]
    Sealed,
    /// 1.2: the Intelligence runtime may run for this instance (what it
    /// enables stays gated by `plan`).
    #[serde(rename = "runtime")]
    Runtime,
    /// 1.9: read the vault's tasks.
    #[serde(rename = "tasks.read")]
    TasksRead,
    /// 1.9: claim tasks, report updates and submit results.
    #[serde(rename = "tasks.work")]
    TasksWork,
    /// 1.9: propose tasks (a person approves them).
    #[serde(rename = "tasks.propose")]
    TasksPropose,
    #[serde(other)]
    Unknown,
}

/// Org policy signing key pinned by the lease (no trust on first use).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySignerKeyV1 {
    pub org: String,
    /// Ed25519 public key, 32 bytes base64.
    pub ed25519_pub: String,
}

/// The signed lease statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveLeaseV1 {
    pub schema: String,
    pub lease_id: String,
    pub org_id: String,
    pub instance_id: String,
    pub plan: HivePlan,
    /// The organization's limit of engine instances (Personal 2,
    /// Team 2 per seat, Enterprise 3 per seat and at least 25); the wire
    /// name is frozen in v1.
    pub seats_limit: u32,
    pub rights: Vec<HiveRight>,
    pub policy_signer_keys: Vec<PolicySignerKeyV1>,
    pub issued_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub grace_until: UtcTimestamp,
    /// Service clock at signing; the engine keeps `server_time - local_now` as offset.
    pub server_time: UtcTimestamp,
    /// 1.4: what the organization allows its engines to upload. Absent
    /// (an older service) allows both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_settings: Option<HiveOrgSettingsV1>,
}

/// Organization-wide switches the lease carries (1.4). They only narrow:
/// an engine uploads only what both its own `[hive]` setting and these
/// allow, and receipts still need the `receipts.upload` right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveOrgSettingsV1 {
    #[serde(default = "allowed")]
    pub metrics_upload: bool,
    #[serde(default = "allowed")]
    pub receipts_upload: bool,
}

const fn allowed() -> bool {
    true
}

/// Where a lease stands at a given (offset-corrected) time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Valid,
    Grace,
    Lapsed,
}

impl HiveLeaseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, LEASE_SCHEMA)?;
        validate_id(&self.lease_id, "lease_id")?;
        validate_id(&self.org_id, "org_id")?;
        validate_id(&self.instance_id, "instance_id")?;
        if self.seats_limit == 0 {
            return Err(ValidationError::new("seats_limit must be positive"));
        }
        if self.rights.len() > 64 || self.policy_signer_keys.len() > 16 {
            return Err(ValidationError::new("lease lists are too long"));
        }
        for key in &self.policy_signer_keys {
            validate_id(&key.org, "policy_signer_keys.org")?;
            validate_b64_len(&key.ed25519_pub, 32, "policy_signer_keys.ed25519_pub")?;
        }
        let issued = parse_time(&self.issued_at)?;
        let expires = parse_time(&self.expires_at)?;
        let grace = parse_time(&self.grace_until)?;
        parse_time(&self.server_time)?;
        if expires <= issued || expires - issued > Duration::hours(MAX_LEASE_VALIDITY_HOURS) {
            return Err(ValidationError::new(
                "expires_at must be after issued_at and at most 24 h later",
            ));
        }
        if grace < expires || grace - expires > Duration::days(MAX_LEASE_GRACE_DAYS) {
            return Err(ValidationError::new(
                "grace_until must be within 7 days after expires_at",
            ));
        }
        Ok(())
    }

    /// Whether the lease grants `right`.
    pub fn grants(&self, right: HiveRight) -> bool {
        right != HiveRight::Unknown && self.rights.contains(&right)
    }

    /// Whether the organization allows metrics uploads (absent settings allow).
    pub fn org_allows_metrics_upload(&self) -> bool {
        self.org_settings
            .is_none_or(|settings| settings.metrics_upload)
    }

    /// Whether the organization allows receipt uploads (absent settings allow).
    pub fn org_allows_receipts_upload(&self) -> bool {
        self.org_settings
            .is_none_or(|settings| settings.receipts_upload)
    }

    /// State at `now` (service time: local clock plus the stored offset).
    /// A lease issued more than 15 minutes after `now` counts as `Lapsed`.
    pub fn state_at(&self, now: DateTime<FixedOffset>) -> Result<LeaseState, ValidationError> {
        let issued = parse_time(&self.issued_at)?;
        if issued - now > Duration::minutes(MAX_LEASE_FUTURE_SKEW_MINUTES) {
            return Ok(LeaseState::Lapsed);
        }
        Ok(if now < parse_time(&self.expires_at)? {
            LeaseState::Valid
        } else if now < parse_time(&self.grace_until)? {
            LeaseState::Grace
        } else {
            LeaseState::Lapsed
        })
    }

    /// Whether the engine should renew now.
    pub fn renew_due(&self, now: DateTime<FixedOffset>) -> Result<bool, ValidationError> {
        Ok(parse_time(&self.expires_at)? - now < Duration::hours(LEASE_RENEW_BEFORE_HOURS))
    }
}

/// Lease as it travels: the raw lease object plus its signature.
///
/// `lease` stays raw JSON so the signature covers exactly what the service
/// signed, including fields this engine does not know.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HiveLeaseEnvelopeV1 {
    pub schema: String,
    pub lease: Value,
    /// Key id within [`HiveKeySetV1`].
    pub kid: String,
    /// Ed25519 signature (base64) over [`HiveLeaseEnvelopeV1::signing_input`].
    pub signature: String,
}

impl HiveLeaseEnvelopeV1 {
    /// Envelope fields only; call [`Self::lease`] after verifying the signature.
    pub fn validate_shape(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, LEASE_ENVELOPE_SCHEMA)?;
        validate_id(&self.kid, "kid")?;
        validate_b64_len(&self.signature, 64, "signature")?;
        if !self.lease.is_object() {
            return Err(ValidationError::new("lease must be a JSON object"));
        }
        Ok(())
    }

    /// `"leanctx.hive.lease.v1\n" || canonical_json(lease)`.
    pub fn signing_input(&self) -> Result<Vec<u8>, ValidationError> {
        signing_input(LEASE_SIGNATURE_DOMAIN, &self.lease)
    }

    /// Signing input for a lease the service is about to sign.
    pub fn signing_input_for(lease: &HiveLeaseV1) -> Result<Vec<u8>, ValidationError> {
        lease.validate()?;
        signing_input(LEASE_SIGNATURE_DOMAIN, &lease_value(lease)?)
    }

    /// Build the envelope for a signed lease.
    pub fn seal(
        lease: &HiveLeaseV1,
        kid: impl Into<String>,
        signature: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        lease.validate()?;
        let envelope = Self {
            schema: LEASE_ENVELOPE_SCHEMA.into(),
            lease: lease_value(lease)?,
            kid: kid.into(),
            signature: signature.into(),
        };
        envelope.validate_shape()?;
        Ok(envelope)
    }

    /// The typed, validated lease.
    pub fn lease(&self) -> Result<HiveLeaseV1, ValidationError> {
        let lease: HiveLeaseV1 = serde_json::from_value(self.lease.clone())
            .map_err(|error| ValidationError::new(format!("lease: {error}")))?;
        lease.validate()?;
        Ok(lease)
    }
}

fn lease_value(lease: &HiveLeaseV1) -> Result<Value, ValidationError> {
    serde_json::to_value(lease).map_err(|error| ValidationError::new(format!("lease: {error}")))
}

/// One Hive signing key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveKeyV1 {
    pub kid: String,
    /// Ed25519 public key, 32 bytes base64.
    pub ed25519_pub: String,
    /// The key signs nothing after this time; signatures made before stay valid
    /// until the signed object itself expires.
    pub not_after: UtcTimestamp,
}

/// `GET /v1/keys`. The signature covers the object without `signature`.
/// Engines keep the raw JSON (`keys.json`) and verify it with
/// [`HiveKeySetV1::signing_input_raw`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveKeySetV1 {
    pub schema: String,
    pub keys: Vec<HiveKeyV1>,
    /// Ed25519 signature (base64) of the Hive root key.
    pub signature: String,
}

impl HiveKeySetV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, KEYSET_SCHEMA)?;
        if self.keys.is_empty() || self.keys.len() > 16 {
            return Err(ValidationError::new("keys must list 1-16 keys"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for key in &self.keys {
            validate_id(&key.kid, "kid")?;
            validate_b64_len(&key.ed25519_pub, 32, "ed25519_pub")?;
            parse_time(&key.not_after)?;
            if !seen.insert(key.kid.as_str()) {
                return Err(ValidationError::new("kid repeats in key set"));
            }
        }
        validate_b64_len(&self.signature, 64, "signature")
    }

    /// Signing input of a key set as received.
    pub fn signing_input_raw(raw: &Value) -> Result<Vec<u8>, ValidationError> {
        signing_input_without_signature(KEYSET_SIGNATURE_DOMAIN, raw)
    }

    /// Signing input of a key set the service is about to sign (`signature` ignored).
    pub fn signing_input(&self) -> Result<Vec<u8>, ValidationError> {
        let value = serde_json::to_value(self)
            .map_err(|error| ValidationError::new(format!("keyset: {error}")))?;
        Self::signing_input_raw(&value)
    }

    /// Key `kid`, if listed.
    pub fn key(&self, kid: &str) -> Option<&HiveKeyV1> {
        self.keys.iter().find(|key| key.kid == kid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_names_are_personal_team_enterprise() {
        let names: Vec<&str> = [
            HivePlan::NoPlan,
            HivePlan::Personal,
            HivePlan::Team,
            HivePlan::Enterprise,
        ]
        .into_iter()
        .map(HivePlan::product_name)
        .collect();
        assert_eq!(names, ["No plan", "Personal", "Team", "Enterprise"]);
        // the wire name stays
        assert_eq!(
            serde_json::to_value(HivePlan::Enterprise).unwrap(),
            serde_json::json!("enterprise")
        );
    }

    fn lease() -> HiveLeaseV1 {
        HiveLeaseV1 {
            schema: LEASE_SCHEMA.into(),
            lease_id: "lse_1".into(),
            org_id: "org_1".into(),
            instance_id: "inst_1".into(),
            plan: HivePlan::Team,
            seats_limit: 25,
            rights: vec![HiveRight::KnowledgeRead],
            policy_signer_keys: vec![],
            issued_at: UtcTimestamp::new("2026-10-20T12:00:00Z").unwrap(),
            expires_at: UtcTimestamp::new("2026-10-21T12:00:00Z").unwrap(),
            grace_until: UtcTimestamp::new("2026-10-28T12:00:00Z").unwrap(),
            server_time: UtcTimestamp::new("2026-10-20T12:00:00Z").unwrap(),
            org_settings: None,
        }
    }

    fn at(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    /// Dogfood 25.10.: a personal workspace (plan `none`) showed "unknown
    /// plan" at connect.
    #[test]
    fn plan_wire_ids_are_none_personal_team_enterprise() {
        let plan = |s: &str| serde_json::from_value::<HivePlan>(serde_json::json!(s)).unwrap();
        assert_eq!(plan("none"), HivePlan::NoPlan);
        assert_eq!(plan("personal"), HivePlan::Personal);
        assert_eq!(plan("none").display_name(), "No plan");
        assert_eq!(plan("team").display_name(), "Team");
        assert_eq!(plan("enterprise").display_name(), "Enterprise");
        // The retired ids are not aliases.
        assert_eq!(plan("pro"), HivePlan::Unknown);
        assert_eq!(plan("business"), HivePlan::Unknown);
        assert_eq!(plan("community"), HivePlan::Unknown);
        assert_eq!(
            plan("galaxy"),
            HivePlan::Unknown,
            "new plans stay tolerated"
        );
        assert_eq!(serde_json::to_value(HivePlan::NoPlan).unwrap(), "none");
        assert_eq!(
            serde_json::to_value(HivePlan::Personal).unwrap(),
            "personal"
        );
    }

    #[test]
    fn lease_moves_from_valid_to_grace_to_lapsed() {
        let lease = lease();
        assert_eq!(
            lease.state_at(at("2026-10-21T11:59:59Z")).unwrap(),
            LeaseState::Valid
        );
        assert_eq!(
            lease.state_at(at("2026-10-21T12:00:00Z")).unwrap(),
            LeaseState::Grace
        );
        assert_eq!(
            lease.state_at(at("2026-10-28T12:00:00Z")).unwrap(),
            LeaseState::Lapsed
        );
        assert_eq!(
            lease.state_at(at("2026-10-20T11:44:59Z")).unwrap(),
            LeaseState::Lapsed
        );
        assert!(lease.renew_due(at("2026-10-21T06:00:01Z")).unwrap());
        assert!(!lease.renew_due(at("2026-10-21T05:59:59Z")).unwrap());
    }

    #[test]
    fn lease_windows_are_bounded() {
        let mut long = lease();
        long.expires_at = UtcTimestamp::new("2026-10-21T12:00:01Z").unwrap();
        assert!(long.validate().is_err());
        let mut grace = lease();
        grace.grace_until = UtcTimestamp::new("2026-10-28T12:00:01Z").unwrap();
        assert!(grace.validate().is_err());
        assert!(lease().validate().is_ok());
    }

    #[test]
    fn signature_covers_unknown_lease_fields() {
        let mut envelope =
            HiveLeaseEnvelopeV1::seal(&lease(), "k1", "A".repeat(86) + "==").unwrap();
        let before = envelope.signing_input().unwrap();
        envelope.lease["future_field"] = Value::from(1);
        assert_ne!(before, envelope.signing_input().unwrap());
        assert!(envelope.lease().is_ok());
    }

    #[test]
    fn unknown_rights_grant_nothing() {
        let mut lease = lease();
        lease.rights.push(HiveRight::Unknown);
        assert!(!lease.grants(HiveRight::Unknown));
        assert!(lease.grants(HiveRight::KnowledgeRead));
        assert!(!lease.grants(HiveRight::KnowledgeMerge));
    }
}
