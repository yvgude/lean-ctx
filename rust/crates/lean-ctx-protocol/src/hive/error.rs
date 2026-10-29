// SPDX-License-Identifier: Apache-2.0

//! Error body of every non-2xx Hive response: `{"error":{...}}`.

use serde::{Deserialize, Serialize};

/// Machine-readable error code. Unknown codes from a newer service decode as
/// [`HiveErrorCode::Unknown`] and are handled by HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveErrorCode {
    /// 400, device flow: the user has not approved yet; poll again after `interval`.
    AuthorizationPending,
    /// 400, device flow: polling too fast; add 5 s to the interval.
    SlowDown,
    /// 400, device flow: the device code expired; start again.
    ExpiredToken,
    /// 400, device flow: the user refused.
    AccessDenied,
    /// 400: body failed validation or carried unknown fields.
    InvalidRequest,
    /// 401: access token missing, expired or not for this instance.
    Unauthorized,
    /// 402: the organization's plan does not include Hive.
    PlanRequired,
    /// 403: the lease lacks the right this route needs.
    ForbiddenRight,
    /// 403 with `kind` (1.4): the organization switched this upload off.
    /// Stop that kind of upload and drop its queue until a lease allows it.
    UploadDisabled,
    /// 404: unknown instance, vault, proposal or lease.
    NotFound,
    /// 409 with `limit`/`used`: every instance seat is taken.
    InstanceLimit,
    /// 409 with `expected`/`actual`: knowledge moved on (1.5: `reason`
    /// `key_exists` when a first sealed key was already granted).
    RevisionConflict,
    /// 409 with `reason` (1.5): the object is not in a state that allows
    /// this (`keys_lost`, `epoch_rotating`, `sealed`).
    InvalidState,
    /// 401: the instance lease is past `grace_until`; renew or reconnect.
    LeaseExpired,
    /// 422: the service screening refused content the engine should have removed.
    ScreeningRequired,
    /// 429 with `retry_after_s`.
    RateLimited,
    /// 426 with `min_engine` (also in the `Hive-Min-Engine` header).
    UpgradeRequired,
    /// 401 with `revocation` (C0 2.0 E12, §3.17.3): a workspace admin
    /// revoked this instance; every engine route answers it until a new
    /// connect. Never retried.
    InstanceRevoked,
    /// Release B quotas (Hive API §8.1): `resource`, `limit`, `used`,
    /// `reset_at`. Counted resources (`registered_engines`,
    /// `private_vaults`) answer 409 (`registered_engines` in Personal adds
    /// `cooldown_days`, `reset_at` = when a cooling-down slot frees);
    /// monthly ones (`transfer_bytes`, `bus_messages`) answer 429 with
    /// `retry_after_s` and `Retry-After` until `reset_at`. Never overage:
    /// the engine waits, its outbox keeps local work.
    QuotaExceeded,
    /// 403 with `member` (dotted policy path) and `level` (the outermost
    /// level whose document refuses; consent refusals add `action`): Hive
    /// itself enforces an `x-enforcement: server` policy member (§7.2).
    PolicyDenied,
    /// Any code this engine does not know.
    #[serde(other)]
    Unknown,
}

impl HiveErrorCode {
    /// HTTP status the service answers with for this code.
    pub fn http_status(self) -> u16 {
        match self {
            Self::AuthorizationPending
            | Self::SlowDown
            | Self::ExpiredToken
            | Self::AccessDenied
            | Self::InvalidRequest => 400,
            Self::Unauthorized | Self::LeaseExpired | Self::InstanceRevoked => 401,
            Self::PlanRequired => 402,
            Self::ForbiddenRight | Self::UploadDisabled | Self::PolicyDenied => 403,
            Self::NotFound => 404,
            Self::InstanceLimit | Self::RevisionConflict | Self::InvalidState => 409,
            Self::ScreeningRequired => 422,
            Self::UpgradeRequired => 426,
            // 409 for counted resources; the monthly ones, the common case
            // on data routes, are 429.
            Self::RateLimited | Self::QuotaExceeded => 429,
            Self::Unknown => 500,
        }
    }
}

/// The `error` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveErrorDetailV1 {
    pub code: HiveErrorCode,
    /// Human-readable, safe to show; never contains secrets or tokens.
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
    /// `instance_limit`: seats in the plan; `quota_exceeded`: the quota
    /// (bytes for `transfer_bytes`, so wider than 32 bits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// `instance_limit`: seats in use; `quota_exceeded`: what is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
    /// `quota_exceeded`: the quota (`registered_engines`, `private_vaults`,
    /// `transfer_bytes`, `bus_messages`, or one a newer Hive adds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// `quota_exceeded`: when it frees (RFC 3339; `null` when nothing frees
    /// by itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
    /// `quota_exceeded` `registered_engines`: the Personal slot cool-down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_days: Option<u32>,
    /// `policy_denied`: the dotted member path that refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// `policy_denied`: the outermost level whose document refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// `policy_denied` of `consent_required_for`: the action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// `revision_conflict`: revision the client expected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<u64>,
    /// `revision_conflict`: current revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<u64>,
    /// `upgrade_required`: lowest engine version accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_engine: Option<String>,
    /// `upload_disabled`: which upload the organization switched off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<HiveUploadKind>,
    /// 1.5: machine-readable detail of the code (`[a-z_]`, ≤ 64), e.g.
    /// `key_exists`, `epoch_rotating`, `sealed_plaintext`, `required`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 1.5: the request field the error is about (`invalid_request`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// 1.6: the proposal an error is about (`proposal_state`, `not_found`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    /// `not_permitted` (Hive task #17): the check that failed, `presence`
    /// (the agent has no live presence on the bus) or `role` (present in
    /// another role).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// `not_permitted`: the task's role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// `not_permitted` with `check: role`: the role the agent is present in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    /// 2.x `plan_required`: the feature the plan lacks (`compute`, `repos`,
    /// `internal_vaults`, …; absent from a 1.x Hive, where it means Hive
    /// itself). `limit`/`used` carry a counted feature's numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// 2.x `plan_required`: the lowest plan that includes `feature`
    /// (`pro`, `team`, `enterprise`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_needed: Option<String>,
}

