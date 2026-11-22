// SPDX-License-Identifier: Apache-2.0
//! Top-up from LeanCTX Code: a Stripe checkout link for prepaid credit, as
//! the Local Read kind `cloud.billing.topup` (local-read-v1 §33).
//!
//! One call of the Cloud's engine route `POST /v1/billing/credit-topup`
//! (Cloud API §17.2). The engine calls it with its instance token and the
//! Cloud acts as the person the instance is linked to, in the engine's own
//! team, with that person's role, plan and confirmed-email rules
//! and rate budgets. A top-up pays model use once the plan's model credit
//! is used up, and computer minutes after the free hours (those come from
//! the top-up only; the plan credit pays models only). The Cloud is the
//! authority on everything (who may buy, how many packs, the price); the
//! engine only checks that `packs` is a whole number from 1 to 10 before
//! anything is sent, and passes no `packs` when none is given (the Cloud then
//! sells its default, $20). The document carries only the Stripe link the
//! Cloud answered, checked to be Stripe's. The engine never talks to Stripe.
//!
//! | kind | local route | Cloud route |
//! |---|---|---|
//! | `cloud.billing.topup` | `POST /v1/local/cloud/billing/topup` | `POST /v1/billing/credit-topup` |
//!
//! CLI: `lean-ctx cloud billing topup [--packs N] --json` prints the same
//! document; without `--json` it prints the link for a person. Wired as one
//! `local_read::Part`; it reuses the Cloud exchange of `billing_part`.

use serde_json::{Map, Value};

use super::billing_part::{self, Call};
use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

pub(crate) const TOPUP: &str = "cloud.billing.topup";

/// The release tool of the top-up (a person's act, like checkout).
const TOOL: &str = "ctx_agent";

/// The Cloud route (below `/v1`).
const CLOUD_PATH: &str = "/billing/credit-topup";

/// The most packs one top-up buys (the Cloud's `MAX_CREDIT_PACKS`).
pub(crate) const MAX_PACKS: u64 = 10;

/// This part's route. No project: billing is the team's.
pub(crate) const ROUTES: &[Route] = &[Route {
    kind: TOPUP,
    method: Method::Post,
    path: "/v1/local/cloud/billing/topup",
    cli: &["cloud", "billing", "topup", "--json"],
    action: &["topup"],
    params: &[
        Param {
            name: "packs",
            flag: "--packs",
            shape: Shape::Value,
        },
        Param {
            name: "idempotency_key",
            flag: "--idempotency-key",
            shape: Shape::Value,
        },
    ],
    project: Project::None,
    names: None,
}];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[TOPUP];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("cloud.billing", &["topup"])];

/// Whether `key` is a UUID in its hyphenated form (8-4-4-4-12 hex digits),
/// the only `idempotency_key` sent on.
fn is_uuid(key: &str) -> bool {
    key.len() == 36 && uuid::Uuid::try_parse(key).is_ok()
}

/// The Cloud call for the arguments (the verb first, then the flags).
/// Nothing is sent here. Without `--packs` the body is `{}`: the Cloud
/// chooses its default. `--idempotency-key` (a UUID) becomes the Cloud
/// request's `Idempotency-Key` header, so a retried top-up opens the same
/// checkout; without it no such header is sent.
pub(crate) fn call_of(args: &[String]) -> Result<Call, Failure> {
    let mut body = Map::new();
    if let Some(raw) = super::flag(args, "--packs")? {
        let packs = raw
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=MAX_PACKS).contains(n))
            .ok_or_else(|| Failure::invalid("packs is a whole number from 1 to 10"))?;
        body.insert("packs".into(), Value::from(packs));
    }
    let idempotency_key = match super::flag(args, "--idempotency-key")? {
        Some(key) if is_uuid(key) => Some(key.to_owned()),
        Some(_) => {
            return Err(Failure::invalid(
                "idempotency_key is a UUID (8-4-4-4-12 hex digits)",
            ));
        }
        None => None,
    };
    Ok(Call {
        path: CLOUD_PATH,
        body: Value::Object(body),
        idempotency_key,
    })
}

