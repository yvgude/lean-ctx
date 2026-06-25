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

use crate::core::gain::model_pricing::{ModelPricing, PricingMatchKind};

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
}

impl ModelUsage {
    fn add(&mut self, u: &super::usage::RealUsage) {
        self.requests += 1;
        self.input_tokens += u.input_tokens;
        self.output_tokens += u.output_tokens;
        self.cache_read_tokens += u.cache_read_tokens;
        self.cache_write_tokens += u.cache_write_tokens;
        self.reasoning_tokens += u.reasoning_tokens;
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
    /// True when pricing came from a heuristic/fallback match, not an exact one.
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
#[must_use]
pub fn total_cost_usd() -> f64 {
    snapshot().iter().map(|m| m.cost_usd).sum()
}

/// Prices a model usage map into sorted [`ModelSpend`] rows. Pure: shared by the
/// in-memory snapshot and the cross-process [`persisted_snapshot`].
#[must_use]
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
    let cost_usd = quote.cost.estimate_usd(
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_write_tokens,
        usage.cache_read_tokens,
    );
    ModelSpend {
        model: model.to_string(),
        requests: usage.requests,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cost_usd,
        pricing_estimated: !matches!(quote.match_kind, PricingMatchKind::Exact),
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
#[must_use]
pub fn load_persisted() -> Option<PersistedUsage> {
    let path = usage_path()?;
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Cross-process priced spend rows, read from disk.
#[must_use]
pub fn persisted_snapshot() -> Vec<ModelSpend> {
    load_persisted()
        .map(|p| price_models(&p.models))
        .unwrap_or_default()
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
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cohort: None,
        }
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
}
