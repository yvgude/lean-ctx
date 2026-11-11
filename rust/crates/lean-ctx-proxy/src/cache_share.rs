// SPDX-License-Identifier: Apache-2.0
//! Cache-share guard: per provider+model cost log, rolling cache-read share,
//! alarm below the threshold, and the provider allowlist for open models.
//!
//! A customer at the plan limit costs several times more when the provider's
//! prompt cache does not hit: every prompt token then bills at the full input
//! rate instead of the cache-read rate. This module watches that per
//! provider+model:
//!
//! - **Cost log** — [`record_turn`] logs every measured provider turn with its
//!   provider, model, input/cache-read/cache-write/output tokens and the
//!   purchase price of each token class (USD per 1M tokens).
//! - **Rolling aggregate** — the turns land in time buckets per
//!   provider+model; [`CacheShareLedger::report`] sums the configured window
//!   into the cache-read share of the prompt tokens and the effective cost
//!   (with what the same tokens would have cost without any cache).
//! - **Alarm** — when a window with enough prompt tokens drops below
//!   `[proxy.cache_share] alarm_threshold` (default 90 %), a warning is logged
//!   and a `VerificationWarning` event (`cache_share_below_threshold`) is
//!   emitted once; it re-arms when the share recovers.
//! - **Open-model allowlist** — an open-weight model served by a provider
//!   outside `open_model_providers` is flagged (warning + event) or, with
//!   `open_model_policy = "refuse"`, refused before the request leaves
//!   ([`open_model_refusal`]). Local endpoints are exempt.
//!
//! The ledger persists to `proxy_cache_share.json` next to `proxy_usage.json`
//! so `lean-ctx spend --cache` reads it from another process.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use lean_ctx::core::config::CacheShareConfig;
use lean_ctx::core::gain::model_pricing::{ModelCost, ModelPricing};

/// File the ledger persists to, in the lean-ctx data directory.
const CACHE_SHARE_FILE: &str = "proxy_cache_share.json";
/// Distinct provider+model keys kept; further keys fold into one overflow key.
const MAX_TRACKED_KEYS: usize = 256;
/// Buckets per window: the window is summed from this many time slices.
const BUCKETS_PER_WINDOW: u64 = 60;
/// Reason code of a refused open model on an unlisted provider.
pub const OPEN_MODEL_PROVIDER_NOT_ALLOWED: &str = "destination.open_model_provider_not_allowed";
/// Event kind of the cache-share alarm.
pub const ALARM_EVENT: &str = "cache_share_below_threshold";
/// Event kind of an open model on an unlisted provider.
pub const OPEN_MODEL_EVENT: &str = "open_model_provider_not_allowlisted";

/// Built-in name fragments of open-weight model families.
const OPEN_MODEL_FRAGMENTS: &[&str] = &[
    "llama",
    "qwen",
    "qwq",
    "deepseek",
    "mistral",
    "mixtral",
    "codestral",
    "devstral",
    "magistral",
    "gemma",
    "kimi",
    "glm-",
    "gpt-oss",
    "minimax",
    "nemotron",
    "olmo",
    "granite",
    "phi-",
];

/// Resolved `[proxy.cache_share]` settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub threshold: f64,
    pub window_secs: u64,
    pub min_prompt_tokens: u64,
    pub open_model_providers: Vec<String>,
    pub open_model_patterns: Vec<String>,
    pub refuse_unlisted_open_models: bool,
}

