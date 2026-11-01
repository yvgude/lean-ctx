// SPDX-License-Identifier: Apache-2.0
//! On-disk record of the proxy's measured provider spend (`proxy_usage.json`).
//!
//! The proxy's usage meter (`proxy::usage_meter`) aggregates the real
//! provider usage of every finalized turn into these per-model sums and
//! persists them; the dashboard, CLI, stats and the savings ledger (which run
//! in *other* processes) read them back through [`load_persisted`] to learn
//! the user's real provider bill.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

/// Where provider usage was observed, not an assertion of answer quality or
/// successful generation. Terminal usage also accompanies failed responses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageBoundary {
    BodyEnd,
    ProviderTerminalUsage,
    StreamEnd,
    StreamError,
}

/// Payload-free local measurement. Unknown cost stays unknown; neither HTTP
/// status nor receipt of usage supplies the private runtime's quality label.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResponseObservation {
    pub schema_version: u8,
    pub http_status: u16,
    /// Monotonic elapsed time from upstream dispatch (including retries) to
    /// the indicated usage boundary, not necessarily full response latency.
    pub usage_observed_ms: u64,
    pub boundary: UsageBoundary,
    pub provider_cost_usd: Option<f64>,
}

/// Cumulative real token counts for one model. Cost is derived at read time so a
/// pricing-table change re-values historical usage consistently.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelUsage {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    /// Requests whose free `count_tokens` probe answered (#701) — the rows the
    /// verified-savings pair below covers. `serde(default)` keeps pre-#701
    /// usage files loadable.
    #[serde(default)]
    pub counterfactual_requests: u64,
    /// Provider-counted input tokens the covered requests would have billed
    /// WITHOUT lean-ctx (sum of probe answers on the original bodies).
    #[serde(default)]
    pub counterfactual_input_tokens: u64,
    /// Input-side tokens those same requests actually billed (input +
    /// cache read + cache write) — same request, same moment, no confound.
    #[serde(default)]
    pub counterfactual_billed_tokens: u64,
    /// The turns whose response carried the provider's own USD charge
    /// (OpenRouter usage accounting, #1179). Their tokens are *inside* the
    /// totals above; pricing subtracts this slice and books its measured USD
    /// instead of a table estimate. `serde(default)` keeps older files loadable.
    #[serde(default)]
    pub measured: MeasuredSlice,
    /// Recent factual response measurements, not quality/training labels.
    /// This local diagnostic history must not be pooled into private ranking:
    /// it has no verified actor/endpoint scope or user-accepted outcome yet.
    #[serde(default)]
    pub response_observations: VecDeque<ResponseObservation>,
}

/// Token/cost sums of the turns that reported a measured provider charge.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct MeasuredSlice {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Sum of provider-reported USD for those turns — the bill, not a table.
    pub cost_usd: f64,
}

impl MeasuredSlice {
    pub fn merge(&mut self, other: &Self) {
        self.requests += other.requests;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.cost_usd += other.cost_usd;
    }
}

impl ModelUsage {
    #[must_use]
    pub fn billable_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    pub fn add_observation(&mut self, observation: ResponseObservation) {
        const MAX_RESPONSE_OBSERVATIONS: usize = 32;
        self.response_observations.push_back(observation);
        while self.response_observations.len() > MAX_RESPONSE_OBSERVATIONS {
            self.response_observations.pop_front();
        }
    }
}

/// Cumulative holdout cohort totals. Keyed by arm name: `"control"` |
/// `"treatment"` for output savings (#895 Track B), `"compression:control"` |
/// `"compression:treatment"` for input compression (#1905). The average tokens
/// per turn are `*_tokens / requests`. Only populated while a holdout is active.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CohortUsage {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Sum of squared per-turn output tokens, enabling an online sample variance
    /// (and therefore a confidence interval) without retaining every turn.
    /// `#[serde(default)]` keeps pre-#895 files loadable.
    #[serde(default)]
    pub sum_sq_output: u64,
    /// Whole prompt per turn — billed input plus cache reads and writes (#1905).
    /// Compression shrinks the prompt wherever it is billed, so the
    /// input-compression comparison uses this, not `input_tokens` alone.
    /// `#[serde(default)]` keeps earlier files loadable.
    #[serde(default)]
    pub prompt_tokens: u64,
    /// Sum of squared per-turn `prompt_tokens`, for its confidence interval.
    #[serde(default)]
    pub sum_sq_prompt: u64,
}

