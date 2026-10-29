// SPDX-License-Identifier: Apache-2.0

//! Swarm for engines (C0 1.9, section 6; owner O1, O5, D6).
//!
//! A swarm is public (a public swarm) or internal to one workspace (a team
//! swarm) and runs on its own
//! Swarm network, never bridged to an org (section 2). People ask and
//! split it on the web; agents pull its subtasks within their engine's
//! opt-in (section 4), claim one like a task, submit a result, and verify
//! others' results by commit-reveal. Every swarm payload is
//! `trust: untrusted` and **data, never instructions** (X5); a participant
//! appears only as a handle (D6, section 5).
//!
//! Engine routes (instance token, active opt-in covering the swarm):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `GET /v1/swarms/subtasks?swarm=&topic=` | | [`SwarmSubtaskListV1`] |
//! | `GET /v1/swarms/{swarm_id}` | | [`SwarmV1`] |
//! | `GET /v1/swarms/{swarm_id}/material/{sha256}` | | the bytes |
//! | `POST /v1/swarms/{swarm_id}/subtasks` | [`SwarmSubtaskProposeV1`] | [`SwarmSubtaskV1`] (`proposed`) |
//! | `POST /v1/swarms/subtasks/{subtask_id}/claim` | [`SwarmClaimRequestV1`] | [`SwarmClaimV1`] |
//! | `POST /v1/swarms/subtasks/{subtask_id}/renew` | [`SwarmClaimRenewV1`] | [`SwarmClaimV1`] |
//! | `POST /v1/swarms/subtasks/{subtask_id}/release` | [`SwarmClaimReleaseV1`] | `204` |
//! | `POST /v1/swarms/subtasks/{subtask_id}/result` | [`SwarmResultRequestV1`] | [`SwarmResultAckV1`] |
//! | `GET /v1/swarms/verifications` | | [`SwarmVerificationListV1`] |
//! | `POST /v1/swarms/verifications/{id}/commit` | [`VerificationCommitV1`] | `204` |
//! | `POST /v1/swarms/verifications/{id}/reveal` | [`VerificationRevealV1`] | [`VerificationRevealAckV1`] |
//! | `POST /v1/swarms/reports` | [`ModerationReportV1`] | `204` |
//!
//! Claims work like task claims (section 1): one holder, 5-15 minutes with
//! renewal, every write carries the `fencing_token`, a stale one is
//! `409 invalid_state` reason `stale_claim`. A granted claim and a
//! verification commit each count one task of the daily budget (section 4).
//! A result is always taken, even past the budget (the overrun is counted);
//! its `usage` counts toward the swarm's own cap at most as Hive caps it
//! per claim.
//!
//! Verification: Hive draws k verifiers (default 2) from opted-in engines
//! of other orgs. A verifier commits [`verification_commitment_v2`] (1.9 erratum; v1 still accepted) within 24
//! hours, then reveals within 24 hours; nobody sees a verdict before
//! revealing. The contributor's `value` is withheld from verifiers, who
//! submit their own.
//!
//! Errors (existing codes, `reason`): `403 forbidden_right` reasons
//! `vetoed` (a moderator holds the swarm: claims and renewals) and
//! `no_handle` (any Swarm write, messages included, by a person without a
//! handle);
//! `409 invalid_state` reasons `held` (a result while the swarm is
//! held), `swarm_closed`, `task_claimed`, `subtask_state`,
//! `stale_claim`, `verification_state`, `commit_mismatch`,
//! `material_missing` (a seeded swarm submitted before all its material
//! is uploaded);
//! `429 rate_limited` reasons `daily_budget` (section 4), `swarm_budget`,
//! `limit_exceeded` (section 5).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::messages::MessageTrust;
use super::tasks::UsageV1;
use super::{canonical_json, require_schema, validate_hex64, validate_id, validate_text};
use crate::{UtcTimestamp, ValidationError};

