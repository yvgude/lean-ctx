// SPDX-License-Identifier: Apache-2.0
//! The cockpit's view of Hive engine metrics (telemetry SPEC §7, §10):
//!
//! - `GET /api/hive/workspace?range=&group=&resolution=&engine=self` returns the
//!   Insights of the workspace this engine is enrolled in. Hive computes them
//!   (`GET /v1/metrics/insights`, C0 2.0 `metrics.schema.json#/$defs/insights_response`)
//!   with the engine's own credentials, as its owner's member view. An engine
//!   that is not enrolled answers `{connected: false}` and makes **no network
//!   call**. Hive being unreachable, the lease lapsing or a refusal is an
//!   `error` the page shows; the dashboard never fails over it.
//! - `GET /api/hive/explained` serves the "Numbers explained" page (K9): the
//!   formula, trust label and local source of every metric, plus the price
//!   table and the fallback price this engine prices spend with. It is local,
//!   and Hive links the same text.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::core::hive::auth::Session;
use crate::core::hive::state::InstanceRecord;
use crate::core::hive::{HiveError, HiveStore};
use lean_ctx_protocol::hive::lease::LeaseState;

use super::helpers::extract_query_param;

const RANGES: [&str; 5] = ["24h", "7d", "30d", "90d", "365d"];
const GROUPS: [&str; 4] = ["total", "engine", "person", "vault"];
const RESOLUTIONS: [&str; 3] = ["hour", "day", "week"];
const INSIGHTS_SCHEMA: &str = "leanctx.hive.insights/v1";
/// One call's budget, token mint included; the page stays responsive offline.
const BUDGET: Duration = Duration::from_secs(10);
/// SPEC §7: the engine caches an answer for 5 minutes.
const CACHE_FOR: Duration = Duration::from_secs(300);

static CACHE: Mutex<Vec<(String, Instant, Value)>> = Mutex::new(Vec::new());

pub(super) fn handle(
    path: &str,
    query: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if !method.eq_ignore_ascii_case("GET") {
        return None;
    }
    match path {
        "/api/hive/workspace" => Some(workspace(query)),
        "/api/hive/explained" => Some(ok(&explained())),
        _ => None,
    }
}

fn ok(value: &Value) -> (&'static str, &'static str, String) {
    ("200 OK", "application/json", value.to_string())
}

/// The Insights request the page asked for, from closed sets only.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    range: &'static str,
    group: &'static str,
    resolution: Option<&'static str>,
    own_engine: bool,
}

impl Request {
    fn parse(query: &str) -> Result<Self, &'static str> {
        fn pick(
            query: &str,
            key: &'static str,
            allowed: &[&'static str],
            default: Option<&'static str>,
        ) -> Result<Option<&'static str>, &'static str> {
            match extract_query_param(query, key) {
                None => Ok(default),
                Some(value) => allowed
                    .iter()
                    .find(|a| **a == value)
                    .copied()
                    .map(Some)
                    .ok_or(key),
            }
        }
        let range = pick(query, "range", &RANGES, Some("7d"))?.unwrap_or("7d");
        let group = pick(query, "group", &GROUPS, Some("total"))?.unwrap_or("total");
        let resolution = pick(query, "resolution", &RESOLUTIONS, None)?;
        let own_engine = pick(query, "engine", &["self"], None)?.is_some();
        Ok(Self {
            range,
            group,
            resolution,
            own_engine,
        })
    }

    /// The engine route (`/v1` is the client's base).
    fn path(&self) -> String {
        let mut path = format!(
            "/metrics/insights?range={}&group={}",
            self.range, self.group
        );
        if let Some(resolution) = self.resolution {
            path.push_str("&resolution=");
            path.push_str(resolution);
        }
        if self.own_engine {
            path.push_str("&engine=self");
        }
        path
    }
}

fn workspace(query: &str) -> (&'static str, &'static str, String) {
    let request = match Request::parse(query) {
        Ok(request) => request,
        Err(field) => {
            return (
                "400 Bad Request",
                "application/json",
                json!({ "error": { "code": "invalid_field", "field": field } }).to_string(),
            );
        }
    };
    let store = match HiveStore::open() {
        Ok(store) => store,
        Err(error) => return ok(&local_failure(&error)),
    };
    let config = crate::core::config::Config::load().hive;
    if config.enabled == Some(false) {
        return ok(&json!({ "connected": false, "disabled": true }));
    }
    let instance = match store.instance() {
        Ok(instance) => instance,
        Err(error) => return ok(&local_failure(&error)),
    };
    let key = instance
        .as_ref()
        .map(|i| format!("{}\0{}", i.instance_id, request.path()));
    if let Some(key) = &key
        && let Some(hit) = cached(key)
    {
        return ok(&hit);
    }
    let answer = answer(instance.as_ref(), || fetch(&store, &config, &request));
    if let Some(key) = key
        && answer.get("insights").is_some()
    {
        remember(key, answer.clone());
    }
    ok(&answer)
}

