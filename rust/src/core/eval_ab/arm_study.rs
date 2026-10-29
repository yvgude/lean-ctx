// SPDX-License-Identifier: Apache-2.0
//! Pure four-arm analysis of externally supplied observations.
//!
//! This is not evidence of temporal preregistration,
//! authenticated measurements, blind review, model execution or customer savings.
//!
//! What is analysed is bounded before anything is computed: the two documents by
//! their bytes, the protocol by its task count and bootstrap iterations, the
//! observations by their rows, and the whole analysis by the number of resample
//! draws it would make. An input over a bound is refused — nothing is truncated,
//! and neither the seed nor a statistic is changed to make it fit.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use super::model::{ObservationOrigin, ProviderExchange};
use super::report::AbReport;

pub const PROTOCOL_KIND: &str = "lean-ctx.arm-study-protocol";
pub const OBSERVATIONS_KIND: &str = "lean-ctx.arm-study-observations";
pub const REPORT_KIND: &str = "lean-ctx.arm-study-report";
pub const SCHEMA_VERSION: u32 = 1;
pub const MIN_TASKS: usize = 10;

/// The largest protocol and observations documents that are parsed at all.
pub const MAX_PROTOCOL_BYTES: usize = 1024 * 1024;
pub const MAX_OBSERVATIONS_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PROVIDER_REPORT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TASKS: usize = 1_000;
pub const MAX_BOOTSTRAP_ITERS: usize = 10_000;
pub const MAX_EVIDENCE_RECEIPTS: usize = 64;
/// Every interval resamples `tasks` values `bootstrap_iters` times; a report has
/// `INTERVALS` of them. Their product is the work of an analysis.
pub const MAX_BOOTSTRAP_DRAWS: u64 = 200_000_000;
/// An UPPER bound: four arms × six metrics, and three arms × four contrasts.
/// A real report makes at most 34 (two arms have no sync bytes), fewer when a
/// provider reported no cache reads; the budget is deliberately the larger number.
const INTERVALS: u64 = 4 * 6 + 3 * 4;

/// How [`ArmSummary::latency_ms_p95`] is taken.
pub const P95_RULE: &str = "nearest_rank";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Arm {
    UnchangedAgent,
    Community,
    Team,
    Company,
}

pub const ALL: [Arm; 4] = [Arm::UnchangedAgent, Arm::Community, Arm::Team, Arm::Company];

