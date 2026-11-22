// SPDX-License-Identifier: Apache-2.0
//! The proxy's per-session token ledger as a Local Read kind
//! (`docs/contracts/session-usage-v1.md`).
//!
//! - `session.usage`: `lean-ctx session usage --json [--session ID]`, or
//!   `GET /v1/local/session/usage[?session=ID]` on the local service.
//!
//! The proxy books every finalized turn on the app session named by its
//! `x-leanctx-session-id` header, and a turn without one on the unattributed
//! bucket (`proxy_usage.json`, `core::proxy_usage`). This document reads that
//! record: token counts, request counts, models and the wire saving per
//! session. Content-free — no prompt, body or path is ever part of it.
//!
//! Wired as one `local_read::Part`.

use std::collections::HashMap;

use serde::Serialize;

use lean_ctx::core::proxy_usage::{ModelUsage, PersistedUsage, SessionUsage};
use lean_ctx_proxy::usage_meter::{self, ModelSpend};

use super::routes::{Method, Param, Project, Route, Shape};
use super::session::usd;
use super::{Answer, Failure, Via};

/// The schema id the document carries (`session-usage-v1`).
pub(crate) const SCHEMA: &str = "leanctx.session-usage/v1";

const SESSION: Param = Param {
    name: "session",
    flag: "--session",
    shape: Shape::Value,
};

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[Route {
    kind: "session.usage",
    method: Method::Get,
    path: "/v1/local/session/usage",
    cli: &["session", "usage", "--json"],
    action: &[],
    params: &[SESSION],
    project: Project::None,
    names: None,
}];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &["session.usage"];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("session", &["usage"])];

/// One model's share of a bucket.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ModelRow {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub saved_tokens: u64,
    pub cost_usd: f64,
    pub pricing_estimated: bool,
}

/// The totals of one bucket (a session, or the unattributed turns).
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Totals {
    /// RFC 3339 (UTC, seconds) of the bucket's newest turn; `null` when it
    /// has none.
    pub last_seen: Option<String>,
    /// The model carrying the most billable tokens (ties: smallest name);
    /// `null` without turns.
    pub model: Option<String>,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    /// The proxy's wire-compression estimate (request bytes / 4).
    pub saved_tokens: u64,
    /// Provider-counted `count_tokens` saving (counterfactual − billed);
    /// `null` when no turn of the bucket was probed. May be negative.
    pub verified_saved_tokens: Option<i64>,
    pub cost_usd: f64,
    /// The share of `cost_usd` the provider itself reported.
    pub measured_usd: f64,
    /// Sorted by model name.
    pub per_model: Vec<ModelRow>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SessionRow {
    pub session_id: String,
    #[serde(flatten)]
    pub totals: Totals,
}

/// The `session.usage` body.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct UsageDoc {
    pub schema: &'static str,
    pub source: &'static str,
    /// `false`: the proxy has written no ledger on this machine yet.
    pub available: bool,
    /// The most recently seen sessions the proxy keeps apart.
    pub max_sessions: usize,
    /// Sorted by `session_id`; with `--session` the one row or none.
    pub sessions: Vec<SessionRow>,
    /// Turns that carried no (valid) session header.
    pub unattributed: Totals,
}

fn rfc3339_secs(secs: u64) -> Option<String> {
    let secs = i64::try_from(secs).ok().filter(|s| *s > 0)?;
    lean_ctx_time::UtcDateTime::from_timestamp(secs, 0)
        .map(|at| at.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true))
}

/// The totals of `bucket`, priced by `price`.
#[allow(clippy::cast_possible_wrap)]
fn totals_of(
    bucket: &SessionUsage,
    price: &dyn Fn(&HashMap<String, ModelUsage>) -> Vec<ModelSpend>,
) -> Totals {
    let models = &bucket.models;
    let mut spend = price(models);
    spend.sort_by(|a, b| a.model.cmp(&b.model));
    let saved_of = |model: &str| models.get(model).map_or(0, |u| u.saved_tokens);
    let probed: u64 = models.values().map(|u| u.counterfactual_requests).sum();
    let counterfactual: u64 = models.values().map(|u| u.counterfactual_input_tokens).sum();
    let billed: u64 = models
        .values()
        .map(|u| u.counterfactual_billed_tokens)
        .sum();
    let model = models
        .iter()
        .filter(|(_, u)| u.billable_tokens() > 0)
        .max_by(|a, b| {
            a.1.billable_tokens()
                .cmp(&b.1.billable_tokens())
                .then_with(|| b.0.cmp(a.0))
        })
        .map(|(name, _)| name.clone());
    Totals {
        last_seen: rfc3339_secs(bucket.last_seen),
        model,
        requests: spend.iter().map(|r| r.requests).sum(),
        input_tokens: spend.iter().map(|r| r.input_tokens).sum(),
        output_tokens: spend.iter().map(|r| r.output_tokens).sum(),
        cache_read_tokens: spend.iter().map(|r| r.cache_read_tokens).sum(),
        cache_write_tokens: spend.iter().map(|r| r.cache_write_tokens).sum(),
        reasoning_tokens: spend.iter().map(|r| r.reasoning_tokens).sum(),
        saved_tokens: models.values().map(|u| u.saved_tokens).sum(),
        verified_saved_tokens: (probed > 0).then(|| counterfactual as i64 - billed as i64),
        cost_usd: usd(spend.iter().map(|r| r.cost_usd).sum()),
        measured_usd: usd(spend.iter().map(|r| r.measured_cost_usd).sum()),
        per_model: spend
            .iter()
            .map(|r| ModelRow {
                model: r.model.clone(),
                requests: r.requests,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                cache_read_tokens: r.cache_read_tokens,
                cache_write_tokens: r.cache_write_tokens,
                reasoning_tokens: r.reasoning_tokens,
                saved_tokens: saved_of(&r.model),
                cost_usd: usd(r.cost_usd),
                pricing_estimated: r.pricing_estimated,
            })
            .collect(),
    }
}

