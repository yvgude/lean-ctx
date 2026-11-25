// SPDX-License-Identifier: Apache-2.0
//! Team billing (3.12.3): a Stripe checkout or portal link for LeanCTX Code,
//! which holds no Cloud token, as Local Read kinds, and the month's model
//! credit in agent-hours (mz16).
//!
//! Each kind is one call of the Cloud's engine billing routes
//! (`/v1/billing/…`, Cloud API §17.2). The engine calls them with its
//! instance token and the Cloud acts as the person the instance is linked
//! to, in the engine's own team, with that person's role (owner or admin),
//! two-factor rule and rate budgets. The document carries only the Stripe
//! link the Cloud answered, checked to be Stripe's; nothing else of the
//! answer reaches the reader. The engine never talks to Stripe itself.
//!
//! | kind | local route | Cloud route |
//! |---|---|---|
//! | `cloud.billing.checkout` | `POST /v1/local/cloud/billing/checkout` | `POST /v1/billing/checkout` |
//! | `cloud.billing.portal` | `POST /v1/local/cloud/billing/portal` | `POST /v1/billing/portal` |
//! | `cloud.billing.credit` | `GET /v1/local/cloud/billing/credit` | `GET /v1/billing` |
//!
//! `cloud.billing.credit` is a read: of the Cloud's billing document it keeps
//! only the plan, whether a company pays, the model credit (`usage.model_credit`,
//! in agent-hours and USD micros), the credit pack on sale and whether this
//! person may buy it (`manage_billing`). Buying more happens on the Cloud's
//! billing page in the browser, never here.
//!
//! CLI: `lean-ctx cloud billing checkout|portal|credit --json …` prints the
//! same document. Wired as one `local_read::Part`.

use serde_json::{Map, Value, json};

use lean_ctx_cloud::transport::CloudClient;
use lean_ctx_cloud::{CloudError, CloudStore};

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

/// The release tool of the billing documents (a person's act, like the bus).
const TOOL: &str = "ctx_agent";

/// The largest Cloud answer read (one link).
const MAX_ANSWER_BYTES: usize = 64 * 1024;

pub(crate) const CHECKOUT: &str = "cloud.billing.checkout";
pub(crate) const PORTAL: &str = "cloud.billing.portal";
pub(crate) const CREDIT: &str = "cloud.billing.credit";

/// The release tool of the credit document (a read of the team's billing).
const READ_TOOL: &str = "ctx_knowledge";

/// The hosts a billing link may name.
const STRIPE_HOSTS: &[&str] = &["checkout.stripe.com", "billing.stripe.com"];

const fn value(name: &'static str, flag: &'static str) -> Param {
    Param {
        name,
        flag,
        shape: Shape::Value,
    }
}

/// This part's routes (Route as in `routes.rs`). No project: billing is the
/// team's, not a repository's.
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: CHECKOUT,
        method: Method::Post,
        path: "/v1/local/cloud/billing/checkout",
        cli: &["cloud", "billing", "checkout", "--json"],
        action: &["checkout"],
        params: &[
            value("plan", "--plan"),
            value("interval", "--interval"),
            value("currency", "--currency"),
            value("seats", "--seats"),
        ],
        project: Project::None,
        names: None,
    },
    Route {
        kind: PORTAL,
        method: Method::Post,
        path: "/v1/local/cloud/billing/portal",
        cli: &["cloud", "billing", "portal", "--json"],
        action: &["portal"],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: CREDIT,
        method: Method::Get,
        path: "/v1/local/cloud/billing/credit",
        cli: &["cloud", "billing", "credit", "--json"],
        action: &["credit"],
        params: &[],
        project: Project::None,
        names: None,
    },
];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[CHECKOUT, PORTAL, CREDIT];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] =
    &[("cloud.billing", &["checkout", "credit", "portal"])];

/// One call of the Cloud's engine billing routes (`path` below `/v1`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Call {
    pub path: &'static str,
    pub body: Value,
    /// The caller's `Idempotency-Key` header (top-up only, §33); `None`
    /// sends none.
    pub idempotency_key: Option<String>,
}

/// The kind of a CLI verb (`portal` → `cloud.billing.portal`).
fn kind_of(verb: &str) -> Option<&'static str> {
    KINDS
        .iter()
        .copied()
        .find(|kind| kind.strip_prefix("cloud.billing.") == Some(verb))
}

