// SPDX-License-Identifier: Apache-2.0

//! Optional Task Compute (Release C, BAUPLAN v1.1 §9; C0 2.x
//! `schemas/compute.schema.json`, `routes/compute_*`,
//! `vectors/compute_cost.vectors.json`).
//!
//! An existing agent hands one clearly defined job (tests, static analysis,
//! an index build) to an isolated, short-lived sandbox and gets a bounded
//! result back. There is no Agent Compute: no model loop runs on the server.
//!
//! Engine routes (all with `Hive-Protocol: 2.x`; a server with Task Compute
//! switched off answers every one `404 feature_disabled`, feature `compute`):
//!
//! - E47 `POST /v1/compute/jobs` ([`ComputeSubmitRequestV1`] →
//!   [`ComputeJobResponseV1`], 201 new, 200 the job of the same `client_ref`)
//! - E48 `GET /v1/compute/jobs/{job_id}` → [`ComputeJobResponseV1`]
//! - E49 `POST /v1/compute/jobs/{job_id}/cancel` → [`ComputeJobResponseV1`]
//! - E50 `GET /v1/compute/jobs/{job_id}/result` → [`ComputeResultResponseV1`]
//!   (409 `invalid_state` while the job is queued or running)
//! - E51 `PUT /v1/compute/bundles/{digest}` (`application/octet-stream`, at
//!   most [`MAX_BUNDLE_BYTES`]) → [`ComputeBundleResponseV1`]
//! - E52 `GET /v1/compute/jobs/{job_id}/blobs/{digest}`: the bytes of one
//!   artifact or of the logs of that job
//!
//! Money: prepaid credit only, 1 credit = 1 vCPU-minute. The maximum cost
//! ([`reserve_for`]) is reserved at submit and must fit the job's budget; the
//! real cost ([`spent_for`]) is settled at the end and the rest refunded.
//! Refusals: `402 credit_exhausted`, `409 cap_reached`, `404
//! feature_disabled` ([`ComputeRefusalV1`]).
//!
//! Requests refuse unknown fields; responses accept them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::require_schema;
use crate::ValidationError;

/// `schema` of an E47 submit request.
pub const COMPUTE_JOB_SCHEMA: &str = "leanctx.hive.compute-job/v1";
/// `feature` of the `feature_disabled` refusal.
pub const COMPUTE_FEATURE: &str = "compute";
/// Largest input bundle, artifact and upload body (64 MiB).
pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;
/// Bundles per job.
pub const MAX_INPUT_BUNDLES: usize = 16;
/// `inputs.version` this engine writes and reads.
pub const INPUTS_VERSION: u8 = 1;
/// Command bounds.
pub const MAX_ARGV: usize = 32;
pub const MAX_ARG_BYTES: usize = 512;
pub const MAX_WORKDIR_BYTES: usize = 128;
/// Resource bounds of one job.
pub const CPU_MIN: u32 = 1;
pub const CPU_MAX: u32 = 8;
pub const MEMORY_MB_MIN: u32 = 256;
pub const MEMORY_MB_MAX: u32 = 16_384;
pub const DISK_MB_MIN: u32 = 256;
pub const DISK_MB_MAX: u32 = 20_480;
pub const RUNTIME_S_MIN: u32 = 10;
pub const RUNTIME_S_MAX: u32 = 3600;
/// Hosts a job may reach (TCP 443); empty = no network.
pub const MAX_NETWORK_HOSTS: usize = 16;
/// Budget bounds of one job, in credits.
pub const BUDGET_MIN: u64 = 1;
pub const BUDGET_MAX: u64 = 1000;
/// Result bounds.
pub const MAX_ARTIFACTS: usize = 32;
pub const MAX_LOG_BYTES: u64 = 262_144;
pub const MAX_SUMMARY_TEXT_CHARS: usize = 2000;
pub const MAX_SUMMARY_COUNTS: usize = 16;
/// `rate.credits_per_cpu_minute` (fixed by the contract).
pub const CREDITS_PER_CPU_MINUTE: u64 = 1;

