//! Measured per-model spend meter.
//!
//! [`record`] aggregates the real provider usage extracted by [`super::usage`]
//! into per-model token sums, prices them with the shared
//! [`ModelPricing`] table, and
//! persists the totals to `proxy_usage.json` so the dashboard, CLI and the
//! savings ledger (which run in *other* processes) can read the user's real
//! provider bill.
//!
//! Unlike [`super::metrics`] (which resets per proxy lifetime), this meter is a
//! lifetime-cumulative spend counter: [`resume_from_disk`] seeds the in-memory
//! totals on proxy startup so a restart never zeroes the user's measured spend.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::core::gain::model_pricing::ModelPricing;
use crate::core::ocla::{OclaRegistry, UsageSink};

/// Projects one managed, provider-measured turn into the OCLA usage sink.
///
/// This is deliberately one-way and best-effort: the existing meter remains
/// authoritative for billing/ledger totals, while OCLA receives a validated
/// payload-free projection only when complete lineage is present.
fn project_ocla_usage(u: &super::usage::RealUsage, sink: &dyn UsageSink) {
    let Ok(Some(record)) = u.to_ocla_usage_record() else {
        return;
    };
    let _ = sink.record_usage(record);
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
    fn merge(&mut self, other: &Self) {
        self.requests += other.requests;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.cost_usd += other.cost_usd;
    }
}

impl ModelUsage {
    fn add(&mut self, u: &super::usage::RealUsage) {
        self.requests += 1;
        self.input_tokens += u.input_tokens;
        self.output_tokens += u.output_tokens;
        self.cache_read_tokens += u.cache_read_tokens;
        self.cache_write_tokens += u.cache_write_tokens;
        self.reasoning_tokens += u.reasoning_tokens;
        if let Some(cost) = u.provider_cost_usd {
            self.measured.merge(&MeasuredSlice {
                requests: 1,
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_read_tokens: u.cache_read_tokens,
                cache_write_tokens: u.cache_write_tokens,
                cost_usd: cost,
            });
        }
        // Verified-savings pair (#701): only when the probe answered by the
        // time the billed usage arrived — both sides of the pair or neither.
        if let Some(counted) = u
            .wire
            .as_deref()
            .and_then(|w| w.counterfactual.as_ref())
            .and_then(super::counterfactual::CounterfactualSlot::get)
        {
            self.counterfactual_requests += 1;
            self.counterfactual_input_tokens += counted;
            self.counterfactual_billed_tokens +=
                u.input_tokens + u.cache_read_tokens + u.cache_write_tokens;
        }
    }

    fn billable_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

/// One model's measured, priced spend for `/status` and the dashboard.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelSpend {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_usd: f64,
    /// Share of `cost_usd` the provider itself reported (OpenRouter usage
    /// accounting, #1179) — a bill, not a table estimate.
    pub measured_cost_usd: f64,
    /// Requests covered by that provider-reported cost.
    pub measured_requests: u64,
    /// True when part of the cost had to be priced from a heuristic/fallback
    /// match — never set by measured or exactly-priced spend.
    pub pricing_estimated: bool,
}

/// Cumulative output-savings cohort totals (#895 Track B). Keyed by arm name
/// (`"control"` | `"treatment"`); the average output tokens per turn is
/// `output_tokens / requests`. Only populated while a holdout is active.
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
}