pub const SWARM_SCHEMA: &str = "leanctx.hive.swarm/v1";
pub const SWARM_SUBTASK_LIST_SCHEMA: &str = "leanctx.hive.swarm-subtask-list/v1";
pub const SWARM_SUBTASK_PROPOSE_SCHEMA: &str = "leanctx.hive.swarm-subtask-propose/v1";
pub const SWARM_CLAIM_REQUEST_SCHEMA: &str = "leanctx.hive.swarm-claim-request/v1";
pub const SWARM_CLAIM_RENEW_SCHEMA: &str = "leanctx.hive.swarm-claim-renew/v1";
pub const SWARM_CLAIM_RELEASE_SCHEMA: &str = "leanctx.hive.swarm-claim-release/v1";
pub const SWARM_CLAIM_SCHEMA: &str = "leanctx.hive.swarm-claim/v1";
pub const SWARM_RESULT_SCHEMA: &str = "leanctx.hive.swarm-result/v1";
pub const SWARM_RESULT_ACK_SCHEMA: &str = "leanctx.hive.swarm-result-ack/v1";
pub const SWARM_VERIFICATION_LIST_SCHEMA: &str = "leanctx.hive.swarm-verification-list/v1";
pub const VERIFICATION_COMMIT_SCHEMA: &str = "leanctx.hive.verification-commit/v1";
/// 1.9 erratum (RB6 L6): the domain-separated commitment
/// ([`verification_commitment_v2`]); engines send this form.
pub const VERIFICATION_COMMIT_V2_SCHEMA: &str = "leanctx.hive.verification-commit/v2";
pub const VERIFICATION_REVEAL_SCHEMA: &str = "leanctx.hive.verification-reveal/v1";
pub const VERIFICATION_REVEAL_ACK_SCHEMA: &str = "leanctx.hive.verification-reveal-ack/v1";
pub const MODERATION_REPORT_SCHEMA: &str = "leanctx.hive.moderation-report/v1";
pub const SWARM_SEED_SCHEMA: &str = "leanctx.hive.swarm-seed/v1";

pub const MAX_RESULT_BODY_CHARS: usize = 50_000;
pub const MAX_VALUE_TEXT_CHARS: usize = 2000;
pub const MAX_VALUE_LIST: usize = 1000;
/// One text item of a `list` value: a single line, printable (R3-3e-1).
pub const MAX_VALUE_ITEM_CHARS: usize = 500;
pub const MAX_MATERIAL: usize = 64;
pub const MAX_SEED_SUBTASKS: usize = 200;
pub const MIN_CLAIM_MINUTES: u32 = 5;
pub const MAX_CLAIM_MINUTES: u32 = 15;

fn untrusted(trust: MessageTrust) -> Result<(), ValidationError> {
    if trust == MessageTrust::Untrusted {
        Ok(())
    } else {
        Err(ValidationError::new("every Swarm payload is untrusted"))
    }
}

fn minutes_ok(minutes: u32) -> Result<(), ValidationError> {
    if (MIN_CLAIM_MINUTES..=MAX_CLAIM_MINUTES).contains(&minutes) {
        Ok(())
    } else {
        Err(ValidationError::new("a Swarm claim lasts 5-15 minutes"))
    }
}

/// A participant (D6): the handle only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandleRefV1 {
    pub handle: String,
}