/// The `session.usage` body of the proxy's record (`None`: no ledger
/// written yet), limited to the session `only` when named.
pub(crate) fn doc_of(
    persisted: Option<PersistedUsage>,
    only: Option<&str>,
    price: &dyn Fn(&HashMap<String, ModelUsage>) -> Vec<ModelSpend>,
) -> UsageDoc {
    let available = persisted.is_some();
    let persisted = persisted.unwrap_or_default();
    let mut sessions: Vec<SessionRow> = persisted
        .sessions
        .iter()
        .filter(|(id, _)| only.is_none_or(|only| only == id.as_str()))
        .map(|(id, bucket)| SessionRow {
            session_id: id.clone(),
            totals: totals_of(bucket, price),
        })
        .collect();
    sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    UsageDoc {
        schema: SCHEMA,
        source: "proxy",
        available,
        max_sessions: usage_meter::MAX_TRACKED_SESSIONS,
        sessions,
        unattributed: totals_of(&persisted.unattributed, price),
    }
}

/// `--session ID` when named, validated like the app's id.
fn only_session(args: &[String]) -> Result<Option<String>, Failure> {
    match super::flag(args, "--session")? {
        None => Ok(None),
        Some(raw) => lean_ctx::core::activity::session_id(raw)
            .map(Some)
            .ok_or_else(|| Failure::invalid("--session <id> is 1-128 of [A-Za-z0-9._:-]")),
    }
}

/// `session usage --json [--session ID]`.
pub(crate) fn build(args: &[String]) -> Result<UsageDoc, Failure> {
    let only = only_session(args)?;
    lean_ctx::core::gain::live_pricing::ensure_loaded();
    Ok(doc_of(
        usage_meter::load_persisted(),
        only.as_deref(),
        &usage_meter::price_models,
    ))
}

