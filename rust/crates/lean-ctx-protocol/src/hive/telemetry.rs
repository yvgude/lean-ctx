// SPDX-License-Identifier: Apache-2.0

//! Cockpit metrics and receipts.
//!
//! - `POST /v1/metrics` [`MetricsUploadV1`] (`leanctx.cloud-metrics/v1`,
//!   unchanged from the cloud cockpit) → [`MetricsAckV1`]. Idempotent replace
//!   per `(device.id, date, vault.id)`; `vault.id` is the resolved Hive
//!   `vault_id`.
//! - 1.9 (X4): a day may carry `hive` ([`MetricsHiveV1`]), how Hive
//!   helped in that vault: counts and durations only, the same for sealed
//!   vaults. Claim conflicts are not reported; Hive derives them from its
//!   own refusals and bus lease conflicts.
//! - `POST /v1/receipts` [`ReceiptsUploadV1`] → [`ReceiptsAckV1`]; a receipt
//!   already stored (same `receipt_id` and `digest`) counts as duplicate.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{MAX_BODY_BYTES, require_schema, validate_id};
use crate::{Sha256Digest, UtcTimestamp, ValidationError};

pub const METRICS_SCHEMA: &str = "leanctx.cloud-metrics/v1";
pub const METRICS_ACK_SCHEMA: &str = "leanctx.hive.metrics-ack/v1";
pub const RECEIPTS_SCHEMA: &str = "leanctx.hive.receipts/v1";
pub const RECEIPTS_ACK_SCHEMA: &str = "leanctx.hive.receipts-ack/v1";