/// The Cloud call of `kind` for its arguments (the verb first, then the
/// flags). Nothing is sent here. The checkout's `currency` is the app's and
/// only `usd` is sold; the Cloud takes `plan`, `interval` and `seats` only.
/// The seat range is the Cloud's: only a positive whole number is checked here.
pub(crate) fn call_of(kind: &str, args: &[String]) -> Result<Call, Failure> {
    match kind {
        CHECKOUT => {
            let plan = super::flag(args, "--plan")?
                .ok_or_else(|| Failure::invalid("plan is required: personal or team"))?;
            if !matches!(plan, "personal" | "team") {
                return Err(Failure::invalid("plan is personal or team"));
            }
            let interval = super::flag(args, "--interval")?
                .ok_or_else(|| Failure::invalid("interval is required: monthly or yearly"))?;
            if !matches!(interval, "monthly" | "yearly") {
                return Err(Failure::invalid("interval is monthly or yearly"));
            }
            if let Some(currency) = super::flag(args, "--currency")? {
                if currency != "usd" {
                    return Err(Failure::invalid("currency is usd"));
                }
            }
            let mut body = Map::new();
            body.insert("plan".into(), Value::String(plan.to_owned()));
            body.insert("interval".into(), Value::String(interval.to_owned()));
            if let Some(raw) = super::flag(args, "--seats")? {
                let seats = raw
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| Failure::invalid("seats is a positive whole number"))?;
                if plan != "team" {
                    return Err(Failure::invalid("seats are only for team"));
                }
                body.insert("seats".into(), Value::from(seats));
            }
            Ok(Call {
                path: "/billing/checkout",
                body: Value::Object(body),
                idempotency_key: None,
            })
        }
        PORTAL => Ok(Call {
            path: "/billing/portal",
            body: json!({}),
            idempotency_key: None,
        }),
        other => Err(Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// Send `call` with `client`: the Cloud's status and JSON body. A `401` is
/// an error, so the session mints a new token once and then reports it.
/// The call's `Idempotency-Key`, when it has one, goes with it.
pub(crate) fn exchange(client: &CloudClient, call: &Call) -> Result<(u16, Value), CloudError> {
    let extra: Vec<(&str, &str)> = call
        .idempotency_key
        .as_deref()
        .map(|key| (lean_ctx_protocol::cloud::HEADER_IDEMPOTENCY_KEY, key))
        .into_iter()
        .collect();
    let (status, body) = client.send_answer_with(
        "POST",
        call.path,
        Some(&call.body),
        &extra,
        MAX_ANSWER_BYTES,
    )?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, body))
}

/// Whether `url` is an `https` link on one of Stripe's billing hosts (no
/// user part, no port, no whitespace or control characters).
pub(crate) fn stripe_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if url.len() > 4096 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    STRIPE_HOSTS.contains(&&rest[..end])
}

/// The Cloud's words, safe for a terminal: no control characters, at most
/// 300 characters; `None` when nothing is left.
pub(crate) fn safe_message(message: &str) -> Option<String> {
    let text: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `error.reason` of a refusal for a team billed through its company.
pub(crate) const COMPANY_BILLED: &str = "company_billed";

/// The document body of a Cloud answer (`{url}` only), or its refusal in
/// the app's codes. The Cloud names a billing state in `error.state`
/// (`invalid_state`) and a step-up in `error.reason`; either counts.
pub(crate) fn outcome(status: u16, body: &Value) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        return match body["url"].as_str() {
            Some(url) if stripe_url(url) => Ok(json!({ "url": url })),
            _ => Err(Failure::new(
                Code::Internal,
                "LeanCTX Cloud answered without a Stripe link",
            )),
        };
    }
    let error = &body["error"];
    let names = |word: &str| {
        ["code", "state", "reason"]
            .iter()
            .any(|key| error[*key].as_str() == Some(word))
    };
    let said = || error["message"].as_str().and_then(safe_message);
    let (code, message) = if names("subscribed") {
        (
            Code::Conflict,
            "already subscribed, open the portal".to_owned(),
        )
    } else if names("no_customer") {
        (
            Code::NotFound,
            "this team has no subscription yet, start one with checkout".to_owned(),
        )
    } else if names(COMPANY_BILLED) {
        (Code::Forbidden, "billed through your company".to_owned())
    } else if names("not_configured") {
        (
            Code::Unavailable,
            "billing is not configured on LeanCTX Cloud".to_owned(),
        )
    } else if names("upstream") {
        (
            Code::Unavailable,
            "the payment provider did not answer, retry later".to_owned(),
        )
    } else if names("sso_required") {
        // Before the step-up: an SSO step-up names `sso_required` as its reason.
        // Its own code, so the app offers the SSO sign-in instead of guessing
        // from the words; the message stays for readers that only know
        // `forbidden`.
        (
            Code::SsoRequired,
            "sign in with SSO in the browser".to_owned(),
        )
    } else if names("totp_required") || names("step_up_required") || names("totp_not_enrolled") {
        (
            Code::Forbidden,
            "confirm with two-factor in the browser".to_owned(),
        )
    } else if names("invalid_field") || status == 400 || status == 422 {
        (
            Code::InvalidArgument,
            said().unwrap_or_else(|| "LeanCTX Cloud refused a field of the request".to_owned()),
        )
    } else if names("forbidden") || names("plan_required") || status == 403 || status == 402 {
        (
            Code::Forbidden,
            said().unwrap_or_else(|| {
                "only an owner or admin of the team manages its billing".to_owned()
            }),
        )
    } else if names("invalid_state") || status == 409 {
        (
            Code::Conflict,
            said().unwrap_or_else(|| "the team's billing does not allow this now".to_owned()),
        )
    } else if names("not_found") || status == 404 {
        (
            Code::NotFound,
            said().unwrap_or_else(|| "LeanCTX Cloud does not know this team".to_owned()),
        )
    } else if status == 429 {
        (
            Code::RateLimited,
            "LeanCTX Cloud asks to retry later".to_owned(),
        )
    } else if status >= 500 {
        (
            Code::Unavailable,
            format!("LeanCTX Cloud could not answer ({status}), retry later"),
        )
    } else {
        (Code::Internal, format!("LeanCTX Cloud answered {status}"))
    };
    let failure = Failure::new(code, message);
    // `forbidden` alone does not say why; a company-billed team gets its
    // own reason so the app points to the company instead of the role.
    Err(if code == Code::Forbidden && names(COMPANY_BILLED) {
        failure.with_reason(COMPANY_BILLED)
    } else {
        failure
    })
}