impl CohortUsage {
    fn add(&mut self, u: &super::usage::RealUsage) {
        self.requests += 1;
        self.input_tokens += u.input_tokens;
        self.output_tokens += u.output_tokens;
        self.sum_sq_output += u.output_tokens.saturating_mul(u.output_tokens);
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
}

/// Distinct-model bucket cap. `record` keys on the raw response model string, so
/// overflow folds into "unknown" to keep the map bounded (real model names < ~50).
const MAX_TRACKED_MODELS: usize = 256;

const PROXY_USAGE_FILE: &str = "proxy_usage.json";

fn store() -> &'static Mutex<HashMap<String, ModelUsage>> {
    static STORE: OnceLock<Mutex<HashMap<String, ModelUsage>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cohort_store() -> &'static Mutex<HashMap<String, CohortUsage>> {
    static STORE: OnceLock<Mutex<HashMap<String, CohortUsage>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Seeds the in-memory totals from `proxy_usage.json`. Call once on proxy
/// startup so measured spend is cumulative across restarts. Idempotent-ish: it
/// merges the persisted totals into whatever is in memory (normally empty).
pub fn resume_from_disk() {
    let Some(persisted) = load_persisted() else {
        return;
    };
    let mut map = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (model, usage) in persisted.models {
        let acc = map.entry(model).or_default();
        acc.requests += usage.requests;
        acc.input_tokens += usage.input_tokens;
        acc.output_tokens += usage.output_tokens;
        acc.cache_read_tokens += usage.cache_read_tokens;
        acc.cache_write_tokens += usage.cache_write_tokens;
        acc.reasoning_tokens += usage.reasoning_tokens;
        acc.counterfactual_requests += usage.counterfactual_requests;
        acc.counterfactual_input_tokens += usage.counterfactual_input_tokens;
        acc.counterfactual_billed_tokens += usage.counterfactual_billed_tokens;
        acc.measured.merge(&usage.measured);
    }
    drop(map);
    let mut cohorts = cohort_store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (arm, usage) in persisted.cohorts {
        let acc = cohorts.entry(arm).or_default();
        acc.requests += usage.requests;
        acc.input_tokens += usage.input_tokens;
        acc.output_tokens += usage.output_tokens;
    }
}

/// Records one turn's measured usage against its model bucket (and its
/// output-savings cohort, when tagged) and persists.
pub fn record(u: &super::usage::RealUsage) {
    // Gateway store subscription (enterprise#17): forward the full record to
    // the installed sink (no-op locally). Never blocks the request path.
    super::usage_sink::push(u);

    // OCLA projection: keep the canonical usage capability on the same single
    // finalized-turn choke-point without feeding back into billing or ledger
    // aggregation. Invalid/missing lineage stays explicitly unmanaged.
    project_ocla_usage(u, OclaRegistry::global().usage_sink.as_ref());

    // Budget windows (enterprise#25): book this turn's measured cost against
    // the person/day and project/month accumulators the policy gate checks.
    // The provider's own reported charge wins (#1179); local turns book the
    // shadow rate — the same valuation the usage store applies — so
    // local-only budgets stay meaningful.
    if let Some(wire) = u.wire.as_deref()
        && (wire.person.is_some() || wire.project.is_some())
    {
        let baseline = crate::core::config::Config::load().proxy.baseline.clone();
        #[allow(clippy::cast_precision_loss)]
        let cost_usd = if wire.is_local {
            let billable =
                u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens;
            baseline.effective_local_shadow_rate() / 1_000_000.0 * billable as f64
        } else if let Some(measured) = u.provider_cost_usd {
            measured
        } else {
            crate::core::gain::model_pricing::ModelPricing::load()
                .quote(Some(&u.model))
                .cost
                .estimate_usd(
                    u.input_tokens,
                    u.output_tokens,
                    u.cache_write_tokens,
                    u.cache_read_tokens,
                )
        };
        super::policy_gate::record_spend(wire.person.as_deref(), wire.project.as_deref(), cost_usd);
    }

    // Mechanism attribution into the local savings ledger (enterprise#19).
    // Routing: the gateway served a cheaper model than requested — value the
    // rate delta on the measured input tokens. Caching: provider prompt-cache
    // reads billed below the input rate. Both best-effort, never blocking.
    if let Some(wire) = u.wire.as_deref()
        && let Some(routed_from) = wire.routed_from.as_deref()
    {
        crate::core::savings_ledger::record_routing_event(routed_from, &u.model, u.input_tokens);
    }
    if u.cache_read_tokens > 0 {
        let cost = crate::core::gain::model_pricing::ModelPricing::load()
            .quote(Some(&u.model))
            .cost;
        #[allow(clippy::cast_precision_loss)]
        let discount_usd =
            (cost.input_per_m - cost.cache_read_per_m) / 1_000_000.0 * u.cache_read_tokens as f64;
        crate::core::savings_ledger::record_caching_event(
            &u.model,
            u.cache_read_tokens,
            discount_usd,
        );
    }

    let key = normalize_key(&u.model);
    {
        let mut map = store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = if !map.contains_key(&key) && map.len() >= MAX_TRACKED_MODELS {
            "unknown".to_string()
        } else {
            key
        };
        map.entry(key).or_default().add(u);
    }
    if let Some(arm) = u.cohort {
        let mut cohorts = cohort_store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cohorts.entry(arm.as_str().to_string()).or_default().add(u);
    }
    persist();
}

/// Live output-savings cohort totals (#895). Empty until a holdout runs.
#[must_use]
pub fn cohort_snapshot() -> HashMap<String, CohortUsage> {
    cohort_store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Cross-process read of the persisted output-savings cohort totals.
#[must_use]
pub fn persisted_cohorts() -> HashMap<String, CohortUsage> {
    load_persisted().map(|p| p.cohorts).unwrap_or_default()
}

fn normalize_key(model: &str) -> String {
    let m = model.trim();
    if m.is_empty() {
        "unknown".to_string()
    } else {
        m.to_string()
    }
}

/// Live per-model measured spend, priced and sorted by USD descending.
pub fn snapshot() -> Vec<ModelSpend> {
    let map = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    price_models(&map)
}

/// Total measured spend across all models (live in-memory totals).
pub fn total_cost_usd() -> f64 {
    snapshot().iter().map(|m| m.cost_usd).sum()
}

/// Cross-model verified-savings totals (#701) for `/status` and the dashboard.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VerifiedSavings {
    /// Requests covered by a successful `count_tokens` probe.
    pub requests: u64,
    /// Provider-counted input tokens those requests would have billed
    /// without lean-ctx.
    pub counterfactual_input_tokens: u64,
    /// Input-side tokens (input + cache read + cache write) they actually
    /// billed.
    pub billed_input_tokens: u64,
    /// `counterfactual - billed`; negative when stub overhead outweighed the
    /// squeeze — reported honestly, never clamped.
    pub verified_saved_tokens: i64,
}

/// Aggregated provider-verified savings across all models (#701), or `None`
/// until at least one probe-covered request has been recorded. Unlike the
/// `tokens_saved` estimate (bytes/4), both sides of this pair were counted by
/// the provider on the same request — receipts, not estimates.
pub fn verified_savings() -> Option<VerifiedSavings> {
    let map = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    verified_of(&map)
}

/// Pure aggregation behind [`verified_savings`], shared with tests.
#[allow(clippy::cast_possible_wrap)]
fn verified_of(map: &HashMap<String, ModelUsage>) -> Option<VerifiedSavings> {
    let (mut requests, mut counterfactual, mut billed) = (0u64, 0u64, 0u64);
    for usage in map.values() {
        requests += usage.counterfactual_requests;
        counterfactual += usage.counterfactual_input_tokens;
        billed += usage.counterfactual_billed_tokens;
    }
    (requests > 0).then(|| VerifiedSavings {
        requests,
        counterfactual_input_tokens: counterfactual,
        billed_input_tokens: billed,
        verified_saved_tokens: counterfactual as i64 - billed as i64,
    })
}

/// Prices a model usage map into sorted [`ModelSpend`] rows. Pure: shared by the
/// in-memory snapshot and the cross-process [`persisted_snapshot`].
pub fn price_models(map: &HashMap<String, ModelUsage>) -> Vec<ModelSpend> {
    let pricing = ModelPricing::load();
    let mut rows: Vec<ModelSpend> = map
        .iter()
        .map(|(model, usage)| price_one(&pricing, model, usage))
        .collect();
    rows.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}

fn price_one(pricing: &ModelPricing, model: &str, usage: &ModelUsage) -> ModelSpend {
    let quote = pricing.quote(Some(model));
    // Measured-first (#1179): turns whose response carried the provider's own
    // USD charge book that exact amount; only the remainder is table-priced.
    let m = &usage.measured;
    let derived_cost = quote.cost.estimate_usd(
        usage.input_tokens.saturating_sub(m.input_tokens),
        usage.output_tokens.saturating_sub(m.output_tokens),
        usage
            .cache_write_tokens
            .saturating_sub(m.cache_write_tokens),
        usage.cache_read_tokens.saturating_sub(m.cache_read_tokens),
    );
    let derived_requests = usage.requests.saturating_sub(m.requests);
    ModelSpend {
        model: model.to_string(),
        requests: usage.requests,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cost_usd: m.cost_usd + derived_cost,
        measured_cost_usd: m.cost_usd,
        measured_requests: m.requests,
        // Fully measured spend is never an estimate, whatever the table says.
        pricing_estimated: quote.match_kind.is_estimated() && derived_requests > 0,
    }
}

fn usage_path() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join(PROXY_USAGE_FILE))
}

