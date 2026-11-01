// SPDX-License-Identifier: Apache-2.0

//! Privacy-bounded, versioned telemetry event contract.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SCHEMA_VERSION: u16 = 2;
pub const MAX_COUNT: u64 = 1_000_000_000;
pub const MAX_HISTOGRAM_BUCKETS: usize = 32;
pub const MAX_BATCH_EVENTS: usize = 64;
/// Upper bound on distinct tools in one `tool_call_aggregate` event. The
/// built-in registry holds fewer than 100 tools.
pub const MAX_TOOL_ENTRIES: usize = 128;
pub const MAX_TOOL_NAME_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryBatchV2 {
    pub schema_version: u16,
    pub deletion_token_hash: String,
    pub events: Vec<TelemetryEnvelopeV2>,
}

impl TelemetryBatchV2 {
    pub fn validate(&self) -> Result<(), TelemetryValidationError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(TelemetryValidationError::SchemaVersion);
        }
        if !valid_digest(&self.deletion_token_hash) {
            return Err(TelemetryValidationError::DeletionTokenHash);
        }
        if self.events.is_empty() || self.events.len() > MAX_BATCH_EVENTS {
            return Err(TelemetryValidationError::BatchSize);
        }
        self.events
            .iter()
            .try_for_each(TelemetryEnvelopeV2::validate)
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryEnvelopeV2 {
    pub schema_version: u16,
    pub timestamp_bucket: String,
    pub installation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<PseudonymousId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<PseudonymousId>,
    pub app_version: String,
    pub event: TelemetryEventV2,
}

impl TelemetryEnvelopeV2 {
    pub fn validate(&self) -> Result<(), TelemetryValidationError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(TelemetryValidationError::SchemaVersion);
        }
        NaiveDate::parse_from_str(&self.timestamp_bucket, "%Y-%m-%d")
            .map_err(|_| TelemetryValidationError::TimestampBucket)?;
        Uuid::parse_str(&self.installation_id)
            .map_err(|_| TelemetryValidationError::InstallationId)?;
        if self.app_version.is_empty()
            || self.app_version.len() > 64
            || !self
                .app_version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        {
            return Err(TelemetryValidationError::AppVersion);
        }
        if self.account_id.as_ref().is_some_and(|id| !id.is_valid())
            || self
                .organization_id
                .as_ref()
                .is_some_and(|id| !id.is_valid())
        {
            return Err(TelemetryValidationError::PseudonymousId);
        }
        self.event.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PseudonymousId(String);

impl<'de> Deserialize<'de> for PseudonymousId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?)
            .map_err(|_| serde::de::Error::custom("invalid pseudonymous ID"))
    }
}

impl PseudonymousId {
    pub fn new(value: impl Into<String>) -> Result<Self, TelemetryValidationError> {
        let value = Self(value.into());
        if value.is_valid() {
            Ok(value)
        } else {
            Err(TelemetryValidationError::PseudonymousId)
        }
    }

