// SPDX-License-Identifier: Apache-2.0
//! LeanCTX Cloud cost per accepted outcome (`GET /v1/cpao`, contract W2):
//! what the team's agents cost over the last 7 and 30 days, divided by the
//! outcomes that were accepted (a landed merge request or an accepted task
//! result), for a local reader such as LeanCTX Code.
//!
//! Read only from a server that announced the `cpao` capability
//! ([`FEATURE_CPAO`], cached by [`crate::session_ref`]); an older server gets
//! no request. The cloud answers:
//! - `200 {schema: leanctx.cloud.cpao/v1, periods: […]}` → [`CloudCpaoV1`];
//! - `503 not_configured`, or `404` from a cloud without the route → not
//!   available (`Ok(None)`);
//! - `403` → [`CloudError::Forbidden`]; [`answer`] also says which refusal
//!   it was ([`Refusal`]).
//!
//! The answer is passed on as the cloud computed it: this module never
//! derives a number. It checks the schema and drops periods and agent rows
//! that are malformed, so one bad row does not hide the rest. When the
//! server does not answer, the last answer of that server is returned with
//! `live: false` (kept in `<data>/cpao.json`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use lean_ctx_time::UtcDateTime;

use crate::CloudError;
use crate::config::CloudConfig;
use crate::state::{CloudStore, write_private};

/// The `features` entry of a server that serves `GET /v1/cpao`.
pub const FEATURE_CPAO: &str = "cpao";

/// `schema` of the answer.
pub const CPAO_SCHEMA: &str = "leanctx.cloud.cpao/v1";

/// The cloud route (under `/v1`).
const PATH: &str = "/cpao";
/// Largest answer read.
const MAX_ANSWER_BYTES: usize = 256 * 1024;
/// The periods a reader knows.
const PERIODS: [&str; 2] = ["7d", "30d"];
/// Most agent rows kept per period.
const MAX_AGENTS: usize = 64;
/// Longest agent name accepted.
const MAX_AGENT_LEN: usize = 64;
/// The last answer.
const CPAO_FILE: &str = "cpao.json";

/// One agent's share of a period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpaoAgentV1 {
    /// `claude`, `codex`, `cursor`, …
    pub agent: String,
    /// Millionths of a US dollar; `null` while the cloud does not price per
    /// agent (passed on as `null`, never as `0`).
    pub cost_usd_micros: Option<u64>,
    pub accepted_outcomes: u64,
}

/// One period of the answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpaoPeriodV1 {
    /// `7d` or `30d`.
    pub period: String,
    /// What the agents cost in the period, in millionths of a US dollar.
    pub cost_usd_micros: u64,
    /// Landed merge requests and accepted task results.
    pub accepted_outcomes: u64,
    /// Cost per accepted outcome as the cloud computed it; `null` without one.
    pub cpao_usd_micros: Option<u64>,
    /// Change against the period before, in percent, when the cloud sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trend_pct: Option<f64>,
    /// `true`: the period's revert window has closed (the figures are not
    /// frozen: a later revert still lowers the count); `false`: provisional,
    /// the window is still open. Left out when the cloud did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled: Option<bool>,
    /// By agent, when the cloud sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_agent: Option<Vec<CpaoAgentV1>>,
}

/// `GET /v1/cpao`, checked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudCpaoV1 {
    pub schema: String,
    pub periods: Vec<CpaoPeriodV1>,
}

/// An answer and when it was read.
#[derive(Debug, Clone, PartialEq)]
pub struct CpaoRead {
    pub cpao: CloudCpaoV1,
    /// When the answer was read (RFC 3339).
    pub as_of: Option<String>,
    /// Read just now (`false`: the last answer; the server did not answer).
    pub live: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Cache {
    /// The origin that answered; another enrolled server reads none of it.
    server: String,
    fetched_at: Option<String>,
    cpao: Option<CloudCpaoV1>,
}

/// Whether the enrolled server announced `cpao` when last asked.
#[must_use]
pub fn announced(store: &CloudStore) -> bool {
    crate::session_ref::announced(store, FEATURE_CPAO)
}

/// Which refusal a `403` of `GET /v1/cpao` was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The person's role in the team may not see the team's costs
    /// (`forbidden` naming a team `required_role`).
    Role,
    /// Any other refusal, with the server's message when it sent one.
    Other(Option<String>),
}