/// Atomically writes the current in-memory totals to disk.
fn persist() {
    let Some(path) = usage_path() else {
        return;
    };
    let models = {
        let map = store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.clone()
    };
    let cohorts = {
        let map = cohort_store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.clone()
    };
    let payload = PersistedUsage {
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        models,
        cohorts,
    };
    let Ok(json) = serde_json::to_string(&payload) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Cross-process read of the persisted measured spend (dashboard / CLI / ledger).
pub fn load_persisted() -> Option<PersistedUsage> {
    let path = usage_path()?;
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Cross-process priced spend rows, read from disk.
pub fn persisted_snapshot() -> Vec<ModelSpend> {
    load_persisted()
        .map(|p| price_models(&p.models))
        .unwrap_or_default()
}

/// The model carrying the most measured tokens (excludes the "unknown" bucket).
/// Used to value savings against the real dominant model when no explicit model
/// is configured.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(
        model: &str,
        input: u64,
        output: u64,
        cache_read: u64,
    ) -> super::super::usage::RealUsage {
        super::super::usage::RealUsage {
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            ..Default::default()
        }
    }

    /// #701: the verified pair is recorded only when the probe answered — both
    /// sides of the pair or neither, never a half row.
    #[test]
    fn counterfactual_pair_recorded_only_when_probe_answered() {
        let slot = super::super::counterfactual::CounterfactualSlot::new();
        slot.set(5_000);
        let with_probe = super::super::usage::RealUsage {
            wire: Some(Box::new(super::super::usage::WireContext {
                counterfactual: Some(slot),
                ..Default::default()
            })),
            cache_write_tokens: 200,
            ..usage("claude-sonnet-4.5", 1_000, 0, 300)
        };
        let mut acc = ModelUsage::default();
        acc.add(&with_probe);
        assert_eq!(acc.counterfactual_requests, 1);
        assert_eq!(acc.counterfactual_input_tokens, 5_000);
        // billed side = input + cache read + cache write of the same turn.
        assert_eq!(acc.counterfactual_billed_tokens, 1_000 + 300 + 200);

        // Empty slot (probe failed / still in flight) → row degrades to the
        // estimate: no pair recorded, normal usage still counted.
        let empty = super::super::usage::RealUsage {
            wire: Some(Box::new(super::super::usage::WireContext {
                counterfactual: Some(super::super::counterfactual::CounterfactualSlot::new()),
                ..Default::default()
            })),
            ..usage("claude-sonnet-4.5", 1_000, 0, 0)
        };
        acc.add(&empty);
        assert_eq!(acc.requests, 2);
        assert_eq!(acc.counterfactual_requests, 1, "empty slot adds no pair");

        // No wire context at all (tests / non-forward paths) → no pair.
        acc.add(&usage("claude-sonnet-4.5", 10, 0, 0));
        assert_eq!(acc.counterfactual_requests, 1);
    }

    /// #701: cross-model aggregation and the honest signed difference.
    #[test]
    fn verified_of_aggregates_and_reports_signed_savings() {
        let mut map = HashMap::new();
        assert!(verified_of(&map).is_none(), "no coverage → None, not zeros");

        map.insert(
            "claude-sonnet-4.5".to_string(),
            ModelUsage {
                counterfactual_requests: 2,
                counterfactual_input_tokens: 10_000,
                counterfactual_billed_tokens: 6_000,
                ..Default::default()
            },
        );
        // Stub overhead outweighed the squeeze on this model: billed MORE
        // than the counterfactual. The total must subtract honestly.
        map.insert(
            "claude-haiku-4.5".to_string(),
            ModelUsage {
                counterfactual_requests: 1,
                counterfactual_input_tokens: 1_000,
                counterfactual_billed_tokens: 1_400,
                ..Default::default()
            },
        );
        let v = verified_of(&map).expect("covered rows present");
        assert_eq!(v.requests, 3);
        assert_eq!(v.counterfactual_input_tokens, 11_000);
        assert_eq!(v.billed_input_tokens, 7_400);
        assert_eq!(v.verified_saved_tokens, 3_600);

        map.get_mut("claude-sonnet-4.5")
            .unwrap()
            .counterfactual_input_tokens = 0;
        map.get_mut("claude-sonnet-4.5")
            .unwrap()
            .counterfactual_billed_tokens = 0;
        let negative = verified_of(&map).unwrap();
        assert_eq!(
            negative.verified_saved_tokens, -400,
            "a net-negative verified saving is reported, never clamped"
        );
    }

    /// #701: persisted usage files from before the feature load cleanly and
    /// the new fields round-trip.
    #[test]
    fn counterfactual_fields_roundtrip_and_default_for_legacy_files() {
        let legacy = r#"{"ts":1,"models":{"m":{"requests":1,"input_tokens":10,"output_tokens":5,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0}}}"#;
        let p: PersistedUsage = serde_json::from_str(legacy).expect("legacy file loads");
        assert_eq!(p.models["m"].counterfactual_requests, 0);

        let mut p = PersistedUsage::default();
        p.models.insert(
            "m".into(),
            ModelUsage {
                counterfactual_requests: 4,
                counterfactual_input_tokens: 9_999,
                counterfactual_billed_tokens: 5_555,
                ..Default::default()
            },
        );
        let back: PersistedUsage =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.models["m"].counterfactual_input_tokens, 9_999);
        assert_eq!(back.models["m"].counterfactual_billed_tokens, 5_555);
    }

    #[test]
    fn prices_known_model_with_cache_split() {
        let mut map = HashMap::new();
        let mut acc = ModelUsage::default();
        acc.add(&usage("claude-sonnet-4.5", 1_000_000, 1_000_000, 1_000_000));
        map.insert("claude-sonnet-4.5".to_string(), acc);

        let rows = price_models(&map);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        // input 3.00 + output 15.00 + cache_read 0.30 (per 1M) = 18.30.
        assert!(
            (row.cost_usd - 18.30).abs() < 1e-6,
            "cost was {}",
            row.cost_usd
        );
        assert!(!row.pricing_estimated, "exact model match");
        assert_eq!(row.requests, 1);
    }

    #[test]
    fn unknown_model_prices_with_fallback_and_is_estimated() {
        let mut map = HashMap::new();
        let mut acc = ModelUsage::default();
        acc.add(&usage("some-novel-model-xyz", 1_000_000, 0, 0));
        map.insert("some-novel-model-xyz".to_string(), acc);

        let rows = price_models(&map);
        assert!(rows[0].pricing_estimated, "fallback pricing is estimated");
        assert!(rows[0].cost_usd > 0.0);
    }

    /// #1179: turns carrying the provider's own charge book that USD; only the
    /// remaining (unmeasured) turns are table-priced — and a fully measured
    /// model is never flagged as estimated, even when the table has no entry.
    /// (Model name chosen to never hit the embedded or live price tables.)
    #[test]
    fn measured_provider_cost_replaces_table_estimate() {
        const MODEL: &str = "vendor/unlisted-model-20990101";
        let mut acc = ModelUsage::default();
        let mut measured = usage(MODEL, 431_600, 22_700, 126_700);
        measured.provider_cost_usd = Some(0.05);
        acc.add(&measured);

        let mut map = HashMap::new();
        map.insert(MODEL.to_string(), acc.clone());
        let row = &price_models(&map)[0];
        assert!(
            (row.cost_usd - 0.05).abs() < 1e-12,
            "the bill, not the table"
        );
        assert!((row.measured_cost_usd - 0.05).abs() < 1e-12);
        assert_eq!(row.measured_requests, 1);
        assert!(
            !row.pricing_estimated,
            "fully measured spend is not an estimate"
        );

        // A second, unmeasured turn on the same model: its tokens are priced
        // from the table ON TOP of the measured USD, never double-counted
        // (10k tokens at the $2.50/M blended fallback ≈ $0.025).
        acc.add(&usage(MODEL, 10_000, 0, 0));
        let mut map = HashMap::new();
        map.insert(MODEL.to_string(), acc);
        let row = &price_models(&map)[0];
        assert!(row.cost_usd > 0.05, "unmeasured remainder adds table cost");
        assert!(row.cost_usd < 0.2, "measured slice must not be re-priced");
        assert!(
            row.pricing_estimated,
            "unmeasured remainder is heuristically priced"
        );
    }

    #[test]
    fn measured_slice_roundtrips_and_defaults_for_legacy_files() {
        let legacy = r#"{"ts":1,"models":{"m":{"requests":1,"input_tokens":10,"output_tokens":5,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0}}}"#;
        let p: PersistedUsage = serde_json::from_str(legacy).expect("legacy file loads");
        assert_eq!(p.models["m"].measured, MeasuredSlice::default());

        let mut p = PersistedUsage::default();
        let mut acc = ModelUsage::default();
        let mut m = usage("m", 100, 10, 0);
        m.provider_cost_usd = Some(0.5);
        acc.add(&m);
        p.models.insert("m".into(), acc);
        let back: PersistedUsage =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.models["m"].measured.requests, 1);
        assert!((back.models["m"].measured.cost_usd - 0.5).abs() < 1e-12);
    }

    #[test]
    fn dominant_model_picks_highest_token_real_model() {
        let mut models = HashMap::new();
        models.insert("claude-haiku-4.5".to_string(), {
            let mut u = ModelUsage::default();
            u.add(&usage("claude-haiku-4.5", 100, 100, 0));
            u
        });
        models.insert("claude-opus-4.5".to_string(), {
            let mut u = ModelUsage::default();
            u.add(&usage("claude-opus-4.5", 10_000, 10_000, 0));
            u
        });
        models.insert("unknown".to_string(), {
            let mut u = ModelUsage::default();
            u.add(&usage("unknown", 999_999, 0, 0));
            u
        });
        let dominant = models
            .iter()
            .filter(|(m, _)| m.as_str() != "unknown")
            .max_by_key(|(_, u)| u.billable_tokens())
            .map(|(m, _)| m.clone());
        assert_eq!(dominant.as_deref(), Some("claude-opus-4.5"));
    }

    #[test]
    fn empty_model_buckets_as_unknown() {
        assert_eq!(normalize_key("  "), "unknown");
        assert_eq!(normalize_key(""), "unknown");
        assert_eq!(normalize_key("gpt-5.4"), "gpt-5.4");
    }

    #[test]
    fn cohort_avg_output_is_mean_per_turn() {
        let mut c = CohortUsage::default();
        assert_eq!(c.avg_output(), None, "no observations → None");
        c.add(&usage("m", 10, 100, 0));
        c.add(&usage("m", 10, 50, 0));
        assert_eq!(c.requests, 2);
        assert_eq!(c.output_tokens, 150);
        assert!((c.avg_output().unwrap() - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn persisted_usage_without_cohorts_field_loads() {
        // proxy_usage.json written before #895 has no `cohorts` key; serde(default)
        // must backfill an empty map so old files stay loadable.
        let json = r#"{"ts":1,"models":{"gpt-5.4":{"requests":1,"input_tokens":10,"output_tokens":5,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0}}}"#;
        let p: PersistedUsage = serde_json::from_str(json).expect("loads legacy file");
        assert_eq!(p.models.len(), 1);
        assert!(p.cohorts.is_empty());
    }

    #[test]
    fn persisted_usage_roundtrips_cohorts() {
        let mut p = PersistedUsage::default();
        p.cohorts.insert(
            "control".into(),
            CohortUsage {
                requests: 3,
                input_tokens: 30,
                output_tokens: 300,
                sum_sq_output: 30_000,
            },
        );
        let json = serde_json::to_string(&p).unwrap();
        let back: PersistedUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cohorts.get("control").unwrap().output_tokens, 300);
    }

    #[test]
    fn managed_usage_projects_once_and_unmanaged_is_skipped() {
        let sink = crate::core::ocla::builtin::usage_sink::BuiltinUsageSink::new();
        let usage = super::super::usage::RealUsage {
            model: "gpt-5".into(),
            input_tokens: 100,
            output_tokens: 40,
            cache_read_tokens: 20,
            cache_write_tokens: 5,
            wire: Some(Box::new(super::super::usage::WireContext {
                lineage: Some(crate::core::ocla::OclaRequestContext {
                    request_id: "request-1".into(),
                    session_id: "session-1".into(),
                    agent_id: "agent-1".into(),
                    content_ref: "blake3:content".into(),
                    tenant_id: None,
                }),
                ..Default::default()
            })),
            ..Default::default()
        };

        project_ocla_usage(&usage, &sink);
        assert_eq!(sink.record_count(), 1);
        assert_eq!(sink.total_input_tokens(), 125);
        assert_eq!(sink.total_output_tokens(), 40);

        project_ocla_usage(
            &super::super::usage::RealUsage {
                model: "unmanaged".into(),
                input_tokens: 999,
                ..Default::default()
            },
            &sink,
        );
        assert_eq!(sink.record_count(), 1);
    }
}