fn cached(key: &str) -> Option<Value> {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|(_, at, _)| at.elapsed() < CACHE_FOR);
    cache
        .iter()
        .find(|(k, _, _)| k == key)
        .map(|(_, _, v)| v.clone())
}

fn remember(key: String, value: Value) {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|(k, at, _)| *k != key && at.elapsed() < CACHE_FOR);
    // Bounded: ranges × groups × resolutions × engine filter.
    if cache.len() >= 64 {
        cache.remove(0);
    }
    cache.push((key, Instant::now(), value));
}

fn local_failure(error: &HiveError) -> Value {
    json!({ "connected": false, "error": { "code": "local_state", "message": error.to_string() } })
}

/// One authenticated, time-bounded `GET /v1/metrics/insights`.
fn fetch(
    store: &HiveStore,
    config: &crate::core::hive::HiveConfig,
    request: &Request,
) -> Result<Value, HiveError> {
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(store, config, &instance.url)?;
    let session = Session::load(store.clone(), root)?;
    if !matches!(
        session.lease.state_at(chrono::Utc::now()),
        LeaseState::Valid | LeaseState::Grace
    ) {
        return Err(HiveError::LeaseLapsed);
    }
    session
        .bounded_client(Instant::now() + BUDGET)?
        .get(&request.path())
}

/// The page's answer. Without an enrolment `fetch` is never called: no
/// network while not connected (SPEC §10).
fn answer(
    instance: Option<&InstanceRecord>,
    fetch: impl FnOnce() -> Result<Value, HiveError>,
) -> Value {
    let Some(instance) = instance else {
        return json!({ "connected": false });
    };
    let mut answer = json!({
        "connected": true,
        "workspace": {
            "label": instance.workspace_label(),
            "kind": instance.org_kind,
            "web_url": instance.web_url,
        },
    });
    match fetch() {
        Ok(insights) if insights.get("schema").and_then(Value::as_str) == Some(INSIGHTS_SCHEMA) => {
            answer["insights"] = insights;
        }
        Ok(_) => {
            answer["error"] = json!({
                "code": "unavailable",
                "message": "Hive answered without engine Insights; it may predate this feature.",
            });
        }
        Err(error) => {
            answer["error"] = json!({ "code": error_code(&error), "message": error.to_string() });
            // Hive answered: a locked workspace (`trial_ended`/`plan_ended`) is
            // a state the page shows, not a failure; the reason is structured.
            if let HiveError::PlanRequired(need) = &error
                && let Some(reason) = need.locked()
            {
                answer["error"]["reason"] = json!(reason);
                answer["error"]["locked"] = json!(true);
            }
        }
    }
    answer
}

fn error_code(error: &HiveError) -> &'static str {
    match error {
        HiveError::Offline(_) => "offline",
        HiveError::NotConnected => "not_connected",
        HiveError::Unauthorized => "unauthorized",
        HiveError::LeaseLapsed => "lease_lapsed",
        HiveError::Revoked(_) => "revoked",
        HiveError::Forbidden => "forbidden",
        HiveError::PolicyDenied(_) => "policy_denied",
        HiveError::PlanRequired(_) => "plan_required",
        HiveError::QuotaExceeded(_) => "quota_exceeded",
        _ => "unavailable",
    }
}

// ---------------------------------------------------------------- K9