impl Arm {
    pub fn label(self) -> &'static str {
        match self {
            Self::UnchangedAgent => "unchanged_agent",
            Self::Community => "community",
            Self::Team => "team",
            Self::Company => "company",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Protocol {
    schema_version: u32,
    kind: String,
    study_id: String,
    tasks: Vec<RegisteredTask>,
    bootstrap_iters: usize,
    bootstrap_seed: u64,
    // In-process provenance only: not accepted from or emitted onto the wire.
    #[serde(skip)]
    parsed_sha256: Option<String>,
}

impl Protocol {
    /// The number of registered tasks.
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// The resamples every interval of this study asks for.
    pub fn bootstrap_iters(&self) -> usize {
        self.bootstrap_iters
    }

    /// The resample draws a whole analysis of this protocol makes, or `None`
    /// when that number does not fit — which is over every budget.
    pub fn bootstrap_draws(&self) -> Option<u64> {
        u64::try_from(self.tasks.len())
            .ok()?
            .checked_mul(u64::try_from(self.bootstrap_iters).ok()?)?
            .checked_mul(INTERVALS)
    }

    /// The bounds of this protocol alone, read from it and changing nothing.
    fn admit(&self) -> Result<(), Refusal> {
        if self.tasks.len() > MAX_TASKS {
            return Err(Refusal::Budget { what: "tasks" });
        }
        if self.bootstrap_iters > MAX_BOOTSTRAP_ITERS {
            return Err(Refusal::Budget {
                what: "bootstrap_iters",
            });
        }
        if self
            .bootstrap_draws()
            .is_none_or(|draws| draws > MAX_BOOTSTRAP_DRAWS)
        {
            return Err(Refusal::Budget {
                what: "bootstrap_draws",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredTask {
    pub task_id: String,
    pub statement_sha256: String,
    pub rubric_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observations {
    pub schema_version: u32,
    pub kind: String,
    pub protocol_sha256: String,
    pub observations: Vec<Observation>,
}

// deserialize_with has no Default: missing keys refuse, explicit null is None.
fn required_nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(d)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub task_id: String,
    pub arm: Arm,
    pub acl_version: String,
    pub model_fingerprint: String,
    pub rated_blind: bool,
    pub correctness: f64,
    pub latency_ms: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub provider_cache_read_tokens: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub sync_bytes: Option<u64>,
    pub evidence_receipts: Vec<String>,
    /// Optional, operator-supplied provenance. Not provider authentication or
    /// proof of the deployment arm, blindness or model consumption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_observation: Option<ProviderObservation>,
}

/// An A/B condition is selected explicitly and never mapped to a deployment arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderCondition {
    Baseline,
    LeanCtx,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderObservation {
    pub task_id: String,
    pub arm: Arm,
    pub condition: ProviderCondition,
    pub model_fingerprint: String,
    pub exchange: ProviderExchange,
}

/// Classification of supplied metadata, not authenticated measurements. These
/// counts have no confidence interval or claim of provider-subset power.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProvenance {
    pub current_reported: usize,
    pub replayed: usize,
    pub observed_partial: usize,
    pub unattributed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricSummary {
    pub n: usize,
    pub median: f64,
    pub ci_low: f64,
    pub ci_high: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmSummary {
    pub arm: Arm,
    pub correctness: MetricSummary,
    pub latency_ms: MetricSummary,
    /// The 95th percentile of the arm's latencies by nearest rank: the value at
    /// rank `ceil(0.95 × n)` of the ascending values. An observed value, with no
    /// interval. It is not `latency_ms.ci_high`, which bounds the MEDIAN.
    pub latency_ms_p95: f64,
    pub tokens_input: MetricSummary,
    pub tokens_output: MetricSummary,
    pub provider_cache_read_tokens: Option<MetricSummary>,
    pub sync_bytes: Option<MetricSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_provenance: Option<ProviderProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contrast {
    pub arm: Arm,
    pub metric: String,
    pub n: usize,
    pub median_delta: f64,
    pub ci_low: f64,
    pub ci_high: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmStudyReport {
    pub schema_version: u32,
    pub kind: String,
    pub study_id: String,
    pub protocol_sha256: String,
    pub tasks: usize,
    pub powered: bool,
    pub p95_rule: String,
    pub acl_version_by_task: BTreeMap<String, String>,
    pub arms: Vec<ArmSummary>,
    pub contrasts: Vec<Contrast>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Parse,
    /// An input is larger than this analysis admits; `what` names the bound.
    Budget {
        what: &'static str,
    },
    ProtocolSchema,
    ObservationSchema,
    TaskCount,
    TaskId {
        task: String,
    },
    TaskDigest {
        task: String,
    },
    Bootstrap,
    ProtocolBinding,
    UnknownTask {
        task: String,
        arm: Arm,
    },
    Duplicate {
        task: String,
        arm: Arm,
    },
    Missing {
        task: String,
        arm: Arm,
    },
    Acl {
        task: String,
        arm: Arm,
    },
    Model {
        task: String,
        arm: Arm,
    },
    Blind {
        task: String,
        arm: Arm,
    },
    Correctness {
        task: String,
        arm: Arm,
    },
    Evidence {
        task: String,
        arm: Arm,
    },
    Sync {
        task: String,
        arm: Arm,
    },
    Provider {
        task: String,
        arm: Arm,
        what: &'static str,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Only rule identity and the affected task/arm, never a measured value.
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Refusal {}

fn provider_refusal(task: &str, arm: Arm, what: &'static str) -> Refusal {
    Refusal::Provider {
        task: task.into(),
        arm,
        what,
    }
}

fn exchange_is_valid(exchange: &ProviderExchange) -> bool {
    bare_digest_is_valid(&exchange.request_body_sha256)
        && bare_digest_is_valid(&exchange.response_body_sha256)
        && exchange
            .usage
            .as_ref()
            .is_none_or(super::model::ProviderUsage::is_well_formed)
}

/// Pure handoff for an explicit producer: select one existing report's task and
/// A/B condition, then serialize the result in an observation. The CLI can load
/// an optional explicit A/B file; it never discovers one. `arm` is a caller declaration, not
/// something the A/B report proves; copied metadata remains operator supplied.
pub fn resolve_provider_observation(
    report: &AbReport,
    task_id: &str,
    arm: Arm,
    condition: ProviderCondition,
) -> Result<ProviderObservation, Refusal> {
    if report.records.len() > MAX_TASKS || report.provider_exchanges.len() > MAX_TASKS {
        return Err(Refusal::Budget {
            what: "provider report tasks",
        });
    }
    if report.kind != super::report::REPORT_KIND
        || report.schema_version != super::report::REPORT_SCHEMA_VERSION
    {
        return Err(provider_refusal(task_id, arm, "report schema"));
    }
    let mut records = report
        .records
        .iter()
        .filter(|record| record.task_id == task_id);
    if records.next().is_none() || records.next().is_some() {
        return Err(provider_refusal(task_id, arm, "unique task record"));
    }
    let mut exchanges = report
        .provider_exchanges
        .iter()
        .filter(|entry| entry.task_id == task_id);
    let entry = exchanges
        .next()
        .ok_or_else(|| provider_refusal(task_id, arm, "unique task exchange"))?;
    if exchanges.next().is_some() {
        return Err(provider_refusal(task_id, arm, "unique task exchange"));
    }
    let exchange = match condition {
        ProviderCondition::Baseline => entry.baseline.as_ref(),
        ProviderCondition::LeanCtx => entry.lean_ctx.as_ref(),
    }
    .ok_or_else(|| provider_refusal(task_id, arm, "selected condition"))?;
    if !exchange_is_valid(exchange) {
        return Err(provider_refusal(task_id, arm, "exchange evidence"));
    }
    Ok(ProviderObservation {
        task_id: task_id.into(),
        arm,
        condition,
        model_fingerprint: report.model.digest(),
        exchange: exchange.clone(),
    })
}

fn validate_provider_observation(row: &Observation) -> Result<(), Refusal> {
    let Some(provider) = &row.provider_observation else {
        return Ok(());
    };
    if provider.task_id != row.task_id
        || provider.arm != row.arm
        || provider.model_fingerprint != row.model_fingerprint
        || !bare_digest_is_valid(&provider.model_fingerprint)
        || !exchange_is_valid(&provider.exchange)
    {
        return Err(provider_refusal(&row.task_id, row.arm, "row binding"));
    }
    let usage = provider.exchange.usage.as_ref();
    if usage
        .and_then(|u| u.input_tokens)
        .is_some_and(|n| n != row.tokens_input)
        || usage
            .and_then(|u| u.output_tokens)
            .is_some_and(|n| n != row.tokens_output)
        || usage.and_then(|u| u.cached_input_tokens) != row.provider_cache_read_tokens
    {
        return Err(provider_refusal(&row.task_id, row.arm, "reported counters"));
    }
    Ok(())
}

fn provider_provenance(rows: &[&Observation]) -> Option<ProviderProvenance> {
    if rows.iter().all(|row| row.provider_observation.is_none()) {
        return None;
    }
    let mut counts = ProviderProvenance::default();
    for row in rows {
        match row.provider_observation.as_ref().map(|p| &p.exchange) {
            None => counts.unattributed += 1,
            Some(exchange) if exchange.origin == ObservationOrigin::Replayed => {
                counts.replayed += 1;
            }
            Some(exchange)
                if exchange.usage.as_ref().is_some_and(|usage| {
                    usage.input_tokens.is_some() && usage.output_tokens.is_some()
                }) =>
            {
                counts.current_reported += 1;
            }
            Some(_) => counts.observed_partial += 1,
        }
    }
    Some(counts)
}

/// An explicitly supplied raw report or signed v1 artifact. The selected type
/// decodes the ORIGINAL bytes, retaining known-field duplicate rejection. ROOT
/// checks are closed; unconsumed historical nesting keeps its existing tolerance.
pub fn parse_provider_report(bytes: &[u8]) -> Result<AbReport, Refusal> {
    if bytes.len() > MAX_PROVIDER_REPORT_BYTES {
        return Err(Refusal::Budget {
            what: "provider report bytes",
        });
    }
    let root: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Refusal::Parse)?;
    let object = root.as_object().ok_or(Refusal::Parse)?;
    match object.get("kind").and_then(serde_json::Value::as_str) {
        Some(super::report::REPORT_KIND) => {
            let report: AbReport = serde_json::from_slice(bytes).map_err(|_| Refusal::Parse)?;
            validate_provider_report_shape(&report, &root)?;
            Ok(report)
        }
        Some("lean-ctx.eval-ab-artifact") => {
            let artifact: super::artifact::SignedAbReportV1 =
                serde_json::from_slice(bytes).map_err(|_| Refusal::Parse)?;
            if object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "schema_version"
                        | "kind"
                        | "created_at"
                        | "lean_ctx_version"
                        | "agent_id"
                        | "determinism_digest"
                        | "verdict"
                        | "report"
                        | "signer_public_key"
                        | "signature"
                )
            }) {
                return Err(Refusal::Parse);
            }
            if artifact.schema_version != 1 || artifact.kind != "lean-ctx.eval-ab-artifact" {
                return Err(provider_refusal("", Arm::UnchangedAgent, "artifact schema"));
            }
            validate_provider_report_shape(
                &artifact.report,
                object.get("report").ok_or(Refusal::Parse)?,
            )?;
            // Announced-key integrity/internal consistency only: this is not
            // enrolled identity, provider/model use or study authentication.
            // Verification covers canonical typed fields, not every raw byte or
            // ignored historical nested field; known exchanges are included.
            if artifact.verdict != artifact.report.verdict || !artifact.verify().ok() {
                return Err(provider_refusal(
                    "",
                    Arm::UnchangedAgent,
                    "artifact integrity",
                ));
            }
            Ok(artifact.report)
        }
        _ => Err(provider_refusal("", Arm::UnchangedAgent, "report kind")),
    }
}

fn validate_provider_report_shape(
    report: &AbReport,
    root: &serde_json::Value,
) -> Result<(), Refusal> {
    let object = root.as_object().ok_or(Refusal::Parse)?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "schema_version"
                | "kind"
                | "created_at"
                | "lean_ctx_version"
                | "suite"
                | "budget_tokens"
                | "model"
                | "records"
                | "stats"
                | "verdict"
                | "evidence_tier"
                | "power"
                | "provider_exchanges"
        )
    }) {
        return Err(Refusal::Parse);
    }
    if report.kind != super::report::REPORT_KIND
        || report.schema_version != super::report::REPORT_SCHEMA_VERSION
    {
        return Err(provider_refusal("", Arm::UnchangedAgent, "report schema"));
    }
    if report.records.len() > MAX_TASKS || report.provider_exchanges.len() > MAX_TASKS {
        return Err(Refusal::Budget {
            what: "provider report tasks",
        });
    }
    Ok(())
}

/// Attach operator-declared provenance to one arm only. Every pending row is
/// resolved and checked before any input is changed; an error leaves it intact.
pub fn attach_provider_observations(
    observations: &mut Observations,
    report: &AbReport,
    arm: Arm,
    condition: ProviderCondition,
) -> Result<(), Refusal> {
    if observations.observations.len() > ALL.len() * MAX_TASKS {
        return Err(Refusal::Budget {
            what: "observation rows",
        });
    }
    if report.records.len() > MAX_TASKS || report.provider_exchanges.len() > MAX_TASKS {
        return Err(Refusal::Budget {
            what: "provider report tasks",
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut pending = Vec::new();
    for (index, row) in observations.observations.iter().enumerate() {
        if row.arm != arm {
            continue;
        }
        if !seen.insert(row.task_id.as_str()) {
            return Err(provider_refusal(&row.task_id, arm, "unique selected row"));
        }
        let resolved = resolve_provider_observation(report, &row.task_id, arm, condition)?;
        if row
            .provider_observation
            .as_ref()
            .is_some_and(|existing| existing != &resolved)
        {
            return Err(provider_refusal(&row.task_id, arm, "existing provenance"));
        }
        let mut checked = row.clone();
        checked.provider_observation = Some(resolved.clone());
        validate_provider_observation(&checked)?;
        pending.push((index, resolved));
    }
    if pending.is_empty() {
        return Err(provider_refusal("", arm, "selected arm rows"));
    }
    for (index, resolved) in pending {
        observations.observations[index].provider_observation = Some(resolved);
    }
    Ok(())
}

pub fn parse_protocol(bytes: &[u8]) -> Result<(Protocol, String), Refusal> {
    if bytes.len() > MAX_PROTOCOL_BYTES {
        return Err(Refusal::Budget {
            what: "protocol bytes",
        });
    }
    let mut protocol: Protocol = serde_json::from_slice(bytes).map_err(|_| Refusal::Parse)?;
    protocol.admit()?;
    let digest = super::sha256_hex(bytes);
    protocol.parsed_sha256 = Some(digest.clone());
    Ok((protocol, digest))
}

pub fn parse_observations(bytes: &[u8]) -> Result<Observations, Refusal> {
    if bytes.len() > MAX_OBSERVATIONS_BYTES {
        return Err(Refusal::Budget {
            what: "observations bytes",
        });
    }
    serde_json::from_slice(bytes).map_err(|_| Refusal::Parse)
}

fn digest_is_valid(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(bare_digest_is_valid)
}

fn bare_digest_is_valid(hex: &str) -> bool {
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        f64::midpoint(values[mid - 1], values[mid])
    } else {
        values[mid]
    }
}

/// Nearest rank: the value at rank `ceil(0.95 × n)` (1-based) of the ascending
/// values. `values` is never empty here: every arm has one row per task and a
/// study has at least [`MIN_TASKS`].
fn p95_nearest_rank(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let rank = (values.len() * 95).div_ceil(100).max(1);
    values[rank - 1]
}

fn summarize(mut values: Vec<f64>, protocol: &Protocol) -> MetricSummary {
    let (ci_low, ci_high) = super::report::bootstrap_ci_with(
        &values,
        protocol.bootstrap_iters,
        protocol.bootstrap_seed,
        median,
    );
    MetricSummary {
        n: values.len(),
        median: median(&mut values),
        ci_low,
        ci_high,
    }
}

const METRICS: [&str; 4] = ["correctness", "latency_ms", "tokens_input", "tokens_output"];

fn metric(row: &Observation, index: usize) -> f64 {
    match index {
        0 => row.correctness,
        1 => row.latency_ms as f64,
        2 => row.tokens_input as f64,
        _ => row.tokens_output as f64,
    }
}

pub fn analyze(
    protocol: &Protocol,
    protocol_sha256: &str,
    observations: &Observations,
) -> Result<ArmStudyReport, Refusal> {
    // Resources first, from read-only facts of the inputs: a protocol that did
    // not come through `parse_protocol` is bounded here all the same, and no row
    // is looked at before the rows are known to be at most four per task.
    protocol.admit()?;
    if observations.observations.len() > protocol.tasks.len().saturating_mul(ALL.len()) {
        return Err(Refusal::Budget {
            what: "observation rows",
        });
    }
    if protocol.schema_version != SCHEMA_VERSION || protocol.kind != PROTOCOL_KIND {
        return Err(Refusal::ProtocolSchema);
    }
    if observations.schema_version != SCHEMA_VERSION || observations.kind != OBSERVATIONS_KIND {
        return Err(Refusal::ObservationSchema);
    }
    if protocol.tasks.len() < MIN_TASKS {
        return Err(Refusal::TaskCount);
    }
    let mut tasks = BTreeMap::new();
    for task in &protocol.tasks {
        if tasks.insert(task.task_id.as_str(), task).is_some() {
            return Err(Refusal::TaskId {
                task: task.task_id.clone(),
            });
        }
        if !digest_is_valid(&task.statement_sha256) || !digest_is_valid(&task.rubric_sha256) {
            return Err(Refusal::TaskDigest {
                task: task.task_id.clone(),
            });
        }
    }
    if protocol.bootstrap_iters == 0 {
        return Err(Refusal::Bootstrap);
    }
    if protocol.parsed_sha256.as_deref() != Some(protocol_sha256)
        || observations.protocol_sha256 != protocol_sha256
    {
        return Err(Refusal::ProtocolBinding);
    }
    let mut rows = BTreeMap::new();
    for row in &observations.observations {
        let task = row.task_id.clone();
        let arm = row.arm;
        if !tasks.contains_key(row.task_id.as_str()) {
            return Err(Refusal::UnknownTask { task, arm });
        }
        if rows.insert((row.task_id.as_str(), arm), row).is_some() {
            return Err(Refusal::Duplicate { task, arm });
        }
        if !row.rated_blind {
            return Err(Refusal::Blind { task, arm });
        }
        if !row.correctness.is_finite() || !(0.0..=1.0).contains(&row.correctness) {
            return Err(Refusal::Correctness { task, arm });
        }
        if row.evidence_receipts.is_empty()
            || row.evidence_receipts.len() > MAX_EVIDENCE_RECEIPTS
            || !row.evidence_receipts.iter().all(|s| digest_is_valid(s))
        {
            return Err(Refusal::Evidence { task, arm });
        }
        if row.sync_bytes.is_some() != matches!(arm, Arm::Team | Arm::Company) {
            return Err(Refusal::Sync { task, arm });
        }
        validate_provider_observation(row)?;
    }
    let mut acl_version_by_task = BTreeMap::new();
    for &task in tasks.keys() {
        let mut first: Option<&Observation> = None;
        for arm in ALL {
            let row = rows.get(&(task, arm)).ok_or_else(|| Refusal::Missing {
                task: task.into(),
                arm,
            })?;
            if row.model_fingerprint.is_empty()
                || first.is_some_and(|base| base.model_fingerprint != row.model_fingerprint)
            {
                return Err(Refusal::Model {
                    task: task.into(),
                    arm,
                });
            }
            if first.is_some_and(|base| base.acl_version != row.acl_version) {
                return Err(Refusal::Acl {
                    task: task.into(),
                    arm,
                });
            }
            first = Some(row);
        }
        // All four rows are present, so this lookup is proven above.
        acl_version_by_task.insert(
            task.into(),
            rows[&(task, Arm::UnchangedAgent)].acl_version.clone(),
        );
    }
    let mut arms = Vec::new();
    let mut contrasts = Vec::new();
    for arm in ALL {
        let arm_rows: Vec<_> = tasks.keys().map(|&task| rows[&(task, arm)]).collect();
        let values =
            |index| -> Vec<f64> { arm_rows.iter().map(|row| metric(row, index)).collect() };
        let nullable = |select: fn(&Observation) -> Option<u64>| {
            arm_rows
                .iter()
                .map(|row| select(row).map(|v| v as f64))
                .collect::<Option<Vec<_>>>()
                .map(|values| summarize(values, protocol))
        };
        arms.push(ArmSummary {
            arm,
            correctness: summarize(values(0), protocol),
            latency_ms: summarize(values(1), protocol),
            latency_ms_p95: p95_nearest_rank(values(1)),
            tokens_input: summarize(values(2), protocol),
            tokens_output: summarize(values(3), protocol),
            provider_cache_read_tokens: nullable(|row| row.provider_cache_read_tokens),
            sync_bytes: nullable(|row| row.sync_bytes),
            provider_provenance: provider_provenance(&arm_rows),
        });
        if arm != Arm::UnchangedAgent {
            for (index, name) in METRICS.iter().enumerate() {
                let deltas = tasks
                    .keys()
                    .map(|&task| {
                        metric(rows[&(task, arm)], index)
                            - metric(rows[&(task, Arm::UnchangedAgent)], index)
                    })
                    .collect();
                let summary = summarize(deltas, protocol);
                contrasts.push(Contrast {
                    arm,
                    metric: (*name).into(),
                    n: summary.n,
                    median_delta: summary.median,
                    ci_low: summary.ci_low,
                    ci_high: summary.ci_high,
                });
            }
        }
    }
    Ok(ArmStudyReport {
        schema_version: SCHEMA_VERSION,
        kind: REPORT_KIND.into(),
        study_id: protocol.study_id.clone(),
        protocol_sha256: protocol_sha256.into(),
        tasks: tasks.len(),
        powered: tasks.len() >= super::report::MIN_POWERED_PAIRS,
        p95_rule: P95_RULE.into(),
        acl_version_by_task,
        arms,
        contrasts,
    })
}

impl ArmStudyReport {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn render(&self) -> String {
        use std::fmt::Write;
        let mut out = format!(
            "{} tasks, powered: {}\n",
            self.tasks,
            if self.powered { "yes" } else { "no" }
        );
        out.push_str("arm metric n median ci_low ci_high\n");
        for arm in &self.arms {
            for (name, summary) in [
                ("correctness", Some(&arm.correctness)),
                ("latency_ms", Some(&arm.latency_ms)),
                ("tokens_input", Some(&arm.tokens_input)),
                ("tokens_output", Some(&arm.tokens_output)),
                (
                    "provider_cache_read_tokens",
                    arm.provider_cache_read_tokens.as_ref(),
                ),
                ("sync_bytes", arm.sync_bytes.as_ref()),
            ] {
                if let Some(s) = summary {
                    let _ = writeln!(
                        out,
                        "{} {} {} {} {} {}",
                        arm.arm.label(),
                        name,
                        s.n,
                        s.median,
                        s.ci_low,
                        s.ci_high
                    );
                }
            }
        }
        let _ = writeln!(out, "arm latency_ms_p95 ({})", self.p95_rule);
        for arm in &self.arms {
            let _ = writeln!(out, "{} {}", arm.arm.label(), arm.latency_ms_p95);
        }
        if self
            .arms
            .iter()
            .any(|arm| arm.provider_provenance.is_some())
        {
            out.push_str(
                "Metrics are externally supplied; provenance is declared local metadata, not authenticated.\n\
                 Provider counts have no claim of subset power; study power is for all tasks.\n\
                 arm provider_provenance current_reported replayed observed_partial unattributed\n",
            );
            for arm in &self.arms {
                if let Some(p) = &arm.provider_provenance {
                    let _ = writeln!(
                        out,
                        "{} provider_provenance {} {} {} {}",
                        arm.arm.label(),
                        p.current_reported,
                        p.replayed,
                        p.observed_partial,
                        p.unattributed
                    );
                }
            }
        }
        out.push_str("arm metric n median_delta ci_low ci_high\n");
        for c in &self.contrasts {
            let _ = writeln!(
                out,
                "{} {} {} {} {} {}",
                c.arm.label(),
                c.metric,
                c.n,
                c.median_delta,
                c.ci_low,
                c.ci_high
            );
        }
        out
    }
}

#[cfg(test)]
#[path = "arm_study_tests.rs"]
mod tests;