impl Settings {
    #[must_use]
    pub fn from_config(cfg: &CacheShareConfig) -> Self {
        Self {
            threshold: cfg.effective_threshold(),
            window_secs: cfg.effective_window_secs(),
            min_prompt_tokens: cfg.effective_min_prompt_tokens(),
            open_model_providers: cfg
                .open_model_providers
                .iter()
                .map(|p| p.trim().to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect(),
            open_model_patterns: cfg
                .open_model_patterns
                .iter()
                .map(|p| p.trim().to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect(),
            refuse_unlisted_open_models: cfg.refuses_unlisted_open_models(),
        }
    }

    /// Settings from the current `config.toml`.
    #[must_use]
    pub fn load() -> Self {
        Self::from_config(&lean_ctx::core::config::Config::load().proxy.cache_share)
    }

    fn bucket_secs(&self) -> u64 {
        (self.window_secs / BUCKETS_PER_WINDOW).max(1)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_config(&CacheShareConfig::default())
    }
}

/// Purchase price per token class, USD per 1M tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct TokenPrices {
    pub input_per_m: f64,
    pub cache_read_per_m: f64,
    pub cache_write_per_m: f64,
    pub output_per_m: f64,
}

impl From<ModelCost> for TokenPrices {
    fn from(c: ModelCost) -> Self {
        Self {
            input_per_m: c.input_per_m,
            cache_read_per_m: c.cache_read_per_m,
            cache_write_per_m: c.cache_write_per_m,
            output_per_m: c.output_per_m,
        }
    }
}

/// One measured provider turn, priced.
#[derive(Debug, Clone, PartialEq)]
pub struct CallCost {
    pub provider: String,
    pub model: String,
    /// Served by a local endpoint: nothing is bought, no alarm.
    pub local: bool,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub prices: TokenPrices,
    /// What the turn cost: the provider's own charge when reported, else the
    /// table price of the four token classes.
    pub cost_usd: f64,
    /// What the same tokens cost with every prompt token at the input rate.
    pub uncached_cost_usd: f64,
}

impl CallCost {
    /// Prices one turn with `prices`; `measured_usd` (the provider's own
    /// charge) wins over the table price.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn priced(
        provider: &str,
        model: &str,
        local: bool,
        input_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        output_tokens: u64,
        prices: TokenPrices,
        measured_usd: Option<f64>,
    ) -> Self {
        #[allow(clippy::cast_precision_loss)]
        let per_m = |tokens: u64, rate: f64| tokens as f64 / 1_000_000.0 * rate;
        let table = per_m(input_tokens, prices.input_per_m)
            + per_m(cache_read_tokens, prices.cache_read_per_m)
            + per_m(cache_write_tokens, prices.cache_write_per_m)
            + per_m(output_tokens, prices.output_per_m);
        let prompt = input_tokens
            .saturating_add(cache_read_tokens)
            .saturating_add(cache_write_tokens);
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            local,
            input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            output_tokens,
            prices,
            cost_usd: measured_usd.unwrap_or(table),
            uncached_cost_usd: per_m(prompt, prices.input_per_m)
                + per_m(output_tokens, prices.output_per_m),
        }
    }
}

/// Cache-read share of the prompt tokens (uncached input + cache reads +
/// cache writes); `None` when there are no prompt tokens.
#[must_use]
pub fn cache_read_share(input: u64, cache_read: u64, cache_write: u64) -> Option<f64> {
    let prompt = input.saturating_add(cache_read).saturating_add(cache_write);
    #[allow(clippy::cast_precision_loss)]
    (prompt > 0).then(|| cache_read as f64 / prompt as f64)
}

/// True when `model` names an open-weight model family (built-in fragments
/// plus `extra`, all lowercase).
#[must_use]
pub fn is_open_model(model: &str, extra: &[String]) -> bool {
    let m = model.to_ascii_lowercase();
    OPEN_MODEL_FRAGMENTS.iter().any(|f| m.contains(f))
        || extra.iter().any(|f| m.contains(f.as_str()))
}

/// How the open-model allowlist judges one provider+model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenModelVerdict {
    /// Not an open model, or served locally.
    NotApplicable,
    /// Open model on an allowlisted provider.
    Allowed,
    /// Open model on a provider without a listed cache discount.
    Flagged,
    /// As `Flagged`, and the policy refuses it.
    Refused,
}