    fn is_valid(&self) -> bool {
        self.0.strip_prefix("hmac-sha256:").is_some_and(|tail| {
            tail.len() == 64
                && tail
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "name",
    content = "metrics",
    rename_all = "snake_case",
    deny_unknown_fields
)]
/// One telemetry event. The contract is shared with the hosted service, which
/// deserializes the same type: variants marked *hosted plane* are never built
/// by the OSS engine and stay only so both sides read one schema
/// (`docs/privacy/TELEMETRY-DATA-DICTIONARY.md` §3).
pub enum TelemetryEventV2 {
    Heartbeat(HeartbeatMetrics),
    SetupProfile(SetupProfileMetrics),
    SetupCompleted(OccurrenceMetrics),
    IntegrationDetected(OccurrenceMetrics),
    SessionAggregate(SessionMetrics),
    ToolUsageAggregate(ToolUsageMetrics),
    ToolCallAggregate(ToolCallMetrics),
    AutopilotAggregate(DecisionMetrics),
    AutopilotFallbackAggregate(DecisionMetrics),
    SyncAggregate(SyncMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    TrialStarted(OccurrenceMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    TrialEnded(OutcomeMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    UpgradeViewed(OccurrenceMetrics),
    CheckoutStarted(OccurrenceMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    SubscriptionActivated(OccurrenceMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    SubscriptionCancelled(OccurrenceMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    TeamCreated(OccurrenceMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    TeamMemberInvited(OccurrenceMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    TeamContextPromoted(OutcomeMetrics),
    ErrorCategoryAggregate(ErrorMetrics),
    VersionUpgrade(VersionUpgradeMetrics),
    /// Hosted plane: never emitted by the OSS engine.
    OrchestrationAggregate(Box<OrchestrationMetrics>),
    UsageHistory(UsageHistoryMetrics),
    /// Daily use of CLI commands and background features (3.11.1+).
    FeatureAggregate(FeatureMetrics),
}

impl TelemetryEventV2 {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Heartbeat(_) => "heartbeat",
            Self::SetupProfile(_) => "setup_profile",
            Self::SetupCompleted(_) => "setup_completed",
            Self::IntegrationDetected(_) => "integration_detected",
            Self::SessionAggregate(_) => "session_aggregate",
            Self::ToolUsageAggregate(_) => "tool_usage_aggregate",
            Self::ToolCallAggregate(_) => "tool_call_aggregate",
            Self::AutopilotAggregate(_) => "autopilot_aggregate",
            Self::AutopilotFallbackAggregate(_) => "autopilot_fallback_aggregate",
            Self::SyncAggregate(_) => "sync_aggregate",
            Self::TrialStarted(_) => "trial_started",
            Self::TrialEnded(_) => "trial_ended",
            Self::UpgradeViewed(_) => "upgrade_viewed",
            Self::CheckoutStarted(_) => "checkout_started",
            Self::SubscriptionActivated(_) => "subscription_activated",
            Self::SubscriptionCancelled(_) => "subscription_cancelled",
            Self::TeamCreated(_) => "team_created",
            Self::TeamMemberInvited(_) => "team_member_invited",
            Self::TeamContextPromoted(_) => "team_context_promoted",
            Self::ErrorCategoryAggregate(_) => "error_category_aggregate",
            Self::VersionUpgrade(_) => "version_upgrade",
            Self::OrchestrationAggregate(_) => "orchestration_aggregate",
            Self::UsageHistory(_) => "usage_history",
            Self::FeatureAggregate(_) => "feature_aggregate",
        }
    }

    fn validate(&self) -> Result<(), TelemetryValidationError> {
        match self {
            Self::Heartbeat(metrics) => metrics.validate(),
            // Both fields are closed enums: deserialization is the validation.
            Self::SetupProfile(_) => Ok(()),
            Self::SetupCompleted(metrics)
            | Self::IntegrationDetected(metrics)
            | Self::TrialStarted(metrics)
            | Self::UpgradeViewed(metrics)
            | Self::CheckoutStarted(metrics)
            | Self::SubscriptionActivated(metrics)
            | Self::SubscriptionCancelled(metrics)
            | Self::TeamCreated(metrics)
            | Self::TeamMemberInvited(metrics) => metrics.validate(),
            Self::SessionAggregate(metrics) => metrics.validate(),
            Self::ToolUsageAggregate(metrics) => metrics.validate(),
            Self::ToolCallAggregate(metrics) => metrics.validate(),
            Self::AutopilotAggregate(metrics) | Self::AutopilotFallbackAggregate(metrics) => {
                metrics.validate()
            }
            Self::SyncAggregate(metrics) => metrics.validate(),
            Self::TrialEnded(metrics) | Self::TeamContextPromoted(metrics) => metrics.validate(),
            Self::ErrorCategoryAggregate(metrics) => metrics.validate(),
            Self::VersionUpgrade(metrics) => metrics.validate(),
            Self::OrchestrationAggregate(metrics) => metrics.validate(),
            Self::UsageHistory(metrics) => metrics.validate(),
            Self::FeatureAggregate(metrics) => metrics.validate(),
        }
    }
}

/// Upper bound for token totals: a heavy installation passes [`MAX_COUNT`]
/// tokens within weeks, so tokens get their own, still finite, bound.
pub const MAX_TOKENS: u64 = 1_000_000_000_000_000;
/// Days of local history sent; matches the server's retention window.
pub const MAX_HISTORY_DAYS: usize = 90;

/// The installation's own daily usage record (`lean-ctx gain`), so history
/// before telemetry and usage outside MCP (shell hooks, CLI) is counted.
/// Days are `YYYY-MM-DD` in the client's calendar, strictly ascending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageHistoryMetrics {
    pub days: Vec<UsageDay>,
    pub lifetime: LifetimeUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageDay {
    pub date: String,
    /// Compressed operations (tool calls, hook-wrapped commands, reads).
    pub commands: u64,
    /// Tokens before compression.
    pub original_tokens: u64,
    /// Tokens that reached the model.
    pub delivered_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifetimeUsage {
    pub commands: u64,
    pub original_tokens: u64,
    pub delivered_tokens: u64,
    /// `YYYY-MM` of first use; month precision only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_use_month: Option<String>,
}

impl UsageHistoryMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        if self.days.len() > MAX_HISTORY_DAYS
            || !self.days.windows(2).all(|pair| pair[0].date < pair[1].date)
        {
            return Err(TelemetryValidationError::History);
        }
        for day in &self.days {
            if !valid_day(&day.date) {
                return Err(TelemetryValidationError::History);
            }
            bounded(day.commands)?;
            tokens_bounded(&[day.original_tokens, day.delivered_tokens])?;
        }
        bounded(self.lifetime.commands)?;
        tokens_bounded(&[
            self.lifetime.original_tokens,
            self.lifetime.delivered_tokens,
        ])?;
        if self
            .lifetime
            .first_use_month
            .as_deref()
            .is_some_and(|month| !valid_day(&format!("{month}-01")))
        {
            return Err(TelemetryValidationError::History);
        }
        Ok(())
    }
}

fn tokens_bounded(values: &[u64]) -> Result<(), TelemetryValidationError> {
    if values.iter().all(|value| *value <= MAX_TOKENS) {
        Ok(())
    } else {
        Err(TelemetryValidationError::CountBound)
    }
}

/// A real calendar day written exactly as `YYYY-MM-DD`.
fn valid_day(value: &str) -> bool {
    value.len() == 10
        && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .is_ok_and(|date| date.format("%Y-%m-%d").to_string() == value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatMetrics {
    pub distribution_channel: DistributionChannel,
    pub client_family: ClientFamily,
    pub operating_system: OperatingSystem,
    pub architecture: Architecture,
    /// Age of the local installation identity, bucketed. Closed enums only;
    /// skipped when unknown so older receivers keep accepting the heartbeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_age: Option<InstallAge>,
    /// Distinct UTC days with a successful send in the trailing 30, bucketed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_days: Option<ActiveDays>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_environment: Option<RuntimeEnvironment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallAge {
    #[serde(rename = "lt_1h")]
    Lt1h,
    #[serde(rename = "lt_1d")]
    Lt1d,
    #[serde(rename = "lt_7d")]
    Lt7d,
    #[serde(rename = "lt_30d")]
    Lt30d,
    #[serde(rename = "gte_30d")]
    Gte30d,
}

impl InstallAge {
    #[must_use]
    pub fn from_seconds(seconds: u64) -> Self {
        match seconds {
            0..3_600 => Self::Lt1h,
            3_600..86_400 => Self::Lt1d,
            86_400..604_800 => Self::Lt7d,
            604_800..2_592_000 => Self::Lt30d,
            _ => Self::Gte30d,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActiveDays {
    #[serde(rename = "d1")]
    D1,
    #[serde(rename = "d2_3")]
    D2To3,
    #[serde(rename = "d4_7")]
    D4To7,
    #[serde(rename = "d8_14")]
    D8To14,
    #[serde(rename = "d15_plus")]
    D15Plus,
}

impl ActiveDays {
    #[must_use]
    pub fn from_count(days: usize) -> Self {
        match days {
            0 | 1 => Self::D1,
            2 | 3 => Self::D2To3,
            4..=7 => Self::D4To7,
            8..=14 => Self::D8To14,
            _ => Self::D15Plus,
        }
    }
}

/// Where the process runs. Separates people from short-lived agent sandboxes
/// without any machine, account or network identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEnvironment {
    Local,
    Container,
    Codespaces,
    Gitpod,
    Replit,
    CloudAgent,
    Ci,
    Unknown,
}

impl HeartbeatMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        Ok(())
    }
}

/// How this installation is wired up, as closed enums only: no paths,
/// project names, model names or config values leave the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupProfileMetrics {
    pub integration_mode: IntegrationMode,
    pub embeddings: EmbeddingsState,
}

/// Configured `hook_mode`; `Default` means the user never set one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationMode {
    Default,
    Mcp,
    Hybrid,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingsState {
    /// Built without the `embeddings` feature.
    Unsupported,
    /// Auto-download is turned off and no model is on disk.
    Disabled,
    /// Allowed but the model has not been downloaded yet.
    NotInstalled,
    Installed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OccurrenceMetrics {
    pub count: u64,
}

impl OccurrenceMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        bounded(self.count)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeMetrics {
    pub accepted: u64,
    pub rejected: u64,
    pub unknown: u64,
}

impl OutcomeMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        bounded_many(&[self.accepted, self.rejected, self.unknown])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMetrics {
    pub sessions: u64,
    pub duration_seconds: Histogram,
}

impl SessionMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        bounded(self.sessions)?;
        self.duration_seconds.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolUsageMetrics {
    pub calls: u64,
    pub failures: u64,
    pub latency_milliseconds: Histogram,
    /// Absent from clients before 3.11.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenMetrics>,
}

/// Tokens of tool output for one day: what it would have cost uncompressed
/// (`original`) and what reached the model (`delivered`). Counts only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenMetrics {
    pub original: u64,
    pub delivered: u64,
}

impl ToolUsageMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        bounded_many(&[self.calls, self.failures])?;
        if self.failures > self.calls {
            return Err(TelemetryValidationError::InconsistentCounts);
        }
        if let Some(tokens) = self.tokens {
            bounded_many(&[tokens.original, tokens.delivered])?;
        }
        self.latency_milliseconds.validate()
    }
}

/// Per-tool call counts for one day.
///
/// Tool names are the static keys of the built-in tool registry, never
/// caller-supplied strings: a name is a short `[a-z0-9_]` identifier, entries
/// are sorted and unique, and every entry has at least one call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallMetrics {
    pub tools: Vec<ToolCallCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallCount {
    pub tool: String,
    pub calls: u64,
    pub failures: u64,
    /// Summed latency of these calls; absent from clients before 3.11.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_milliseconds_total: Option<u64>,
    /// Failures by class (never the message); absent before 3.11.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kinds:
        Option<std::collections::BTreeMap<crate::core::telemetry_failure::FailureKind, u64>>,
    /// The most frequent scrubbed failure templates (see
    /// `core::failure_template`); absent before 3.11.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_messages: Option<Vec<FailureMessage>>,
}

/// One scrubbed failure template and how often it occurred that day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureMessage {
    pub template: String,
    pub count: u64,
}

impl FailureMessage {
    /// Shape of a template the scrubber can produce: bounded, a closed
    /// character set and no digits.
    fn valid(&self) -> bool {
        let length = self.template.chars().count();
        (1..=crate::core::failure_template::MAX_TEMPLATE_CHARS).contains(&length)
            && self.count > 0
            && self.template.chars().all(|c| {
                (c.is_ascii_alphanumeric() && !c.is_ascii_digit())
                    || " .,:;()[]{}<>!?'-_/‹›…".contains(c)
            })
    }
}

impl ToolCallMetrics {
    pub(crate) fn validate(&self) -> Result<(), TelemetryValidationError> {
        if self.tools.is_empty() || self.tools.len() > MAX_TOOL_ENTRIES {
            return Err(TelemetryValidationError::ToolEntries);
        }
        if !self
            .tools
            .windows(2)
            .all(|pair| pair[0].tool < pair[1].tool)
        {
            return Err(TelemetryValidationError::ToolEntries);
        }
        self.tools.iter().try_for_each(ToolCallCount::validate)
    }
}

impl ToolCallCount {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        if !valid_tool_name(&self.tool) {
            return Err(TelemetryValidationError::ToolName);
        }
        bounded_many(&[self.calls, self.failures])?;
        bounded(self.latency_milliseconds_total.unwrap_or_default())?;
        let classified: u64 = self
            .failure_kinds
            .iter()
            .flat_map(|kinds| kinds.values())
            .sum();
        if classified > self.failures
            || self
                .failure_kinds
                .as_ref()
                .is_some_and(|kinds| kinds.values().any(|count| *count == 0))
        {
            return Err(TelemetryValidationError::InconsistentCounts);
        }
        if self.calls == 0 || self.failures > self.calls {
            return Err(TelemetryValidationError::InconsistentCounts);
        }
        if let Some(messages) = &self.failure_messages {
            let sum: u64 = messages.iter().map(|message| message.count).sum();
            if messages.is_empty()
                || messages.len() > crate::core::telemetry_failure::MAX_FAILURE_MESSAGES
                || sum > self.failures
                || !messages.iter().all(FailureMessage::valid)
            {
                return Err(TelemetryValidationError::InconsistentCounts);
            }
        }
        Ok(())
    }
}

/// `[a-z][a-z0-9_]*`, at most [`MAX_TOOL_NAME_LEN`] bytes.
pub fn valid_tool_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_TOOL_NAME_LEN
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// Most distinct feature codes per day; extra codes are dropped on the client.
pub const MAX_FEATURE_ENTRIES: usize = 96;

/// Daily counts per feature code from the closed registry in
/// `core::telemetry_features` (e.g. `cli.pack.export`, `index.graph`).
/// Sorted by code; a code never carries arguments, paths or user input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureMetrics {
    pub features: Vec<FeatureCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureCount {
    pub feature: String,
    pub count: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failures: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// `segment(.segment){0,2}`, each `[a-z][a-z0-9_]{0,23}`.
#[must_use]
pub fn valid_feature_code(code: &str) -> bool {
    let segments: Vec<&str> = code.split('.').collect();
    (1..=3).contains(&segments.len())
        && segments.iter().all(|segment| {
            let mut bytes = segment.bytes();
            segment.len() <= 24
                && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
                && bytes
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

impl FeatureMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        if self.features.is_empty()
            || self.features.len() > MAX_FEATURE_ENTRIES
            || !self
                .features
                .windows(2)
                .all(|pair| pair[0].feature < pair[1].feature)
        {
            return Err(TelemetryValidationError::FeatureEntries);
        }
        for entry in &self.features {
            bounded_many(&[entry.count, entry.failures])?;
            if !valid_feature_code(&entry.feature) {
                return Err(TelemetryValidationError::FeatureCode);
            }
            if entry.count == 0 || entry.failures > entry.count {
                return Err(TelemetryValidationError::InconsistentCounts);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionMetrics {
    pub admitted: u64,
    pub denied: u64,
    pub fallback: u64,
}

impl DecisionMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        bounded_many(&[self.admitted, self.denied, self.fallback])
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncMetrics {
    pub attempts: u64,
    pub successes: u64,
    pub failures: u64,
}

impl SyncMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        bounded_many(&[self.attempts, self.successes, self.failures])?;
        if self.successes.saturating_add(self.failures) > self.attempts {
            return Err(TelemetryValidationError::InconsistentCounts);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorMetrics {
    pub category: ErrorCategory,
    pub count: u64,
}

impl ErrorMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        bounded(self.count)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionUpgradeMetrics {
    pub from_major: u16,
    pub to_major: u16,
}

impl VersionUpgradeMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        if self.to_major < self.from_major {
            return Err(TelemetryValidationError::VersionDirection);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationMetrics {
    pub admitted_tasks: u64,
    pub execution_plans: u64,
    pub retries: u64,
    pub fallbacks: u64,
    pub cancellations: u64,
    pub lease_conflicts: u64,
    pub receipts: u64,
    pub outcomes: OutcomeMetrics,
    pub node_count: Histogram,
    pub fan_out: Histogram,
    pub depth: Histogram,
    pub parallel_nodes: Histogram,
}

impl OrchestrationMetrics {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        bounded_many(&[
            self.admitted_tasks,
            self.execution_plans,
            self.retries,
            self.fallbacks,
            self.cancellations,
            self.lease_conflicts,
            self.receipts,
        ])?;
        self.outcomes.validate()?;
        self.node_count.validate()?;
        self.fan_out.validate()?;
        self.depth.validate()?;
        self.parallel_nodes.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Histogram {
    pub upper_bounds: Vec<u64>,
    pub counts: Vec<u64>,
}

impl Histogram {
    fn validate(&self) -> Result<(), TelemetryValidationError> {
        if self.upper_bounds.is_empty()
            || self.upper_bounds.len() > MAX_HISTOGRAM_BUCKETS
            || self.upper_bounds.len() != self.counts.len()
            || !self
                .upper_bounds
                .windows(2)
                .all(|window| window[0] < window[1])
        {
            return Err(TelemetryValidationError::Histogram);
        }
        bounded_many(&self.counts)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistributionChannel {
    Cargo,
    Homebrew,
    Npm,
    Docker,
    Source,
    Aur,
    Pypi,
    /// A release binary installed by the install script or by hand.
    Binary,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientFamily {
    Claude,
    Codex,
    Cursor,
    Gemini,
    Windsurf,
    Zed,
    VscodeCopilot,
    Kiro,
    Antigravity,
    Codebuddy,
    Codewhale,
    // 3.11.1: more MCP hosts. The server accepts these from 3.11.1 on.
    Cline,
    RooCode,
    KiloCode,
    ContinueDev,
    Opencode,
    Goose,
    Amp,
    Augment,
    Jetbrains,
    Warp,
    Trae,
    QwenCode,
    Crush,
    ClaudeDesktop,
    Chatgpt,
    LmStudio,
    CopilotCli,
    VisualStudio,
    Neovim,
    Emacs,
    Factory,
    Pi,
    OhMyPi,
    /// No MCP client was seen: LeanCTX runs only through the CLI or shell hooks.
    None,
    /// An MCP client LeanCTX does not recognise.
    Other,
}

impl ClientFamily {
    /// Maps a `client_capabilities` client id (from the MCP initialize
    /// handshake) onto the fixed wire enum; unknown ids never leak as text.
    #[must_use]
    pub fn from_client_id(id: &str) -> Option<Self> {
        Some(match id {
            "claude-code" => Self::Claude,
            "codex" => Self::Codex,
            "cursor" => Self::Cursor,
            "gemini-cli" => Self::Gemini,
            "windsurf" => Self::Windsurf,
            "zed" => Self::Zed,
            "vscode-copilot" => Self::VscodeCopilot,
            "kiro" => Self::Kiro,
            "antigravity" => Self::Antigravity,
            "codebuddy" => Self::Codebuddy,
            "codewhale" => Self::Codewhale,
            "cline" => Self::Cline,
            "roo-code" => Self::RooCode,
            "kilo-code" => Self::KiloCode,
            "continue" => Self::ContinueDev,
            "opencode" => Self::Opencode,
            "goose" => Self::Goose,
            "amp" => Self::Amp,
            "augment" => Self::Augment,
            "jetbrains" => Self::Jetbrains,
            "warp" => Self::Warp,
            "trae" => Self::Trae,
            "qwen-code" => Self::QwenCode,
            "crush" => Self::Crush,
            "claude-desktop" => Self::ClaudeDesktop,
            "chatgpt" => Self::Chatgpt,
            "lm-studio" => Self::LmStudio,
            "copilot-cli" => Self::CopilotCli,
            "visual-studio" => Self::VisualStudio,
            "neovim" => Self::Neovim,
            "emacs" => Self::Emacs,
            "factory" => Self::Factory,
            "pi" => Self::Pi,
            "oh-my-pi" => Self::OhMyPi,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    Macos,
    Linux,
    Windows,
    Other,
}

impl OperatingSystem {
    #[must_use]
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Self::Macos,
            "linux" => Self::Linux,
            "windows" => Self::Windows,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
    Other,
}

impl Architecture {
    #[must_use]
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Authentication,
    Authorization,
    Configuration,
    Network,
    Provider,
    Timeout,
    Validation,
    Internal,
    /// A command run through a shell tool exited non-zero: the user's
    /// command failed, not LeanCTX. Sent from 3.11.1 on.
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryValidationError {
    SchemaVersion,
    DeletionTokenHash,
    TimestampBucket,
    InstallationId,
    PseudonymousId,
    AppVersion,
    CountBound,
    InconsistentCounts,
    Histogram,
    VersionDirection,
    BatchSize,
    ToolEntries,
    ToolName,
    History,
    FeatureEntries,
    FeatureCode,
}

fn bounded(value: u64) -> Result<(), TelemetryValidationError> {
    if value <= MAX_COUNT {
        Ok(())
    } else {
        Err(TelemetryValidationError::CountBound)
    }
}

fn bounded_many(values: &[u64]) -> Result<(), TelemetryValidationError> {
    values.iter().try_for_each(|value| bounded(*value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(event: TelemetryEventV2) -> TelemetryEnvelopeV2 {
        TelemetryEnvelopeV2 {
            schema_version: SCHEMA_VERSION,
            timestamp_bucket: "2026-09-08".into(),
            installation_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            account_id: None,
            organization_id: None,
            app_version: "4.0.0".into(),
            event,
        }
    }

    #[test]
    fn valid_typed_event_roundtrips() {
        let event = envelope(TelemetryEventV2::ToolUsageAggregate(ToolUsageMetrics {
            calls: 4,
            failures: 1,
            latency_milliseconds: Histogram {
                upper_bounds: vec![10, 100],
                counts: vec![1, 3],
            },
            tokens: Some(TokenMetrics {
                original: 12_000,
                delivered: 3_000,
            }),
        }));
        event.validate().unwrap();
        let encoded = serde_json::to_vec(&event).unwrap();
        let decoded: TelemetryEnvelopeV2 = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn batch_is_bounded_and_validates_every_event() {
        let event = envelope(TelemetryEventV2::Heartbeat(HeartbeatMetrics {
            distribution_channel: DistributionChannel::Cargo,
            client_family: ClientFamily::Codex,
            operating_system: OperatingSystem::Linux,
            architecture: Architecture::X86_64,
            install_age: None,
            active_days: None,
            runtime_environment: None,
        }));
        let batch = TelemetryBatchV2 {
            schema_version: SCHEMA_VERSION,
            deletion_token_hash: "a".repeat(64),
            events: vec![event],
        };
        assert!(batch.validate().is_ok());
        let mut invalid_credential = batch.clone();
        invalid_credential.deletion_token_hash = "raw-secret".into();
        assert_eq!(
            invalid_credential.validate(),
            Err(TelemetryValidationError::DeletionTokenHash)
        );
        let empty = TelemetryBatchV2 {
            schema_version: SCHEMA_VERSION,
            deletion_token_hash: "a".repeat(64),
            events: Vec::new(),
        };
        assert_eq!(empty.validate(), Err(TelemetryValidationError::BatchSize));
    }

    #[test]
    fn ops_context_fields_use_closed_wire_names_and_are_omitted_when_unset() {
        let mut metrics = HeartbeatMetrics {
            distribution_channel: DistributionChannel::Aur,
            client_family: ClientFamily::Codex,
            operating_system: OperatingSystem::Linux,
            architecture: Architecture::X86_64,
            install_age: None,
            active_days: None,
            runtime_environment: None,
        };
        let bare = serde_json::to_value(&metrics).unwrap();
        assert_eq!(
            bare.as_object().unwrap().len(),
            4,
            "unset fields stay off the wire: {bare}"
        );
        metrics.install_age = Some(InstallAge::Lt1h);
        metrics.active_days = Some(ActiveDays::D2To3);
        metrics.runtime_environment = Some(RuntimeEnvironment::CloudAgent);
        let full = serde_json::to_value(&metrics).unwrap();
        assert_eq!(full["distribution_channel"], "aur");
        assert_eq!(full["install_age"], "lt_1h");
        assert_eq!(full["active_days"], "d2_3");
        assert_eq!(full["runtime_environment"], "cloud_agent");
        let decoded: HeartbeatMetrics = serde_json::from_value(full).unwrap();
        assert_eq!(decoded, metrics);
        assert!(
            serde_json::from_value::<HeartbeatMetrics>(serde_json::json!({
                "distribution_channel": "cargo", "client_family": "codex",
                "operating_system": "linux", "architecture": "x86_64", "install_age": "3 days"
            }))
            .is_err()
        );
    }

    #[test]
    fn ops_context_buckets_have_exact_boundaries() {
        assert_eq!(InstallAge::from_seconds(0), InstallAge::Lt1h);
        assert_eq!(InstallAge::from_seconds(3_599), InstallAge::Lt1h);
        assert_eq!(InstallAge::from_seconds(3_600), InstallAge::Lt1d);
        assert_eq!(InstallAge::from_seconds(86_400), InstallAge::Lt7d);
        assert_eq!(InstallAge::from_seconds(604_800), InstallAge::Lt30d);
        assert_eq!(InstallAge::from_seconds(2_592_000), InstallAge::Gte30d);
        assert_eq!(ActiveDays::from_count(0), ActiveDays::D1);
        assert_eq!(ActiveDays::from_count(1), ActiveDays::D1);
        assert_eq!(ActiveDays::from_count(3), ActiveDays::D2To3);
        assert_eq!(ActiveDays::from_count(7), ActiveDays::D4To7);
        assert_eq!(ActiveDays::from_count(14), ActiveDays::D8To14);
        assert_eq!(ActiveDays::from_count(15), ActiveDays::D15Plus);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let raw = r#"{"schema_version":2,"timestamp_bucket":"2026-09-08","installation_id":"550e8400-e29b-41d4-a716-446655440000","app_version":"4.0.0","raw_task":"secret","event":{"name":"heartbeat","metrics":{"distribution_channel":"cargo","client_family":"codex","operating_system":"linux","architecture":"x86_64"}}}"#;
        assert!(serde_json::from_str::<TelemetryEnvelopeV2>(raw).is_err());
    }

    #[test]
    fn event_wrapper_preserves_valid_v2_wire_bytes() {
        let raw = r#"{"name":"setup_completed","metrics":{"count":1}}"#;
        let decoded: TelemetryEventV2 = serde_json::from_str(raw).unwrap();
        assert_eq!(decoded.name(), "setup_completed");
        decoded.validate().unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), raw);
        let event = envelope(decoded);
        let encoded = serde_json::to_vec(&event).unwrap();
        let decoded: TelemetryEnvelopeV2 = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, event);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
    }

    #[test]
    fn event_wrapper_rejects_forbidden_fields_in_every_key_position() {
        // Synthetic privacy fixtures only; no captured content or credentials.
        let forbidden = [
            ("source_code", "fn synthetic_fixture() {}"),
            ("absolute_path", "/synthetic/private/example.rs"),
            ("prompt", "synthetic private task instructions"),
            ("secret", "SYNTHETIC_NOT_A_CREDENTIAL"),
            (
                "repository_url",
                "https://example.invalid/private/project.git",
            ),
        ];
        for base in [
            r#"{"name":"setup_completed","metrics":{"count":1}}"#,
            r#"{"metrics":{"count":1},"name":"setup_completed"}"#,
        ] {
            let valid: TelemetryEventV2 = serde_json::from_str(base).unwrap();
            valid.validate().unwrap();
            for (field, value) in forbidden {
                let property = format!(
                    "{}:{}",
                    serde_json::to_string(field).unwrap(),
                    serde_json::to_string(value).unwrap()
                );
                let middle = base.find(',').unwrap();
                for injected in [
                    format!("{{{property},{}", &base[1..]),
                    format!("{},{property}{}", &base[..middle], &base[middle..]),
                    format!("{},{property}}}", &base[..base.len() - 1]),
                ] {
                    // Establish valid JSON independently: parse errors must be
                    // caused by the forbidden event field, not broken fixtures.
                    let parsed: serde_json::Value = serde_json::from_str(&injected).unwrap();
                    assert_eq!(parsed[field], value);
                    assert!(
                        serde_json::from_str::<TelemetryEventV2>(&injected).is_err(),
                        "unknown event field accepted: {field}"
                    );
                    let mut wrapped = serde_json::to_value(envelope(valid.clone())).unwrap();
                    wrapped["event"] = parsed;
                    assert!(serde_json::from_value::<TelemetryEnvelopeV2>(wrapped).is_err());
                }
            }
        }
    }

    #[test]
    fn malformed_identity_date_and_version_are_rejected() {
        let mut event = envelope(TelemetryEventV2::Heartbeat(HeartbeatMetrics {
            distribution_channel: DistributionChannel::Cargo,
            client_family: ClientFamily::Codex,
            operating_system: OperatingSystem::Linux,
            architecture: Architecture::X86_64,
            install_age: None,
            active_days: None,
            runtime_environment: None,
        }));
        event.installation_id = "not-a-uuid".into();
        assert_eq!(
            event.validate(),
            Err(TelemetryValidationError::InstallationId)
        );
        event.installation_id = "550e8400-e29b-41d4-a716-446655440000".into();
        event.timestamp_bucket = "2026-02-30".into();
        assert_eq!(
            event.validate(),
            Err(TelemetryValidationError::TimestampBucket)
        );
        event.timestamp_bucket = "2026-09-08".into();
        event.schema_version = 3;
        assert_eq!(
            event.validate(),
            Err(TelemetryValidationError::SchemaVersion)
        );
    }

    #[test]
    fn bounds_and_cross_field_counts_fail_closed() {
        let event = envelope(TelemetryEventV2::SyncAggregate(SyncMetrics {
            attempts: 1,
            successes: 1,
            failures: 1,
        }));
        assert_eq!(
            event.validate(),
            Err(TelemetryValidationError::InconsistentCounts)
        );
        let event = envelope(TelemetryEventV2::SetupCompleted(OccurrenceMetrics {
            count: MAX_COUNT + 1,
        }));
        assert_eq!(event.validate(), Err(TelemetryValidationError::CountBound));
    }

    #[test]
    fn histogram_shape_and_order_are_bounded() {
        let histogram = Histogram {
            upper_bounds: vec![100, 10],
            counts: vec![1, 1],
        };
        assert_eq!(
            histogram.validate(),
            Err(TelemetryValidationError::Histogram)
        );
    }

    #[test]
    fn pseudonymous_ids_never_accept_raw_identifiers() {
        assert!(PseudonymousId::new("customer@example.com").is_err());
        assert!(PseudonymousId::new(format!("hmac-sha256:{}", "a".repeat(64))).is_ok());
    }

    #[test]
    fn pseudonymous_id_deserialization_preserves_constructor_invariant() {
        for raw in [
            "synthetic@example.invalid".to_owned(),
            "/synthetic/private/identity".to_owned(),
            String::new(),
            format!("hmac-sha256:{}", "a".repeat(63)),
            format!("hmac-sha256:{}", "A".repeat(64)),
            format!("hmac-sha256:{}", "g".repeat(64)),
        ] {
            assert!(PseudonymousId::new(raw.clone()).is_err());
            let encoded = serde_json::to_string(&raw).unwrap();
            let result = serde_json::from_str::<PseudonymousId>(&encoded);
            assert!(result.is_err(), "constructor invariant bypassed on wire");
            let message = result.unwrap_err().to_string();
            assert!(message.starts_with("invalid pseudonymous ID"));
            if !raw.is_empty() {
                assert!(!message.contains(&raw));
            }
        }
        let raw = format!("hmac-sha256:{}", "a".repeat(64));
        let encoded = serde_json::to_string(&raw).unwrap();
        let decoded: PseudonymousId = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, PseudonymousId::new(raw).unwrap());
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    }

    #[test]
    fn envelope_rejects_raw_pseudonymous_ids_before_semantic_validation() {
        let event = envelope(TelemetryEventV2::SetupCompleted(OccurrenceMetrics {
            count: 1,
        }));
        for field in ["account_id", "organization_id"] {
            let mut wire = serde_json::to_value(&event).unwrap();
            wire[field] = serde_json::json!("synthetic@example.invalid");
            assert!(serde_json::from_value::<TelemetryEnvelopeV2>(wire).is_err());
        }
        let mut event_with_ids = event;
        event_with_ids.account_id =
            Some(PseudonymousId::new(format!("hmac-sha256:{}", "a".repeat(64))).unwrap());
        event_with_ids.organization_id =
            Some(PseudonymousId::new(format!("hmac-sha256:{}", "b".repeat(64))).unwrap());
        let encoded = serde_json::to_vec(&event_with_ids).unwrap();
        let decoded: TelemetryEnvelopeV2 = serde_json::from_slice(&encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, event_with_ids);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
    }

    fn tool_call(tool: &str, calls: u64, failures: u64) -> ToolCallCount {
        ToolCallCount {
            tool: tool.into(),
            calls,
            failures,
            latency_milliseconds_total: None,
            failure_kinds: None,
            failure_messages: None,
        }
    }

    fn tool_call_event(tools: Vec<ToolCallCount>) -> TelemetryEnvelopeV2 {
        envelope(TelemetryEventV2::ToolCallAggregate(ToolCallMetrics {
            tools,
        }))
    }

    #[test]
    fn setup_profile_is_closed_enums_on_the_wire() {
        let event = TelemetryEventV2::SetupProfile(SetupProfileMetrics {
            integration_mode: IntegrationMode::Replace,
            embeddings: EmbeddingsState::NotInstalled,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "setup_profile",
                "metrics": {"integration_mode": "replace", "embeddings": "not_installed"}
            })
        );
        assert_eq!(
            serde_json::from_value::<TelemetryEventV2>(json).unwrap(),
            event
        );
        for bad in [
            serde_json::json!({"name": "setup_profile", "metrics": {"integration_mode": "/home/me", "embeddings": "installed"}}),
            serde_json::json!({"name": "setup_profile", "metrics": {"integration_mode": "mcp", "embeddings": "installed", "model": "x"}}),
        ] {
            assert!(serde_json::from_value::<TelemetryEventV2>(bad).is_err());
        }
    }

    #[test]
    fn every_detected_client_id_maps_to_a_stable_wire_family() {
        let cases = [
            ("claude-code", "claude"),
            ("codex", "codex"),
            ("cursor", "cursor"),
            ("gemini-cli", "gemini"),
            ("windsurf", "windsurf"),
            ("zed", "zed"),
            ("vscode-copilot", "vscode_copilot"),
            ("kiro", "kiro"),
            ("antigravity", "antigravity"),
            ("codebuddy", "codebuddy"),
            ("codewhale", "codewhale"),
            ("pi", "pi"),
            ("oh-my-pi", "oh_my_pi"),
        ];
        for (id, wire) in cases {
            let family = ClientFamily::from_client_id(id).expect(id);
            assert_eq!(serde_json::to_value(family).unwrap(), wire);
        }
        assert_eq!(ClientFamily::from_client_id("unknown"), None);
        assert_eq!(ClientFamily::from_client_id("my-private-fork"), None);
    }

    #[test]
    fn tool_call_aggregate_roundtrips_on_the_wire() {
        let event = tool_call_event(vec![tool_call("ctx_read", 5, 1), tool_call("shell", 2, 0)]);
        event.validate().unwrap();
        let encoded = serde_json::to_string(&event.event).unwrap();
        assert_eq!(
            encoded,
            r#"{"name":"tool_call_aggregate","metrics":{"tools":[{"tool":"ctx_read","calls":5,"failures":1},{"tool":"shell","calls":2,"failures":0}]}}"#
        );
        let decoded: TelemetryEnvelopeV2 =
            serde_json::from_slice(&serde_json::to_vec(&event).unwrap()).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn tool_call_aggregate_rejects_free_text_and_inconsistent_entries() {
        let reject = |tools: Vec<ToolCallCount>, expected| {
            assert_eq!(tool_call_event(tools).validate(), Err(expected));
        };
        use TelemetryValidationError::{InconsistentCounts, ToolEntries, ToolName};
        reject(Vec::new(), ToolEntries);
        reject(
            vec![tool_call("ctx_shell", 1, 0), tool_call("ctx_read", 1, 0)],
            ToolEntries,
        );
        reject(
            vec![tool_call("ctx_read", 1, 0), tool_call("ctx_read", 1, 0)],
            ToolEntries,
        );
        for name in [
            "",
            "Ctx_read",
            "ctx-read",
            "/etc/passwd",
            "ctx read",
            "_ctx",
        ] {
            reject(vec![tool_call(name, 1, 0)], ToolName);
        }
        reject(
            vec![tool_call(&"a".repeat(MAX_TOOL_NAME_LEN + 1), 1, 0)],
            ToolName,
        );
        reject(vec![tool_call("ctx_read", 0, 0)], InconsistentCounts);
        reject(vec![tool_call("ctx_read", 1, 2)], InconsistentCounts);
        let too_many = (0..=MAX_TOOL_ENTRIES)
            .map(|index| tool_call(&format!("tool_{index:04}"), 1, 0))
            .collect();
        reject(too_many, ToolEntries);
        let unknown_field = r#"{"name":"tool_call_aggregate","metrics":{"tools":[{"tool":"ctx_read","calls":1,"failures":0,"path":"/secret"}]}}"#;
        assert!(serde_json::from_str::<TelemetryEventV2>(unknown_field).is_err());
    }
}