impl CohortUsage {
    /// Adds another cohort's running totals — every field, so the sums of
    /// squares (and with them the variance / CI) survive a proxy restart.
    pub fn merge(&mut self, other: &Self) {
        self.requests += other.requests;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.sum_sq_output = self.sum_sq_output.saturating_add(other.sum_sq_output);
        self.prompt_tokens += other.prompt_tokens;
        self.sum_sq_prompt = self.sum_sq_prompt.saturating_add(other.sum_sq_prompt);
    }

    /// Average output tokens per turn, or `None` with no observations.
    #[must_use]
    pub fn avg_output(&self) -> Option<f64> {
        if self.requests == 0 {
            None
        } else {
            #[allow(clippy::cast_precision_loss)]
            Some(self.output_tokens as f64 / self.requests as f64)
        }
    }

    /// Unbiased sample variance of per-turn output tokens, or `None` with < 2
    /// observations. Computed from the running sum / sum-of-squares (clamped at
    /// 0 to absorb floating-point error on near-constant samples).
    #[must_use]
    pub fn variance_output(&self) -> Option<f64> {
        if self.requests < 2 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let n = self.requests as f64;
        #[allow(clippy::cast_precision_loss)]
        let sum = self.output_tokens as f64;
        #[allow(clippy::cast_precision_loss)]
        let sum_sq = self.sum_sq_output as f64;
        let var = (sum_sq - sum * sum / n) / (n - 1.0);
        Some(var.max(0.0))
    }
}

/// On-disk shape of the measured spend totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedUsage {
    pub ts: u64,
    pub models: HashMap<String, ModelUsage>,
    /// Output-savings cohort totals (#895). `#[serde(default)]` keeps older
    /// `proxy_usage.json` files (written before the holdout existed) loadable.
    #[serde(default)]
    pub cohorts: HashMap<String, CohortUsage>,
    /// The foreground model of the most recent finalized turn and when it was
    /// seen (unix seconds), so other processes can attribute work to the model
    /// in use now rather than the all-time largest bucket.
    #[serde(default)]
    pub last_model: Option<LastModel>,
    /// The same totals per project key and model (the folder the agent named
    /// in each request). Turns that named none stay in `models` only, which
    /// remains the device total.
    #[serde(default)]
    pub projects: HashMap<String, HashMap<String, ModelUsage>>,
}

/// The model the proxy saw last, with the time it saw it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastModel {
    pub model: String,
    pub at: u64,
}

const PROXY_USAGE_FILE: &str = "proxy_usage.json";

/// Where the measured spend totals live in the data directory.
#[must_use]
pub fn usage_path() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join(PROXY_USAGE_FILE))
}

/// Cross-process read of the persisted measured spend (dashboard / CLI / ledger).
#[must_use]
pub fn load_persisted() -> Option<PersistedUsage> {
    let path = usage_path()?;
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// The model carrying the most measured tokens (excludes the "unknown" bucket).
/// Used to value savings against the real dominant model when no explicit model
/// is configured.
#[must_use]
pub fn persisted_dominant_model() -> Option<String> {
    let persisted = load_persisted()?;
    persisted
        .models
        .iter()
        .filter(|(m, _)| m.as_str() != "unknown" && !m.trim().is_empty())
        .max_by_key(|(_, u)| u.billable_tokens())
        .filter(|(_, u)| u.billable_tokens() > 0)
        .map(|(m, _)| m.clone())
}