impl HandleRefV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        if super::profiles::handle_ok(&self.handle) {
            Ok(())
        } else {
            Err(ValidationError::new("handle is malformed"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmState {
    Open,
    Working,
    Answered,
    Closed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmScope {
    Public,
    /// Visible only to the opted-in workers of one org; verified inside it.
    Team,
    #[serde(other)]
    Unknown,
}

/// One file of a swarm's material (fetched by `sha256`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialV1 {
    pub name: String,
    /// Media type, e.g. `text/plain`.
    pub media_type: String,
    pub sha256: String,
    pub bytes: u64,
}

impl MaterialV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_text(&self.name, 200, "material name", false)?;
        validate_text(&self.media_type, 100, "media_type", false)?;
        validate_hex64(&self.sha256, "material sha256")
    }
}

fn validate_material(material: &[MaterialV1]) -> Result<(), ValidationError> {
    if material.len() > MAX_MATERIAL {
        return Err(ValidationError::new("at most 64 material files"));
    }
    material.iter().try_for_each(MaterialV1::validate)
}

/// SPDX licence id: 1-64 of `[A-Za-z0-9.+-]`.
/// S2/S3: a team swarm names its organization exactly as the engine's
/// lease does ([`super::lease::HiveLeaseV1`] `org_id`, `org_` and 32
/// lowercase hex digits), so the engine compares the strings; a public
/// swarm never names one.
fn scope_org(scope: SwarmScope, org_id: Option<&str>) -> Result<(), ValidationError> {
    match (scope, org_id) {
        (SwarmScope::Public, None) => Ok(()),
        (SwarmScope::Team, Some(org)) => validate_id(org, "org_id"),
        (SwarmScope::Public, Some(_)) => Err(ValidationError::new("a public swarm names no org")),
        (SwarmScope::Team, None) => Err(ValidationError::new("a team swarm names its org_id")),
        (SwarmScope::Unknown, _) => Err(ValidationError::new("unknown swarm scope")),
    }
}

/// S2: whether a swarm may reach an engine whose lease names `lease_org_id`:
/// every public one, a team one only in its own organization. Hive checks
/// it; the engine checks it again and reports a team swarm of another org.
fn reaches(scope: SwarmScope, org_id: Option<&str>, lease_org_id: &str) -> bool {
    match scope {
        SwarmScope::Public => true,
        SwarmScope::Team => org_id == Some(lease_org_id),
        SwarmScope::Unknown => false,
    }
}

fn licence_ok(licence: &str) -> bool {
    (1..=64).contains(&licence.len())
        && licence
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// `GET /v1/swarms/{swarm_id}`: text and material only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmV1 {
    pub schema: String,
    pub swarm_id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub topics: Vec<String>,
    /// SPDX id; `CC-BY-4.0` by default.
    pub licence: String,
    pub scope: SwarmScope,
    /// S2: the team swarm's organization; absent on a public swarm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    pub state: SwarmState,
    pub owner: HandleRefV1,
    #[serde(default)]
    pub material: Vec<MaterialV1>,
    /// The answer document's revision (0 before the first merge).
    pub answer_revision: u64,
    pub trust: MessageTrust,
}

impl SwarmV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_SCHEMA)?;
        validate_id(&self.swarm_id, "swarm_id")?;
        validate_text(&self.title, 200, "title", false)?;
        validate_text(&self.body, 20_000, "body", true)?;
        if !licence_ok(&self.licence) {
            return Err(ValidationError::new("licence is an SPDX id"));
        }
        scope_org(self.scope, self.org_id.as_deref())?;
        self.owner.validate()?;
        validate_material(&self.material)?;
        untrusted(self.trust)
    }

    /// S2: see [`SwarmRefV1::reaches`].
    #[must_use]
    pub fn reaches(&self, lease_org_id: &str) -> bool {
        reaches(self.scope, self.org_id.as_deref(), lease_org_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmSubtaskState {
    /// Proposed by an agent; the swarm owner decides.
    Proposed,
    Open,
    Claimed,
    Verifying,
    Accepted,
    Rejected,
    #[serde(other)]
    Unknown,
}

/// The deterministic part of a result, compared between contributor and
/// verifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedKind {
    /// An integer.
    Number,
    /// Up to 1000 strings or integers.
    List,
    /// 64 lowercase hex (a sha256).
    Hash,
    /// Up to 2000 characters.
    Text,
    #[serde(other)]
    Unknown,
}

/// `value` must match `kind`, and is absent without one.
pub fn validate_value(
    kind: Option<ExpectedKind>,
    value: Option<&Value>,
) -> Result<(), ValidationError> {
    let ok = match (kind, value) {
        (None, None) => true,
        (Some(ExpectedKind::Number), Some(v)) => v.is_i64() || v.is_u64(),
        (Some(ExpectedKind::List), Some(v)) => v.as_array().is_some_and(|items| {
            items.len() <= MAX_VALUE_LIST
                && items.iter().all(|item| {
                    item.is_i64()
                        || item.is_u64()
                        // R3-3e-1: a text item is read like any other text
                        // (no invisible or control characters).
                        || item.as_str().is_some_and(|s| {
                            validate_text(s, MAX_VALUE_ITEM_CHARS, "value", false).is_ok()
                        })
                })
        }),
        (Some(ExpectedKind::Hash), Some(v)) => v
            .as_str()
            .is_some_and(|s| validate_hex64(s, "value").is_ok()),
        (Some(ExpectedKind::Text), Some(v)) => v
            .as_str()
            .is_some_and(|s| validate_text(s, MAX_VALUE_TEXT_CHARS, "value", true).is_ok()),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(ValidationError::new(
            "value must match expected_kind and is absent without one",
        ))
    }
}

/// A swarm as listings name it. S3: `scope` lets the engine mark a team
/// swarm apart from a public one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmRefV1 {
    pub swarm_id: String,
    pub title: String,
    pub licence: String,
    pub scope: SwarmScope,
    /// S2: the team swarm's organization; absent on a public swarm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
}

