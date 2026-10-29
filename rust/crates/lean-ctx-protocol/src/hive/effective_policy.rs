// SPDX-License-Identifier: Apache-2.0

//! Unified Control: the signed effective policy of one engine (Release B,
//! C0 2.x `schemas/policy.schema.json`).
//!
//! `GET /v1/policy/bundle` with `Hive-Protocol: 2.x` keeps every 1.x member
//! and adds `effective_policy` ([`EffectivePolicyBundleV1`], or `null` when
//! the server has no lease signing key). Hive merges the policy hierarchy
//! (org → workspace → vault → engine) itself; the engine never merges, it
//! verifies and applies the result:
//!
//! - signature: Ed25519 by a key-set v3 key of purpose `policy` over
//!   [`EFFECTIVE_POLICY_SIGNATURE_DOMAIN`] ‖ canonical_json(bundle without
//!   `signature`), 128 lowercase hex characters;
//! - `digest` = `"sha256:" + hex(sha256(canonical_json(policy)))`
//!   ([`policy_digest`]), the value the engine acks;
//! - every member carries `x-enforcement`: `engine` members map onto the
//!   gateway policy pack ([`ENGINE_MEMBERS`]), `server` members are enforced
//!   by Hive itself at the point their `x-server` names (`403 policy_denied
//!   {member, level}`, [`SERVER_MEMBERS`]), `advisory` members are
//!   distributed and shown only. The ack's `unenforced` reports what is
//!   enforced nowhere: every member it received that this engine does not
//!   enforce and Hive does not enforce either ([`unenforced_members`]:
//!   advisory members and members this engine does not know yet), never
//!   pretending to enforce them. Server members are left out: Hive
//!   enforces them whatever the engine does ([`server_members`], shown
//!   apart in status).
//!
//! The bundle and its policy sections accept unknown members (they are
//! covered by the signature and reported as unenforced); a known member with
//! a value outside the contract refuses the whole bundle, so the engine keeps
//! its last valid one.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{canonical_json, require_schema, signing_input_without_signature};
use crate::{UtcTimestamp, ValidationError};

pub const EFFECTIVE_POLICY_SCHEMA: &str = "leanctx.hive.effective-policy/v1";
pub const POLICY_DOCUMENT_SCHEMA: &str = "leanctx.hive.policy/v1";
pub const POLICY_ACK_SCHEMA: &str = "leanctx.hive.policy-ack/v1";
/// Domain of the effective-policy signature (key-set v3 purpose `policy`).
pub const EFFECTIVE_POLICY_SIGNATURE_DOMAIN: &str = "leanctx.hive.effective-policy.v1\n";
/// Largest number of `sources` and per-vault entries of one bundle.
pub const MAX_POLICY_SOURCES: usize = 35;
pub const MAX_POLICY_VAULTS: usize = 32;
/// Largest `unenforced` list of an ack.
pub const MAX_UNENFORCED: usize = 64;
/// Error code of an ack for a version/digest this engine was not issued.
pub const POLICY_MISMATCH: &str = "policy_mismatch";

/// Bound of every set member of an effective section.
const MAX_SET: usize = 256;
const MAX_MODEL_GROUPS: usize = 16;
const MAX_MODEL_GROUP: usize = 32;

/// `section.member` → the gateway policy pack field that enforces it, for
/// every member the schema marks `x-enforcement: engine` (its `x-engine`).
pub const ENGINE_MEMBERS: &[(&str, &str)] = &[
    ("context.allowed_tools", "context.allow_tools"),
    ("context.denied_tools", "context.deny_tools"),
    ("context.injection_filter", "filters.injection"),
    ("context.max_context_tokens", "context.max_context_tokens"),
    (
        "context.max_cost_cents_per_person_per_day",
        "budgets.max_cost_usd_per_person_per_day",
    ),
    (
        "context.max_cost_cents_per_project_per_month",
        "budgets.max_cost_usd_per_project_per_month",
    ),
    (
        "context.max_requests_per_minute_per_person",
        "budgets.max_requests_per_minute_per_person",
    ),
    ("context.max_writes_per_min", "egress.max_writes_per_min"),
    ("context.read_mode", "context.default_read_mode"),
    ("security.allowed_models", "routing.model_ceiling_groups"),
    ("security.block_secrets", "egress.block_secrets"),
    ("security.blocked_labels", "filters.blocked_labels"),
    ("security.classification", "filters.classification"),
    ("security.pii", "filters.pii"),
];