/// The document body of a Cloud answer (`{url}` only), or its refusal in
/// the app's codes. The top-up's own refusals come first; the rest
/// (company billing, SSO, a refused field, rate, the payment provider) are
/// those of checkout and portal (§27). A top-up asks no two-factor step-up
/// (owner 19.11.).
pub(crate) fn outcome(status: u16, body: &Value) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        return billing_part::outcome(status, body);
    }
    let error = &body["error"];
    let code = error["code"].as_str();
    let refused = |code: Code, message: &str| Err(Failure::new(code, message));
    match (status, code) {
        (_, Some("plan_required")) | (402, _) => {
            if error["reason"].as_str() == Some("trial") {
                refused(Code::PlanRequired, "no top-up during the trial")
            } else {
                refused(
                    Code::PlanRequired,
                    "a top-up needs a paid plan in effect, choose a plan first",
                )
            }
        }
        (_, Some("email_unverified")) => refused(
            Code::EmailUnverified,
            "confirm your email address before paying, the link is in the confirmation email",
        ),
        (403, Some("forbidden")) => Err(Failure::new(
            Code::Forbidden,
            error["message"]
                .as_str()
                .and_then(billing_part::safe_message)
                .unwrap_or_else(|| "only an owner of the team tops up credit".to_owned()),
        )),
        (404, _) => refused(
            Code::Unavailable,
            "this LeanCTX Cloud does not sell top-ups to the app yet, open billing in the browser",
        ),
        (_, Some("not_configured")) => refused(
            Code::Unavailable,
            "top-ups are not on sale on this LeanCTX Cloud right now",
        ),
        _ => billing_part::outcome(status, body),
    }
}

/// Make `call` as the connected instance.
fn run(call: &Call) -> Result<Value, Failure> {
    let (status, body) = billing_part::with_cloud(&|client| billing_part::exchange(client, call))?;
    outcome(status, &body)
}

/// The released document, for the CLI or the socket. The request goes
/// through the release point before anything is sent.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    if kind != TOPUP {
        return Answer::failure(&Failure::invalid(format!("unknown kind {kind}")));
    }
    let call = match call_of(args) {
        Ok(call) => call,
        Err(failure) => return Answer::failure(&failure),
    };
    super::answer_released_write(TOOL, "", kind, via, &call.body, || run(&call))
}

/// The text a person reads for the document of a top-up.
fn human_text(doc: &Value) -> String {
    format!(
        "Open this Stripe checkout in your browser to top up credit for computer minutes and models:\n  {}",
        doc["url"].as_str().unwrap_or_default()
    )
}

/// `lean-ctx cloud billing topup [--packs N]` without `--json`: the link,
/// or the refusal on stderr with the document's exit status.
fn run_human(args: &[String]) {
    let result = call_of(args).and_then(|call| {
        super::release_text_write(TOOL, "", TOPUP, &call.body)?;
        run(&call)
    });
    match result {
        Ok(doc) => println!("{}", human_text(&doc)),
        Err(failure) => {
            eprintln!("lean-ctx: {}", failure.message);
            std::process::exit(failure.code.exit_status());
        }
    }
}

/// `cloud billing topup --help`: printed instead of asking for a link.
const USAGE: &str = "Usage: lean-ctx cloud billing topup [--packs N] [--idempotency-key UUID] [--json]\n\n\
Opens a Stripe checkout to top up prepaid credit (1 to 10 packs of $10;\n\
LeanCTX Cloud sells $20 when --packs is not given). Credit pays model use\n\
after the plan's model credit and computer minutes after the free hours.\n\
--idempotency-key UUID: a retry with the same key opens the same checkout.";

/// `lean-ctx cloud billing topup [--packs N] [--json]`; `false` for every
/// other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    if command != "cloud"
        || rest.first().map(String::as_str) != Some("billing")
        || rest.get(1).map(String::as_str) != Some("topup")
    {
        return false;
    }
    let args = &rest[1..];
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return true;
    }
    if super::wants_json(args) {
        super::print(&answer(TOPUP, args, Via::Cli));
    } else {
        run_human(args);
    }
    true
}