impl SwarmRefV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.swarm_id, "swarm_id")?;
        validate_text(&self.title, 200, "title", false)?;
        if !licence_ok(&self.licence) {
            return Err(ValidationError::new("licence is an SPDX id"));
        }
        scope_org(self.scope, self.org_id.as_deref())
    }

    /// S2: whether this swarm may reach an engine whose lease names
    /// `lease_org_id`; a team swarm of another organization does not, and
    /// the engine leaves it out and reports it.
    #[must_use]
    pub fn reaches(&self, lease_org_id: &str) -> bool {
        reaches(self.scope, self.org_id.as_deref(), lease_org_id)
    }
}

/// One subtask as an engine sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmSubtaskV1 {
    pub subtask_id: String,
    pub swarm_id: String,
    pub number: u64,
    pub title: String,
    /// The acceptance criterion.
    pub acceptance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_kind: Option<ExpectedKind>,
    pub state: SwarmSubtaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<HandleRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_until: Option<UtcTimestamp>,
    /// Whether the asking engine holds the claim.
    pub mine: bool,
    /// In listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<SwarmRefV1>,
}

impl SwarmSubtaskV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.subtask_id, "subtask_id")?;
        validate_id(&self.swarm_id, "swarm_id")?;
        validate_text(&self.title, 200, "title", false)?;
        validate_text(&self.acceptance, 2000, "acceptance", true)?;
        if let Some(by) = &self.claimed_by {
            by.validate()?;
        }
        if self.claimed_by.is_some() != (self.state == SwarmSubtaskState::Claimed) {
            return Err(ValidationError::new(
                "claimed_by comes exactly with a claimed subtask",
            ));
        }
        if let Some(swarm) = &self.swarm {
            swarm.validate()?;
            if swarm.swarm_id != self.swarm_id {
                return Err(ValidationError::new("swarm does not match swarm_id"));
            }
        }
        Ok(())
    }
}

/// `GET /v1/swarms/subtasks`: open subtasks the engine may claim, and its
/// own running claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmSubtaskListV1 {
    pub schema: String,
    pub items: Vec<SwarmSubtaskV1>,
    pub trust: MessageTrust,
}

impl SwarmSubtaskListV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_SUBTASK_LIST_SCHEMA)?;
        if self.items.len() > 200 {
            return Err(ValidationError::new("at most 200 subtasks per page"));
        }
        self.items.iter().try_for_each(SwarmSubtaskV1::validate)?;
        untrusted(self.trust)
    }
}

/// An agent proposes one decomposition step; the swarm owner decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmSubtaskProposeV1 {
    pub schema: String,
    pub title: String,
    pub acceptance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_kind: Option<ExpectedKind>,
}

impl SwarmSubtaskProposeV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_SUBTASK_PROPOSE_SCHEMA)?;
        validate_text(&self.title, 200, "title", false)?;
        validate_text(&self.acceptance, 2000, "acceptance", true)?;
        if self.expected_kind == Some(ExpectedKind::Unknown) {
            return Err(ValidationError::new("unknown expected_kind"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmClaimRequestV1 {
    pub schema: String,
    /// The opt-in the engine last read (section 4).
    pub optin_id: String,
    pub minutes: u32,
}

impl SwarmClaimRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_CLAIM_REQUEST_SCHEMA)?;
        validate_id(&self.optin_id, "optin_id")?;
        minutes_ok(self.minutes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmClaimRenewV1 {
    pub schema: String,
    pub fencing_token: u64,
    pub minutes: u32,
}

impl SwarmClaimRenewV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_CLAIM_RENEW_SCHEMA)?;
        minutes_ok(self.minutes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmClaimReleaseV1 {
    pub schema: String,
    pub fencing_token: u64,
}

impl SwarmClaimReleaseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_CLAIM_RELEASE_SCHEMA)
    }
}