pub const MAX_METRIC_DAYS: usize = 62;
pub const MAX_METRIC_SPEND_ROWS: usize = 32;
pub const MAX_METRIC_AGENT_ROWS: usize = 16;
pub const MAX_RECEIPTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsUploadV1 {
    pub schema: String,
    pub device: MetricsDeviceV1,
    pub days: Vec<MetricsDayV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsDeviceV1 {
    pub id: String,
    pub label: Option<String>,
    pub engine_version: String,
    pub os: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsDayV1 {
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub vault: MetricsVaultV1,
    pub savings: MetricsSavingsV1,
    pub spend: Vec<MetricsSpendV1>,
    pub ledger: MetricsLedgerV1,
    pub guards: MetricsGuardsV1,
    pub agents: Vec<MetricsAgentV1>,
    pub memory: MetricsMemoryV1,
    pub receipts: u64,
    /// 1.9 (X4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive: Option<MetricsHiveV1>,
}

pub const MAX_METRIC_TASK_ROWS: usize = 64;
/// 1.9 R3: ceilings of [`MetricsHiveV1`] (counts per day, the median in
/// milliseconds up to one day, tokens and USD micros per task row); the
/// names match the Hive server's.
pub const MAX_METRIC_DAY_COUNT: u64 = 1_000_000;
pub const MAX_METRIC_DAY_MS: u64 = 86_400_000;
pub const MAX_METRIC_TASK_TOKENS: u64 = 10_000_000_000;
pub const MAX_METRIC_TASK_USD_MICROS: u64 = 10_000_000_000;

/// 1.9 (X4): one vault day of Hive use.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsHiveV1 {
    /// Hive facts delivered in a session and then used in it (M1).
    pub knowledge_hits_referenced: u64,
    /// Ledger usage while a session held a task claim, per task (M2);
    /// sorted by `number`, at most one row per task.
    #[serde(default)]
    pub task_usage: Vec<MetricsTaskUsageV1>,
    /// Median time from session start to the first Hive context delivered,
    /// over `first_context_sessions` sessions (M5); absent with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_context_ms_p50: Option<u64>,
    pub first_context_sessions: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsTaskUsageV1 {
    pub number: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// List price, not an invoice amount.
    pub list_usd_micros: u64,
}

impl MetricsHiveV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.task_usage.len() > MAX_METRIC_TASK_ROWS
            || self.task_usage.iter().any(|row| row.number == 0)
            || self
                .task_usage
                .windows(2)
                .any(|pair| pair[0].number >= pair[1].number)
        {
            return Err(ValidationError::new(
                "task_usage holds at most 64 rows sorted by task number without repeats",
            ));
        }
        if self.first_context_ms_p50.is_some() != (self.first_context_sessions > 0) {
            return Err(ValidationError::new(
                "first_context_ms_p50 comes exactly with first_context_sessions",
            ));
        }
        // R3: ceilings no real day reaches, so a day cannot skew the rollup.
        if self.knowledge_hits_referenced > MAX_METRIC_DAY_COUNT
            || self.first_context_sessions > MAX_METRIC_DAY_COUNT
            || self
                .first_context_ms_p50
                .is_some_and(|ms| ms > MAX_METRIC_DAY_MS)
            || self.task_usage.iter().any(|row| {
                row.input_tokens > MAX_METRIC_TASK_TOKENS
                    || row.output_tokens > MAX_METRIC_TASK_TOKENS
                    || row.list_usd_micros > MAX_METRIC_TASK_USD_MICROS
            })
        {
            return Err(ValidationError::new(
                "hive day values exceed their ceilings",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsVaultV1 {
    pub id: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSavingsV1 {
    pub tokens_requested: u64,
    pub tokens_delivered: u64,
    pub tokens_saved: u64,
    pub tool_calls: u64,
    pub cache_hits: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSpendV1 {
    pub model: String,
    pub provider: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    /// List-price USD, not an invoice amount.
    pub list_usd: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsLedgerV1 {
    pub reduction: u64,
    pub edit: u64,
    pub correction: u64,
    pub cache_discount: u64,
    pub other: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsGuardsV1 {
    pub secrets_redacted: u64,
    pub pii_redacted: u64,
    pub injection_flags: u64,
    pub content_withheld: u64,
    pub paths_blocked: u64,
    pub shell_blocked: u64,
    pub partial_inspections: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsAgentV1 {
    pub host: String,
    pub sessions: u64,
    pub tool_calls: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsMemoryV1 {
    pub facts_current: u64,
    pub facts_added: u64,
    pub lessons_linked: u64,
}

impl MetricsUploadV1 {
    /// Shape and size bounds. The per-field rules of the cockpit metrics
    /// contract (§3: id/label/version syntax, date window, count ceilings)
    /// stay with the engine's metrics builder, which never sends a body the
    /// service would refuse.
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, METRICS_SCHEMA)?;
        if self.days.is_empty() || self.days.len() > MAX_METRIC_DAYS {
            return Err(ValidationError::new("days must hold 1-62 entries"));
        }
        let mut seen = std::collections::HashSet::new();
        for day in &self.days {
            if day.spend.len() > MAX_METRIC_SPEND_ROWS || day.agents.len() > MAX_METRIC_AGENT_ROWS {
                return Err(ValidationError::new("too many spend or agent rows"));
            }
            if !seen.insert((day.date.as_str(), day.vault.id.as_str())) {
                return Err(ValidationError::new("a (date, vault) pair repeats"));
            }
            if let Some(hive) = &day.hive {
                hive.validate()?;
            }
        }
        let size = serde_json::to_vec(self)
            .map_err(|error| ValidationError::new(format!("metrics: {error}")))?
            .len();
        if size > MAX_BODY_BYTES {
            return Err(ValidationError::new("metrics body exceeds 1 MiB"));
        }
        Ok(())
    }
}

/// Response of `POST /v1/metrics`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsAckV1 {
    pub schema: String,
    /// Day entries stored (inserted or replaced).
    pub accepted: u32,
}

/// One receipt. `document` is the receipt as persisted locally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptItemV1 {
    pub receipt_id: String,
    /// `engine` or `agent_connector`.
    pub kind: String,
    pub vault_id: String,
    pub created_at: UtcTimestamp,
    /// [`receipt_digest`] of `document`.
    pub digest: Sha256Digest,
    pub document: Value,
}

/// `sha256:<hex>` of `document` with object keys sorted and no whitespace
/// (numbers as `serde_json` prints them; receipts are not signed here).
pub fn receipt_digest(document: &Value) -> Result<Sha256Digest, ValidationError> {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), sorted(value)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.iter().map(sorted).collect()),
            scalar => scalar.clone(),
        }
    }
    let bytes = serde_json::to_vec(&sorted(document))
        .map_err(|error| ValidationError::new(format!("receipt: {error}")))?;
    let mut digest = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut digest, "{byte:02x}").expect("writing to String cannot fail");
    }
    Sha256Digest::new(digest)
}

/// `POST /v1/receipts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptsUploadV1 {
    pub schema: String,
    pub receipts: Vec<ReceiptItemV1>,
}

impl ReceiptsUploadV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, RECEIPTS_SCHEMA)?;
        if self.receipts.is_empty() || self.receipts.len() > MAX_RECEIPTS {
            return Err(ValidationError::new("receipts must hold 1-256 items"));
        }
        for item in &self.receipts {
            validate_id(&item.receipt_id, "receipt_id")?;
            super::validate_vault_id(&item.vault_id)?;
            if !matches!(item.kind.as_str(), "engine" | "agent_connector") {
                return Err(ValidationError::new(
                    "receipt kind must be engine or agent_connector",
                ));
            }
            if !item.document.is_object() {
                return Err(ValidationError::new("receipt document must be an object"));
            }
            if receipt_digest(&item.document)? != item.digest {
                return Err(ValidationError::new(
                    "receipt digest does not match document",
                ));
            }
        }
        let size = serde_json::to_vec(self)
            .map_err(|error| ValidationError::new(format!("receipts: {error}")))?
            .len();
        if size > MAX_BODY_BYTES {
            return Err(ValidationError::new("receipts body exceeds 1 MiB"));
        }
        Ok(())
    }
}

/// Response of `POST /v1/receipts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptsAckV1 {
    pub schema: String,
    pub accepted: u32,
    pub duplicates: u32,
}
