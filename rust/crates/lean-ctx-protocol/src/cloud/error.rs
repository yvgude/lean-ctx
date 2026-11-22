// SPDX-License-Identifier: Apache-2.0

//! Error body of every non-2xx Cloud response: `{"error":{...}}`.

use serde::{Deserialize, Serialize};

/// Machine-readable error code. Unknown codes from a newer service decode as
/// [`CloudErrorCode::Unknown`] and are handled by HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudErrorCode {
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
    /// 402: the organization's plan does not include Cloud.
    PlanRequired,
    /// 403: the lease lacks the right this route needs.
    ForbiddenRight,
    /// 403 with `kind` (1.4): the organization switched this upload off.
    /// Stop that kind of upload and drop its queue until a lease allows it.
    UploadDisabled,
    /// 404: unknown instance, vault, proposal or lease.
    NotFound,
    /// 404 with `feature` (Cloud API.md "feature switches"): the operator
    /// switched this feature off on this Cloud (`instance_metrics` while
    /// `CLOUD_ENGINE_METRICS` is off). The route exists; try again much later.
    FeatureDisabled,
    /// 409 with `limit`/`used`: every instance seat is taken.
    InstanceLimit,
    /// 409 with `expected`/`actual`: knowledge moved on.
    RevisionConflict,
    /// 409 with `reason` (1.5): the object is not in a state that allows
    /// this.
    InvalidState,
    /// 401: the instance lease is past `grace_until`; renew or reconnect.
    LeaseExpired,
    /// 422: the service screening refused content the engine should have removed.
    ScreeningRequired,
    /// 429 with `retry_after_s`.
    RateLimited,
    /// 426 with `min_instance` (also in the `Cloud-Min-Instance` header).
    UpgradeRequired,
    /// 401 with `revocation` (C0 2.0 E12, §3.17.3): a workspace admin
    /// revoked this instance; every engine route answers it until a new
    /// connect. Never retried.
    InstanceRevoked,
    /// Release B quotas (Cloud API §8.1): `resource`, `limit`, `used`,
    /// `reset_at`. Counted resources (`registered_instances`,
    /// `private_vaults`) answer 409 (`registered_instances` in Personal adds
    /// `cooldown_days`, `reset_at` = when a cooling-down slot frees);
    /// monthly ones (`transfer_bytes`, `bus_messages`) answer 429 with
    /// `retry_after_s` and `Retry-After` until `reset_at`. Never overage:
    /// the engine waits, its outbox keeps local work.
    QuotaExceeded,
    /// 403 with `member` (dotted policy path) and `level` (the outermost
    /// level whose document refuses; consent refusals add `action`): Cloud
    /// itself enforces an `x-enforcement: server` policy member (§7.2).
    PolicyDenied,
    /// 403 with `resource: "storage_bytes"`, `limit`/`used` and
    /// `limit_bytes`/`used_bytes`: the workspace's LeanCTX Cloud storage is
    /// full. Nothing frees by waiting; a person frees storage or upgrades.
    StorageQuotaExceeded,
    /// 429 with `limit_bytes_per_hour`, `retry_after_s` and `Retry-After`:
    /// the sliding-hour upload allowance (max(storage/4, 256 MiB)) is used
    /// up. Transient: wait `retry_after_s`, then upload the rest.
    UploadRateLimited,
    /// Any code this engine does not know.
    #[serde(other)]
    Unknown,
}

impl CloudErrorCode {
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
            Self::ForbiddenRight
            | Self::UploadDisabled
            | Self::PolicyDenied
            | Self::StorageQuotaExceeded => 403,
            Self::NotFound | Self::FeatureDisabled => 404,
            Self::InstanceLimit | Self::RevisionConflict | Self::InvalidState => 409,
            Self::ScreeningRequired => 422,
            Self::UpgradeRequired => 426,
            // 409 for counted resources; the monthly ones, the common case
            // on data routes, are 429.
            Self::RateLimited | Self::QuotaExceeded | Self::UploadRateLimited => 429,
            Self::Unknown => 500,
        }
    }
}