/// A granted or renewed claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmClaimV1 {
    pub schema: String,
    pub subtask_id: String,
    pub swarm_id: String,
    pub number: u64,
    pub fencing_token: u64,
    pub until: UtcTimestamp,
    pub trust: MessageTrust,
}

impl SwarmClaimV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_CLAIM_SCHEMA)?;
        validate_id(&self.subtask_id, "subtask_id")?;
        validate_id(&self.swarm_id, "swarm_id")?;
        untrusted(self.trust)
    }
}

/// `POST …/result`: the holder's result. Hive runs no screening on it;
/// the engine's filter packs run before the upload like for any upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmResultRequestV1 {
    pub schema: String,
    pub fencing_token: u64,
    pub summary: String,
    #[serde(default)]
    pub body: String,
    /// The deterministic part, when the subtask has an `expected_kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
}

impl SwarmResultRequestV1 {
    /// Shape checks; `value` against the subtask's kind is checked with
    /// [`validate_value`] by whoever knows the subtask.
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_RESULT_SCHEMA)?;
        validate_text(&self.summary, 500, "summary", false)?;
        if !self.body.is_empty() {
            validate_text(&self.body, MAX_RESULT_BODY_CHARS, "body", true)?;
        }
        Ok(())
    }

    /// `sha256` hex of `canonical_json({summary, body, value|null})`: the
    /// result's id in verification and export.
    pub fn digest(&self) -> Result<String, ValidationError> {
        let doc = json!({"summary": self.summary, "body": self.body, "value": self.value});
        Ok(hex(&Sha256::digest(canonical_json(&doc)?)))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmResultAckV1 {
    pub schema: String,
    pub result_id: String,
    /// [`SwarmResultRequestV1::digest`].
    pub sha256: String,
    pub state: SwarmSubtaskState,
    pub verifiers_assigned: u32,
    pub trust: MessageTrust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    Assigned,
    Committed,
    Revealed,
    #[serde(other)]
    Unknown,
}

/// What a verifier sees of the result: never the contributor's `value`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedResultV1 {
    pub summary: String,
    pub body: String,
    pub by: HandleRefV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedSubtaskV1 {
    pub title: String,
    pub acceptance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_kind: Option<ExpectedKind>,
}

/// One verification assignment of the asking engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmVerificationV1 {
    pub verification_id: String,
    pub state: VerificationState,
    /// The end of the current phase (commit or reveal, 24 hours each).
    pub due_at: UtcTimestamp,
    pub result: VerifiedResultV1,
    pub subtask: VerifiedSubtaskV1,
    pub swarm: SwarmRefV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmVerificationListV1 {
    pub schema: String,
    pub items: Vec<SwarmVerificationV1>,
    pub trust: MessageTrust,
}

impl SwarmVerificationListV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_VERIFICATION_LIST_SCHEMA)?;
        for item in &self.items {
            validate_id(&item.verification_id, "verification_id")?;
            item.result.by.validate()?;
            item.swarm.validate()?;
            if item.state == VerificationState::Revealed {
                return Err(ValidationError::new("revealed assignments are not listed"));
            }
        }
        untrusted(self.trust)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Agree,
    Disagree,
    #[serde(other)]
    Unknown,
}

/// The commitment a verifier sends first: `sha256` hex of
/// `canonical_json({nonce, value|null, verdict})`, `nonce` as sent in the
/// reveal (standard padded base64 of at least 16 random bytes).
pub fn verification_commitment(
    verdict: Verdict,
    value: Option<&Value>,
    nonce: &str,
) -> Result<String, ValidationError> {
    let doc = json!({"nonce": nonce, "value": value, "verdict": verdict});
    Ok(hex(&Sha256::digest(canonical_json(&doc)?)))
}