#[cfg(test)]
mod tests {
    use lean_ctx_cloud::CloudError;
    use lean_ctx_cloud::transport::CloudClient;
    use serde_json::json;

    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    fn route() -> &'static Route {
        super::super::routes::all()
            .find(|r| r.kind == TOPUP)
            .expect("topup is routed")
    }

    #[test]
    fn the_topup_kind_is_dispatched_routed_and_listed() {
        assert!(super::super::part_of(TOPUP).is_some());
        let route = route();
        assert_eq!(route.method, Method::Post);
        assert_eq!(route.path, "/v1/local/cloud/billing/topup");
        assert_eq!(route.project, Project::None);
        let table = super::super::routes::table_json();
        assert!(table.to_string().contains(route.path));
        let caps = serde_json::to_value(super::super::capabilities::build_with(table, Value::Null))
            .unwrap();
        assert!(
            caps["kinds"].as_array().unwrap().contains(&json!(TOPUP)),
            "capabilities lists {TOPUP}"
        );
        let names: Vec<&str> = route.params.iter().map(|p| p.name).collect();
        assert_eq!(names, ["packs", "idempotency_key"]);
        assert!(
            caps.to_string()
                .contains(r#""params":["packs","idempotency_key"]"#),
            "capabilities report idempotency_key"
        );
        let verbs = caps["commands"]["cloud.billing"].as_array().unwrap();
        for verb in ["checkout", "credit", "portal", "topup"] {
            assert!(
                verbs.iter().any(|v| v == verb || v["verb"] == verb),
                "{verb} in {verbs:?}"
            );
        }
    }

    #[test]
    fn the_apps_body_becomes_the_clouds() {
        let call = |inputs: Value| {
            let inputs: Vec<(String, Value)> =
                inputs.as_object().unwrap().clone().into_iter().collect();
            let args = super::super::routes::args_of(route(), None, &inputs)?;
            call_of(&args)
        };
        assert_eq!(
            call(json!({"packs": 3})).unwrap(),
            Call {
                path: "/billing/credit-topup",
                body: json!({"packs": 3}),
                idempotency_key: None,
            }
        );
        assert_eq!(
            call(json!({"packs": "10"})).unwrap().body,
            json!({"packs": 10})
        );
        // No packs: the Cloud's default ($20), nothing chosen here.
        assert_eq!(call(json!({})).unwrap().body, json!({}));
        assert_eq!(
            call(json!({"pack": 2})).expect_err("unknown").code,
            Code::InvalidArgument
        );
        assert_eq!(
            call_of(&words(&["topup", "--packs", "1"])).unwrap().body,
            json!({"packs": 1})
        );
    }

    #[test]
    fn packs_outside_one_to_ten_are_refused_before_anything_is_sent() {
        for bad in ["0", "11", "-1", "two", "2.5", ""] {
            let failure =
                call_of(&words(&["topup", "--json", &format!("--packs={bad}")])).expect_err(bad);
            assert_eq!(failure.code, Code::InvalidArgument, "{bad}");
        }
        assert_eq!(
            call_of(&words(&["topup", "--json", "--packs"]))
                .expect_err("no value")
                .code,
            Code::InvalidArgument
        );
    }

    #[test]
    fn an_idempotency_key_is_a_uuid_or_refused() {
        let key = "3f2b8c1e-7d4a-4e9b-9c2f-0a1b2c3d4e5f";
        let call = call_of(&words(&["topup", "--json", "--idempotency-key", key])).unwrap();
        assert_eq!(call.idempotency_key.as_deref(), Some(key));
        assert_eq!(call.body, json!({}), "the key is a header, not body");
        let inputs = vec![("idempotency_key".to_owned(), json!(key))];
        let args = super::super::routes::args_of(route(), None, &inputs).unwrap();
        assert_eq!(
            call_of(&args).unwrap().idempotency_key.as_deref(),
            Some(key)
        );
        for bad in [
            "abc",
            "3f2b8c1e7d4a4e9b9c2f0a1b2c3d4e5f",
            "{3f2b8c1e-7d4a-4e9b-9c2f-0a1b2c3d4e5f}",
            "3f2b8c1e-7d4a-4e9b-9c2f-0a1b2c3d4e5g",
            "3f2b8c1e-7d4a-4e9b-9c2f-0a1b2c3d4e5f\r\nX: y",
        ] {
            let failure =
                call_of(&words(&["topup", "--json", "--idempotency-key", bad])).expect_err(bad);
            assert_eq!(failure.code, Code::InvalidArgument, "{bad:?}");
            assert_eq!(
                failure.message, "idempotency_key is a UUID (8-4-4-4-12 hex digits)",
                "{bad:?}"
            );
        }
        assert_eq!(
            call_of(&words(&["topup", "--json"]))
                .unwrap()
                .idempotency_key,
            None
        );
    }

    #[test]
    fn the_idempotency_key_is_sent_as_the_header_and_only_then() {
        let key = "3f2b8c1e-7d4a-4e9b-9c2f-0a1b2c3d4e5f";
        let header = |request: &str| {
            request
                .split("\r\n\r\n")
                .next()
                .unwrap()
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("idempotency-key")
                        .then(|| value.trim().to_owned())
                })
        };
        let link = r#"{"url":"https://checkout.stripe.com/c/pay/cs_test_topup"}"#;
        let (origin, server) = billing_part::tests::serve_once(200, link);
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of(&words(&[
            "topup",
            "--json",
            "--packs",
            "2",
            "--idempotency-key",
            key,
        ]))
        .unwrap();
        billing_part::exchange(&client, &call).expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/billing/credit-topup HTTP/1.1\r\n"),
            "{request}"
        );
        assert_eq!(header(&request).as_deref(), Some(key));

        let (origin, server) = billing_part::tests::serve_once(200, link);
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of(&words(&["topup", "--json"])).unwrap();
        billing_part::exchange(&client, &call).expect("answered");
        assert_eq!(header(&server.join().unwrap()), None, "no flag, no header");
    }

    #[test]
    fn cloud_refusals_keep_their_meaning() {
        let check = |status: u16, error: Value, code: Code, message: &str| {
            let failure = outcome(status, &json!({ "error": error })).expect_err("refused");
            assert_eq!(
                (failure.code, failure.message.as_str()),
                (code, message),
                "{status} {error}"
            );
        };
        // p37 `credit_needs_plan`: no top-up in a trial, nor without a plan.
        check(
            402,
            json!({"code": "plan_required", "message": "x", "feature": "credit_topup", "reason": "trial", "next_step": "subscribe"}),
            Code::PlanRequired,
            "no top-up during the trial",
        );
        for reason in ["no_plan", "trial_ended", "plan_ended"] {
            check(
                402,
                json!({"code": "plan_required", "message": "x", "reason": reason}),
                Code::PlanRequired,
                "a top-up needs a paid plan in effect, choose a plan first",
            );
        }
        // p37 `require_verified_email`.
        check(
            403,
            json!({"code": "email_unverified", "message": "x", "next_step": "verify_email"}),
            Code::EmailUnverified,
            "confirm your email address before paying, the link is in the confirmation email",
        );
        // The Cloud's words pass through, as for checkout and portal.
        check(
            403,
            json!({"code": "forbidden", "message": "only owners may top up"}),
            Code::Forbidden,
            "only owners may top up",
        );
        check(
            403,
            json!({"code": "forbidden"}),
            Code::Forbidden,
            "only an owner of the team tops up credit",
        );
        check(
            403,
            json!({"code": "sso_required", "message": "x"}),
            Code::SsoRequired,
            "sign in with SSO in the browser",
        );
        check(
            409,
            json!({"code": "invalid_state", "message": "x", "state": "company_billed"}),
            Code::Forbidden,
            "billed through your company",
        );
        check(
            409,
            json!({"code": "invalid_state", "message": "x"}),
            Code::Conflict,
            "x",
        );
        check(
            400,
            json!({"code": "invalid_field", "field": "packs", "message": "buy 1 to 10 packs at a time"}),
            Code::InvalidArgument,
            "buy 1 to 10 packs at a time",
        );
        check(
            429,
            json!({"code": "rate_limited", "message": "x"}),
            Code::RateLimited,
            "LeanCTX Cloud asks to retry later",
        );
        check(
            502,
            json!({"code": "upstream", "message": "x"}),
            Code::Unavailable,
            "the payment provider did not answer, retry later",
        );
        check(
            503,
            json!({"code": "not_configured", "message": "x"}),
            Code::Unavailable,
            "top-ups are not on sale on this LeanCTX Cloud right now",
        );
        check(
            404,
            json!({"code": "not_found", "message": "x"}),
            Code::Unavailable,
            "this LeanCTX Cloud does not sell top-ups to the app yet, open billing in the browser",
        );
        let down = outcome(500, &json!("<html>")).expect_err("down");
        assert_eq!(down.code, Code::Unavailable);
        let doc: Value = serde_json::from_str(&super::super::error_document(
            &outcome(403, &json!({"error": {"code": "email_unverified"}})).unwrap_err(),
        ))
        .unwrap();
        assert_eq!(doc["kind"], "error");
        assert_eq!(doc["error"]["code"], "email_unverified");
        assert!(doc["error"].get("reason").is_none());
        // 409 invalid_state with state=company_billed: forbidden, and why.
        let billed = outcome(
            409,
            &json!({"error": {"code": "invalid_state", "message": "x", "state": "company_billed"}}),
        )
        .unwrap_err();
        let doc: Value = serde_json::from_str(&super::super::error_document(&billed)).unwrap();
        assert_eq!(doc["error"]["code"], "forbidden");
        assert_eq!(doc["error"]["reason"], "company_billed");
        let owner_only = outcome(
            403,
            &json!({"error": {"code": "forbidden", "message": "x"}}),
        )
        .unwrap_err();
        assert_eq!(owner_only.code, Code::Forbidden);
        assert_eq!(owner_only.reason, None);
        let doc: Value = serde_json::from_str(&super::super::error_document(&owner_only)).unwrap();
        assert!(doc["error"].get("reason").is_none());
    }

    #[test]
    fn a_topup_sends_the_clouds_body_and_keeps_only_the_link() {
        let (origin, server) = billing_part::tests::serve_once(
            200,
            r#"{"url":"https://checkout.stripe.com/c/pay/cs_test_topup","packs":4,"customer":"cus_1"}"#,
        );
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of(&words(&["topup", "--json", "--packs", "4"])).unwrap();
        let (status, body) = billing_part::exchange(&client, &call).expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/billing/credit-topup HTTP/1.1\r\n"),
            "{request}"
        );
        let sent: Value =
            serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(sent, json!({"packs": 4}));
        let text = super::super::document(TOPUP, &outcome(status, &body).unwrap());
        assert_eq!(
            text,
            r#"{"contract":"local-read/1","kind":"cloud.billing.topup","url":"https://checkout.stripe.com/c/pay/cs_test_topup"}"#
        );
        assert_eq!(
            human_text(&outcome(status, &body).unwrap()),
            "Open this Stripe checkout in your browser to top up credit for computer minutes and models:\n  https://checkout.stripe.com/c/pay/cs_test_topup"
        );
    }

    #[test]
    fn the_document_is_the_same_bytes_for_the_same_answer() {
        let body =
            json!({"url": "https://checkout.stripe.com/c/pay/cs_1", "expires_at": 1_763_560_000});
        let first = super::super::document(TOPUP, &outcome(200, &body).unwrap());
        let second = super::super::document(TOPUP, &outcome(200, &body).unwrap());
        assert_eq!(first, second);
        assert!(!first.contains("expires_at"), "{first}");
        assert_eq!(
            outcome(200, &json!({"url": "https://pay.example/x"}))
                .expect_err("not stripe")
                .code,
            Code::Internal
        );
    }

    #[test]
    fn a_refused_token_or_no_connection_is_not_connected() {
        let (origin, server) = billing_part::tests::serve_once(
            401,
            r#"{"error":{"code":"unauthorized","message":"x"}}"#,
        );
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of(&words(&["topup", "--json"])).unwrap();
        let error = billing_part::exchange(&client, &call).expect_err("401");
        server.join().unwrap();
        assert_eq!(error, CloudError::Unauthorized);

        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        assert_eq!(
            run(&call).expect_err("no instance").code,
            Code::NotConnected
        );
    }

    #[test]
    fn only_the_topup_command_line_is_taken() {
        assert!(!try_run("cloud", &words(&["billing", "portal", "--json"])));
        assert!(!try_run("cloud", &words(&["billing"])));
        assert!(!try_run("billing", &words(&["topup", "--json"])));
        // Help is answered locally; nothing is sent to the Cloud.
        assert!(try_run("cloud", &words(&["billing", "topup", "--help"])));
        assert!(USAGE.contains("--packs N") && !USAGE.contains("engine"));
    }
}