impl Refusal {
    fn of(body: &Value) -> Self {
        let error = &body["error"];
        let team_role = error["required_role"]
            .as_str()
            .is_some_and(|role| !role.is_empty() && role != "company_admin");
        if error["code"].as_str() == Some("forbidden") && team_role {
            Self::Role
        } else {
            Self::Other(
                error["message"]
                    .as_str()
                    .and_then(crate::error::safe_message),
            )
        }
    }
}

/// The CPAO for a reader: read now, else the last answer of this server.
/// `None` when the server did not announce `cpao` (no request is sent) or
/// answered that it does not offer it. A refusal (`403`) is never answered
/// from the last answer; it is [`CloudError::Forbidden`].
pub fn read(
    store: &CloudStore,
    config: &CloudConfig,
    now: UtcDateTime,
) -> Result<Option<CpaoRead>, CloudError> {
    answer(store, config, now)?.map_err(|_| CloudError::Forbidden)
}

/// [`read`], with a refusal (`403`) as the inner error that says which one.
pub fn answer(
    store: &CloudStore,
    config: &CloudConfig,
    now: UtcDateTime,
) -> Result<Result<Option<CpaoRead>, Refusal>, CloudError> {
    if !announced(store) {
        return Ok(Ok(None));
    }
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let mut refusal = None;
    let live = crate::scheduler::connected_session(store, config).and_then(|session| {
        let (status, body) = session.with_client(|client| {
            let (status, body) = client.send_answer("GET", PATH, None, MAX_ANSWER_BYTES)?;
            // A refused token goes through the session's refresh.
            if status == 401 {
                return Err(CloudError::from_response(status, &body));
            }
            Ok((status, body))
        })?;
        if status == 403 {
            refusal = Some(Refusal::of(&body));
        }
        classify(status, &body)
    });
    if let Some(refusal) = refusal {
        return Ok(Err(refusal));
    }
    let mut cache = load_for(store, &instance.url);
    let read = match live {
        Ok(Some(cpao)) => {
            cache.cpao = Some(cpao.clone());
            cache.fetched_at = Some(stamp(now));
            save(store, &cache);
            Ok(Some(CpaoRead {
                cpao,
                as_of: cache.fetched_at,
                live: true,
            }))
        }
        Ok(None) => Ok(None),
        Err(error) if matches!(error, CloudError::Offline(_)) || error.is_transient() => {
            match cache.cpao {
                Some(cpao) => Ok(Some(CpaoRead {
                    cpao,
                    as_of: cache.fetched_at,
                    live: false,
                })),
                None => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    read.map(Ok)
}

/// The answer of `status` and `body`: the checked document, `None` for a
/// cloud that does not offer it, or the error.
fn classify(status: u16, body: &Value) -> Result<Option<CloudCpaoV1>, CloudError> {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    match status {
        200 => decode(body).map(Some),
        503 if code == "not_configured" => Ok(None),
        404 if code.is_empty() || code == "not_found" => Ok(None),
        403 => Err(CloudError::Forbidden),
        _ => Err(CloudError::from_response(status, body)),
    }
}

/// The checked document: the schema must match and `periods` must be a
/// list; malformed periods and agent rows are left out, never repaired.
fn decode(body: &Value) -> Result<CloudCpaoV1, CloudError> {
    let bad = |why: &str| CloudError::Protocol(format!("cpao: {why}"));
    if body["schema"].as_str() != Some(CPAO_SCHEMA) {
        return Err(bad(&format!("schema must be {CPAO_SCHEMA}")));
    }
    let periods = body["periods"]
        .as_array()
        .ok_or_else(|| bad("periods is not a list"))?;
    let mut kept: Vec<CpaoPeriodV1> = Vec::new();
    for raw in periods {
        let Some(period) = period(raw) else { continue };
        // One row per period; the first wins.
        if kept.iter().all(|p| p.period != period.period) {
            kept.push(period);
        }
    }
    Ok(CloudCpaoV1 {
        schema: CPAO_SCHEMA.to_string(),
        periods: kept,
    })
}

/// A wire period as sent; required members typed, optional ones loose.
#[derive(Deserialize)]
struct WirePeriod {
    period: String,
    cost_usd_micros: u64,
    accepted_outcomes: u64,
    #[serde(default)]
    cpao_usd_micros: Option<u64>,
    #[serde(default)]
    trend_pct: Option<Value>,
    #[serde(default)]
    settled: Option<Value>,
    #[serde(default)]
    by_agent: Option<Value>,
}

fn period(raw: &Value) -> Option<CpaoPeriodV1> {
    let wire: WirePeriod = serde_json::from_value(raw.clone()).ok()?;
    if !PERIODS.contains(&wire.period.as_str()) {
        return None;
    }
    // A cost per outcome without any outcome is a broken row.
    if wire.accepted_outcomes == 0 && wire.cpao_usd_micros.is_some() {
        return None;
    }
    let trend_pct = match wire.trend_pct {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_f64().filter(|t| t.is_finite())?),
    };
    let by_agent = match wire.by_agent {
        None | Some(Value::Null) => None,
        Some(Value::Array(rows)) => {
            let mut agents: Vec<CpaoAgentV1> = Vec::new();
            for row in &rows {
                let Some(agent) = agent(row) else { continue };
                if agents.len() < MAX_AGENTS && agents.iter().all(|a| a.agent != agent.agent) {
                    agents.push(agent);
                }
            }
            Some(agents)
        }
        Some(_) => return None,
    };
    Some(CpaoPeriodV1 {
        period: wire.period,
        cost_usd_micros: wire.cost_usd_micros,
        accepted_outcomes: wire.accepted_outcomes,
        cpao_usd_micros: wire.cpao_usd_micros,
        trend_pct,
        // An older cloud sends no `settled`; anything but a bool is unknown.
        settled: wire.settled.and_then(|v| v.as_bool()),
        by_agent,
    })
}

fn agent(raw: &Value) -> Option<CpaoAgentV1> {
    let agent: CpaoAgentV1 = serde_json::from_value(raw.clone()).ok()?;
    let name_ok = !agent.agent.is_empty()
        && agent.agent.len() <= MAX_AGENT_LEN
        && agent.agent.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
        });
    name_ok.then_some(agent)
}