/// 1.9 erratum (RB6 L6): `sha256` hex of
/// `canonical_json({domain: "leanctx.hive.verification-commit/v2",
/// verification_id, nonce, value|null, verdict})`. Bound to one assignment
/// and to its purpose, a commitment cannot be replayed on another
/// verification or read as another kind of hash.
pub fn verification_commitment_v2(
    verification_id: &str,
    verdict: Verdict,
    value: Option<&Value>,
    nonce: &str,
) -> Result<String, ValidationError> {
    let doc = json!({
        "domain": VERIFICATION_COMMIT_V2_SCHEMA,
        "verification_id": verification_id,
        "nonce": nonce,
        "value": value,
        "verdict": verdict,
    });
    Ok(hex(&Sha256::digest(canonical_json(&doc)?)))
}

/// The commit. `schema` names the form: v2 ([`verification_commitment_v2`],
/// what engines send) or v1 ([`verification_commitment`], still accepted
/// from engines built before the erratum). Hive checks the reveal against
/// the form the commit named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationCommitV1 {
    pub schema: String,
    pub sha256: String,
}

impl VerificationCommitV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema != VERIFICATION_COMMIT_V2_SCHEMA {
            require_schema(&self.schema, VERIFICATION_COMMIT_SCHEMA)?;
        }
        validate_hex64(&self.sha256, "sha256")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRevealV1 {
    pub schema: String,
    pub verdict: Verdict,
    /// The verifier's own deterministic value, when the subtask has a kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    pub nonce: String,
    /// Required with `disagree`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl VerificationRevealV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, VERIFICATION_REVEAL_SCHEMA)?;
        if self.verdict == Verdict::Unknown {
            return Err(ValidationError::new("verdict is agree or disagree"));
        }
        // 24 characters of padded base64 hold at least 16 bytes.
        if self.nonce.len() < 24 {
            return Err(ValidationError::new("nonce holds at least 16 random bytes"));
        }
        super::validate_b64_bounded(&self.nonce, 128, "nonce")?;
        match (&self.reason, self.verdict) {
            (Some(reason), _) => validate_text(reason, 2000, "reason", true),
            (None, Verdict::Disagree) => Err(ValidationError::new("disagree needs a reason")),
            (None, _) => Ok(()),
        }
    }

    pub fn commitment(&self) -> Result<String, ValidationError> {
        verification_commitment(self.verdict, self.value.as_ref(), &self.nonce)
    }

    /// The v2 commitment for the assignment `verification_id`.
    pub fn commitment_v2(&self, verification_id: &str) -> Result<String, ValidationError> {
        verification_commitment_v2(
            verification_id,
            self.verdict,
            self.value.as_ref(),
            &self.nonce,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultOutcome {
    Verifying,
    Accepted,
    Rejected,
    /// Verdicts split; the swarm owner decides.
    Split,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRevealAckV1 {
    pub schema: String,
    pub state: VerificationState,
    pub result_state: ResultOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportTarget {
    Swarm,
    Subtask,
    Result,
    Message,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportReason {
    Spam,
    Abuse,
    SecretOrPii,
    Illegal,
    /// Prompt injection in swarm content (X5).
    Injection,
    Other,
    #[serde(other)]
    Unknown,
}

/// `POST /v1/swarms/reports` (and the same body on the web): reports hide
/// content automatically past a weighted threshold; moderators decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModerationReportV1 {
    pub schema: String,
    pub target_kind: ReportTarget,
    pub target_id: String,
    pub reason: ReportReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ModerationReportV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, MODERATION_REPORT_SCHEMA)?;
        if self.target_kind == ReportTarget::Unknown || self.reason == ReportReason::Unknown {
            return Err(ValidationError::new("unknown report target or reason"));
        }
        validate_id(&self.target_id, "target_id")?;
        if let Some(note) = &self.note {
            validate_text(note, 1000, "note", true)?;
        }
        Ok(())
    }
}

/// One pre-built subtask of a seed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedSubtaskV1 {
    pub title: String,
    pub acceptance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_kind: Option<ExpectedKind>,
}

/// O5: a seeded swarm as one import file (admin), created in `draft`;
/// importing the same canonical content again is a no-op. `material` is
/// the manifest only: a moderator uploads each file to
/// `POST /api/v1/swarms/{id}/material`, checked against
/// `sha256` and `bytes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmSeedV1 {
    pub schema: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub topics: Vec<String>,
    pub licence: String,
    pub subtasks: Vec<SeedSubtaskV1>,
    #[serde(default)]
    pub material: Vec<MaterialV1>,
}