/// `tests`, `static_analysis` or `index_build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobType {
    Tests,
    StaticAnalysis,
    IndexBuild,
}

impl JobType {
    pub const ALL: [JobType; 3] = [Self::Tests, Self::StaticAnalysis, Self::IndexBuild];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tests => "tests",
            Self::StaticAnalysis => "static_analysis",
            Self::IndexBuild => "index_build",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == value)
    }

    /// The result formats the contract allows for this type.
    pub fn formats(self) -> &'static [ResultFormat] {
        match self {
            Self::Tests => &[ResultFormat::JunitXml, ResultFormat::JsonSummary],
            Self::StaticAnalysis => &[ResultFormat::Sarif, ResultFormat::JsonSummary],
            Self::IndexBuild => &[ResultFormat::ArtifactBundle],
        }
    }

    /// The format a job of this type gets when the caller names none.
    pub fn default_format(self) -> ResultFormat {
        self.formats()[0]
    }
}

/// `queued -> running -> succeeded | failed | cancelled | expired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Expired,
}

impl JobState {
    pub const ALL: [JobState; 6] = [
        Self::Queued,
        Self::Running,
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
        Self::Expired,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    /// Whether a job in this state carries a terminal reason.
    pub fn has_reason(self) -> bool {
        matches!(self, Self::Failed | Self::Cancelled | Self::Expired)
    }
}

/// Why a job is failed, cancelled or expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalReason {
    ExitNonzero,
    ProviderError,
    RunnerLost,
    IsolationRefused,
    Cancelled,
    QueueTimeout,
    RuntimeExceeded,
}

impl TerminalReason {
    pub const ALL: [TerminalReason; 7] = [
        Self::ExitNonzero,
        Self::ProviderError,
        Self::RunnerLost,
        Self::IsolationRefused,
        Self::Cancelled,
        Self::QueueTimeout,
        Self::RuntimeExceeded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExitNonzero => "exit_nonzero",
            Self::ProviderError => "provider_error",
            Self::RunnerLost => "runner_lost",
            Self::IsolationRefused => "isolation_refused",
            Self::Cancelled => "cancelled",
            Self::QueueTimeout => "queue_timeout",
            Self::RuntimeExceeded => "runtime_exceeded",
        }
    }

    /// One line for people.
    pub fn describe(self) -> &'static str {
        match self {
            Self::ExitNonzero => "the command exited non-zero",
            Self::ProviderError => "the sandbox could not run it",
            Self::RunnerLost => "the node running it stopped",
            Self::IsolationRefused => "the job broke an isolation rule before anything ran",
            Self::Cancelled => "cancelled",
            Self::QueueTimeout => "not started before its queue deadline",
            Self::RuntimeExceeded => "stopped at its maximum runtime",
        }
    }
}

/// What a job's result carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultFormat {
    JunitXml,
    Sarif,
    JsonSummary,
    ArtifactBundle,
}

impl ResultFormat {
    pub const ALL: [ResultFormat; 4] = [
        Self::JunitXml,
        Self::Sarif,
        Self::JsonSummary,
        Self::ArtifactBundle,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::JunitXml => "junit_xml",
            Self::Sarif => "sarif",
            Self::JsonSummary => "json_summary",
            Self::ArtifactBundle => "artifact_bundle",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == value)
    }
}

/// `summary.outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Error,
}

impl Outcome {
    pub const ALL: [Outcome; 3] = [Self::Passed, Self::Failed, Self::Error];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Error => "error",
        }
    }
}

/// The hard cap a `cap_reached` refusal names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapName {
    MaxRuntimeS,
    MaxJobCredits,
    JobBudget,
    DailyCredits,
    QueuedJobs,
    BundleBytes,
}