/// Judges `model` on `provider` against the open-model allowlist.
#[must_use]
pub fn open_model_verdict(
    provider: &str,
    model: &str,
    local: bool,
    settings: &Settings,
) -> OpenModelVerdict {
    if local || !is_open_model(model, &settings.open_model_patterns) {
        return OpenModelVerdict::NotApplicable;
    }
    let provider = provider.trim().to_ascii_lowercase();
    if settings.open_model_providers.contains(&provider) {
        OpenModelVerdict::Allowed
    } else if settings.refuse_unlisted_open_models {
        OpenModelVerdict::Refused
    } else {
        OpenModelVerdict::Flagged
    }
}

/// Request-time check: the refusal code when the policy refuses this open
/// model on this provider; a flag-only verdict is reported at record time.
#[must_use]
pub fn open_model_refusal(provider: &str, model: &str, local: bool) -> Option<&'static str> {
    (open_model_verdict(provider, model, local, &Settings::load()) == OpenModelVerdict::Refused)
        .then_some(OPEN_MODEL_PROVIDER_NOT_ALLOWED)
}

/// Token and cost sums of one time slice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Bucket {
    start: u64,
    requests: u64,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    uncached_cost_usd: f64,
}

/// Rolling state of one provider+model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct KeyState {
    provider: String,
    model: String,
    #[serde(default)]
    local: bool,
    buckets: VecDeque<Bucket>,
    /// Prices of the latest turn.
    prices: TokenPrices,
    #[serde(default)]
    alarm_active: bool,
    #[serde(default)]
    alarms_fired: u64,
    #[serde(default)]
    last_alarm_at: Option<u64>,
    #[serde(default)]
    open_model_flagged: bool,
}

/// State change worth a log line and an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    AlarmRaised {
        provider: String,
        model: String,
        share: f64,
        threshold: f64,
        prompt_tokens: u64,
    },
    AlarmCleared {
        provider: String,
        model: String,
        share: f64,
    },
    OpenModelFlagged {
        provider: String,
        model: String,
    },
}

/// Health of one provider+model over the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareStatus {
    Ok,
    BelowThreshold,
    /// Fewer prompt tokens than `min_prompt_tokens` in the window.
    InsufficientData,
    /// Served locally: nothing bought, no alarm.
    Local,
}

/// One provider+model over the window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CacheShareRow {
    pub provider: String,
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub prompt_tokens: u64,
    /// Cache reads / prompt tokens; `None` without prompt tokens.
    pub cache_read_share: Option<f64>,
    pub cost_usd: f64,
    /// Cost of the same tokens with every prompt token at the input rate.
    pub uncached_cost_usd: f64,
    /// Purchase price per token class of the latest turn (USD per 1M).
    pub prices: TokenPrices,
    pub status: ShareStatus,
    pub alarm_active: bool,
    pub alarms_fired: u64,
    pub last_alarm_at: Option<u64>,
    pub open_model: OpenModelVerdict,
}

/// The window report: settings plus one row per provider+model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CacheShareReport {
    pub window_secs: u64,
    pub alarm_threshold: f64,
    pub min_prompt_tokens: u64,
    pub rows: Vec<CacheShareRow>,
}

/// Rolling per provider+model ledger. Pure: the caller passes the clock and
/// the settings, and turns the returned [`Signal`]s into logs and events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheShareLedger {
    keys: Vec<KeyState>,
}

struct Window {
    requests: u64,
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    cost: f64,
    uncached: f64,
}

impl KeyState {
    fn window(&self, now: u64, settings: &Settings) -> Window {
        let from = now.saturating_sub(settings.window_secs);
        let mut w = Window {
            requests: 0,
            input: 0,
            cache_read: 0,
            cache_write: 0,
            output: 0,
            cost: 0.0,
            uncached: 0.0,
        };
        for b in self.buckets.iter().filter(|b| b.start >= from) {
            w.requests += b.requests;
            w.input += b.input_tokens;
            w.cache_read += b.cache_read_tokens;
            w.cache_write += b.cache_write_tokens;
            w.output += b.output_tokens;
            w.cost += b.cost_usd;
            w.uncached += b.uncached_cost_usd;
        }
        w
    }