/// The `error` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudErrorDetailV1 {
    pub code: CloudErrorCode,
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
    /// `quota_exceeded`: the quota (`registered_instances`, `private_vaults`,
    /// `transfer_bytes`, `bus_messages`, or one a newer Cloud adds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// `quota_exceeded`: when it frees (RFC 3339; `null` when nothing frees
    /// by itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
    /// `quota_exceeded` `registered_instances`: the Personal slot cool-down.
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
    pub min_instance: Option<String>,
    /// `upload_disabled`: which upload the organization switched off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CloudUploadKind>,
    /// 1.5: machine-readable detail of the code (`[a-z_]`, ≤ 64), e.g.
    /// `required`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 1.5: the request field the error is about (`invalid_request`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// 1.6: the proposal an error is about (`proposal_state`, `not_found`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    /// `not_permitted` (Cloud task #17): the check that failed, `presence`
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
    /// 2.x `plan_required`: the feature the plan lacks (`repos`,
    /// `internal_vaults`, …; absent from a 1.x Cloud, where it means Cloud
    /// itself). `limit`/`used` carry a counted feature's numbers.
    /// `feature_disabled`: the feature switched off on this Cloud
    /// (`instance_metrics`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// 2.x `plan_required`: the lowest plan that includes `feature`
    /// (`pro`, `team`, `enterprise`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_needed: Option<String>,
    /// Limit refusals (`quota_exceeded`, `storage_quota_exceeded`,
    /// `upload_rate_limited`, `instance_limit`, `plan_required`): the
    /// workspace's plan (`personal`, `team`, `enterprise`, `locked`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Limit refusals: what helps (`subscribe`, `top_up`, `upgrade_team`,
    /// `add_seats`, `contact`, `wait`, or one a newer service adds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step: Option<String>,
    /// `storage_quota_exceeded`: the storage quota in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_bytes: Option<u64>,
    /// `storage_quota_exceeded`: the storage used in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_bytes: Option<u64>,
    /// `upload_rate_limited`: the upload allowance per sliding hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_bytes_per_hour: Option<u64>,
    /// `instance_limit` in Team: the workspace's seats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seats: Option<u64>,
    /// `instance_limit` in Team: the allowance of each seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_seat: Option<u64>,
}

/// An upload an organization can switch off (1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudUploadKind {
    Metrics,
    Receipts,
    #[serde(other)]
    Unknown,
}

impl CloudErrorDetailV1 {
    /// An error with only code and message.
    pub fn new(code: CloudErrorCode, message: impl Into<String>) -> Self {
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
            min_instance: None,
            kind: None,
            reason: None,
            field: None,
            number: None,
            check: None,
            role: None,
            agent_role: None,
            feature: None,
            plan_needed: None,
            plan: None,
            next_step: None,
            limit_bytes: None,
            used_bytes: None,
            limit_bytes_per_hour: None,
            seats: None,
            per_seat: None,
        }
    }
}