impl CapName {
    pub const ALL: [CapName; 6] = [
        Self::MaxRuntimeS,
        Self::MaxJobCredits,
        Self::JobBudget,
        Self::DailyCredits,
        Self::QueuedJobs,
        Self::BundleBytes,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxRuntimeS => "max_runtime_s",
            Self::MaxJobCredits => "max_job_credits",
            Self::JobBudget => "job_budget",
            Self::DailyCredits => "daily_credits",
            Self::QueuedJobs => "queued_jobs",
            Self::BundleBytes => "bundle_bytes",
        }
    }

    /// The cap as people read it.
    pub fn describe(self) -> &'static str {
        match self {
            Self::MaxRuntimeS => "maximum runtime per job (seconds)",
            Self::MaxJobCredits => "maximum credits per job",
            Self::JobBudget => "this job's budget (credits)",
            Self::DailyCredits => "daily compute credits",
            Self::QueuedJobs => "queued jobs",
            Self::BundleBytes => "stored input bundles (bytes)",
        }
    }
}

/// One uploaded input bundle, by digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleRefV1 {
    pub name: String,
    pub digest: String,
    pub size_bytes: u64,
}

impl BundleRefV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        check(is_name(&self.name), "inputs.bundles[].name")?;
        check(is_digest(&self.digest), "inputs.bundles[].digest")?;
        check(
            (1..=MAX_BUNDLE_BYTES).contains(&self.size_bytes),
            "inputs.bundles[].size_bytes",
        )
    }
}

/// Versioned, content-addressed inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeInputsV1 {
    pub version: u8,
    pub bundles: Vec<BundleRefV1>,
}

impl ComputeInputsV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        check(self.version == INPUTS_VERSION, "inputs.version")?;
        check(
            (1..=MAX_INPUT_BUNDLES).contains(&self.bundles.len()),
            "inputs.bundles",
        )?;
        self.bundles.iter().try_for_each(BundleRefV1::validate)
    }
}

/// The command the sandbox runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeCommandV1 {
    pub argv: Vec<String>,
    /// Relative to `/work`, no `..`; empty = `/work`.
    #[serde(default)]
    pub workdir: String,
}

impl ComputeCommandV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        check(
            (1..=MAX_ARGV).contains(&self.argv.len()),
            "command.argv (1-32 items)",
        )?;
        check(
            self.argv
                .iter()
                .all(|a| (1..=MAX_ARG_BYTES).contains(&a.chars().count())),
            "command.argv items (1-512 characters)",
        )?;
        check(is_workdir(&self.workdir), "command.workdir")
    }
}

/// vCPUs, memory and scratch disk of one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeResourcesV1 {
    pub cpu: u32,
    pub memory_mb: u32,
    pub disk_mb: u32,
}

impl ComputeResourcesV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        check(
            (CPU_MIN..=CPU_MAX).contains(&self.cpu),
            "resources.cpu (1-8)",
        )?;
        check(
            (MEMORY_MB_MIN..=MEMORY_MB_MAX).contains(&self.memory_mb),
            "resources.memory_mb (256-16384)",
        )?;
        check(
            (DISK_MB_MIN..=DISK_MB_MAX).contains(&self.disk_mb),
            "resources.disk_mb (256-20480)",
        )
    }
}

/// The most a job may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeBudgetV1 {
    pub credits: u64,
}

/// E47 `POST /v1/compute/jobs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeSubmitRequestV1 {
    pub schema: String,
    pub vault_id: String,
    #[serde(rename = "type")]
    pub job_type: JobType,
    pub inputs: ComputeInputsV1,
    pub image: String,
    pub command: ComputeCommandV1,
    pub resources: ComputeResourcesV1,
    pub max_runtime_s: u32,
    pub network_allowlist: Vec<String>,
    pub budget: ComputeBudgetV1,
    pub result_format: ResultFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
}