    fn status(&self, w: &Window, settings: &Settings) -> (Option<f64>, ShareStatus) {
        let share = cache_read_share(w.input, w.cache_read, w.cache_write);
        let prompt = w.input + w.cache_read + w.cache_write;
        let status = if self.local {
            ShareStatus::Local
        } else if prompt < settings.min_prompt_tokens.max(1) {
            ShareStatus::InsufficientData
        } else if share.unwrap_or(0.0) < settings.threshold {
            ShareStatus::BelowThreshold
        } else {
            ShareStatus::Ok
        };
        (share, status)
    }
}

impl CacheShareLedger {
    /// Adds one turn at `now` and returns the state changes it caused.
    pub fn record(&mut self, call: &CallCost, now: u64, settings: &Settings) -> Vec<Signal> {
        let idx = self.key_index(&call.provider, &call.model);
        let key = &mut self.keys[idx];
        key.local = call.local;
        key.prices = call.prices;
        let bucket_secs = settings.bucket_secs();
        let start = now - now % bucket_secs;
        match key.buckets.back_mut() {
            Some(b) if b.start == start => add(b, call),
            _ => {
                let mut b = Bucket {
                    start,
                    ..Bucket::default()
                };
                add(&mut b, call);
                key.buckets.push_back(b);
            }
        }
        let from = now.saturating_sub(settings.window_secs);
        while key.buckets.front().is_some_and(|b| b.start < from) {
            key.buckets.pop_front();
        }

        let mut signals = Vec::new();
        if !key.open_model_flagged
            && open_model_verdict(&key.provider, &key.model, key.local, settings)
                == OpenModelVerdict::Flagged
        {
            key.open_model_flagged = true;
            signals.push(Signal::OpenModelFlagged {
                provider: key.provider.clone(),
                model: key.model.clone(),
            });
        }
        let w = key.window(now, settings);
        let (share, status) = key.status(&w, settings);
        match status {
            ShareStatus::BelowThreshold if !key.alarm_active => {
                key.alarm_active = true;
                key.alarms_fired += 1;
                key.last_alarm_at = Some(now);
                signals.push(Signal::AlarmRaised {
                    provider: key.provider.clone(),
                    model: key.model.clone(),
                    share: share.unwrap_or(0.0),
                    threshold: settings.threshold,
                    prompt_tokens: w.input + w.cache_read + w.cache_write,
                });
            }
            ShareStatus::Ok if key.alarm_active => {
                key.alarm_active = false;
                signals.push(Signal::AlarmCleared {
                    provider: key.provider.clone(),
                    model: key.model.clone(),
                    share: share.unwrap_or(0.0),
                });
            }
            _ => {}
        }
        signals
    }

    /// The window ending at `now`, one row per provider+model with turns in
    /// it, sorted by cost descending.
    #[must_use]
    pub fn report(&self, now: u64, settings: &Settings) -> CacheShareReport {
        let mut rows: Vec<CacheShareRow> = self
            .keys
            .iter()
            .filter_map(|key| {
                let w = key.window(now, settings);
                if w.requests == 0 {
                    return None;
                }
                let (share, status) = key.status(&w, settings);
                Some(CacheShareRow {
                    provider: key.provider.clone(),
                    model: key.model.clone(),
                    requests: w.requests,
                    input_tokens: w.input,
                    cache_read_tokens: w.cache_read,
                    cache_write_tokens: w.cache_write,
                    output_tokens: w.output,
                    prompt_tokens: w.input + w.cache_read + w.cache_write,
                    cache_read_share: share,
                    cost_usd: w.cost,
                    uncached_cost_usd: w.uncached,
                    prices: key.prices,
                    status,
                    alarm_active: key.alarm_active,
                    alarms_fired: key.alarms_fired,
                    last_alarm_at: key.last_alarm_at,
                    open_model: open_model_verdict(&key.provider, &key.model, key.local, settings),
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            b.cost_usd
                .partial_cmp(&a.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)))
        });
        CacheShareReport {
            window_secs: settings.window_secs,
            alarm_threshold: settings.threshold,
            min_prompt_tokens: settings.min_prompt_tokens,
            rows,
        }
    }