impl SwarmSeedV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SWARM_SEED_SCHEMA)?;
        validate_text(&self.title, 200, "title", false)?;
        validate_text(&self.body, 20_000, "body", true)?;
        if !licence_ok(&self.licence) {
            return Err(ValidationError::new("licence is an SPDX id"));
        }
        if self.subtasks.is_empty() || self.subtasks.len() > MAX_SEED_SUBTASKS {
            return Err(ValidationError::new("a seed has 1-200 subtasks"));
        }
        for subtask in &self.subtasks {
            validate_text(&subtask.title, 200, "title", false)?;
            validate_text(&subtask.acceptance, 2000, "acceptance", true)?;
        }
        validate_material(&self.material)
    }

    /// `sha256` hex of the canonical seed: the import's idempotency key.
    pub fn digest(&self) -> Result<String, ValidationError> {
        let value = serde_json::to_value(self)
            .map_err(|error| ValidationError::new(format!("seed: {error}")))?;
        Ok(hex(&Sha256::digest(canonical_json(&value)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_team_swarm_names_its_org_and_reaches_only_it() {
        let swarm = |scope, org_id: Option<&str>| SwarmRefV1 {
            swarm_id: "sw_01jbretry".into(),
            title: "Retry budget".into(),
            licence: "CC-BY-4.0".into(),
            scope,
            org_id: org_id.map(str::to_string),
        };
        let public = swarm(SwarmScope::Public, None);
        assert!(
            public.validate().is_ok() && public.reaches("org_acme") && public.reaches("org_other")
        );
        let team = swarm(SwarmScope::Team, Some("org_acme"));
        assert!(team.validate().is_ok());
        assert!(team.reaches("org_acme") && !team.reaches("org_other"));
        // A public swarm never names an org; a team swarm always does.
        assert!(
            swarm(SwarmScope::Public, Some("org_acme"))
                .validate()
                .is_err()
        );
        assert!(swarm(SwarmScope::Team, None).validate().is_err());
        let unknown = swarm(SwarmScope::Unknown, None);
        assert!(unknown.validate().is_err() && !unknown.reaches("org_acme"));
    }

    #[test]
    fn a_reveal_reproduces_its_commitment_and_a_changed_verdict_does_not() {
        let reveal = VerificationRevealV1 {
            schema: VERIFICATION_REVEAL_SCHEMA.into(),
            verdict: Verdict::Agree,
            value: Some(json!(3)),
            nonce: "AAECAwQFBgcICQoLDA0ODxAREhM=".into(),
            reason: None,
        };
        reveal.validate().unwrap();
        let commitment = reveal.commitment().unwrap();
        assert_eq!(
            commitment,
            verification_commitment(Verdict::Agree, Some(&json!(3)), &reveal.nonce).unwrap()
        );
        assert_ne!(
            commitment,
            verification_commitment(Verdict::Disagree, Some(&json!(3)), &reveal.nonce).unwrap()
        );
    }

    #[test]
    fn values_follow_the_expected_kind() {
        assert!(validate_value(None, None).is_ok());
        assert!(validate_value(Some(ExpectedKind::Number), Some(&json!(5))).is_ok());
        assert!(validate_value(Some(ExpectedKind::Number), Some(&json!("5"))).is_err());
        assert!(validate_value(None, Some(&json!(5))).is_err());
        assert!(validate_value(Some(ExpectedKind::Hash), Some(&json!("A".repeat(64)))).is_err());
        // R3-3e-1: list text items are printable single lines of bounded length.
        let list = Some(ExpectedKind::List);
        assert!(validate_value(list, Some(&json!(["retry", 3, "backoff"]))).is_ok());
        for bad in [
            json!(["ok\u{E0041}\u{E0042}"]),
            json!(["a\u{202E}b"]),
            json!(["a\u{200B}b"]),
            json!(["two\nlines"]),
            json!([""]),
            json!(["x".repeat(MAX_VALUE_ITEM_CHARS + 1)]),
        ] {
            assert!(validate_value(list, Some(&bad)).is_err(), "{bad}");
        }
        assert!(validate_value(Some(ExpectedKind::Text), Some(&json!("a\u{E0041}"))).is_err());
    }
}