impl ComputeSubmitRequestV1 {
    /// Every rule of `submit_request` the engine can check before sending,
    /// the job budget against the reserve included (`cap_reached` cap
    /// `job_budget` on the server).
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, COMPUTE_JOB_SCHEMA)?;
        check(is_lower_hex(&self.vault_id, 32), "vault_id")?;
        self.inputs.validate()?;
        check(is_image(&self.image), "image")?;
        self.command.validate()?;
        self.resources.validate()?;
        check(
            (RUNTIME_S_MIN..=RUNTIME_S_MAX).contains(&self.max_runtime_s),
            "max_runtime_s (10-3600)",
        )?;
        validate_network(&self.network_allowlist)?;
        check(
            (BUDGET_MIN..=BUDGET_MAX).contains(&self.budget.credits),
            "budget.credits (1-1000)",
        )?;
        check(
            self.job_type.formats().contains(&self.result_format),
            "result_format (not allowed for this job type)",
        )?;
        if let Some(client_ref) = &self.client_ref {
            check(is_client_ref(client_ref), "client_ref")?;
        }
        budget_covers(self.resources.cpu, self.max_runtime_s, self.budget.credits).map(|_| ())
    }
}

/// `rate` of a cost record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeRateV1 {
    pub credits_per_cpu_minute: u64,
}

/// The job's cost record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeCostV1 {
    pub credits_reserved: u64,
    pub credits_spent: Option<u64>,
    pub credits_refunded: Option<u64>,
    pub wall_seconds: Option<u64>,
    pub rate: ComputeRateV1,
}

impl ComputeCostV1 {
    fn validate(&self, terminal: bool) -> Result<(), ValidationError> {
        check(self.credits_reserved >= 1, "cost.credits_reserved")?;
        check(
            self.rate.credits_per_cpu_minute == CREDITS_PER_CPU_MINUTE,
            "cost.rate.credits_per_cpu_minute",
        )?;
        match (terminal, self.credits_spent, self.credits_refunded) {
            (true, Some(spent), Some(refunded)) => check(
                spent.checked_add(refunded) == Some(self.credits_reserved),
                "cost (spent + refunded must equal reserved)",
            ),
            (false, None, None) => Ok(()),
            (true, _, _) => Err(ValidationError::new(
                "cost: a finished job carries credits_spent and credits_refunded",
            )),
            (false, _, _) => Err(ValidationError::new(
                "cost: an unfinished job has no credits_spent or credits_refunded yet",
            )),
        }
    }
}

/// Who submitted a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmittedByV1 {
    pub instance_id: Option<String>,
    pub user_id: Option<String>,
}

/// One job as E47, E48 and E49 return it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeJobV1 {
    pub job_id: String,
    pub workspace: String,
    pub vault_id: String,
    #[serde(rename = "type")]
    pub job_type: JobType,
    pub state: JobState,
    pub reason: Option<TerminalReason>,
    pub inputs: ComputeInputsV1,
    pub image: String,
    pub command: ComputeCommandV1,
    pub resources: ComputeResourcesV1,
    pub max_runtime_s: u32,
    pub network_allowlist: Vec<String>,
    pub budget: ComputeBudgetV1,
    pub result_format: ResultFormat,
    pub client_ref: Option<String>,
    pub submitted_by: SubmittedByV1,
    pub cost: ComputeCostV1,
    pub created_ms: u64,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    pub queue_deadline_ms: u64,
    pub run_deadline_ms: Option<u64>,
    pub cancel_requested: bool,
    pub artifacts_expire_ms: Option<u64>,
    pub result_available: bool,
}