/// Every member the schema marks `x-enforcement: server`: Hive enforces it
/// (its `x-server` point), this engine does not.
pub const SERVER_MEMBERS: &[&str] = &[
    "security.excluded_paths",
    "sharing.allowed_vaults",
    "sharing.auto_merge_results",
    "sharing.consent_required_for",
    "sharing.cross_team_sharing",
    "sharing.personal_workspace",
    // Also honoured by the engine's metrics upload (it sends and buffers
    // nothing when off); Hive refuses the upload whatever the engine does.
    "sharing.share_engine_metrics",
    "sharing.task_claim_min_role",
];

/// Every member the schema marks `x-enforcement: advisory`.
pub const ADVISORY_MEMBERS: &[&str] = &[
    "context.allowed_sources",
    "context.auto_extraction",
    "context.extraction_requires_source_anchor",
    "context.max_source_age_days",
    "context.ranking_profile",
    "security.confidential_out_of_sharing",
    "security.external_model_excluded_classes",
];

/// Filter action, loosest first (`x-merge: rank`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyActionV1 {
    Off,
    Warn,
    Redact,
    Block,
}

impl PolicyActionV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Warn => "warn",
            Self::Redact => "redact",
            Self::Block => "block",
        }
    }
}

const READ_MODES: &[&str] = &[
    "auto",
    "full",
    "map",
    "signatures",
    "diff",
    "task",
    "reference",
    "aggressive",
    "entropy",
];
const DATA_CLASSES: &[&str] = &[
    "secrets",
    "pii",
    "confidential",
    "customer_data",
    "source_code",
];
const CLAIM_ROLES: &[&str] = &["reader", "contributor", "maintainer", "admin", "owner"];
/// `sharing.personal_workspace`, loosest first.
pub const PERSONAL_WORKSPACE_MODES: &[&str] = &["allow", "forbid"];
const CONSENT_ACTIONS: &[&str] = &[
    "trust_boundary",
    "permission_expansion",
    "security_command",
    "destructive_change",
    "team_sharing",
    "external_model",
];

/// `security` of an effective policy. Absent member = no level sets it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SecurityPolicyV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_secrets: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pii: Option<PolicyActionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<PolicyActionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_model_excluded_classes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidential_out_of_sharing: Option<bool>,
    /// Members this crate does not know (reported as unenforced).
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

/// `context` of an effective policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextPolicyV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injection_filter: Option<PolicyActionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_sources: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_writes_per_min: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_cents_per_person_per_day: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_cents_per_project_per_month: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_requests_per_minute_per_person: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_extraction: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_requires_source_anchor: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_source_age_days: Option<u64>,
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

/// `sharing` of an effective policy (enforced by Hive, not the engine).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SharingPolicyV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_vaults: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_team_sharing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_claim_min_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge_results: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_required_for: Option<Vec<String>>,
    /// `allow` | `forbid` (Hive C line): `forbid` keeps the team's project
    /// knowledge out of Personal workspaces (Hive enforces W11; this engine
    /// refuses a personal vault for a project it worked on in the team,
    /// `core::hive::company_bound`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personal_workspace: Option<String>,
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

/// The merged policy in normal form (schema `$defs/effective`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectivePolicyV1 {
    pub schema: String,
    pub security: SecurityPolicyV1,
    pub context: ContextPolicyV1,
    pub sharing: SharingPolicyV1,
}

/// The effective policy of one vault with a vault-level document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultEffectivePolicyV1 {
    pub vault_id: String,
    pub effective: EffectivePolicyV1,
}

/// `policy` of a bundle: the default for every vault without an entry, and
/// the per-vault entries ascending by `vault_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePolicyV1 {
    pub effective: EffectivePolicyV1,
    pub vaults: Vec<VaultEffectivePolicyV1>,
}

impl BundlePolicyV1 {
    /// The policy that applies to `vault_id` (`None`: no linked vault).
    pub fn for_vault(&self, vault_id: Option<&str>) -> &EffectivePolicyV1 {
        vault_id
            .and_then(|id| self.vaults.iter().find(|v| v.vault_id == id))
            .map_or(&self.effective, |v| &v.effective)
    }
}