/// Body of every non-2xx response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudErrorBody {
    pub error: CloudErrorDetailV1,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_codes_decode_as_unknown() {
        let body: CloudErrorBody =
            serde_json::from_str(r#"{"error":{"code":"brand_new","message":"x","extra":1}}"#)
                .unwrap();
        assert_eq!(body.error.code, CloudErrorCode::Unknown);
    }

    /// Cloud answers `POST /v1/metrics/hours` with this while
    /// `CLOUD_ENGINE_METRICS` is off (dogfood 05.11.: decoded as `unknown`).
    #[test]
    fn feature_disabled_decodes_with_its_feature() {
        let body: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"feature_disabled","message":"this feature is switched off on this Cloud","feature":"instance_metrics"}}"#,
        )
        .unwrap();
        assert_eq!(body.error.code, CloudErrorCode::FeatureDisabled);
        assert_eq!(body.error.code.http_status(), 404);
        assert_eq!(body.error.feature.as_deref(), Some("instance_metrics"));
    }

    /// C0 2.0 `routes/instance_revoked.error.json` decodes to its own code
    /// (dogfood 25.10.: it decoded as `unknown`, shown as "401 (unknown)").
    #[test]
    fn instance_revoked_decodes_from_the_c0_fixture() {
        let body: CloudErrorBody = serde_json::from_str(include_str!(
            "../../tests/fixtures/cloud-v2/routes/instance_revoked.error.json"
        ))
        .unwrap();
        assert_eq!(body.error.code, CloudErrorCode::InstanceRevoked);
        assert_eq!(body.error.code.http_status(), 401);
    }

    /// C0 2.x `routes/quota_exceeded.error.json` (Release B §8.1): a byte
    /// quota wider than 32 bits decodes, with resource and reset.
    #[test]
    fn quota_exceeded_decodes_from_the_c0_fixture() {
        let body: CloudErrorBody = serde_json::from_str(include_str!(
            "../../tests/fixtures/cloud-v2/routes/quota_exceeded.error.json"
        ))
        .unwrap();
        let e = body.error;
        assert_eq!(e.code, CloudErrorCode::QuotaExceeded);
        assert_eq!(e.resource.as_deref(), Some("transfer_bytes"));
        assert_eq!(e.limit, Some(42_949_672_960));
        assert_eq!(e.used, Some(42_949_672_960));
        assert_eq!(e.reset_at.as_deref(), Some("2026-11-01T00:00:00Z"));
        assert_eq!(e.retry_after_s, Some(86_400));
    }

    /// The server's own shapes (API §7.2 and §3): `policy_denied {member,
    /// level, action?}` and the Personal slot cool-down; a 1.x engine that does
    /// not know them still decodes the body (unknown code, extra fields).
    #[test]
    fn policy_denied_and_cooldown_decode_and_stay_tolerant() {
        let denied: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"policy_denied","message":"x","member":"sharing.consent_required_for","level":"org","action":"trust_boundary"}}"#,
        )
        .unwrap();
        assert_eq!(denied.error.code, CloudErrorCode::PolicyDenied);
        assert_eq!(denied.error.code.http_status(), 403);
        assert_eq!(
            denied.error.member.as_deref(),
            Some("sharing.consent_required_for")
        );
        assert_eq!(denied.error.level.as_deref(), Some("org"));
        assert_eq!(denied.error.action.as_deref(), Some("trust_boundary"));
        let slot: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"quota_exceeded","message":"x","resource":"registered_instances","limit":3,"used":3,"reset_at":"2026-11-02T08:00:00Z","cooldown_days":7}}"#,
        )
        .unwrap();
        assert_eq!(slot.error.cooldown_days, Some(7));
        let newer: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"quota_exceeded","message":"x","resource":"gpu_minutes","reset_at":null,"extra":1}}"#,
        )
        .unwrap();
        assert_eq!(newer.error.resource.as_deref(), Some("gpu_minutes"));
        assert_eq!(newer.error.reset_at, None);
    }

    /// The cloud's limit refusals (plan limits of 17.11.) decode with
    /// every field; none of them is `unknown` any more.
    #[test]
    fn cloud_limit_refusals_decode_with_their_fields() {
        let storage: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"storage_quota_exceeded","message":"x","resource":"storage_bytes","limit":1073741824,"used":1073741824,"limit_bytes":1073741824,"used_bytes":1073741824,"plan":"personal","next_step":"upgrade_team"}}"#,
        )
        .unwrap();
        let e = storage.error;
        assert_eq!(e.code, CloudErrorCode::StorageQuotaExceeded);
        assert_eq!(e.code.http_status(), 403);
        assert_eq!(e.resource.as_deref(), Some("storage_bytes"));
        assert_eq!(e.limit_bytes, Some(1_073_741_824));
        assert_eq!(e.used_bytes, Some(1_073_741_824));
        assert_eq!(e.plan.as_deref(), Some("personal"));
        assert_eq!(e.next_step.as_deref(), Some("upgrade_team"));

        let rate: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"upload_rate_limited","message":"x","limit_bytes_per_hour":268435456,"plan":"personal","next_step":"wait","retry_after_s":1200}}"#,
        )
        .unwrap();
        let e = rate.error;
        assert_eq!(e.code, CloudErrorCode::UploadRateLimited);
        assert_eq!(e.code.http_status(), 429);
        assert_eq!(e.limit_bytes_per_hour, Some(268_435_456));
        assert_eq!(e.retry_after_s, Some(1200));
        assert_eq!(e.next_step.as_deref(), Some("wait"));

        let seats: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"instance_limit","message":"x","resource":"connected_instances","limit":4,"used":4,"plan":"team","next_step":"add_seats","seats":2,"per_seat":2}}"#,
        )
        .unwrap();
        let e = seats.error;
        assert_eq!(e.code, CloudErrorCode::InstanceLimit);
        assert_eq!(e.resource.as_deref(), Some("connected_instances"));
        assert_eq!((e.seats, e.per_seat), (Some(2), Some(2)));
        assert_eq!(e.plan.as_deref(), Some("team"));

        let locked: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"plan_required","message":"x","feature":"read_only","reason":"trial_ended","plan":"locked","next_step":"subscribe"}}"#,
        )
        .unwrap();
        assert_eq!(locked.error.code, CloudErrorCode::PlanRequired);
        assert_eq!(locked.error.plan.as_deref(), Some("locked"));
        assert_eq!(locked.error.next_step.as_deref(), Some("subscribe"));
    }

    #[test]
    fn codes_map_to_their_status() {
        assert_eq!(CloudErrorCode::PlanRequired.http_status(), 402);
        assert_eq!(CloudErrorCode::InstanceLimit.http_status(), 409);
        assert_eq!(CloudErrorCode::UpgradeRequired.http_status(), 426);
        assert_eq!(CloudErrorCode::UploadDisabled.http_status(), 403);
    }

    #[test]
    fn upload_disabled_names_its_kind_and_tolerates_new_ones() {
        let body: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"upload_disabled","message":"x","kind":"receipts"}}"#,
        )
        .unwrap();
        assert_eq!(body.error.code, CloudErrorCode::UploadDisabled);
        assert_eq!(body.error.kind, Some(CloudUploadKind::Receipts));
        let newer: CloudErrorBody = serde_json::from_str(
            r#"{"error":{"code":"upload_disabled","message":"x","kind":"traces"}}"#,
        )
        .unwrap();
        assert_eq!(newer.error.kind, Some(CloudUploadKind::Unknown));
    }
}