    fn key_index(&mut self, provider: &str, model: &str) -> usize {
        if let Some(i) = self
            .keys
            .iter()
            .position(|k| k.provider == provider && k.model == model)
        {
            return i;
        }
        let (provider, model) = if self.keys.len() >= MAX_TRACKED_KEYS {
            ("other", "other")
        } else {
            (provider, model)
        };
        if let Some(i) = self
            .keys
            .iter()
            .position(|k| k.provider == provider && k.model == model)
        {
            return i;
        }
        self.keys.push(KeyState {
            provider: provider.to_string(),
            model: model.to_string(),
            ..KeyState::default()
        });
        self.keys.len() - 1
    }
}

fn add(b: &mut Bucket, call: &CallCost) {
    b.requests += 1;
    b.input_tokens += call.input_tokens;
    b.cache_read_tokens += call.cache_read_tokens;
    b.cache_write_tokens += call.cache_write_tokens;
    b.output_tokens += call.output_tokens;
    b.cost_usd += call.cost_usd;
    b.uncached_cost_usd += call.uncached_cost_usd;
}

fn ledger() -> &'static Mutex<CacheShareLedger> {
    static LEDGER: OnceLock<Mutex<CacheShareLedger>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(load_persisted().unwrap_or_default()))
}

fn ledger_path() -> Option<std::path::PathBuf> {
    lean_ctx::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join(CACHE_SHARE_FILE))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Cross-process read of the persisted ledger.
#[must_use]
pub fn load_persisted() -> Option<CacheShareLedger> {
    let data = std::fs::read_to_string(ledger_path()?).ok()?;
    serde_json::from_str(&data).ok()
}

/// The persisted window report for the CLI, or `None` before any turn.
#[must_use]
pub fn persisted_report() -> Option<CacheShareReport> {
    load_persisted().map(|l| l.report(now_secs(), &Settings::load()))
}

/// Logs, aggregates and persists one measured turn (called from
/// [`super::usage_meter::record`], the single choke-point of measured turns).
pub fn record_turn(u: &super::usage::RealUsage, pricing: &ModelPricing) {
    let (provider, local) = u
        .wire
        .as_deref()
        .map_or(("unknown", false), |w| (w.provider.as_str(), w.is_local));
    let model = match u.model.trim() {
        "" => "unknown",
        m => m,
    };
    let prices = TokenPrices::from(pricing.quote(Some(model)).cost);
    let call = CallCost::priced(
        provider,
        model,
        local,
        u.input_tokens,
        u.cache_read_tokens,
        u.cache_write_tokens,
        u.output_tokens,
        prices,
        u.provider_cost_usd,
    );
    tracing::info!(
        target: "lean_ctx_proxy::cost",
        provider = %call.provider,
        model = %call.model,
        input_tokens = call.input_tokens,
        cache_read_tokens = call.cache_read_tokens,
        cache_write_tokens = call.cache_write_tokens,
        output_tokens = call.output_tokens,
        input_per_m = prices.input_per_m,
        cache_read_per_m = prices.cache_read_per_m,
        cache_write_per_m = prices.cache_write_per_m,
        output_per_m = prices.output_per_m,
        cost_usd = call.cost_usd,
        measured = u.provider_cost_usd.is_some(),
        "provider call cost"
    );
    let settings = Settings::load();
    let (signals, snapshot) = {
        let mut ledger = ledger()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let signals = ledger.record(&call, now_secs(), &settings);
        (signals, ledger.clone())
    };
    for signal in &signals {
        emit(signal);
    }
    persist(&snapshot);
}