fn stamp(at: UtcDateTime) -> String {
    at.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true)
}

/// The cache of `server`; another server's starts empty.
fn load_for(store: &CloudStore, server: &str) -> Cache {
    let cache: Cache = std::fs::read(store.data_dir().join(CPAO_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if cache.server == server {
        cache
    } else {
        Cache {
            server: server.to_string(),
            ..Cache::default()
        }
    }
}

fn save(store: &CloudStore, cache: &Cache) {
    if let Ok(bytes) = serde_json::to_vec_pretty(cache) {
        let _ = write_private(store.data_dir(), CPAO_FILE, &bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answer() -> Value {
        json!({
            "schema": CPAO_SCHEMA,
            "periods": [
                {"period": "7d", "cost_usd_micros": 12_500_000u64, "accepted_outcomes": 5,
                 "cpao_usd_micros": 2_500_000u64, "trend_pct": -12.5,
                 "by_agent": [
                     {"agent": "claude", "cost_usd_micros": 10_000_000u64, "accepted_outcomes": 4},
                     {"agent": "codex", "cost_usd_micros": 2_500_000u64, "accepted_outcomes": 1}
                 ]},
                {"period": "30d", "cost_usd_micros": 0, "accepted_outcomes": 0,
                 "cpao_usd_micros": null}
            ]
        })
    }

    #[test]
    fn the_answer_is_passed_on_as_the_cloud_computed_it() {
        let cpao = classify(200, &answer()).expect("ok").expect("some");
        assert_eq!(cpao.schema, CPAO_SCHEMA);
        assert_eq!(cpao.periods.len(), 2);
        let week = &cpao.periods[0];
        assert_eq!(week.period, "7d");
        assert_eq!(week.cpao_usd_micros, Some(2_500_000));
        assert_eq!(week.trend_pct, Some(-12.5));
        let agents = week.by_agent.as_ref().expect("agents");
        assert_eq!(agents[0].agent, "claude");
        assert_eq!(agents[0].cost_usd_micros, Some(10_000_000));
        assert_eq!(agents[1].accepted_outcomes, 1);
        let month = serde_json::to_value(&cpao.periods[1]).expect("json");
        // no outcome: the cost per outcome stays null, nothing is derived
        assert!(month["cpao_usd_micros"].is_null());
        assert!(month.get("trend_pct").is_none());
        assert!(month.get("by_agent").is_none());
    }

    /// LeanCTX Cloud's answer as it sends it (p37 !150): it does not price per
    /// agent yet, so every agent row carries `cost_usd_micros: null`.
    fn cloud_fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../lean-ctx-protocol/tests/fixtures/cloud/cpao.response.json"
        ))
        .expect("fixture")
    }

    #[test]
    fn the_cloud_answer_keeps_agent_rows_without_a_cost() {
        let cpao = classify(200, &cloud_fixture()).expect("ok").expect("some");
        assert_eq!(cpao.periods.len(), 2);
        for period in &cpao.periods {
            let agents = period.by_agent.as_ref().expect("agents");
            assert!(!agents.is_empty(), "{}: agent rows kept", period.period);
            assert!(agents.iter().all(|a| a.cost_usd_micros.is_none()));
        }
        let week = &cpao.periods[0];
        assert_eq!(week.cost_usd_micros, 42_000_000);
        assert_eq!(week.cpao_usd_micros, Some(3_500_000));
        let agents = week.by_agent.as_ref().expect("agents");
        assert_eq!(
            agents.iter().map(|a| a.agent.as_str()).collect::<Vec<_>>(),
            ["claude", "codex", "unknown"]
        );
        assert_eq!(agents[0].accepted_outcomes, 7);
        // passed on as null, never as 0
        let json = serde_json::to_value(week).expect("json");
        assert!(json["by_agent"][0]["cost_usd_micros"].is_null());
        assert_eq!(cpao.periods[1].trend_pct, None);
        assert_eq!(week.settled, Some(false));
    }

    #[test]
    fn settled_is_passed_on_true_false_or_missing() {
        let with = |settled: Value| {
            let mut raw = answer();
            match settled {
                Value::Null => {}
                v => raw["periods"][0]["settled"] = v,
            }
            let cpao = decode(&raw).expect("decoded");
            assert_eq!(cpao.periods.len(), 2, "the period is kept");
            let json = serde_json::to_value(&cpao.periods[0]).expect("json");
            (cpao.periods[0].settled, json.get("settled").cloned())
        };
        assert_eq!(with(json!(true)), (Some(true), Some(json!(true))));
        assert_eq!(with(json!(false)), (Some(false), Some(json!(false))));
        // an older cloud does not send it: unknown, left out of the JSON
        assert_eq!(with(Value::Null), (None, None));
        // not a bool: unknown, the period still counts
        assert_eq!(with(json!("yes")), (None, None));
        // a cache written before `settled` existed still reads
        let old: CpaoPeriodV1 = serde_json::from_value(json!({
            "period": "7d", "cost_usd_micros": 1, "accepted_outcomes": 1,
            "cpao_usd_micros": 1
        }))
        .expect("old cache");
        assert_eq!(old.settled, None);
    }

    #[test]
    fn malformed_periods_and_agents_are_left_out() {
        let mut raw = answer();
        raw["later"] = json!(1);
        let periods = raw["periods"].as_array_mut().expect("list");
        periods[0]["by_agent"]
            .as_array_mut()
            .expect("agents")
            .extend([
                json!({"agent": "Bad Name", "cost_usd_micros": 1, "accepted_outcomes": 1}),
                json!({"agent": "cursor", "cost_usd_micros": -1, "accepted_outcomes": 1}),
                json!({"agent": "claude", "cost_usd_micros": 9, "accepted_outcomes": 9}),
                json!("cursor"),
            ]);
        periods.extend([
            json!({"period": "90d", "cost_usd_micros": 1, "accepted_outcomes": 1}),
            json!({"period": "7d", "cost_usd_micros": 1, "accepted_outcomes": 1}),
            json!({"period": "30d", "cost_usd_micros": "1", "accepted_outcomes": 1}),
            json!({"period": "30d", "cost_usd_micros": 1, "accepted_outcomes": 0,
                   "cpao_usd_micros": 5}),
            json!({"period": "30d", "cost_usd_micros": 1, "accepted_outcomes": 1,
                   "trend_pct": "up"}),
            json!(null),
        ]);
        let cpao = decode(&raw).expect("decoded");
        assert_eq!(cpao, decode(&answer()).expect("clean"));
    }

    #[test]
    fn another_schema_or_no_period_list_is_refused() {
        let mut raw = answer();
        raw["schema"] = "leanctx.cloud.cpao/v2".into();
        assert!(classify(200, &raw).is_err());
        assert!(classify(200, &json!({"schema": CPAO_SCHEMA})).is_err());
    }

    #[test]
    fn a_cloud_without_cpao_is_not_available_and_a_refusal_is_forbidden() {
        let err = |code: &str| json!({"error": {"code": code}});
        assert_eq!(classify(503, &err("not_configured")).expect("off"), None);
        assert_eq!(classify(404, &Value::Null).expect("older cloud"), None);
        assert!(matches!(
            classify(403, &err("forbidden")),
            Err(CloudError::Forbidden)
        ));
        assert!(matches!(
            classify(403, &Value::Null),
            Err(CloudError::Forbidden)
        ));
        // a real outage is not "not configured"
        assert!(classify(503, &Value::Null).is_err());
        assert!(classify(500, &err("internal")).is_err());
    }

    #[test]
    fn only_a_team_role_refusal_is_a_role_refusal() {
        // what Cloud answers when the role may not see the team's costs
        let role = json!({"error": {"code": "forbidden", "message": "this needs the admin role", "required_role": "admin"}});
        assert_eq!(Refusal::of(&role), Refusal::Role);
        // a company-reserved action names another role: its own words stay
        let company = json!({"error": {"code": "forbidden", "message": "only a Company admin does this", "required_role": "company_admin"}});
        assert_eq!(
            Refusal::of(&company),
            Refusal::Other(Some("only a Company admin does this".into()))
        );
        // another code keeps the server's message, made safe for a terminal
        let other =
            json!({"error": {"code": "sso_required", "message": "sign in\u{1b}[31m with SSO"}});
        assert_eq!(
            Refusal::of(&other),
            Refusal::Other(Some("sign in [31m with SSO".into()))
        );
        // no role named, no body: no reason is made up
        let bare = json!({"error": {"code": "forbidden"}});
        assert_eq!(Refusal::of(&bare), Refusal::Other(None));
        assert_eq!(Refusal::of(&Value::Null), Refusal::Other(None));
    }

    #[test]
    fn a_person_without_a_team_reads_as_not_available_not_as_an_error() {
        // local-read-v1 §31: no team → Cloud answers 503 not_configured → available:false.
        let no_team = json!({"error": {"code": "not_configured", "message": "no team"}});
        assert_eq!(classify(503, &no_team).expect("not available"), None);
    }

    #[test]
    fn at_most_64_agent_rows_per_period_are_kept() {
        let rows: Vec<Value> = (0..100)
            .map(
                |i| json!({"agent": format!("a{i}"), "cost_usd_micros": 1, "accepted_outcomes": 1}),
            )
            .collect();
        let raw = json!({"schema": CPAO_SCHEMA, "periods": [
            {"period": "7d", "cost_usd_micros": 100, "accepted_outcomes": 100,
             "cpao_usd_micros": 1, "by_agent": rows}
        ]});
        let cpao = decode(&raw).expect("decoded");
        let agents = cpao.periods[0].by_agent.as_ref().expect("agents");
        assert_eq!(agents.len(), MAX_AGENTS);
        assert_eq!(agents[0].agent, "a0");
        assert_eq!(agents[MAX_AGENTS - 1].agent, "a63");
    }

    #[test]
    fn an_older_server_gets_no_request() {
        let dir = tempfile::tempdir().expect("dir");
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("cloud"));
        let now = UtcDateTime::parse_from_rfc3339("2026-11-18T12:00:00Z").expect("time");
        assert_eq!(
            read(&store, &CloudConfig::default(), now).expect("read"),
            None
        );
        assert!(!store.data_dir().join(CPAO_FILE).exists());
    }
}