impl ComputeJobV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        check(is_job_id(&self.job_id), "job_id")?;
        check(
            prefixed_hex(&self.workspace, "org_", 32),
            "workspace (org_ + 32 hex)",
        )?;
        check(is_lower_hex(&self.vault_id, 32), "vault_id")?;
        check(
            self.reason.is_some() == self.state.has_reason(),
            "reason (set exactly when failed, cancelled or expired)",
        )?;
        self.inputs.validate()?;
        check(is_image(&self.image), "image")?;
        self.command.validate()?;
        self.resources.validate()?;
        check(
            (RUNTIME_S_MIN..=RUNTIME_S_MAX).contains(&self.max_runtime_s),
            "max_runtime_s",
        )?;
        validate_network(&self.network_allowlist)?;
        check(
            (BUDGET_MIN..=BUDGET_MAX).contains(&self.budget.credits),
            "budget.credits",
        )?;
        if let Some(client_ref) = &self.client_ref {
            check(is_client_ref(client_ref), "client_ref")?;
        }
        if let Some(instance) = &self.submitted_by.instance_id {
            check(
                prefixed_hex(instance, "inst_", 32),
                "submitted_by.instance_id",
            )?;
        }
        if let Some(user) = &self.submitted_by.user_id {
            check(is_uuid(user), "submitted_by.user_id")?;
        }
        match self.state {
            JobState::Queued => check(
                self.started_ms.is_none() && self.finished_ms.is_none(),
                "a queued job has neither started_ms nor finished_ms",
            )?,
            JobState::Running => check(
                self.started_ms.is_some()
                    && self.finished_ms.is_none()
                    && self.run_deadline_ms.is_some(),
                "a running job has started_ms and run_deadline_ms, no finished_ms",
            )?,
            _ => check(self.finished_ms.is_some(), "a finished job has finished_ms")?,
        }
        self.cost.validate(self.state.is_terminal())
    }
}

/// E47, E48 and E49 answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeJobResponseV1 {
    pub job: ComputeJobV1,
}

impl ComputeJobResponseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.job.validate()
    }
}

/// One artifact below `/work/out`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRefV1 {
    pub name: String,
    pub digest: String,
    pub size_bytes: u64,
}

/// The job's logs (stdout and stderr, the last 256 KiB, screened).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsRefV1 {
    pub digest: String,
    pub size_bytes: u64,
    pub truncated: bool,
    pub redactions: u64,
}

/// The job's summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryV1 {
    pub outcome: Outcome,
    pub counts: BTreeMap<String, u64>,
    pub text: String,
}

/// A terminal job's bounded result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeResultV1 {
    pub exit_code: Option<i64>,
    pub summary: SummaryV1,
    pub artifacts: Vec<ArtifactRefV1>,
    pub logs: Option<LogsRefV1>,
}

impl ComputeResultV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        check(
            self.summary.counts.len() <= MAX_SUMMARY_COUNTS
                && self.summary.counts.keys().all(|k| is_count_key(k)),
            "result.summary.counts",
        )?;
        check(
            self.summary.text.chars().count() <= MAX_SUMMARY_TEXT_CHARS,
            "result.summary.text",
        )?;
        check(
            self.artifacts.len() <= MAX_ARTIFACTS,
            "result.artifacts (at most 32)",
        )?;
        let mut total: u64 = 0;
        for artifact in &self.artifacts {
            check(is_artifact_name(&artifact.name), "result.artifacts[].name")?;
            check(is_digest(&artifact.digest), "result.artifacts[].digest")?;
            check(
                artifact.size_bytes <= MAX_BUNDLE_BYTES,
                "result.artifacts[].size_bytes",
            )?;
            total = total.saturating_add(artifact.size_bytes);
        }
        check(
            total <= MAX_BUNDLE_BYTES,
            "result.artifacts (64 MiB in total)",
        )?;
        if let Some(logs) = &self.logs {
            check(is_digest(&logs.digest), "result.logs.digest")?;
            check(logs.size_bytes <= MAX_LOG_BYTES, "result.logs.size_bytes")?;
        }
        Ok(())
    }
}

