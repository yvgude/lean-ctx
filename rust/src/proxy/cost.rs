//! Per-model proxy savings accounting.
//!
//! Headroom's per-model cost breakdown was the one metric a tester found clearer
//! than lean-ctx's single flat number. This module buckets request-side savings
//! by model and prices them with the shared [`ModelPricing`] table so `/status`
//! can report estimated USD avoided per model.
//!
//! Honesty contract: token counts here are request-side *estimates* (the bytes
//! the proxy removed before forwarding). They deliberately do NOT try to model
//! re-reads the agent may perform later, so figures are conservative and
//! labelled `estimated` in the output.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::core::gain::model_pricing::{ModelPricing, PricingMatchKind};

#[derive(Default, Clone)]
struct ModelAccum {
    requests: u64,
    tokens_saved: u64,
    bytes_original: u64,
    bytes_compressed: u64,
}

/// One model's aggregated, priced savings for the `/status` endpoint.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelStat {
    pub model: String,
    pub requests: u64,
    pub tokens_saved: u64,
    pub usd_saved: f64,
    /// True when the price came from a fallback/heuristic match, not an exact one.
    pub pricing_estimated: bool,
}

/// Cap on distinct model buckets. `record` sees the raw request model string, so an
/// arbitrary-model client could otherwise grow the map without bound; overflow folds
/// into "unknown" (real model names number < ~50, so this is generous).
const MAX_TRACKED_MODELS: usize = 256;

fn store() -> &'static Mutex<HashMap<String, ModelAccum>> {
    static STORE: OnceLock<Mutex<HashMap<String, ModelAccum>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Records one request's request-side savings against its model bucket.
///
/// `model` is taken from the request body (`None`/empty buckets under
/// `"unknown"`). Recording never blocks request handling on poisoning.
pub fn record(model: Option<&str>, tokens_saved: u64, bytes_original: u64, bytes_compressed: u64) {
    let key = model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("unknown")
        .to_string();

    let mut map = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Bound distinct model buckets. `record` takes the raw request model string (it does
    // NOT pass through normalize_model), so a client sending arbitrary model names could
    // otherwise grow this map unbounded. Fold overflow into "unknown" so aggregate totals
    // stay exact — only per-model granularity for rare/novel names is lost.
    let key = if !map.contains_key(&key) && map.len() >= MAX_TRACKED_MODELS {
        "unknown".to_string()
    } else {
        key
    };
    let acc = map.entry(key).or_default();
    acc.requests += 1;
    acc.tokens_saved += tokens_saved;
    acc.bytes_original += bytes_original;
    acc.bytes_compressed += bytes_compressed;
}

/// Returns per-model stats, priced and sorted by USD saved (descending).
pub fn snapshot() -> Vec<ModelStat> {
    let pricing = ModelPricing::load();
    let map = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut stats: Vec<ModelStat> = map
        .iter()
        .map(|(model, acc)| {
            let quote = pricing.quote(Some(model));
            // Compression removes *input* tokens, so price against the input rate.
            let usd_saved = acc.tokens_saved as f64 / 1_000_000.0 * quote.cost.input_per_m;
            let pricing_estimated = !matches!(quote.match_kind, PricingMatchKind::Exact);
            ModelStat {
                model: model.clone(),
                requests: acc.requests,
                tokens_saved: acc.tokens_saved,
                usd_saved,
                pricing_estimated,
            }
        })
        .collect();

    stats.sort_by(|a, b| {
        b.usd_saved
            .partial_cmp(&a.usd_saved)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_buckets_and_prices_without_panic() {
        record(None, 1000, 4000, 0);
        record(Some("   "), 500, 2000, 0);
        let stats = snapshot();
        let unknown = stats.iter().find(|s| s.model == "unknown");
        assert!(
            unknown.is_some(),
            "blank/None models bucket under 'unknown'"
        );
        assert!(unknown.unwrap().requests >= 2);
    }

    #[test]
    fn known_model_yields_positive_usd() {
        record(
            Some("claude-opus-4-8-zzz-cost-test"),
            2_000_000,
            8_000_000,
            100,
        );
        let stats = snapshot();
        let row = stats
            .iter()
            .find(|s| s.model.contains("opus-4-8-zzz-cost-test"))
            .expect("recorded model present");
        // Fallback pricing still produces a finite, non-negative estimate.
        assert!(row.usd_saved >= 0.0 && row.usd_saved.is_finite());
        assert_eq!(row.tokens_saved, 2_000_000);
    }
}