/// A document that went into the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySourceV1 {
    pub level: String,
    pub scope: String,
    pub version: u64,
    pub digest: String,
}

/// `effective_policy` of `GET /v1/policy/bundle` (2.x).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectivePolicyBundleV1 {
    pub schema: String,
    pub org_id: String,
    pub instance_id: String,
    pub version: u64,
    pub issued_at: UtcTimestamp,
    pub not_after: UtcTimestamp,
    pub sources: Vec<PolicySourceV1>,
    pub policy: BundlePolicyV1,
    pub digest: String,
    pub key_id: String,
    pub signature: String,
}

/// `POST /v1/policy/ack`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAckRequestV1 {
    pub version: u64,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unenforced: Vec<String>,
    /// SERVER-B4: the engine received this bundle (`version`, `digest` as
    /// received) but refused it; `unenforced` is then empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<PolicyAckRefusalV1>,
}

/// Why an engine refused a received bundle (C0 `$defs/refusal_reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyRefusalReasonV1 {
    /// Does not parse or validate.
    InvalidBundle,
    /// Issued for another org or engine.
    WrongTarget,
    /// No verified key set v3, or no valid policy key for `key_id`.
    KeyUnavailable,
    SignatureInvalid,
    /// `digest` is not the policy's.
    DigestMismatch,
    /// An older version, or the same version with another digest.
    Downgrade,
    /// `not_after` already passed.
    Expired,
    /// Verified but could not be stored.
    InstallFailed,
}

/// Longest `detail` of a refusal.
pub const MAX_REFUSAL_DETAIL: usize = 300;

/// `refused` of an ack (C0 `$defs/ack_refusal`): the bundle the engine
/// keeps applying (`applied_*` both `null` when it holds none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAckRefusalV1 {
    pub reason_code: PolicyRefusalReasonV1,
    /// The engine's diagnostic line, no content: 1..=300 chars, no control
    /// characters ([`refusal_detail`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub applied_version: Option<u64>,
    pub applied_digest: Option<String>,
}

/// A diagnostic line as `detail` takes it: control characters become
/// spaces, every place on the machine (a path, a home folder, a file URL)
/// becomes `<path>` (K2 M2: the data directory names the user), at most
/// [`MAX_REFUSAL_DETAIL`] characters, `None` when empty.
///
/// A path may hold spaces (`/Users/John Smith/…`), so the whole run is
/// scrubbed: from the first word that names a place up to and including
/// the first word that ends with `:` (the `in <dir>: <reason>` shape), or
/// to the end of the line when no word does.
pub fn refusal_detail(line: &str) -> Option<String> {
    let clean: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let place = |word: &str| {
        word.contains('/')
            || word.contains('\\')
            || word.starts_with('~')
            || (word.len() > 2
                && word.as_bytes()[1] == b':'
                && word.as_bytes()[0].is_ascii_alphabetic())
    };
    let mut words: Vec<&str> = Vec::new();
    let mut run = false;
    for word in clean.split(' ') {
        if !run && !place(word) {
            words.push(word);
            continue;
        }
        run = true;
        if word.ends_with(':') {
            words.push("<path>:");
            run = false;
        }
    }
    if run {
        words.push("<path>");
    }
    let text: String = words.join(" ").chars().take(MAX_REFUSAL_DETAIL).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

impl PolicyAckRequestV1 {
    /// The refusal report of a received bundle.
    pub fn refusal(version: u64, digest: String, refused: PolicyAckRefusalV1) -> Self {
        Self {
            version,
            digest,
            unenforced: Vec::new(),
            refused: Some(refused),
        }
    }
}

/// Answer of `POST /v1/policy/ack` to a refusal (C0
/// `$defs/ack_refused_response`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyAckRefusedResponseV1 {
    pub schema: String,
    pub version: u64,
    pub digest: String,
    /// Always `refused`.
    pub state: String,
    pub reason_code: PolicyRefusalReasonV1,
    pub applied_version: Option<u64>,
    pub applied_digest: Option<String>,
    pub refused_at: UtcTimestamp,
}