/// Every metric of the contract (`metrics.schema.json#/$defs/metric_name`) with
/// its trust label (`labels`, K7), formula and local source. The order is the
/// contract's.
pub(crate) const METRICS: &[(&str, &str, &str, &str)] = &[
    (
        "tokens.input",
        "estimated",
        "Tokens the agent would have received without LeanCTX (the baseline).",
        "savings ledger: baseline_tokens",
    ),
    (
        "tokens.output",
        "measured",
        "Tokens LeanCTX actually delivered to the agent.",
        "savings ledger: actual_tokens",
    ),
    (
        "tokens.saved",
        "estimated",
        "tokens.input − tokens.output, after bounce corrections.",
        "savings ledger: saved_tokens − bounce_adjustment",
    ),
    (
        "savings.measured",
        "measured",
        "Saved tokens whose evidence class is measured (counted by a local tokenizer).",
        "savings ledger: evidence_class = measured",
    ),
    (
        "savings.estimated",
        "estimated",
        "Saved tokens that rest on an estimate (approximated, statistical, declared or unclassified evidence).",
        "savings ledger: evidence_class ≠ measured",
    ),
    (
        "savings.estimated_low",
        "estimated",
        "Lower end of the range of savings.estimated.",
        "savings ledger: confidence",
    ),
    (
        "savings.estimated_high",
        "estimated",
        "Upper end of the range of savings.estimated.",
        "savings ledger: confidence",
    ),
    (
        "savings.evidenced",
        "evidenced",
        "The part of the savings backed by a gateway receipt or provider reconciliation.",
        "savings ledger: measurement_method = provider_reconciled, receipts",
    ),
    (
        "savings.overhead",
        "measured",
        "Tokens LeanCTX itself added (instructions, tool schemas, hints). Net own savings = savings.measured + savings.estimated − savings.overhead, and may be negative. The provider cache is not part of it (ledger.cache_discount).",
        "engine instruction and schema counters",
    ),
    (
        "calls.tool_calls",
        "measured",
        "Tool calls through LeanCTX.",
        "savings ledger events",
    ),
    (
        "calls.cache_reads",
        "measured",
        "Re-reads that could be served from the read cache.",
        "stats: cep.total_cache_reads",
    ),
    (
        "calls.cache_hits",
        "measured",
        "Re-reads served from the read cache. Cache-hit rate = cache_hits ÷ cache_reads.",
        "stats: cep.total_cache_hits",
    ),
    (
        "compression.original",
        "measured",
        "Tokens of tool output before compression.",
        "stats: cep.total_tokens_original",
    ),
    (
        "compression.compressed",
        "measured",
        "Tokens of tool output after compression. Compression = 1 − compressed ÷ original.",
        "stats: cep.total_tokens_compressed",
    ),
    (
        "ledger.reduction",
        "estimated",
        "Savings from shortening context.",
        "savings ledger kind: reduction",
    ),
    (
        "ledger.cache_discount",
        "measured",
        "The provider's cache discount the proxy observed; not LeanCTX's own saving.",
        "savings ledger kind: proxy cache",
    ),
    (
        "ledger.correction",
        "estimated",
        "Corrections for content the agent had to fetch again.",
        "savings ledger kind: bounce correction",
    ),
    (
        "ledger.edit",
        "estimated",
        "Savings from edit tools.",
        "savings ledger kind: edit",
    ),
    (
        "ledger.other",
        "estimated",
        "Every other ledger entry.",
        "savings ledger kind: other",
    ),
    (
        "guards.secrets_redacted",
        "measured",
        "Secrets replaced before the model saw them.",
        "audit trail: secret_redacted",
    ),
    (
        "guards.pii_redacted",
        "measured",
        "PII values masked by the gateway.",
        "audit trail: pii_redacted",
    ),
    (
        "guards.injection_flags",
        "measured",
        "Tool outputs flagged for prompt injection.",
        "audit trail: injection_flagged",
    ),
    (
        "guards.content_withheld",
        "measured",
        "Sources the gateway withheld.",
        "audit trail: content_withheld",
    ),
    (
        "guards.paths_blocked",
        "measured",
        "Paths outside the project jail that were refused.",
        "audit trail: path_blocked",
    ),
    (
        "guards.shell_blocked",
        "measured",
        "Shell commands the allowlist refused.",
        "audit trail: shell_blocked",
    ),
    (
        "guards.partial_inspections",
        "measured",
        "Sources delivered although a detector did not inspect all of them.",
        "audit trail: coverage_incomplete",
    ),
    (
        "policy.blocked",
        "measured",
        "Refusals by rules in force, per rule.",
        "gateway decisions",
    ),
    (
        "policy.would_block",
        "measured",
        "Refusals a rule in monitor mode would make, before it goes live.",
        "gateway decisions (monitor)",
    ),
    (
        "refetch.truncated",
        "measured",
        "Outputs shortened by a filter (read mode or compression pattern).",
        "truncation pointers",
    ),
    (
        "refetch.refetched",
        "measured",
        "Shortened outputs the agent fetched back in full. Re-fetch rate = refetched ÷ truncated.",
        "ctx_expand of a truncation pointer",
    ),
    (
        "unregulated.calls",
        "measured",
        "Calls no enforcing rule covers: programs a command ran, tool calls of a host family without an intercepting hook, and calls relayed to MCP servers.",
        "the gateway's decision record: no enforcing rule matched",
    ),
    (
        "sessions.started",
        "measured",
        "Agent sessions started.",
        "session store",
    ),
    (
        "hosts.tool_calls",
        "measured",
        "Tool calls per agent host.",
        "session store: client",
    ),
    (
        "spend.requests",
        "measured",
        "Model requests where the engine measures spend.",
        "proxy usage meter, host logs",
    ),
    (
        "spend.input_tokens",
        "measured",
        "Model input tokens.",
        "proxy usage meter, host logs",
    ),
    (
        "spend.output_tokens",
        "measured",
        "Model output tokens.",
        "proxy usage meter, host logs",
    ),
    (
        "spend.cached_tokens",
        "measured",
        "Model input tokens served from the provider cache.",
        "proxy usage meter, host logs",
    ),
    (
        "spend.usd_micros",
        "estimated",
        "Cost at list price (tokens × price table below, the fallback price for unknown models); estimated · list price, never an invoice.",
        "price table",
    ),
    (
        "knowledge.facts_current",
        "measured",
        "Knowledge facts in the vault at the end of the period (a level, not a sum).",
        "knowledge store",
    ),
    (
        "engines.active",
        "derived",
        "Engines that reported at least one hour in the period (Hive only).",
        "Hive",
    ),
];