/// E50 answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeResultResponseV1 {
    pub job_id: String,
    pub state: JobState,
    pub reason: Option<TerminalReason>,
    pub result: Option<ComputeResultV1>,
    pub cost: ComputeCostV1,
}

impl ComputeResultResponseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        check(is_job_id(&self.job_id), "job_id")?;
        check(self.state.is_terminal(), "state (a result is terminal)")?;
        check(
            self.reason.is_some() == self.state.has_reason(),
            "reason (set exactly when failed, cancelled or expired)",
        )?;
        if let Some(result) = &self.result {
            result.validate()?;
        }
        self.cost.validate(true)
    }

    /// The artifact or log of this result with `digest`, as `(name, size)`;
    /// the logs are named `logs.txt`.
    pub fn blob(&self, digest: &str) -> Option<(&str, u64)> {
        let result = self.result.as_ref()?;
        result
            .artifacts
            .iter()
            .find(|a| a.digest == digest)
            .map(|a| (a.name.as_str(), a.size_bytes))
            .or_else(|| {
                result
                    .logs
                    .as_ref()
                    .filter(|l| l.digest == digest)
                    .map(|l| ("logs.txt", l.size_bytes))
            })
    }
}

/// E51 answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeBundleResponseV1 {
    pub digest: String,
    pub size_bytes: u64,
    pub expires_ms: u64,
    /// `true` stored now (201), `false` already there, its expiry renewed (200).
    pub created: bool,
}

impl ComputeBundleResponseV1 {
    /// The answer for an upload of `digest` with `size_bytes`.
    pub fn validate_for(&self, digest: &str, size_bytes: u64) -> Result<(), ValidationError> {
        check(is_digest(&self.digest), "digest")?;
        check(self.digest == digest, "digest (not the uploaded bundle)")?;
        check(
            self.size_bytes == size_bytes && (1..=MAX_BUNDLE_BYTES).contains(&size_bytes),
            "size_bytes (not the uploaded bundle)",
        )
    }
}

/// A compute refusal the engine words for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeRefusalV1 {
    /// 402: the prepaid balance does not cover the job's reserve.
    CreditExhausted { balance: u64, needed: u64 },
    /// 409: a hard cap refuses the job; nothing was queued.
    CapReached {
        cap: CapName,
        limit: u64,
        requested: u64,
    },
    /// 404: Task Compute is switched off on this Hive.
    FeatureDisabled,
}

impl ComputeRefusalV1 {
    /// The refusal in an error body, if it is one of the three.
    pub fn parse(body: &Value) -> Option<Self> {
        let error = body.get("error")?;
        let number = |key: &str| error.get(key).and_then(Value::as_u64);
        match error.get("code")?.as_str()? {
            "credit_exhausted" => Some(Self::CreditExhausted {
                balance: number("balance")?,
                needed: number("needed")?,
            }),
            "cap_reached" => Some(Self::CapReached {
                cap: serde_json::from_value(error.get("cap")?.clone()).ok()?,
                limit: number("limit")?,
                requested: number("requested")?,
            }),
            "feature_disabled"
                if error.get("feature").and_then(Value::as_str) == Some(COMPUTE_FEATURE) =>
            {
                Some(Self::FeatureDisabled)
            }
            _ => None,
        }
    }
}

/// `ceil(a / b)` for `b > 0`.
fn ceil_div(a: u64, b: u64) -> u64 {
    a / b + u64::from(!a.is_multiple_of(b))
}

/// Credits reserved at submit: `ceil(cpu × max_runtime_s / 60)`.
pub fn reserve_for(cpu: u32, max_runtime_s: u32) -> u64 {
    ceil_div(u64::from(cpu) * u64::from(max_runtime_s), 60) * CREDITS_PER_CPU_MINUTE
}