impl PolicyAckRefusedResponseV1 {
    /// The answer records exactly the refusal `request`.
    pub fn validate_for(&self, request: &PolicyAckRequestV1) -> Result<(), ValidationError> {
        require_schema(&self.schema, POLICY_ACK_SCHEMA)?;
        let Some(refused) = &request.refused else {
            return Err(ValidationError::new("not a refusal request"));
        };
        if self.state != "refused"
            || self.version != request.version
            || self.digest != request.digest
            || self.reason_code != refused.reason_code
            || self.applied_version != refused.applied_version
            || self.applied_digest != refused.applied_digest
        {
            return Err(ValidationError::new(
                "the refusal answer names another bundle or reason",
            ));
        }
        Ok(())
    }
}

/// State of an ack: `outdated` when a newer bundle was issued since.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAckStateV1 {
    Acked,
    Outdated,
}

/// Answer of `POST /v1/policy/ack`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyAckResponseV1 {
    pub schema: String,
    pub version: u64,
    pub digest: String,
    pub state: PolicyAckStateV1,
    pub acked_at: UtcTimestamp,
}

impl PolicyAckResponseV1 {
    /// The answer acknowledges exactly `request`.
    pub fn validate_for(&self, request: &PolicyAckRequestV1) -> Result<(), ValidationError> {
        require_schema(&self.schema, POLICY_ACK_SCHEMA)?;
        if self.version != request.version || self.digest != request.digest {
            return Err(ValidationError::new(
                "the ack answer names another policy version or digest",
            ));
        }
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// `"sha256:" + hex(sha256(canonical_json(value)))`.
pub fn policy_digest(value: &Value) -> Result<String, ValidationError> {
    Ok(format!(
        "sha256:{}",
        hex(&Sha256::digest(canonical_json(value)?))
    ))
}

/// The exact bytes the `policy` key signs: the raw bundle as received,
/// without its `signature`.
pub fn signing_input_raw(raw: &Value) -> Result<Vec<u8>, ValidationError> {
    signing_input_without_signature(EFFECTIVE_POLICY_SIGNATURE_DOMAIN, raw)
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_digest(value: &str, field: &str) -> Result<(), ValidationError> {
    match value.strip_prefix("sha256:") {
        Some(hex) if is_lower_hex(hex, 64) => Ok(()),
        _ => Err(ValidationError::new(format!(
            "{field} must be sha256: and 64 lowercase hex characters"
        ))),
    }
}

fn prefixed_hex32(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|rest| is_lower_hex(rest, 32))
}

fn valid_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b':' | b'-')
        })
}

/// 1..=`max` characters without C0/C1 control characters or DEL.
fn valid_pattern(value: &str, max: usize) -> bool {
    let count = value.chars().count();
    (1..=max).contains(&count)
        && !value
            .chars()
            .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
}

fn invalid(field: &str) -> ValidationError {
    ValidationError::new(format!("invalid_policy: {field}"))
}

/// A set in normal form: at most [`MAX_SET`] items, strictly ascending
/// (bytewise; sorted and without repeats), each one valid.
fn check_set(
    set: Option<&Vec<String>>,
    field: &str,
    item_ok: impl Fn(&str) -> bool,
) -> Result<(), ValidationError> {
    let Some(set) = set else { return Ok(()) };
    if set.len() > MAX_SET
        || !set.windows(2).all(|w| w[0] < w[1])
        || !set.iter().all(|item| item_ok(item))
    {
        return Err(invalid(field));
    }
    Ok(())
}

fn check_range(value: Option<u64>, min: u64, max: u64, field: &str) -> Result<(), ValidationError> {
    match value {
        Some(v) if !(min..=max).contains(&v) => Err(invalid(field)),
        _ => Ok(()),
    }
}

fn check_enum(
    value: Option<&String>,
    allowed: &[&str],
    field: &str,
) -> Result<(), ValidationError> {
    match value {
        Some(v) if !allowed.contains(&v.as_str()) => Err(invalid(field)),
        _ => Ok(()),
    }
}