/// K7 trust labels, worded exactly like the Cockpit chip (`static/ui/badge.js`).
const LABELS: &[(&str, &str)] = &[
    (
        "measured",
        "Counted by the Engine on this device; reproducible.",
    ),
    (
        "estimated",
        "Rests on a model or a price table; shown with its range where one exists.",
    ),
    (
        "evidenced",
        "Backed by a signed receipt, the hash-chained ledger or the provider's own figures.",
    ),
    ("derived", "Computed from other figures."),
];

fn explained() -> Value {
    let pricing = crate::core::gain::model_pricing::ModelPricing::load();
    let fallback = pricing.quote(None);
    let mut prices: Vec<Value> = pricing
        .rows()
        .into_iter()
        .filter(|(model, _)| *model != fallback.model_key)
        .map(|(model, cost)| price(model, &cost))
        .collect();
    prices.sort_by(|a, b| a["model"].as_str().cmp(&b["model"].as_str()));
    json!({
        "labels": LABELS.iter().map(|(label, meaning)| json!({ "label": label, "meaning": meaning })).collect::<Vec<_>>(),
        "metrics": METRICS.iter().map(|(name, label, formula, source)| json!({
            "name": name, "label": label, "formula": formula, "source": source,
        })).collect::<Vec<_>>(),
        "prices": prices,
        "fallback": price(&fallback.model_key, &fallback.cost),
        "price_unit": "USD per 1M tokens",
    })
}