/// Read the Cloud's billing document with `client` (`GET /v1/billing`). A
/// `401` is an error, so the session mints a new token once and then
/// reports it.
pub(crate) fn fetch_credit(client: &CloudClient) -> Result<(u16, Value), CloudError> {
    let (status, body) = client.send_answer("GET", "/billing", None, MAX_ANSWER_BYTES)?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, body))
}

/// The model credit's whole USD-micro fields (`usage.model_credit`).
const CREDIT_MICROS: &[&str] = &[
    "used_usd_micros",
    "limit_usd_micros",
    "allowance_usd_micros",
    "topup_available_usd_micros",
    "left_usd_micros",
    "agent_hour_usd_micros",
];

/// The model credit's agent-hour fields (one decimal, rounded down by the
/// Cloud).
const CREDIT_HOURS: &[&str] = &["used_agent_hours", "limit_agent_hours", "left_agent_hours"];

fn unreadable() -> Failure {
    Failure::new(
        Code::Internal,
        "LeanCTX Cloud answered an unreadable model credit",
    )
}

fn hours(value: &Value) -> Result<Value, Failure> {
    value
        .as_f64()
        .filter(|h| h.is_finite() && *h >= 0.0)
        .map(Value::from)
        .ok_or_else(unreadable)
}

/// The model credit as the app shows it, from the Cloud's
/// `usage.model_credit`. The Cloud reports it on every plan, so a missing or
/// `null` credit is unreadable, never guessed.
fn credit_of(raw: &Value) -> Result<Value, Failure> {
    let mut out = Map::new();
    for field in CREDIT_MICROS {
        let micros = raw[*field].as_i64().ok_or_else(unreadable)?;
        out.insert((*field).to_owned(), Value::from(micros));
    }
    for field in CREDIT_HOURS {
        out.insert((*field).to_owned(), hours(&raw[*field])?);
    }
    let state = raw["state"].as_str().ok_or_else(unreadable)?;
    if !matches!(state, "ok" | "soft" | "hard") {
        return Err(unreadable());
    }
    out.insert("state".into(), Value::String(state.to_owned()));
    let reset = raw["reset_at"]
        .as_str()
        .filter(|t| t.len() <= 64 && t.chars().all(|c| c.is_ascii_graphic()))
        .ok_or_else(unreadable)?;
    out.insert("reset_at".into(), Value::String(reset.to_owned()));
    out.insert("used_pct".into(), used_pct(&raw["used_pct"])?);
    Ok(Value::Object(out))
}

/// rc4-96: the share of this month's usage used (allowance plus packs
/// bought), a whole percent `0..=100` rounded down by the Cloud; `null` from a Cloud that sends none (the app
/// then shows no percentage, never one it computes itself).
fn used_pct(value: &Value) -> Result<Value, Failure> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    value
        .as_u64()
        .filter(|pct| *pct <= 100)
        .map(Value::from)
        .ok_or_else(unreadable)
}

/// The credit pack on sale (`credit_topup`), `null` when none is sold.
fn pack_of(raw: &Value) -> Result<Value, Failure> {
    if raw.is_null() {
        return Ok(Value::Null);
    }
    let whole = |field: &str| {
        raw[field]
            .as_i64()
            .filter(|n| *n > 0)
            .map(Value::from)
            .ok_or_else(unreadable)
    };
    Ok(json!({
        "pack_usd_micros": whole("pack_usd_micros")?,
        "pack_agent_hours": hours(&raw["pack_agent_hours"])?,
        "max_packs": whole("max_packs")?,
    }))
}