fn emit(signal: &Signal) {
    match signal {
        Signal::AlarmRaised {
            provider,
            model,
            share,
            threshold,
            prompt_tokens,
        } => {
            let detail = format!(
                "{provider}/{model}: cache-read share {:.1} % below {:.1} % over {prompt_tokens} prompt tokens",
                share * 100.0,
                threshold * 100.0
            );
            tracing::warn!(target: "lean_ctx_proxy::cost", "cache share alarm: {detail}");
            lean_ctx::core::events::emit_verification_warning(ALARM_EVENT, &detail, "warning");
        }
        Signal::AlarmCleared {
            provider,
            model,
            share,
        } => {
            tracing::info!(
                target: "lean_ctx_proxy::cost",
                "cache share recovered: {provider}/{model} at {:.1} %",
                share * 100.0
            );
        }
        Signal::OpenModelFlagged { provider, model } => {
            let detail = format!(
                "open model {model} served by {provider}, which is not in [proxy.cache_share] open_model_providers"
            );
            tracing::warn!(target: "lean_ctx_proxy::cost", "{detail}");
            lean_ctx::core::events::emit_verification_warning(OPEN_MODEL_EVENT, &detail, "warning");
        }
    }
}

fn persist(ledger: &CacheShareLedger) {
    let Some(path) = ledger_path() else {
        return;
    };
    let Ok(json) = serde_json::to_string(ledger) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRICES: TokenPrices = TokenPrices {
        input_per_m: 3.0,
        cache_read_per_m: 0.3,
        cache_write_per_m: 3.75,
        output_per_m: 15.0,
    };

    fn call(provider: &str, model: &str, input: u64, read: u64, write: u64) -> CallCost {
        CallCost::priced(
            provider, model, false, input, read, write, 1_000, PRICES, None,
        )
    }

    fn settings() -> Settings {
        Settings {
            min_prompt_tokens: 100_000,
            ..Settings::default()
        }
    }

    #[test]
    fn share_and_cost_follow_the_token_classes() {
        assert_eq!(cache_read_share(0, 0, 0), None);
        assert_eq!(cache_read_share(5, 90, 5), Some(0.9));

        let c = call("Anthropic", "claude-sonnet-4-5", 100_000, 900_000, 0);
        // 0.1 M × 3 + 0.9 M × 0.3 + 0.001 M × 15
        assert!((c.cost_usd - 0.585).abs() < 1e-9);
        // 1 M × 3 + 0.001 M × 15
        assert!((c.uncached_cost_usd - 3.015).abs() < 1e-9);
        let measured = CallCost::priced("OpenRouter", "m", false, 1, 0, 0, 0, PRICES, Some(0.42));
        assert!((measured.cost_usd - 0.42).abs() < f64::EPSILON);

        let mut ledger = CacheShareLedger::default();
        let s = settings();
        ledger.record(&c, 10_000, &s);
        ledger.record(
            &call("Anthropic", "claude-sonnet-4-5", 0, 0, 100_000),
            10_010,
            &s,
        );
        let report = ledger.report(10_020, &s);
        let row = &report.rows[0];
        assert_eq!(row.requests, 2);
        assert_eq!(row.prompt_tokens, 1_100_000);
        assert!((row.cache_read_share.unwrap() - 900_000.0 / 1_100_000.0).abs() < 1e-12);
        assert_eq!(row.prices, PRICES);
        // Turns older than the window drop out of the report.
        assert!(
            ledger
                .report(10_020 + s.window_secs + 60, &s)
                .rows
                .is_empty()
        );
    }

    #[test]
    fn alarm_fires_once_below_threshold_and_rearms_on_recovery() {
        let s = settings();
        let mut ledger = CacheShareLedger::default();
        let key = ("Anthropic", "claude-sonnet-4-5");

        // Cold start: all cache writes, below min_prompt_tokens → no alarm.
        assert!(
            ledger
                .record(&call(key.0, key.1, 0, 0, 50_000), 0, &s)
                .is_empty()
        );
        // Exactly at the threshold (90 %) is not below it.
        let at = ledger.record(&call(key.0, key.1, 0, 450_000, 0), 10, &s);
        assert!(at.is_empty(), "{at:?}");

        // A cache miss turn pulls the window under 90 %.
        let miss = ledger.record(&call(key.0, key.1, 400_000, 0, 0), 20, &s);
        assert!(matches!(
            miss.as_slice(),
            [Signal::AlarmRaised { share, threshold, .. }] if *share < 0.9 && (*threshold - 0.9).abs() < 1e-12
        ));
        // Still below: no second alarm.
        assert!(
            ledger
                .record(&call(key.0, key.1, 1_000, 0, 0), 30, &s)
                .is_empty()
        );
        let row = ledger.report(30, &s).rows.remove(0);
        assert_eq!(
            (row.status, row.alarm_active, row.alarms_fired),
            (ShareStatus::BelowThreshold, true, 1)
        );

        // Recovery re-arms; a configurable threshold moves the line.
        let back = ledger.record(&call(key.0, key.1, 0, 20_000_000, 0), 40, &s);
        assert!(matches!(back.as_slice(), [Signal::AlarmCleared { .. }]));
        let strict = Settings {
            threshold: 0.99,
            ..settings()
        };
        let again = ledger.record(&call(key.0, key.1, 0, 1, 0), 50, &strict);
        assert!(matches!(again.as_slice(), [Signal::AlarmRaised { .. }]));
        assert_eq!(ledger.report(50, &strict).rows[0].alarms_fired, 2);

        // Local endpoints never alarm.
        let mut local = call("ollama", "qwen3-coder", 1_000_000, 0, 0);
        local.local = true;
        assert!(ledger.record(&local, 60, &s).is_empty());
    }

    #[test]
    fn open_models_need_an_allowlisted_provider() {
        let mut s = Settings::from_config(&CacheShareConfig {
            open_model_providers: vec![" DeepSeek ".into()],
            open_model_patterns: vec!["my-oss".into()],
            ..CacheShareConfig::default()
        });
        assert_eq!(
            open_model_verdict("Anthropic", "claude-opus-4-5", false, &s),
            OpenModelVerdict::NotApplicable
        );
        assert_eq!(
            open_model_verdict("deepseek", "deepseek-chat", false, &s),
            OpenModelVerdict::Allowed
        );
        assert_eq!(
            open_model_verdict("groq", "meta-llama/Llama-3.3-70B", false, &s),
            OpenModelVerdict::Flagged
        );
        assert_eq!(
            open_model_verdict("groq", "acme/My-OSS-7b", false, &s),
            OpenModelVerdict::Flagged
        );
        assert_eq!(
            open_model_verdict("ollama", "qwen3-coder", true, &s),
            OpenModelVerdict::NotApplicable
        );

        // Flagged once per provider+model in the ledger.
        let mut ledger = CacheShareLedger::default();
        let first = ledger.record(&call("groq", "qwen3-32b", 10, 0, 0), 0, &s);
        assert!(matches!(
            first.as_slice(),
            [Signal::OpenModelFlagged { .. }]
        ));
        assert!(
            ledger
                .record(&call("groq", "qwen3-32b", 10, 0, 0), 1, &s)
                .is_empty()
        );

        assert!(
            CacheShareConfig {
                open_model_policy: Some(" Refuse".into()),
                ..CacheShareConfig::default()
            }
            .refuses_unlisted_open_models()
        );
        s.refuse_unlisted_open_models = true;
        assert_eq!(
            open_model_verdict("groq", "qwen3-32b", false, &s),
            OpenModelVerdict::Refused
        );
        assert_eq!(
            open_model_verdict("deepseek", "deepseek-chat", false, &s),
            OpenModelVerdict::Allowed
        );
    }
}