fn price(model: &str, cost: &crate::core::gain::model_pricing::ModelCost) -> Value {
    json!({
        "model": model,
        "input": cost.input_per_m,
        "output": cost.output_per_m,
        "cache_write": cost.cache_write_per_m,
        "cache_read": cost.cache_read_per_m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = include_str!(
        "../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/schemas/metrics.schema.json"
    );
    const ENGINE_VIEW: &str = include_str!(
        "../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes/metrics_insights.response.json"
    );

    fn instance() -> InstanceRecord {
        InstanceRecord {
            instance_id: "inst_5d1f3b7a9c2e4f6a8b0d1c3e5f7a9b2d".into(),
            org_id: "org_1".into(),
            org_name: "Platform".into(),
            label: "ada-mbp".into(),
            url: "https://hive.leanctx.com".into(),
            org_salt_id: "salt".into(),
            enrolled_at: "2026-10-02T07:00:00Z".into(),
            web_url: Some("https://hive.leanctx.com".into()),
            org_kind: Some("team".into()),
            company_name: None,
        }
    }

    #[test]
    fn not_connected_makes_no_network_call() {
        let answer = answer(None, || panic!("fetched while not connected"));
        assert_eq!(answer, json!({ "connected": false }));
    }

    #[test]
    fn the_contract_answer_passes_and_anything_else_is_an_error() {
        let insights: Value = serde_json::from_str(ENGINE_VIEW).unwrap();
        let shown = answer(Some(&instance()), || Ok(insights.clone()));
        assert_eq!(shown["connected"], true);
        assert_eq!(shown["workspace"]["label"], "Team Platform");
        assert_eq!(shown["insights"], insights);
        assert!(shown.get("error").is_none());

        let old = answer(Some(&instance()), || Ok(json!({ "items": [] })));
        assert_eq!(old["error"]["code"], "unavailable");
        assert!(old.get("insights").is_none());

        let offline = answer(Some(&instance()), || {
            Err(HiveError::Offline("connect refused".into()))
        });
        assert_eq!(offline["error"]["code"], "offline");
        let lapsed = answer(Some(&instance()), || Err(HiveError::LeaseLapsed));
        assert_eq!(lapsed["error"]["code"], "lease_lapsed");
        assert!(lapsed["error"].get("locked").is_none());
    }

    #[test]
    fn a_locked_workspace_carries_its_reason_and_one_line() {
        let need = |reason: &str| {
            HiveError::PlanRequired(Box::new(crate::core::hive::error::PlanNeed {
                feature: Some("read_only".into()),
                plan_needed: Some("personal".into()),
                reason: Some(reason.into()),
                ..Default::default()
            }))
        };
        let locked = answer(Some(&instance()), || Err(need("trial_ended")));
        assert_eq!(locked["connected"], true);
        assert_eq!(locked["error"]["code"], "plan_required");
        assert_eq!(locked["error"]["reason"], "trial_ended");
        assert_eq!(locked["error"]["locked"], true);
        assert_eq!(
            locked["error"]["message"],
            "Your Personal trial has ended: this workspace is locked (read and export only). Choose a plan in Hive to sync again."
        );
        assert_eq!(locked["workspace"]["web_url"], "https://hive.leanctx.com");
        let ended = answer(Some(&instance()), || Err(need("plan_ended")));
        assert_eq!(ended["error"]["reason"], "plan_ended");
        // `no_plan` is not a lock: the plain refusal, without the flag.
        let none = answer(Some(&instance()), || Err(need("no_plan")));
        assert_eq!(none["error"]["code"], "plan_required");
        assert!(none["error"].get("locked").is_none());
    }

    #[test]
    fn only_closed_parameters_reach_hive() {
        assert_eq!(
            Request::parse("").unwrap().path(),
            "/metrics/insights?range=7d&group=total"
        );
        assert_eq!(
            Request::parse("range=24h&group=engine&resolution=hour&engine=self")
                .unwrap()
                .path(),
            "/metrics/insights?range=24h&group=engine&resolution=hour&engine=self"
        );
        assert_eq!(Request::parse("range=1y"), Err("range"));
        assert_eq!(Request::parse("group=workspace"), Err("group"));
        assert_eq!(Request::parse("engine=inst_1"), Err("engine"));
        assert_eq!(Request::parse("resolution=minute"), Err("resolution"));
        assert_eq!(Request::parse("range=7d%26x%3D1"), Err("range"));
    }

    #[test]
    fn the_explained_table_is_the_contracts_metric_list_with_its_labels() {
        let schema: Value = serde_json::from_str(SCHEMA).unwrap();
        let names: Vec<&str> = schema["$defs"]["metric_name"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let ours: Vec<&str> = METRICS.iter().map(|m| m.0).collect();
        assert_eq!(ours, names, "same names, same order");
        let labels: Vec<&str> = schema["$defs"]["trust_label"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(LABELS.iter().map(|l| l.0).collect::<Vec<_>>(), labels);
        // The labels Hive sends are the ones this page explains.
        let view: Value = serde_json::from_str(ENGINE_VIEW).unwrap();
        for (name, label, formula, source) in METRICS {
            assert_eq!(view["labels"][name], *label, "{name}");
            assert!(!formula.is_empty() && !source.is_empty(), "{name}");
        }
    }

    #[test]
    fn explained_lists_prices_and_the_fallback() {
        let page = explained();
        assert_eq!(page["metrics"].as_array().unwrap().len(), METRICS.len());
        assert!(!page["prices"].as_array().unwrap().is_empty());
        assert_eq!(page["fallback"]["model"], "fallback-blended");
        assert!(page["fallback"]["input"].as_f64().unwrap() > 0.0);
    }

    /// K9: the formulas Hive shows (`records/metrics_fields.json`) are this
    /// page's, word for word.
    #[test]
    fn the_formulas_are_the_contracts_field_list() {
        let fields: Value = serde_json::from_str(include_str!(
            "../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/records/metrics_fields.json"
        ))
        .unwrap();
        let theirs: Vec<(&str, &str, &str, &str)> = fields["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["name"].as_str().unwrap(),
                    m["label"].as_str().unwrap(),
                    m["formula"].as_str().unwrap(),
                    m["source"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(theirs, METRICS.to_vec());
    }
}