/// The credit document of a Cloud answer to `GET /v1/billing`, or its
/// refusal. Only the plan, whether a company pays, the model credit, the
/// pack on sale and whether this person may buy (`manage_billing`) reach the
/// reader; seats, prices and the subscription stay in the Cloud.
/// `manage_billing` is true only when the Cloud says `true` (an owner of a
/// team not billed through a company); a Cloud from before the field reads
/// as false, so the app offers no purchase it cannot make. A Cloud without
/// the engine route (`404`) is `unavailable`: the app opens the billing page
/// in the browser instead.
pub(crate) fn credit_outcome(status: u16, body: &Value) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        let plan = body["plan"]
            .as_str()
            .filter(|p| p.len() <= 32 && p.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            .ok_or_else(unreadable)?;
        return Ok(json!({
            "plan": plan,
            "company_billed": body["company"].is_object(),
            "model_credit": credit_of(&body["usage"]["model_credit"])?,
            "credit_topup": pack_of(&body["credit_topup"])?,
            "manage_billing": body["manage_billing"].as_bool() == Some(true),
        }));
    }
    if status == 404 {
        return Err(Failure::new(
            Code::Unavailable,
            "this LeanCTX Cloud does not show the model credit to the app yet, open billing in the browser",
        ));
    }
    if status == 403 && body["error"]["code"].as_str() == Some("forbidden") {
        return Err(Failure::new(
            Code::Forbidden,
            "only an owner or admin of the team sees its model credit",
        ));
    }
    outcome(status, body)
}

/// Make one Cloud exchange as the connected instance.
pub(crate) fn with_cloud(
    send: &dyn Fn(&CloudClient) -> Result<(u16, Value), CloudError>,
) -> Result<(u16, Value), Failure> {
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = lean_ctx_cloud::auth::Session::load(store, trust)?;
    Ok(session.with_client(|client| send(client))?)
}

/// Read the team's model credit as the connected instance.
fn read_credit() -> Result<Value, Failure> {
    let (status, body) = with_cloud(&fetch_credit)?;
    credit_outcome(status, &body)
}

/// Make `call` as the connected instance.
fn run(call: &Call) -> Result<Value, Failure> {
    let (status, body) = with_cloud(&|client| exchange(client, call))?;
    outcome(status, &body)
}

/// The released document of `kind`, for the CLI or the socket. A write goes
/// through the release point before anything is sent.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    if kind == CREDIT {
        return super::answer_released(READ_TOOL, "", kind, via, read_credit);
    }
    let call = match call_of(kind, args) {
        Ok(call) => call,
        Err(failure) => return Answer::failure(&failure),
    };
    super::answer_released_write(TOOL, "", kind, via, &call.body, || run(&call))
}