impl EffectivePolicyV1 {
    /// Schema, value ranges and normal form of every known member.
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, POLICY_DOCUMENT_SCHEMA)?;
        let s = &self.security;
        check_set(s.excluded_paths.as_ref(), "security.excluded_paths", |p| {
            valid_pattern(p, 256)
        })?;
        check_set(
            s.blocked_labels.as_ref(),
            "security.blocked_labels",
            valid_name,
        )?;
        check_set(
            s.external_model_excluded_classes.as_ref(),
            "security.external_model_excluded_classes",
            |c| DATA_CLASSES.contains(&c),
        )?;
        if let Some(groups) = &s.allowed_models {
            let field = "security.allowed_models";
            if groups.is_empty()
                || groups.len() > MAX_MODEL_GROUPS
                || !groups.windows(2).all(|w| w[0] < w[1])
            {
                return Err(invalid(field));
            }
            for group in groups {
                if group.len() > MAX_MODEL_GROUP {
                    return Err(invalid(field));
                }
                check_set(Some(group), field, |p| valid_pattern(p, 128))?;
            }
        }
        let c = &self.context;
        check_range(
            c.max_context_tokens,
            1,
            10_000_000,
            "context.max_context_tokens",
        )?;
        check_enum(c.read_mode.as_ref(), READ_MODES, "context.read_mode")?;
        if c.ranking_profile.as_deref().is_some_and(|p| !valid_name(p)) {
            return Err(invalid("context.ranking_profile"));
        }
        check_set(
            c.allowed_tools.as_ref(),
            "context.allowed_tools",
            valid_name,
        )?;
        check_set(c.denied_tools.as_ref(), "context.denied_tools", valid_name)?;
        check_set(
            c.allowed_sources.as_ref(),
            "context.allowed_sources",
            valid_name,
        )?;
        check_range(
            c.max_writes_per_min,
            0,
            10_000,
            "context.max_writes_per_min",
        )?;
        check_range(
            c.max_cost_cents_per_person_per_day,
            0,
            100_000_000,
            "context.max_cost_cents_per_person_per_day",
        )?;
        check_range(
            c.max_cost_cents_per_project_per_month,
            0,
            100_000_000,
            "context.max_cost_cents_per_project_per_month",
        )?;
        check_range(
            c.max_requests_per_minute_per_person,
            1,
            1_000_000,
            "context.max_requests_per_minute_per_person",
        )?;
        check_range(
            c.max_source_age_days,
            1,
            3650,
            "context.max_source_age_days",
        )?;
        let h = &self.sharing;
        check_set(h.allowed_vaults.as_ref(), "sharing.allowed_vaults", |v| {
            is_lower_hex(v, 32)
        })?;
        check_enum(
            h.task_claim_min_role.as_ref(),
            CLAIM_ROLES,
            "sharing.task_claim_min_role",
        )?;
        check_set(
            h.consent_required_for.as_ref(),
            "sharing.consent_required_for",
            |a| CONSENT_ACTIONS.contains(&a),
        )?;
        check_enum(
            h.personal_workspace.as_ref(),
            PERSONAL_WORKSPACE_MODES,
            "sharing.personal_workspace",
        )?;
        for (section, other) in [
            ("security", &s.other),
            ("context", &c.other),
            ("sharing", &h.other),
        ] {
            if let Some(name) = other.keys().find(|k| {
                k.is_empty()
                    || k.len() > 64
                    || !k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            }) {
                return Err(invalid(&format!("{section}.{name}")));
            }
        }
        Ok(())
    }

    /// Every member this policy sets, as `section.member`, sorted.
    pub fn members(&self) -> BTreeSet<String> {
        let section = |name: &str, value: Value| -> Vec<String> {
            value
                .as_object()
                .map(|fields| fields.keys().map(|k| format!("{name}.{k}")).collect())
                .unwrap_or_default()
        };
        let mut out = BTreeSet::new();
        out.extend(section(
            "security",
            serde_json::to_value(&self.security).unwrap_or(Value::Null),
        ));
        out.extend(section(
            "context",
            serde_json::to_value(&self.context).unwrap_or(Value::Null),
        ));
        out.extend(section(
            "sharing",
            serde_json::to_value(&self.sharing).unwrap_or(Value::Null),
        ));
        out
    }
}

/// Whether the engine enforces `member` (`section.member`).
pub fn is_engine_member(member: &str) -> bool {
    ENGINE_MEMBERS.iter().any(|(m, _)| *m == member)
}