/// The document of `kind` for `args` (same bytes on the CLI and the
/// socket), released through the `ctx_session` tool run.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    match kind {
        "session.usage" => super::answer_released("ctx_session", "", kind, via, || build(args)),
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `lean-ctx session usage --json [--session ID]`.
pub(crate) fn run(args: &[String]) {
    super::print(&answer("session.usage", args, Via::Cli));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed price table so the document is independent of live pricing.
    fn flat_price(models: &HashMap<String, ModelUsage>) -> Vec<ModelSpend> {
        models
            .iter()
            .map(|(model, u)| ModelSpend {
                model: model.clone(),
                requests: u.requests,
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_read_tokens: u.cache_read_tokens,
                cache_write_tokens: u.cache_write_tokens,
                reasoning_tokens: u.reasoning_tokens,
                #[allow(clippy::cast_precision_loss)]
                cost_usd: u.billable_tokens() as f64 / 1_000_000.0,
                measured_cost_usd: 0.0,
                measured_requests: 0,
                pricing_estimated: false,
            })
            .collect()
    }

    fn bucket(last_seen: u64, rows: &[(&str, u64, u64, u64)]) -> SessionUsage {
        SessionUsage {
            last_seen,
            models: rows
                .iter()
                .map(|(model, requests, input, saved)| {
                    (
                        (*model).to_owned(),
                        ModelUsage {
                            requests: *requests,
                            input_tokens: *input,
                            output_tokens: 10 * requests,
                            saved_tokens: *saved,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        }
    }

    fn ledger() -> PersistedUsage {
        let mut persisted = PersistedUsage::default();
        persisted.sessions.insert(
            "app-b".into(),
            bucket(
                1_763_200_000,
                &[("claude-sonnet-4.5", 2, 3_000, 400), ("gpt-5", 1, 500, 0)],
            ),
        );
        let mut probed = bucket(1_763_100_000, &[("claude-opus-4.1", 1, 800, 120)]);
        let opus = probed.models.get_mut("claude-opus-4.1").unwrap();
        opus.counterfactual_requests = 1;
        opus.counterfactual_input_tokens = 1_000;
        opus.counterfactual_billed_tokens = 800;
        persisted.sessions.insert("app-a".into(), probed);
        persisted.unattributed = bucket(1_763_000_000, &[("gpt-5", 4, 900, 60)]);
        persisted
    }

    #[test]
    fn session_usage_lists_every_session_and_the_unattributed_bucket() {
        let doc = doc_of(Some(ledger()), None, &flat_price);
        assert!(doc.available);
        assert_eq!(doc.schema, SCHEMA);
        let ids: Vec<&str> = doc.sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, ["app-a", "app-b"], "sorted by session id");
        let b = &doc.sessions[1].totals;
        assert_eq!(b.requests, 3);
        assert_eq!(b.input_tokens, 3_500);
        assert_eq!(b.saved_tokens, 400);
        assert_eq!(b.model.as_deref(), Some("claude-sonnet-4.5"));
        assert_eq!(b.last_seen.as_deref(), Some("2025-11-15T09:46:40Z"));
        assert_eq!(
            b.verified_saved_tokens, None,
            "no probe, no verified saving"
        );
        let models: Vec<&str> = b.per_model.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(models, ["claude-sonnet-4.5", "gpt-5"]);
        assert_eq!(doc.sessions[0].totals.verified_saved_tokens, Some(200));
        assert_eq!(doc.unattributed.requests, 4);
        assert_eq!(doc.unattributed.saved_tokens, 60);

        let text = super::super::document("session.usage", &doc);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], "session.usage");
        assert_eq!(
            value["sessions"][0]["session_id"], "app-a",
            "flat rows: {text}"
        );
        assert_eq!(value["sessions"][0]["requests"], 1, "flat rows: {text}");
        assert_eq!(value["unattributed"]["model"], "gpt-5");
        assert!(value["unattributed"].get("session_id").is_none());
    }

    #[test]
    fn session_usage_is_byte_deterministic_and_filters_one_session() {
        let first =
            super::super::document("session.usage", &doc_of(Some(ledger()), None, &flat_price));
        for _ in 0..8 {
            let again =
                super::super::document("session.usage", &doc_of(Some(ledger()), None, &flat_price));
            assert_eq!(first, again, "same record, same bytes");
        }
        let one = doc_of(Some(ledger()), Some("app-b"), &flat_price);
        assert_eq!(one.sessions.len(), 1);
        assert_eq!(one.unattributed.requests, 4, "unattributed always present");
        let none = doc_of(Some(ledger()), Some("app-zzz"), &flat_price);
        assert!(none.sessions.is_empty());
    }

    #[test]
    fn session_usage_without_a_ledger_is_unavailable_and_empty() {
        let doc = doc_of(None, None, &flat_price);
        assert!(!doc.available);
        assert!(doc.sessions.is_empty());
        assert_eq!(doc.unattributed.last_seen, None);
        assert_eq!(doc.unattributed.model, None);
        assert_eq!(doc.unattributed.requests, 0);
    }

    /// The CLI builder reads the proxy's file from the data dir and checks
    /// `--session`.
    #[test]
    fn session_usage_builder_reads_the_persisted_ledger() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let path = lean_ctx::core::proxy_usage::usage_path().expect("usage path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&ledger()).unwrap()).unwrap();
        let args = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let doc = build(&args(&["--json", "--session", "app-a"])).expect("doc");
        assert!(doc.available);
        assert_eq!(doc.sessions.len(), 1);
        assert_eq!(doc.sessions[0].totals.input_tokens, 800);
        assert_eq!(doc.unattributed.saved_tokens, 60);
        assert!(build(&args(&["--json", "--session", "../x"])).is_err());
    }

    /// The kind is on the local service and in the one capabilities
    /// document (route, kind, command verb on both transports).
    #[test]
    fn session_usage_route_kind_and_verb_are_listed() {
        use super::super::{capabilities, routes};
        let table = routes::table_json();
        let route = routes::all()
            .find(|r| r.path == "/v1/local/session/usage")
            .expect("routed");
        assert_eq!(route.kind, "session.usage");
        let row = table
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == "/v1/local/session/usage")
            .expect("in capabilities.routes");
        assert_eq!(row["kind"], "session.usage");
        assert_eq!(row["method"], "GET");
        assert_eq!(row["command"], "session", "{row}");
        assert_eq!(row["verb"], "usage", "{row}");
        let commands = capabilities::commands_of(&table);
        assert!(
            commands["session"].as_array().is_some_and(|verbs| verbs
                .contains(&serde_json::json!({"verb": "usage", "via": ["cli", "socket"]}))),
            "{commands:?}"
        );
        assert!(
            capabilities::build_with(table.clone(), routes::events_json())
                .kinds
                .contains(&"session.usage")
        );
        assert!(super::super::part_of("session.usage").is_some());
        let args = routes::args_of(route, None, &[]).expect("args");
        assert!(args.iter().any(|a| a == "--json"));
        assert!(only_session(&args).unwrap().is_none());
    }
}
