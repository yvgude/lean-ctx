// SPDX-License-Identifier: Apache-2.0
//! The engine's own copy of the `leanctx.dashboard-pro/1` wire shapes it
//! exchanges with the licensed runtime. Field names match the runtime's
//! contract one to one; the engine never links the runtime crate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub(crate) const SCHEMA: &str = "leanctx.dashboard-pro/1";

/// One computation request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Request {
    pub(crate) schema: String,
    pub(crate) op: String,
    pub(crate) now_utc: String,
    pub(crate) project: Project,
    pub(crate) query: Map<String, Value>,
    pub(crate) body: Value,
    pub(crate) inputs: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Project {
    pub(crate) root_hash: String,
    pub(crate) labels: Vec<ProjectLabel>,
    pub(crate) repo_hashes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProjectLabel {
    pub(crate) hash: String,
    pub(crate) name: String,
}

/// The runtime's answer.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Response {
    pub(crate) status: u16,
    #[serde(default)]
    pub(crate) body: Option<Value>,
    #[serde(default)]
    pub(crate) file: Option<FileOut>,
    /// Kept untyped until each plan is checked: an unknown effect is a 409,
    /// not an unreadable answer.
    #[serde(default)]
    pub(crate) plans: Option<Vec<Value>>,
    #[serde(default)]
    pub(crate) error: Option<ErrorOut>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FileOut {
    pub(crate) name: String,
    pub(crate) mime: String,
    pub(crate) bytes_b64: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ErrorOut {
    pub(crate) code: String,
    pub(crate) message: String,
}

/// A write the engine validates, executes and reads back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "effect", deny_unknown_fields)]
pub(crate) enum Plan {
    #[serde(rename = "alerts.settings")]
    AlertsSettings { settings: AlertSettings },
    #[serde(rename = "alerts.notify")]
    AlertsNotify {
        at: String,
        text: String,
        counts: BTreeMap<String, i64>,
        test: bool,
        desktop: bool,
        webhook: String,
    },
    #[serde(rename = "profiles.put")]
    ProfilesPut {
        name: String,
        values: Map<String, Value>,
        saved_at: String,
    },
    #[serde(rename = "profiles.delete")]
    ProfilesDelete { name: String },
    #[serde(rename = "config.set")]
    ConfigSet {
        key: String,
        wire: String,
        expected: Value,
        weakening: bool,
    },
    #[serde(rename = "memory.snapshot")]
    MemorySnapshot { reason: String },
    #[serde(rename = "memory.restore")]
    MemoryRestore {
        id: String,
        missing: Vec<String>,
        changed: Vec<String>,
    },
    #[serde(rename = "memory.forget")]
    MemoryForget { refs: Vec<String>, reason: String },
    #[serde(rename = "memory.consolidate")]
    MemoryConsolidate { run: bool },
    #[serde(rename = "context.overlay")]
    ContextOverlay { action: String, path: String },
    #[serde(rename = "adaptive.record")]
    AdaptiveRecord { item: AppliedItem },
    #[serde(rename = "adaptive.dismiss")]
    AdaptiveDismiss { path: String },
    #[serde(rename = "ledger.export")]
    LedgerExport {
        format: String,
        file_name: String,
        mime: String,
        filters: LedgerFilters,
        project_column: ProjectColumn,
    },
}

impl Plan {
    pub(crate) fn effect(&self) -> &'static str {
        match self {
            Self::AlertsSettings { .. } => "alerts.settings",
            Self::AlertsNotify { .. } => "alerts.notify",
            Self::ProfilesPut { .. } => "profiles.put",
            Self::ProfilesDelete { .. } => "profiles.delete",
            Self::ConfigSet { .. } => "config.set",
            Self::MemorySnapshot { .. } => "memory.snapshot",
            Self::MemoryRestore { .. } => "memory.restore",
            Self::MemoryForget { .. } => "memory.forget",
            Self::MemoryConsolidate { .. } => "memory.consolidate",
            Self::ContextOverlay { .. } => "context.overlay",
            Self::AdaptiveRecord { .. } => "adaptive.record",
            Self::AdaptiveDismiss { .. } => "adaptive.dismiss",
            Self::LedgerExport { .. } => "ledger.export",
        }
    }
}

/// The Ledger page's checked filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LedgerFilters {
    pub(crate) scope: Option<Vec<String>>,
    pub(crate) repos: Option<RepoSelection>,
    pub(crate) proxy: bool,
    pub(crate) tool: String,
    pub(crate) model: String,
    pub(crate) text: String,
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RepoSelection {
    Only { hashes: Vec<String> },
    Except { hashes: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ProjectColumn {
    Fixed { name: String },
    Labels { labels: Vec<ProjectLabel> },
}

/// Python-style tolerant readers of engine files.
pub(crate) mod lenient {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    pub(crate) fn text_of(value: &Value) -> Option<String> {
        match value {
            Value::Null => None,
            Value::String(text) => Some(text.clone()),
            Value::Bool(flag) => Some(if *flag { "True" } else { "False" }.to_string()),
            Value::Number(number) => Some(number.to_string()),
            other => Some(other.to_string()),
        }
    }

    pub(crate) fn int_of(value: &Value) -> Option<i64> {
        match value {
            Value::Number(number) => number
                .as_i64()
                .or_else(|| {
                    number
                        .as_u64()
                        .map(|n| i64::try_from(n).unwrap_or(i64::MAX))
                })
                .or_else(|| number.as_f64().map(|f| f.trunc() as i64)),
            Value::Bool(flag) => Some(i64::from(*flag)),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    pub(crate) fn float_of(value: &Value) -> Option<f64> {
        match value {
            Value::Number(number) => number.as_f64(),
            Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    pub(crate) fn opt_str<'de, D: Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
        Ok(text_of(&Value::deserialize(de)?))
    }

    pub(crate) fn opt_i64<'de, D: Deserializer<'de>>(de: D) -> Result<Option<i64>, D::Error> {
        Ok(int_of(&Value::deserialize(de)?))
    }

    pub(crate) fn opt_f64<'de, D: Deserializer<'de>>(de: D) -> Result<Option<f64>, D::Error> {
        Ok(float_of(&Value::deserialize(de)?))
    }
}

/// One line of the signed savings ledger, as far as the bridge reads it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct LedgerEntry {
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) ts: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) tool: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) mechanism: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) model_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) version: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) baseline_tokens: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) actual_tokens: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) saved_tokens: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) bounce_adjustment: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_f64")]
    pub(crate) saved_usd: Option<f64>,
    #[serde(default, deserialize_with = "lenient::opt_f64")]
    pub(crate) unit_price_per_m_usd: Option<f64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) lines_added: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) lines_removed: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) loc_added: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) loc_removed: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) measurement_method: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) evidence_class: Option<String>,
    /// Confidence `[0, 1]` of the savings measurement (engine metrics K7 range).
    #[serde(
        default,
        deserialize_with = "lenient::opt_f64",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) confidence: Option<f64>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) attribution_group: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) repo_hash: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) prev_hash: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) entry_hash: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct LedgerPage {
    pub(crate) available: bool,
    pub(crate) total: i64,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
    pub(crate) entries: Vec<LedgerEntry>,
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct LedgerFacets {
    pub(crate) entries: i64,
    pub(crate) first: String,
    pub(crate) last: String,
    pub(crate) proxied: i64,
    pub(crate) models: Vec<ModelCount>,
    pub(crate) tools: Vec<ToolCount>,
    pub(crate) repos: Vec<RepoCount>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ModelCount {
    pub(crate) model: String,
    pub(crate) entries: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToolCount {
    pub(crate) tool: String,
    pub(crate) entries: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RepoCount {
    pub(crate) repo: String,
    pub(crate) entries: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct LedgerTotals {
    pub(crate) tools: BTreeMap<String, ToolTotals>,
    pub(crate) days: Vec<DayTotals>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ToolTotals {
    pub(crate) calls: i64,
    pub(crate) saved: i64,
    pub(crate) baseline: i64,
    pub(crate) usd: f64,
    pub(crate) corrected: i64,
    pub(crate) lines_added: i64,
    pub(crate) lines_removed: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct DayTotals {
    pub(crate) day: String,
    pub(crate) calls: i64,
    pub(crate) saved: i64,
    pub(crate) corrected: i64,
    pub(crate) usd: f64,
    pub(crate) cache_usd: f64,
}

/// `[reductions, saved, usd]`.
pub(crate) type GroupRow = (i64, i64, f64);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ReportBucket {
    pub(crate) entries: i64,
    pub(crate) reductions: i64,
    pub(crate) saved: i64,
    pub(crate) baseline: i64,
    pub(crate) actual: i64,
    pub(crate) usd: f64,
    pub(crate) would_usd: f64,
    pub(crate) blended_usd: f64,
    pub(crate) blended_saved: i64,
    pub(crate) stand_in_usd: f64,
    pub(crate) corrections: i64,
    pub(crate) corrected: i64,
    pub(crate) corrected_usd: f64,
    pub(crate) cache_entries: i64,
    pub(crate) edits: i64,
    pub(crate) tools: BTreeMap<String, GroupRow>,
    pub(crate) models: BTreeMap<String, GroupRow>,
    pub(crate) mechanisms: BTreeMap<String, GroupRow>,
}

pub(crate) type LedgerDays = BTreeMap<String, BTreeMap<String, ReportBucket>>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct Chain {
    pub(crate) lines: i64,
    pub(crate) broken: Option<i64>,
    pub(crate) spans: BTreeMap<String, (i64, i64, String, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GuardEvent {
    pub(crate) at: String,
    pub(crate) kind: String,
    pub(crate) count: i64,
    pub(crate) tool: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct EgressRow {
    pub(crate) mtime: i64,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) locality: String,
    pub(crate) outcome: String,
    pub(crate) original: i64,
    pub(crate) delivered: i64,
    pub(crate) redactions: i64,
    pub(crate) blocked: i64,
    pub(crate) injection: i64,
    pub(crate) incomplete: i64,
    pub(crate) mode: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AlertSettings {
    pub(crate) enabled: bool,
    pub(crate) kinds: Vec<String>,
    pub(crate) desktop: bool,
    pub(crate) webhook: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AlertRecord {
    pub(crate) at: String,
    pub(crate) text: String,
    pub(crate) channels: Vec<String>,
    pub(crate) test: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct Profile {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) values: Map<String, Value>,
    #[serde(default)]
    pub(crate) saved_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ProxyDays {
    pub(crate) first: String,
    pub(crate) days: Vec<ProxyDay>,
    pub(crate) kept: ProxyKept,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ProxyDay {
    pub(crate) day: String,
    pub(crate) models: BTreeMap<String, ProxyModelDay>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ProxyModelDay {
    pub(crate) prompts: i64,
    pub(crate) cached_tokens: i64,
    pub(crate) discount_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ProxyKept {
    pub(crate) saved: i64,
    pub(crate) corrected: i64,
    pub(crate) usd: f64,
    pub(crate) reductions: i64,
    pub(crate) corrections: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct HostUsageFile {
    pub(crate) project: String,
    pub(crate) host: String,
    pub(crate) days: BTreeMap<String, BTreeMap<String, HostCounts>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct HostCounts {
    pub(crate) requests: i64,
    pub(crate) input: i64,
    pub(crate) cache_read: i64,
    pub(crate) cache_write: i64,
    pub(crate) output: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct SnapshotMeta {
    pub(crate) id: String,
    pub(crate) created_at: Option<Value>,
    pub(crate) facts: Option<Value>,
    pub(crate) reason: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct AdaptiveState {
    #[serde(default)]
    pub(crate) applied: Vec<AppliedItem>,
    #[serde(default)]
    pub(crate) dismissed: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct AppliedItem {
    pub(crate) path: String,
    pub(crate) action: String,
    #[serde(default)]
    pub(crate) applied_at: String,
    #[serde(default)]
    pub(crate) baseline: Baseline,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct Baseline {
    #[serde(default)]
    pub(crate) reads: i64,
    #[serde(default)]
    pub(crate) original: i64,
    #[serde(default)]
    pub(crate) saved: i64,
    #[serde(default)]
    pub(crate) ratio: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct HeatmapEntry {
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) access_count: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) total_original_tokens: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_i64")]
    pub(crate) total_tokens_saved: Option<i64>,
    #[serde(default, deserialize_with = "lenient::opt_str")]
    pub(crate) last_access: Option<String>,
}