/// Whether Hive enforces `member` itself (`x-enforcement: server`).
pub fn is_server_member(member: &str) -> bool {
    SERVER_MEMBERS.contains(&member)
}

/// Every member `policy` sets anywhere (default or any vault), sorted.
fn set_members(policy: &BundlePolicyV1) -> BTreeSet<String> {
    let mut all = policy.effective.members();
    for vault in &policy.vaults {
        all.extend(vault.effective.members());
    }
    all
}

/// The members `policy` sets that neither this engine nor Hive enforces
/// (advisory and unknown members), sorted bytewise: what an ack reports.
pub fn unenforced_members(policy: &BundlePolicyV1) -> Vec<String> {
    set_members(policy)
        .into_iter()
        .filter(|m| !is_engine_member(m) && !is_server_member(m))
        .collect()
}

/// The members `policy` sets that Hive enforces itself, sorted bytewise.
pub fn server_members(policy: &BundlePolicyV1) -> Vec<String> {
    set_members(policy)
        .into_iter()
        .filter(|m| is_server_member(m))
        .collect()
}

impl EffectivePolicyBundleV1 {
    /// Everything but the signature and the policy digest, which need the
    /// raw bytes ([`signing_input_raw`], [`policy_digest`]).
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, EFFECTIVE_POLICY_SCHEMA)?;
        if !prefixed_hex32(&self.org_id, "org_") {
            return Err(ValidationError::new("org_id must be org_ and 32 hex"));
        }
        if !prefixed_hex32(&self.instance_id, "inst_") {
            return Err(ValidationError::new("instance_id must be inst_ and 32 hex"));
        }
        if self.version == 0 {
            return Err(ValidationError::new("version starts at 1"));
        }
        if self.not_after <= self.issued_at {
            return Err(ValidationError::new("not_after must follow issued_at"));
        }
        if self.sources.len() > MAX_POLICY_SOURCES {
            return Err(ValidationError::new("too many policy sources"));
        }
        for source in &self.sources {
            // `orgn_` names an organisation's own org-level document (a
            // workspace linked to an organisation, C0 `$defs/source`): only
            // at level `org`.
            let scope_ok = prefixed_hex32(&source.scope, "org_")
                || (source.level == "org" && prefixed_hex32(&source.scope, "orgn_"))
                || prefixed_hex32(&source.scope, "inst_")
                || is_lower_hex(&source.scope, 32);
            if !matches!(
                source.level.as_str(),
                "org" | "workspace" | "vault" | "engine"
            ) || !scope_ok
                || source.version == 0
            {
                return Err(ValidationError::new("a policy source is malformed"));
            }
            validate_digest(&source.digest, "sources.digest")?;
        }
        if self.policy.vaults.len() > MAX_POLICY_VAULTS
            || !self
                .policy
                .vaults
                .windows(2)
                .all(|w| w[0].vault_id < w[1].vault_id)
        {
            return Err(ValidationError::new(
                "policy.vaults must be at most 32, ascending by vault_id",
            ));
        }
        self.policy.effective.validate()?;
        for vault in &self.policy.vaults {
            if !is_lower_hex(&vault.vault_id, 32) {
                return Err(ValidationError::new("policy.vaults.vault_id is malformed"));
            }
            vault.effective.validate()?;
        }
        validate_digest(&self.digest, "digest")?;
        if !is_lower_hex(&self.key_id, 16) {
            return Err(ValidationError::new("key_id must be 16 lowercase hex"));
        }
        if !is_lower_hex(&self.signature, 128) {
            return Err(ValidationError::new("signature must be 128 lowercase hex"));
        }
        Ok(())
    }

    /// The ack for this bundle as applied. The wire carries at most
    /// [`MAX_UNENFORCED`] members (the schema bound, more than the contract
    /// defines); the full list stays in the engine's own status.
    pub fn ack(&self) -> PolicyAckRequestV1 {
        let mut unenforced = unenforced_members(&self.policy);
        unenforced.truncate(MAX_UNENFORCED);
        PolicyAckRequestV1 {
            version: self.version,
            digest: self.digest.clone(),
            unenforced,
            refused: None,
        }
    }
}

#[cfg(test)]
#[path = "effective_policy_tests.rs"]
mod tests;
