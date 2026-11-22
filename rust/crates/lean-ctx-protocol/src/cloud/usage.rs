// SPDX-License-Identifier: Apache-2.0

//! `GET /v1/usage` (LeanCTX Cloud, capability [`FEATURE_USAGE`]): the
//! workspace's quotas as the Plan page shows them, so an instance warns at
//! 80 % instead of meeting a refusal.
//!
//! The answer is `{schema: leanctx.cloud.usage/v1, plan, read_only, quotas:
//! {<resource>: {limit, used, reset_at, state, next_step}}, notices,
//! model_credit, suggest_team}`. Like every response type it accepts unknown
//! members; `notices`, `model_credit` and `suggest_team` are carried for a
//! local reader but not interpreted here, and an
//! unknown `state` reads as [`CloudQuotaStateV1::Unknown`] rather than
//! failing the whole answer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// The `features` entry of a server that serves `GET /v1/usage`.
pub const FEATURE_USAGE: &str = "usage";

/// `schema` of the usage answer.
pub const USAGE_SCHEMA: &str = "leanctx.cloud.usage/v1";

/// Bounds what an answer may hold (the service names seven resources).
const MAX_QUOTAS: usize = 32;
const MAX_RESOURCE_LEN: usize = 64;

/// How far one quota is used: `warn` from 80 %, `full` at 100 %.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudQuotaStateV1 {
    Ok,
    Warn,
    Full,
    /// A state a newer service added; shown like `ok`.
    #[serde(other)]
    Unknown,
}

impl CloudQuotaStateV1 {
    /// `ok` 0, `warn` 1, `full` 2 (an unknown state counts as `ok`).
    #[must_use]
    pub fn level(self) -> u8 {
        match self {
            Self::Ok | Self::Unknown => 0,
            Self::Warn => 1,
            Self::Full => 2,
        }
    }
}

/// One quota of the usage answer. Amounts are in the resource's unit:
/// bytes for `*_bytes`, millionths of a US dollar for `*_usd_micros`,
/// counts otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudQuotaV1 {
    /// A negative amount (a credit refunded past zero) reads as 0.
    #[serde(deserialize_with = "non_negative")]
    pub limit: u64,
    #[serde(deserialize_with = "non_negative")]
    pub used: u64,
    /// When a monthly quota (or a revoked instance slot) frees again.
    #[serde(default)]
    pub reset_at: Option<String>,
    pub state: CloudQuotaStateV1,
    /// Where more comes from (`subscribe`, `top_up`, `upgrade_team`,
    /// `add_seats`, `contact`).
    #[serde(default)]
    pub next_step: Option<String>,
}

/// An amount that should never be negative but may arrive so (a refund or
/// a correction booked past zero): any integer the wire carries is read,
/// a negative one as 0, so one odd quota does not fail the whole answer.
pub(crate) fn non_negative<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Amount;
    impl serde::de::Visitor<'_> for Amount {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an integer amount")
        }
        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<u64, E> {
            Ok(value)
        }
        fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<u64, E> {
            Ok(u64::try_from(value).unwrap_or(0))
        }
    }
    deserializer.deserialize_i64(Amount)
}

/// [`non_negative`] for an optional amount.
pub(crate) fn non_negative_opt<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Wrapped(#[serde(deserialize_with = "non_negative")] u64);
    Ok(Option::<Wrapped>::deserialize(deserializer)?.map(|Wrapped(v)| v))
}

/// `GET /v1/usage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudUsageV1 {
    pub schema: String,
    /// `personal`, `team`, `enterprise` or `locked`.
    pub plan: String,
    /// The workspace has no plan: read and export only.
    #[serde(default)]
    pub read_only: bool,
    /// By resource name (`storage_bytes`, `transfer_bytes`, …).
    #[serde(default)]
    pub quotas: BTreeMap<String, CloudQuotaV1>,
    /// The cloud's notices for the Plan page, passed through unread to a
    /// local reader (`lean-ctx cloud usage --json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notices: Option<serde_json::Value>,
    /// Agent-hour credit and top-up, passed through unread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_credit: Option<serde_json::Value>,
    /// The cloud suggests the Team plan, passed through unread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest_team: Option<bool>,
}