/// An upload an organization can switch off (1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveUploadKind {
    Metrics,
    Receipts,
    #[serde(other)]
    Unknown,
}

impl HiveErrorDetailV1 {
    /// An error with only code and message.
    pub fn new(code: HiveErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_s: None,
            limit: None,
            used: None,
            resource: None,
            reset_at: None,
            cooldown_days: None,
            member: None,
            level: None,
            action: None,
            expected: None,
            actual: None,
            min_engine: None,
            kind: None,
            reason: None,
            field: None,
            number: None,
            check: None,
            role: None,
            agent_role: None,
            feature: None,
            plan_needed: None,
        }
    }
}

/// Body of every non-2xx response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveErrorBody {
    pub error: HiveErrorDetailV1,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_codes_decode_as_unknown() {
        let body: HiveErrorBody =
            serde_json::from_str(r#"{"error":{"code":"brand_new","message":"x","extra":1}}"#)
                .unwrap();
        assert_eq!(body.error.code, HiveErrorCode::Unknown);
    }

    /// C0 2.0 `routes/instance_revoked.error.json` decodes to its own code
    /// (dogfood 25.10.: it decoded as `unknown`, shown as "401 (unknown)").
    #[test]
    fn instance_revoked_decodes_from_the_c0_fixture() {
        let body: HiveErrorBody = serde_json::from_str(include_str!(
            "../../tests/fixtures/hive-v2/routes/instance_revoked.error.json"
        ))
        .unwrap();
        assert_eq!(body.error.code, HiveErrorCode::InstanceRevoked);
        assert_eq!(body.error.code.http_status(), 401);
    }

    /// C0 2.x `routes/quota_exceeded.error.json` (Release B §8.1): a byte
    /// quota wider than 32 bits decodes, with resource and reset.
    #[test]
    fn quota_exceeded_decodes_from_the_c0_fixture() {
        let body: HiveErrorBody = serde_json::from_str(include_str!(
            "../../tests/fixtures/hive-v2/routes/quota_exceeded.error.json"
        ))
        .unwrap();
        let e = body.error;
        assert_eq!(e.code, HiveErrorCode::QuotaExceeded);
        assert_eq!(e.resource.as_deref(), Some("transfer_bytes"));
        assert_eq!(e.limit, Some(53_687_091_200));
        assert_eq!(e.used, Some(53_687_091_200));
        assert_eq!(e.reset_at.as_deref(), Some("2026-11-01T00:00:00Z"));
        assert_eq!(e.retry_after_s, Some(86_400));
    }

    /// The server's own shapes (API §7.2 and §3): `policy_denied {member,
    /// level, action?}` and the Personal slot cool-down; a 1.x engine that does
    /// not know them still decodes the body (unknown code, extra fields).
    #[test]
    fn policy_denied_and_cooldown_decode_and_stay_tolerant() {
        let denied: HiveErrorBody = serde_json::from_str(
            r#"{"error":{"code":"policy_denied","message":"x","member":"sharing.consent_required_for","level":"org","action":"trust_boundary"}}"#,
        )
        .unwrap();
        assert_eq!(denied.error.code, HiveErrorCode::PolicyDenied);
        assert_eq!(denied.error.code.http_status(), 403);
        assert_eq!(
            denied.error.member.as_deref(),
            Some("sharing.consent_required_for")
        );
        assert_eq!(denied.error.level.as_deref(), Some("org"));
        assert_eq!(denied.error.action.as_deref(), Some("trust_boundary"));
        let slot: HiveErrorBody = serde_json::from_str(
            r#"{"error":{"code":"quota_exceeded","message":"x","resource":"registered_engines","limit":3,"used":3,"reset_at":"2026-11-02T08:00:00Z","cooldown_days":7}}"#,
        )
        .unwrap();
        assert_eq!(slot.error.cooldown_days, Some(7));
        let newer: HiveErrorBody = serde_json::from_str(
            r#"{"error":{"code":"quota_exceeded","message":"x","resource":"gpu_minutes","reset_at":null,"extra":1}}"#,
        )
        .unwrap();
        assert_eq!(newer.error.resource.as_deref(), Some("gpu_minutes"));
        assert_eq!(newer.error.reset_at, None);
    }

    #[test]
    fn codes_map_to_their_status() {
        assert_eq!(HiveErrorCode::PlanRequired.http_status(), 402);
        assert_eq!(HiveErrorCode::InstanceLimit.http_status(), 409);
        assert_eq!(HiveErrorCode::UpgradeRequired.http_status(), 426);
        assert_eq!(HiveErrorCode::UploadDisabled.http_status(), 403);
    }

    #[test]
    fn upload_disabled_names_its_kind_and_tolerates_new_ones() {
        let body: HiveErrorBody = serde_json::from_str(
            r#"{"error":{"code":"upload_disabled","message":"x","kind":"receipts"}}"#,
        )
        .unwrap();
        assert_eq!(body.error.code, HiveErrorCode::UploadDisabled);
        assert_eq!(body.error.kind, Some(HiveUploadKind::Receipts));
        let newer: HiveErrorBody = serde_json::from_str(
            r#"{"error":{"code":"upload_disabled","message":"x","kind":"traces"}}"#,
        )
        .unwrap();
        assert_eq!(newer.error.kind, Some(HiveUploadKind::Unknown));
    }
}