/// `lean-ctx cloud billing checkout|portal|credit --json …`: the Local Read
/// document; `false` for every other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    if command != "cloud"
        || rest.first().map(String::as_str) != Some("billing")
        || !super::wants_json(rest)
    {
        return false;
    }
    let args = &rest[1..];
    let verb = args.first().map(String::as_str).unwrap_or_default();
    let answer = match kind_of(verb) {
        Some(kind) => answer(kind, args, Via::Cli),
        None => Answer::failure(&Failure::invalid(format!(
            "unknown billing command {verb:?}: checkout, portal, credit or topup"
        ))),
    };
    super::print(&answer);
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{Read, Write};

    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    /// One HTTP exchange on a loopback port: the request as text, and the
    /// answer `status` with `body` (as LeanCTX Cloud sends it).
    pub(crate) fn serve_once(status: u16, body: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let answer = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nCloud-Protocol: 2.2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });
        (origin, handle)
    }

    #[test]
    fn billing_kinds_are_dispatched_and_routed_without_a_project() {
        let text = super::super::routes::table_json().to_string();
        for kind in KINDS {
            assert!(super::super::part_of(kind).is_some(), "{kind} has no part");
            let route = super::super::routes::all()
                .find(|r| r.kind == *kind)
                .unwrap_or_else(|| panic!("{kind} is not routed"));
            assert!(
                route.path.starts_with("/v1/local/cloud/billing/"),
                "{}",
                route.path
            );
            let read = *kind == CREDIT;
            assert_eq!(
                route.method,
                if read { Method::Get } else { Method::Post },
                "{kind}"
            );
            assert_eq!(route.project, Project::None);
            assert!(text.contains(route.path), "{} is listed", route.path);
        }
    }

    #[test]
    fn the_apps_checkout_body_becomes_the_clouds_without_currency() {
        let route = super::super::routes::all()
            .find(|r| r.kind == CHECKOUT)
            .unwrap();
        // As p43 billing.rs `checkout_body` sends it.
        let inputs: Vec<(String, Value)> =
            json!({"plan": "team", "interval": "yearly", "currency": "usd", "seats": 5})
                .as_object()
                .unwrap()
                .clone()
                .into_iter()
                .collect();
        let args = super::super::routes::args_of(route, None, &inputs).unwrap();
        assert_eq!(
            call_of(CHECKOUT, &args).unwrap(),
            Call {
                path: "/billing/checkout",
                body: json!({"plan": "team", "interval": "yearly", "seats": 5}),
                idempotency_key: None,
            }
        );
        let personal = call_of(
            CHECKOUT,
            &words(&[
                "checkout",
                "--json",
                "--plan",
                "personal",
                "--interval",
                "monthly",
            ]),
        )
        .unwrap();
        assert_eq!(
            personal.body,
            json!({"plan": "personal", "interval": "monthly"})
        );
        let portal = super::super::routes::all()
            .find(|r| r.kind == PORTAL)
            .unwrap();
        let args = super::super::routes::args_of(portal, None, &[]).unwrap();
        assert_eq!(
            call_of(PORTAL, &args).unwrap(),
            Call {
                path: "/billing/portal",
                body: json!({}),
                idempotency_key: None,
            }
        );
    }

    #[test]
    fn the_seat_range_is_left_to_the_cloud() {
        let team = |seats: &str| {
            call_of(
                CHECKOUT,
                &words(&[
                    "checkout",
                    "--json",
                    "--plan",
                    "team",
                    "--interval",
                    "monthly",
                    "--seats",
                    seats,
                ]),
            )
        };
        // Team starts at two seats; the Cloud, not the engine, holds the range.
        assert_eq!(
            team("2").unwrap().body,
            json!({"plan": "team", "interval": "monthly", "seats": 2})
        );
        assert_eq!(team("26").unwrap().body["seats"], json!(26));
        assert_eq!(team("0").expect_err("no seats").code, Code::InvalidArgument);
    }

    #[test]
    fn bad_checkouts_are_refused_before_anything_is_sent() {
        let refused = |w: &[&str]| call_of(CHECKOUT, &words(w)).expect_err("refused").code;
        let base = [
            "checkout",
            "--json",
            "--plan",
            "team",
            "--interval",
            "monthly",
        ];
        let with = |extra: &[&str]| {
            let mut w = base.to_vec();
            w.extend_from_slice(extra);
            refused(&w)
        };
        assert_eq!(
            refused(&["checkout", "--json", "--interval", "monthly"]),
            Code::InvalidArgument
        );
        assert_eq!(
            refused(&[
                "checkout",
                "--json",
                "--plan",
                "enterprise",
                "--interval",
                "monthly"
            ]),
            Code::InvalidArgument
        );
        assert_eq!(
            refused(&[
                "checkout",
                "--json",
                "--plan",
                "team",
                "--interval",
                "weekly"
            ]),
            Code::InvalidArgument
        );
        assert_eq!(with(&["--currency", "eur"]), Code::InvalidArgument);
        assert_eq!(with(&["--seats", "0"]), Code::InvalidArgument);
        assert_eq!(with(&["--seats", "-1"]), Code::InvalidArgument);
        assert_eq!(with(&["--seats", "five"]), Code::InvalidArgument);
        assert_eq!(
            refused(&[
                "checkout",
                "--json",
                "--plan",
                "personal",
                "--interval",
                "monthly",
                "--seats",
                "3"
            ]),
            Code::InvalidArgument
        );
    }

    #[test]
    fn only_a_stripe_link_is_the_document() {
        assert!(stripe_url(
            "https://checkout.stripe.com/c/pay/cs_test_a1#fid"
        ));
        assert!(stripe_url("https://billing.stripe.com/p/session/test_1"));
        for bad in [
            "http://checkout.stripe.com/c/pay/x",
            "https://checkout.stripe.com.evil.example/x",
            "https://evil.example/?checkout.stripe.com",
            "https://user@checkout.stripe.com/x",
            "https://checkout.stripe.com:8443/x",
            "https://stripe.com/x",
            "https://billing.stripe.com/p x",
            "javascript:alert(1)",
        ] {
            assert!(!stripe_url(bad), "{bad}");
        }
        let doc = outcome(
            200,
            &json!({"url": "https://checkout.stripe.com/c/pay/cs_1", "session": "secret", "token": "t"}),
        )
        .unwrap();
        assert_eq!(
            doc,
            json!({"url": "https://checkout.stripe.com/c/pay/cs_1"})
        );
        let foreign = outcome(200, &json!({"url": "https://pay.example/x"})).expect_err("foreign");
        assert_eq!(foreign.code, Code::Internal);
        assert_eq!(
            outcome(200, &json!({})).expect_err("none").code,
            Code::Internal
        );
    }

    #[test]
    fn cloud_refusals_take_the_leads_codes_and_words() {
        let refused = |status: u16, error: Value| {
            outcome(status, &json!({ "error": error })).expect_err("refused")
        };
        let check = |status: u16, error: Value, code: Code, message: &str| {
            let failure = refused(status, error);
            assert_eq!((failure.code, failure.message.as_str()), (code, message));
        };
        check(
            409,
            json!({"code": "invalid_state", "message": "x", "state": "subscribed"}),
            Code::Conflict,
            "already subscribed, open the portal",
        );
        check(
            409,
            json!({"code": "invalid_state", "message": "x", "state": "no_customer"}),
            Code::NotFound,
            "this team has no subscription yet, start one with checkout",
        );
        check(
            409,
            json!({"code": "invalid_state", "message": "x", "state": "company_billed", "company": "acme"}),
            Code::Forbidden,
            "billed through your company",
        );
        let billed = refused(
            409,
            json!({"code": "invalid_state", "message": "x", "state": "company_billed", "company": "acme"}),
        );
        let doc: Value =
            serde_json::from_str(&super::super::error_document(&billed)).expect("json");
        assert_eq!(doc["error"]["reason"], "company_billed");
        assert_eq!(doc["error"]["code"], "forbidden");
        let plain = refused(403, json!({"code": "forbidden", "message": "x"}));
        assert_eq!(plain.code, Code::Forbidden);
        assert_eq!(plain.reason, None);
        let doc: Value = serde_json::from_str(&super::super::error_document(&plain)).expect("json");
        assert!(doc["error"].get("reason").is_none());
        check(
            503,
            json!({"code": "not_configured", "message": "x"}),
            Code::Unavailable,
            "billing is not configured on LeanCTX Cloud",
        );
        check(
            502,
            json!({"code": "upstream", "message": "x"}),
            Code::Unavailable,
            "the payment provider did not answer, retry later",
        );
        for error in [
            json!({"code": "totp_required", "message": "x"}),
            json!({"code": "step_up_required", "message": "x", "reason": "totp_not_enrolled"}),
        ] {
            check(
                403,
                error,
                Code::Forbidden,
                "confirm with two-factor in the browser",
            );
        }
        for error in [
            json!({"code": "sso_required", "message": "x"}),
            json!({"code": "step_up_required", "message": "x", "reason": "sso_required"}),
        ] {
            check(
                403,
                error,
                Code::SsoRequired,
                "sign in with SSO in the browser",
            );
        }
        check(
            422,
            json!({"code": "invalid_field", "field": "seats", "message": "Team has 2–25 seats"}),
            Code::InvalidArgument,
            "Team has 2–25 seats",
        );
        assert_eq!(
            refused(403, json!({"code": "forbidden"})).code,
            Code::Forbidden
        );
        assert_eq!(
            refused(429, json!({"code": "rate_limited"})).code,
            Code::RateLimited
        );
        assert_eq!(
            outcome(500, &json!("<html>")).expect_err("down").code,
            Code::Unavailable
        );
        let doc: Value = serde_json::from_str(&super::super::error_document(&refused(
            409,
            json!({"code": "invalid_state", "state": "subscribed"}),
        )))
        .unwrap();
        assert_eq!(doc["kind"], "error");
        assert_eq!(doc["error"]["code"], "conflict");
    }

    #[test]
    fn a_checkout_sends_the_clouds_body_and_keeps_only_the_link() {
        let (origin, server) = serve_once(
            200,
            r#"{"url":"https://checkout.stripe.com/c/pay/cs_test_1","customer":"cus_1"}"#,
        );
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of(
            CHECKOUT,
            &words(&[
                "checkout",
                "--json",
                "--plan",
                "team",
                "--interval",
                "monthly",
                "--currency",
                "usd",
                "--seats",
                "3",
            ]),
        )
        .unwrap();
        let (status, body) = exchange(&client, &call).expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/billing/checkout HTTP/1.1\r\n"),
            "{request}"
        );
        let sent: Value =
            serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(
            sent,
            json!({"plan": "team", "interval": "monthly", "seats": 3})
        );
        let doc: Value = serde_json::from_str(&super::super::document(
            CHECKOUT,
            &outcome(status, &body).unwrap(),
        ))
        .unwrap();
        assert_eq!(doc["contract"], "local-read/1");
        assert_eq!(doc["kind"], CHECKOUT);
        assert_eq!(doc["url"], "https://checkout.stripe.com/c/pay/cs_test_1");
        assert!(doc.get("customer").is_none());
    }

    #[test]
    fn a_refused_token_is_not_connected() {
        let (origin, server) =
            serve_once(401, r#"{"error":{"code":"unauthorized","message":"x"}}"#);
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of(PORTAL, &words(&["portal", "--json"])).unwrap();
        let error = exchange(&client, &call).expect_err("401");
        server.join().unwrap();
        assert_eq!(error, CloudError::Unauthorized);
        assert_eq!(Failure::from(error).code, Code::NotConnected);
    }

    #[test]
    fn without_a_connection_nothing_is_sent_and_the_answer_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let call = call_of(PORTAL, &words(&["portal", "--json"])).unwrap();
        assert_eq!(
            run(&call).expect_err("no instance").code,
            Code::NotConnected
        );
    }

    #[test]
    fn only_the_cloud_billing_json_command_line_is_taken() {
        assert!(
            !try_run("cloud", &words(&["billing", "portal"])),
            "needs --json"
        );
        assert!(!try_run("cloud", &words(&["chat", "channels", "--json"])));
        assert!(!try_run("billing", &words(&["portal", "--json"])));
        assert_eq!(kind_of("checkout"), Some(CHECKOUT));
        assert_eq!(kind_of("credit"), Some(CREDIT));
        assert_eq!(kind_of("seats"), None);
    }

    /// The Cloud's `GET /v1/billing` answer for an owner (p37 `billing_json`
    /// with `credit_topup`), trimmed to what matters here.
    fn cloud_billing() -> Value {
        json!({
            "company": null,
            "plan": "team",
            "status": "active",
            "interval": "monthly",
            "seats": 5,
            "unit_amount_cents": 4900,
            "currency": "usd",
            "portal_available": true,
            "instances": {"limit": 10, "used": 2},
            "usage": {
                "quotas": [{"resource": "storage", "used": 1}],
                "model_credit": {
                    "used_usd_micros": 2_500_000,
                    "limit_usd_micros": 10_000_000,
                    "allowance_usd_micros": 7_500_000,
                    "topup_available_usd_micros": 2_500_000,
                    "left_usd_micros": 7_500_000,
                    "state": "ok",
                    "reset_at": "2026-12-01T00:00:00Z",
                    "agent_hour_usd_micros": 250_000,
                    "used_agent_hours": 10.0,
                    "limit_agent_hours": 40.0,
                    "left_agent_hours": 30.0
                }
            },
            "trial": null,
            "read_only": false,
            "credit_topup": {"pack_usd_micros": 5_000_000, "pack_agent_hours": 20.0, "max_packs": 10},
            "manage_billing": true
        })
    }

    #[test]
    fn the_credit_document_keeps_only_the_credit_in_agent_hours() {
        let doc = credit_outcome(200, &cloud_billing()).expect("credit");
        assert_eq!(
            doc,
            json!({
                "plan": "team",
                "company_billed": false,
                "model_credit": {
                    "used_usd_micros": 2_500_000,
                    "limit_usd_micros": 10_000_000,
                    "allowance_usd_micros": 7_500_000,
                    "topup_available_usd_micros": 2_500_000,
                    "left_usd_micros": 7_500_000,
                    "agent_hour_usd_micros": 250_000,
                    "used_agent_hours": 10.0,
                    "limit_agent_hours": 40.0,
                    "left_agent_hours": 30.0,
                    "state": "ok",
                    "reset_at": "2026-12-01T00:00:00Z",
                    "used_pct": null
                },
                "credit_topup": {"pack_usd_micros": 5_000_000, "pack_agent_hours": 20.0, "max_packs": 10},
                "manage_billing": true
            })
        );
        let text = super::super::document(CREDIT, &doc);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], CREDIT);
        assert!(value.get("seats").is_none() && value.get("unit_amount_cents").is_none());

        // A company's team on a server that sells no pack: nobody buys here.
        let mut bare = cloud_billing();
        bare["credit_topup"] = Value::Null;
        bare["company"] = json!({"slug": "acme-co", "name": "Acme"});
        bare["manage_billing"] = json!(false);
        let doc = credit_outcome(200, &bare).expect("bare");
        assert_eq!(doc["credit_topup"], Value::Null);
        assert_eq!(doc["company_billed"], true);
        assert_eq!(doc["manage_billing"], false);
    }

    #[test]
    fn only_a_cloud_true_lets_the_person_buy() {
        // An admin (false), a Cloud from before the field (absent) and a
        // malformed value all read as false: the app offers no purchase.
        for raw in [json!(false), Value::Null, json!("true"), json!(1)] {
            let mut body = cloud_billing();
            body["manage_billing"] = raw.clone();
            let doc = credit_outcome(200, &body).expect("credit");
            assert_eq!(doc["manage_billing"], false, "{raw}");
        }
        let mut body = cloud_billing();
        body.as_object_mut().unwrap().remove("manage_billing");
        assert_eq!(
            credit_outcome(200, &body).expect("credit")["manage_billing"],
            false
        );
    }

    /// rc4-96: the Cloud's percentage passes as sent, 0 and 100 included.
    #[test]
    fn the_used_share_passes_as_the_cloud_sends_it() {
        for pct in [0, 25, 100] {
            let mut body = cloud_billing();
            body["usage"]["model_credit"]["used_pct"] = json!(pct);
            let doc = credit_outcome(200, &body).expect("credit");
            assert_eq!(doc["model_credit"]["used_pct"], json!(pct));
        }
    }

    #[test]
    fn an_unreadable_credit_is_refused_not_guessed() {
        for (pointer, bad) in [
            ("/usage/model_credit", Value::Null),
            ("/usage/model_credit/state", json!("empty")),
            ("/usage/model_credit/left_agent_hours", json!(-1.0)),
            ("/usage/model_credit/left_usd_micros", json!("7")),
            ("/usage/model_credit/reset_at", json!("2026-12-01\u{1b}[2J")),
            ("/usage/model_credit/used_pct", json!(101)),
            ("/usage/model_credit/used_pct", json!(-1)),
            ("/usage/model_credit/used_pct", json!(12.5)),
            ("/usage/model_credit/used_pct", json!("25")),
            ("/credit_topup/max_packs", json!(0)),
            ("/plan", json!("Team <b>")),
        ] {
            let mut body = cloud_billing();
            body["usage"]["model_credit"]["used_pct"] = json!(25);
            *body.pointer_mut(pointer).unwrap() = bad;
            let failure = credit_outcome(200, &body).expect_err(pointer);
            assert_eq!(failure.code, Code::Internal, "{pointer}");
        }
    }

    #[test]
    fn credit_refusals_say_what_the_app_does_next() {
        let refused =
            |status: u16, body: Value| credit_outcome(status, &body).expect_err("refused");
        let member = refused(403, json!({"error": {"code": "forbidden", "message": "x"}}));
        assert_eq!(
            (member.code, member.message.as_str()),
            (
                Code::Forbidden,
                "only an owner or admin of the team sees its model credit"
            )
        );
        let sso = refused(
            403,
            json!({"error": {"code": "sso_required", "message": "x"}}),
        );
        assert_eq!(
            (sso.code, sso.message.as_str()),
            (Code::SsoRequired, "sign in with SSO in the browser")
        );
        // A Cloud from before the engine route: open billing in the browser.
        let old = refused(404, json!({"error": {"code": "not_found", "message": "x"}}));
        assert_eq!(old.code, Code::Unavailable);
        assert!(old.message.contains("browser"), "{}", old.message);
        assert_eq!(refused(429, json!({})).code, Code::RateLimited);
        assert_eq!(refused(503, json!("<html>")).code, Code::Unavailable);
    }

    #[test]
    fn an_sso_team_reaches_the_reader_as_sso_required() {
        // p37 answers engine billing for a team that requires SSO with
        // `403 sso_required`; the app needs that code, not `forbidden`, and
        // keeps the words for readers that only know `forbidden`. p37's own
        // message (orgs.rs) never says "SSO", so the code has to carry it.
        let sso = json!({"error": {
            "code": "sso_required",
            "message": "this organization requires signing in with its single sign-on"
        }});
        for failure in [
            credit_outcome(403, &sso).expect_err("credit"),
            outcome(403, &sso).expect_err("checkout or portal"),
        ] {
            let doc: Value =
                serde_json::from_str(&super::super::error_document(&failure)).expect("json");
            assert_eq!(doc["error"]["code"], "sso_required");
            assert_eq!(doc["error"]["message"], "sign in with SSO in the browser");
            assert_eq!(failure.code.exit_status(), 1);
        }
    }

    #[test]
    fn a_credit_read_is_one_get_of_the_clouds_billing() {
        let (origin, server) = serve_once(200, &cloud_billing().to_string());
        let client = CloudClient::new(&origin).unwrap();
        let (status, body) = fetch_credit(&client).expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /v1/billing HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.ends_with("\r\n\r\n"), "no body: {request}");
        let doc = credit_outcome(status, &body).unwrap();
        assert_eq!(doc["model_credit"]["left_agent_hours"], json!(30.0));

        let (origin, server) =
            serve_once(401, r#"{"error":{"code":"unauthorized","message":"x"}}"#);
        let client = CloudClient::new(&origin).unwrap();
        assert_eq!(
            fetch_credit(&client).expect_err("401"),
            CloudError::Unauthorized
        );
        server.join().unwrap();
    }

    #[test]
    fn without_a_connection_the_credit_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        assert_eq!(
            read_credit().expect_err("no instance").code,
            Code::NotConnected
        );
    }
}