impl CloudUsageV1 {
    /// The schema is this one and the quotas stay within bounds.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema != USAGE_SCHEMA {
            return Err(ValidationError::new(format!(
                "usage schema must be {USAGE_SCHEMA}"
            )));
        }
        if self.quotas.len() > MAX_QUOTAS {
            return Err(ValidationError::new("usage names too many quotas"));
        }
        if self.quotas.keys().any(|r| {
            r.is_empty()
                || r.len() > MAX_RESOURCE_LEN
                || !r.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        }) {
            return Err(ValidationError::new("usage names an invalid resource"));
        }
        Ok(())
    }

    /// The quotas at `warn` or `full`, by resource name.
    pub fn pressing(&self) -> impl Iterator<Item = (&str, &CloudQuotaV1)> {
        self.quotas
            .iter()
            .filter(|(_, q)| q.state.level() > 0)
            .map(|(r, q)| (r.as_str(), q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/cloud/usage.response.json"
        ))
        .unwrap()
    }

    #[test]
    fn the_cloud_answer_decodes_with_states_and_next_steps() {
        let usage: CloudUsageV1 = serde_json::from_value(fixture()).unwrap();
        usage.validate().unwrap();
        assert_eq!(usage.plan, "personal");
        assert!(!usage.read_only);
        assert_eq!(usage.quotas.len(), 7);
        let storage = &usage.quotas["storage_bytes"];
        assert_eq!(storage.limit, 40 << 30);
        assert_eq!(storage.state, CloudQuotaStateV1::Warn);
        assert_eq!(storage.next_step.as_deref(), Some("upgrade_team"));
        assert_eq!(storage.reset_at, None);
        let transfer = &usage.quotas["transfer_bytes"];
        assert_eq!(transfer.state, CloudQuotaStateV1::Full);
        assert_eq!(transfer.reset_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        assert_eq!(
            usage.quotas["model_credit_usd_micros"].next_step.as_deref(),
            Some("top_up")
        );
        let pressing: Vec<&str> = usage.pressing().map(|(r, _)| r).collect();
        assert_eq!(pressing, ["storage_bytes", "transfer_bytes"]);
    }

    #[test]
    fn unknown_members_and_states_do_not_break_the_answer() {
        let mut raw = fixture();
        raw["later"] = serde_json::json!({"x": 1});
        raw["quotas"]["storage_bytes"]["state"] = "almost".into();
        raw["quotas"]["storage_bytes"]["extra"] = true.into();
        let usage: CloudUsageV1 = serde_json::from_value(raw).unwrap();
        usage.validate().unwrap();
        let storage = &usage.quotas["storage_bytes"];
        assert_eq!(storage.state, CloudQuotaStateV1::Unknown);
        assert_eq!(storage.state.level(), 0);
    }

    #[test]
    fn another_schema_or_a_bad_resource_is_refused() {
        let mut raw = fixture();
        raw["schema"] = "leanctx.cloud.usage/v2".into();
        let usage: CloudUsageV1 = serde_json::from_value(raw).unwrap();
        assert!(usage.validate().is_err());
        let mut raw = fixture();
        raw["quotas"]["Bad Name"] = raw["quotas"]["storage_bytes"].clone();
        let usage: CloudUsageV1 = serde_json::from_value(raw).unwrap();
        assert!(usage.validate().is_err());
    }

    #[test]
    fn a_negative_amount_reads_as_zero() {
        let mut raw = fixture();
        raw["quotas"]["storage_bytes"]["used"] = (-5).into();
        raw["quotas"]["storage_bytes"]["limit"] = (-1).into();
        raw["quotas"]["transfer_bytes"]["used"] = u64::MAX.into();
        let usage: CloudUsageV1 = serde_json::from_value(raw).unwrap();
        assert_eq!(usage.quotas["storage_bytes"].used, 0);
        assert_eq!(usage.quotas["storage_bytes"].limit, 0);
        assert_eq!(usage.quotas["transfer_bytes"].used, u64::MAX);
        let mut raw = fixture();
        raw["quotas"]["storage_bytes"]["used"] = 1.5.into();
        assert!(serde_json::from_value::<CloudUsageV1>(raw).is_err());

        let task: crate::cloud::tasks::UsageV1 =
            serde_json::from_value(serde_json::json!({"tokens": -3, "usd_micros": -7})).unwrap();
        assert_eq!((task.tokens, task.usd_micros), (0, Some(0)));
        let task: crate::cloud::tasks::UsageV1 =
            serde_json::from_value(serde_json::json!({"tokens": 12})).unwrap();
        assert_eq!((task.tokens, task.usd_micros), (12, None));
    }

    #[test]
    fn states_order_by_level() {
        assert!(CloudQuotaStateV1::Ok.level() < CloudQuotaStateV1::Warn.level());
        assert!(CloudQuotaStateV1::Warn.level() < CloudQuotaStateV1::Full.level());
    }
}