/// Credits spent: 0 when the job never started, else
/// `min(reserved, max(1, ceil(cpu × wall_seconds / 60)))`.
pub fn spent_for(cpu: u32, max_runtime_s: u32, wall_seconds: Option<u64>) -> u64 {
    let Some(wall) = wall_seconds else {
        return 0;
    };
    let used = ceil_div(u64::from(cpu).saturating_mul(wall), 60).max(1) * CREDITS_PER_CPU_MINUTE;
    used.min(reserve_for(cpu, max_runtime_s))
}

/// The reserve when `budget` covers it; else the reserve as the error
/// (`cap_reached` cap `job_budget` on the server).
pub fn budget_covers(cpu: u32, max_runtime_s: u32, budget: u64) -> Result<u64, ValidationError> {
    let reserve = reserve_for(cpu, max_runtime_s);
    if reserve > budget {
        return Err(ValidationError::new(format!(
            "budget.credits {budget} does not cover the reserve of {reserve} credits (cpu {cpu} x {max_runtime_s} s / 60)"
        )));
    }
    Ok(reserve)
}

/// `"sha256:" + hex(sha256(bytes))`: the content address of a bundle,
/// artifact or log.
pub fn content_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let hash = Sha256::digest(bytes);
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in hash.iter() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn check(ok: bool, what: &str) -> Result<(), ValidationError> {
    if ok {
        Ok(())
    } else {
        Err(ValidationError::new(format!("invalid {what}")))
    }
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn prefixed_hex(value: &str, prefix: &str, len: usize) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|rest| is_lower_hex(rest, len))
}

fn is_uuid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts.iter().all(|p| is_lower_hex(p, p.len()))
}

/// `sha256:` + 64 lowercase hex.
pub fn is_digest(value: &str) -> bool {
    prefixed_hex(value, "sha256:", 64)
}

/// `cj_` + 32 lowercase hex.
pub fn is_job_id(value: &str) -> bool {
    prefixed_hex(value, "cj_", 32)
}

/// A bundle name: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
pub fn is_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A runner image name: `^[a-z0-9][a-z0-9._-]{0,63}$`.
pub fn is_image(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// `^[A-Za-z0-9._:-]{1,64}$`.
pub fn is_client_ref(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn relative_path(value: &str, min: usize) -> bool {
    (min..=MAX_WORKDIR_BYTES).contains(&value.len())
        && !value.starts_with('/')
        && !value.contains("..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
}

/// `command.workdir`: relative to `/work`, no `..`, may be empty.
pub fn is_workdir(value: &str) -> bool {
    relative_path(value, 0)
}

/// An artifact name: a path below `/work/out`, no `..`.
pub fn is_artifact_name(value: &str) -> bool {
    relative_path(value, 1)
}

fn is_count_key(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// A host of `network_allowlist`: lowercase DNS labels, a letters-only TLD.
pub fn is_host(value: &str) -> bool {
    if !(1..=253).contains(&value.len()) {
        return false;
    }
    let labels: Vec<&str> = value.split('.').collect();
    let Some((tld, rest)) = labels.split_last() else {
        return false;
    };
    !rest.is_empty()
        && (2..=63).contains(&tld.len())
        && tld.bytes().all(|b| b.is_ascii_lowercase())
        && rest.iter().all(|label| {
            let b = label.as_bytes();
            (1..=63).contains(&b.len())
                && b[0] != b'-'
                && b[b.len() - 1] != b'-'
                && b.iter()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        })
}

fn validate_network(hosts: &[String]) -> Result<(), ValidationError> {
    check(
        hosts.len() <= MAX_NETWORK_HOSTS,
        "network_allowlist (at most 16 hosts)",
    )?;
    check(
        hosts.iter().all(|h| is_host(h)),
        "network_allowlist (lowercase host names)",
    )?;
    let mut seen = std::collections::BTreeSet::new();
    check(
        hosts.iter().all(|h| seen.insert(h.as_str())),
        "network_allowlist (each host once)",
    )
}

#[cfg(test)]
#[path = "compute_tests.rs"]
mod tests;
