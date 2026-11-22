// SPDX-License-Identifier: Apache-2.0
use serde_json::json;

use super::*;

fn instance(kind: &str) -> InstanceRecord {
    InstanceRecord {
        instance_id: "inst_1".into(),
        org_id: "org_acme".into(),
        org_name: "acme".into(),
        label: "laptop".into(),
        url: "https://cloud.example.net".into(),
        org_salt_id: "salt".into(),
        enrolled_at: "2026-11-01T00:00:00Z".into(),
        web_url: None,
        org_kind: Some(kind.into()),
        company_name: None,
    }
}

fn args(raw: &[&str]) -> Args {
    let raw: Vec<String> = raw.iter().map(|a| (*a).to_owned()).collect();
    Args::parse(&raw).expect("parses")
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("object"),
    }
}

fn row(id: &str, name: &str, team: Option<&str>) -> Map<String, Value> {
    let scope = match team {
        Some(team) => json!({ "kind": "team", "team": team }),
        None => json!({ "kind": "personal" }),
    };
    object(json!({
        "id": id, "name": name, "state": "stopped", "role": "weave_host", "scope": scope,
        "price": { "usd_per_hour": 0.09 },
        "balance": { "usd": 12.4, "state": "ok", "free_hours_left": 494.8, "usd_per_hour": 0.09 }
    }))
}

#[test]
fn cloud_computer_list_passes_rows_and_adds_the_local_ssh_fact() {
    let mut with_ssh = row("cmp_1", "weave-host", Some("org_acme"));
    with_ssh.insert("ssh".into(), json!({ "host_alias": "leanctx-weave-host" }));
    let list = ComputerList {
        computers: vec![with_ssh, row("cmp_2", "other", None)],
    };
    let doc = list_doc(Answer::Done(list), |name, ssh| {
        name == "weave-host" && ssh.contains_key("host_alias")
    })
    .expect("doc");
    let value = serde_json::to_value(&doc).expect("json");
    assert_eq!(value["available"], true);
    assert_eq!(value["live"], true);
    assert_eq!(value["computers"][0]["ssh"]["config_written"], true);
    assert!(
        value["computers"][1].get("ssh").is_none(),
        "no invented ssh"
    );
    assert_eq!(value["computers"][1]["price"]["usd_per_hour"], 0.09);
    assert!(
        value.get("balance").is_none(),
        "the balance is per row (§4.3)"
    );
    assert_eq!(value["computers"][1]["balance"]["usd"], 12.4);
}

#[test]
fn cloud_computer_list_of_a_cloud_without_computers_is_unavailable() {
    let doc = list_doc(Answer::NotOffered, |_, _| false).expect("doc");
    let value = serde_json::to_value(&doc).expect("json");
    assert_eq!(
        value,
        json!({ "available": false, "live": false, "computers": [] })
    );
}

#[test]
fn cloud_computer_list_is_never_an_error_when_computers_are_not_available() {
    // LeanCTX Code shows "not available yet" only for the list document.
    let off = json!({ "available": false, "live": false, "computers": [] });
    for (status, body) in [
        (404, Value::Null),
        (404, json!({ "error": { "code": "not_found" } })),
        (503, json!({ "error": { "code": "not_configured" } })),
        (
            503,
            json!({ "error": { "code": "computer_not_available", "message": "raw" } }),
        ),
        (200, json!({ "available": false, "computers": [] })),
    ] {
        let send = move |_: &str,
                         _: &str,
                         _: Option<&Value>|
              -> Result<(u16, Value), lean_ctx_cloud::CloudError> {
            Ok((status, body.clone()))
        };
        let answer = computers::list(&send).expect("answer");
        let doc = list_doc(answer, |_, _| false).expect("list document, not an error");
        let text = local_read::answer(KIND_LIST, || Ok(doc)).text;
        let value: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["kind"], "cloud.computers", "{status}");
        for key in ["available", "live", "computers"] {
            assert_eq!(value[key], off[key], "{status} {key}");
        }
    }
}

#[test]
fn cloud_computer_list_document_is_byte_stable() {
    // #498: the same cloud answer gives the same bytes; no local clock.
    let build = || {
        let list = ComputerList {
            computers: vec![row("cmp_1", "weave-host", Some("org_acme"))],
        };
        let doc = list_doc(Answer::Done(list), |_, _| false).expect("doc");
        local_read::answer(KIND_LIST, || Ok(doc)).text
    };
    let once = build();
    // Across a second boundary, so a seconds clock would show.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(once, build(), "same answer, same bytes");
    assert!(!once.contains("as_of"), "{once}");
}

#[test]
fn cloud_computer_refusals_keep_the_cloud_code_and_message() {
    let refusal = Refusal {
        status: 402,
        code: "balance_empty".into(),
        message: Some("The top-up is used up.".into()),
        next_step: Some("top_up".into()),
        retry_after_s: None,
        details: None,
        reason: None,
    };
    let failure = refused(&refusal);
    assert_eq!(failure.code, Code::BalanceEmpty);
    assert_eq!(
        failure.message,
        "The top-up is used up. Add a top-up to continue. (LeanCTX Cloud: 402 balance_empty)"
    );
    for (status, code, want) in [
        (409, "computer_exists", Code::ComputerExists),
        (409, "computer_busy", Code::ComputerBusy),
        (404, "no_computer", Code::NoComputer),
        (403, "forbidden", Code::Forbidden),
        (403, "sso_required", Code::SsoRequired),
        (402, "plan_required", Code::PlanRequired),
        (422, "invalid_request", Code::InvalidField),
        (429, "rate_limited", Code::RateLimited),
        (
            503,
            "computer_provider_unavailable",
            Code::ComputerProviderUnavailable,
        ),
        (409, "http_409", Code::Conflict),
        (500, "weird", Code::Internal),
    ] {
        let refusal = Refusal {
            status,
            code: code.into(),
            message: None,
            next_step: None,
            retry_after_s: (status == 429).then_some(30),
            details: None,
            reason: None,
        };
        let failure = refused(&refusal);
        assert_eq!(failure.code, want, "{code}");
        assert!(
            failure
                .message
                .ends_with(&format!("(LeanCTX Cloud: {status} {code})"))
        );
        if status == 429 {
            assert!(failure.message.contains("Try again in 30 s."));
        }
    }
    // The metering routes' shape for an existing computer.
    let exists = Refusal {
        status: 409,
        code: "invalid_state".into(),
        message: None,
        next_step: None,
        retry_after_s: None,
        details: None,
        reason: Some("computer_exists".into()),
    };
    assert_eq!(refused(&exists).code, Code::ComputerExists);
    let other = Refusal {
        reason: Some("stopping".into()),
        ..exists
    };
    assert_eq!(refused(&other).code, Code::InvalidState);
    assert!(!refused(&other).message.contains("workspace"));
}

#[test]
fn cloud_computer_not_available_is_said_plainly() {
    // p37 without a provider: writes answer `503 computer_not_available`.
    let refusal = Refusal {
        status: 503,
        code: "computer_not_available".into(),
        message: Some("raw cloud text".into()),
        next_step: None,
        retry_after_s: None,
        details: None,
        reason: None,
    };
    let failure = refused(&refusal);
    assert_eq!(failure.code, Code::ComputerNotAvailable);
    assert_eq!(failure.message, "LeanCTX computers are not available yet.");
    assert!(failure.details.is_none());
    assert_eq!(not_offered(), failure);
}

#[test]
fn cloud_computer_role_follows_the_scope() {
    assert_eq!(role_of(&args(&["create"]), true).expect("ok"), "general");
    assert_eq!(
        role_of(&args(&["create"]), false).expect("ok"),
        "weave_host"
    );
    assert_eq!(
        role_of(&args(&["create", "--role", "general"]), false).expect("ok"),
        "general"
    );
    // A Weave host is for teams: refused locally, before any terms.
    let failure = role_of(&args(&["create", "--role", "weave_host"]), true).expect_err("personal");
    assert_eq!(failure.code, Code::InvalidArgument);
    assert!(failure.message.contains("for teams"), "{}", failure.message);
    assert!(role_of(&args(&["create", "--role", "x"]), false).is_err());
}

#[test]
fn cloud_computer_start_ack_names_who_pays() {
    let started = ack(
        Action::Start,
        &row("cmp_1", "weave-host", Some("org_acme")),
        "req_12345678".into(),
        None,
    );
    let value = serde_json::to_value(&started).expect("json");
    assert_eq!(value["action"], "start");
    assert_eq!(value["id"], "cmp_1");
    assert_eq!(value["request_id"], "req_12345678");
    assert_eq!(value["billing"]["basis"], "free");
    assert_eq!(value["billing"]["usd_per_hour"], 0.09);
    let mut general = row("cmp_2", "box", None);
    general.insert("role".into(), json!("general"));
    let value = serde_json::to_value(ack(Action::Start, &general, "req_12345678".into(), None))
        .expect("json");
    assert_eq!(value["billing"]["basis"], "billed");
    // Free hours are a team's: a personal weave_host is billed.
    let personal = ack(
        Action::Start,
        &row("cmp_3", "weave-host", None),
        "req_12345678".into(),
        None,
    );
    let value = serde_json::to_value(personal).expect("json");
    assert_eq!(value["billing"]["basis"], "billed");
    // An answer row without scope or balance borrows them from the list.
    let bare = object(json!({ "id": "cmp_1", "name": "weave-host", "state": "starting" }));
    let listed = row("cmp_1", "weave-host", Some("org_acme"));
    let value = serde_json::to_value(ack(
        Action::Start,
        &bare,
        "req_12345678".into(),
        Some(&listed),
    ))
    .expect("json");
    assert_eq!(value["billing"]["basis"], "free");
    assert_eq!(value["billing"]["topup_usd"], 12.4);
    assert_eq!(value["billing"]["topup_state"], "ok");
    let stopped = ack(
        Action::Stop,
        &row("cmp_1", "weave-host", None),
        "req_12345678".into(),
        None,
    );
    assert!(serde_json::to_value(stopped).expect("json")["billing"].is_null());
}

fn terms(value: Value) -> Terms {
    computers::terms(Some(&object(value)))
}

fn error_details(failure: &Failure) -> Value {
    failure.details.clone().expect("details")
}

#[test]
fn cloud_computer_writes_without_yes_carry_the_cloud_terms() {
    let free = terms(json!({
        "billing": "free", "usd_per_hour": 0.09, "free_hours_left": 12.5, "topup_usd": 0.0
    }));
    let failure = confirmation(Action::Start, "weave-host", free, false, Asked::Unconfirmed);
    assert_eq!(failure.code, Code::ConfirmationRequired);
    assert_eq!(failure.code.exit_status(), 1);
    assert_eq!(
        error_details(&failure),
        json!({ "billing": "free", "usd_per_hour": 0.09, "free_hours_left": 12.5, "topup_usd": 0.0 })
    );
    assert!(
        failure
            .message
            .contains("free hours (12.5 left this month), then $0.09 per hour"),
        "{}",
        failure.message
    );
    assert!(failure.message.contains("--yes"));

    let billed = terms(json!({ "billing": "billed", "usd_per_hour": 0.09, "topup_usd": 4.0 }));
    let failure = confirmation(Action::Create, "box", billed, false, Asked::Unconfirmed);
    assert!(
        failure
            .message
            .contains("billed to the top-up at $0.09 per hour (top-up $4.00 left)"),
        "{}",
        failure.message
    );
    assert_eq!(error_details(&failure)["free_hours_left"], Value::Null);
}

#[test]
fn cloud_computer_first_create_never_invents_a_price_or_free_hours() {
    // The first create: no listed computer in the scope, nothing to read terms from.
    let unknown = terms(json!({ "billing": "free", "free_hours_left": 500.0 }));
    let failure = confirmation(
        Action::Create,
        "weave-host",
        unknown,
        false,
        Asked::Unconfirmed,
    );
    assert!(!failure.message.contains("$0.00"), "{}", failure.message);
    assert!(
        failure.message.contains("did not report"),
        "{}",
        failure.message
    );
    assert_eq!(error_details(&failure)["usd_per_hour"], Value::Null);
    assert_eq!(error_details(&failure)["topup_usd"], Value::Null);

    let billed = terms(json!({ "billing": "billed" }));
    let failure = confirmation(Action::Create, "box", billed, false, Asked::Unconfirmed);
    assert!(!failure.message.contains("$0.00"), "{}", failure.message);
    assert!(
        failure
            .message
            .contains("did not report the price per hour"),
        "{}",
        failure.message
    );

    // Personal is never free, whatever the details say.
    let claimed = terms(json!({ "billing": "free", "usd_per_hour": 0.09, "free_hours_left": 3.0 }));
    let failure = confirmation(Action::Create, "mine", claimed, true, Asked::Unconfirmed);
    assert_eq!(error_details(&failure)["billing"], "billed");
    assert!(
        !failure.message.contains("free hours"),
        "{}",
        failure.message
    );

    let silent = confirmation(
        Action::Start,
        "w",
        computers::terms(None),
        false,
        Asked::Unconfirmed,
    );
    assert_eq!(error_details(&silent)["billing"], Value::Null);
    assert!(silent.message.contains("did not say who pays"));
}

#[test]
fn cloud_computer_confirmation_error_document_has_details() {
    let failure = confirmation(
        Action::Start,
        "weave-host",
        terms(json!({ "billing": "billed", "usd_per_hour": 0.09 })),
        false,
        Asked::Unconfirmed,
    );
    let doc: Value = serde_json::from_str(&local_read::error_document(&failure)).expect("json");
    assert_eq!(doc["error"]["code"], "confirmation_required");
    assert_eq!(doc["error"]["details"]["billing"], "billed");
    assert_eq!(doc["error"]["details"]["usd_per_hour"], 0.09);
}

#[test]
fn cloud_computer_ack_text_never_shows_a_zero_price() {
    let bill = Billing {
        basis: Some("billed"),
        free_hours_left: None,
        usd_per_hour: None,
        topup_usd: None,
        topup_state: None,
    };
    assert_eq!(
        billing_text(&serde_json::to_value(&bill).expect("json")),
        "billed; LeanCTX Cloud did not report the price per hour"
    );
    let unknown = Billing {
        basis: None,
        ..bill
    };
    assert_eq!(
        billing_text(&serde_json::to_value(&unknown).expect("json")),
        "LeanCTX Cloud did not say who pays"
    );
    let started = ack(
        Action::Start,
        &row("cmp_1", "weave-host", Some("org_acme")),
        "req_12345678".into(),
        None,
    );
    assert_eq!(
        ack_text(&serde_json::to_value(&started).expect("json")),
        "start: computer 'weave-host' (cmp_1) is stopped\nruns on the team's free hours (494.8 left this month)"
    );
}

#[test]
fn cloud_computer_team_defaults_to_the_connected_team() {
    let team = instance("team");
    assert_eq!(
        team_of(&args(&["create"]), &team).expect("ok").as_deref(),
        Some("org_acme")
    );
    assert_eq!(
        team_of(&args(&["create", "--team", "org_acme"]), &team)
            .expect("ok")
            .as_deref(),
        Some("org_acme")
    );
    assert_eq!(
        team_of(&args(&["create"]), &instance("personal")).expect("ok"),
        None
    );
    assert_eq!(
        team_of(&args(&["create", "--personal"]), &instance("personal")).expect("ok"),
        None
    );
    assert!(
        team_of(
            &args(&["create", "--team", "org_acme", "--personal"]),
            &team
        )
        .is_err()
    );
}

#[test]
fn cloud_computer_personal_is_refused_on_a_team_machine() {
    // The cloud would create (and bill) a team computer for this machine's
    // team, so the CLI refuses before anything is sent.
    let failure = team_of(&args(&["create", "--personal"]), &instance("team")).expect_err("team");
    assert_eq!(failure.code, Code::InvalidArgument);
    assert!(
        failure.message.contains("connected to a team"),
        "{}",
        failure.message
    );
}

#[test]
fn cloud_computer_other_team_is_refused() {
    for (machine, team) in [("team", "org_b"), ("team", "a b"), ("personal", "org_acme")] {
        let failure =
            team_of(&args(&["create", "--team", team]), &instance(machine)).expect_err(team);
        assert_eq!(failure.code, Code::InvalidArgument, "{machine} {team}");
        assert!(
            failure.message.starts_with("--team:"),
            "{}",
            failure.message
        );
    }
}

#[test]
fn cloud_computer_names_and_ids_pick_one_row() {
    let me = instance("team");
    let rows = vec![
        row("cmp_1", "weave-host", Some("org_acme")),
        row("cmp_2", "weave-host", None),
        row("cmp_3", "box", Some("org_other")),
    ];
    assert_eq!(pick(&rows, "weave-host", &me).expect("row")["id"], "cmp_1");
    assert_eq!(pick(&rows, "cmp_2", &me).expect("row")["id"], "cmp_2");
    assert_eq!(pick(&rows, "box", &me).expect("row")["id"], "cmp_3");
    assert_eq!(
        pick(&rows, "gone", &me).expect_err("none").code,
        Code::NoComputer
    );
    let both_foreign = vec![row("cmp_4", "w", Some("x")), row("cmp_5", "w", Some("y"))];
    assert_eq!(
        pick(&both_foreign, "w", &me).expect_err("ambiguous").code,
        Code::InvalidArgument
    );
}

#[test]
fn cloud_computer_request_ids_are_checked() {
    assert!(
        request_id(&args(&["start", "w"]))
            .expect("fresh")
            .starts_with("req_")
    );
    assert_eq!(
        request_id(&args(&["start", "w", "--request-id", "req_retry_01"])).expect("given"),
        "req_retry_01"
    );
    assert!(request_id(&args(&["start", "w", "--request-id", "x"])).is_err());
}

#[test]
fn cloud_computer_topup_line_reads_the_cloud_numbers() {
    let line = topup_line(&object(
        json!({ "usd": 12.4, "state": "ok", "free_hours_left": 494.8, "usd_per_hour": 0.09 }),
    ));
    assert_eq!(
        line,
        "top-up $12.40 left (ok) · 494.8 free hours left this month · $0.09 per hour after them"
    );
}

#[test]
fn cloud_computer_billing_shows_only_finite_non_negative_numbers() {
    // m7: a negative price, top-up or free hours is unknown, never shown.
    let mut odd = row("cmp_1", "weave-host", Some("org_acme"));
    odd.insert("price".into(), json!({ "usd_per_hour": -0.09 }));
    odd.insert(
        "balance".into(),
        json!({ "usd": -3.0, "state": "ok", "free_hours_left": -1.0, "usd_per_hour": 0.09 }),
    );
    let bill = billing(&odd);
    assert_eq!(
        bill.usd_per_hour,
        Some(0.09),
        "the balance's price stands in"
    );
    assert_eq!(bill.topup_usd, None);
    assert_eq!(bill.free_hours_left, None);
    // m2: a team weave_host without readable free hours has no basis.
    assert_eq!(bill.basis, None);
    let mut bare = row("cmp_2", "weave-host", Some("org_acme"));
    bare.remove("balance");
    assert_eq!(billing(&bare).basis, None);
    let value = serde_json::to_value(billing(&bare)).expect("json");
    assert!(value["basis"].is_null(), "{value}");
}

#[test]
fn cloud_computer_scope_matches_the_team_id_not_its_name() {
    let me = instance("team");
    let by_id = row("cmp_1", "w", Some("org_acme"));
    let by_name = row("cmp_2", "w", Some("acme"));
    assert!(in_scope(&by_id, Some("org_acme")));
    assert!(
        !in_scope(&by_name, Some("org_acme")),
        "a display name is not the team id"
    );
    assert!(in_scope(&row("cmp_3", "w", None), None));
    // Two rows named `w`: only the one with this team's id is this machine's.
    assert_eq!(
        pick(&[by_name, by_id], "w", &me).expect("row")["id"],
        "cmp_1"
    );
}

#[test]
fn cloud_computer_create_offers_no_size_region_or_idle_stop() {
    // LeanCTX Cloud neither takes nor enforces them yet: refused as unknown.
    for flag in ["--size", "--region", "--idle-stop"] {
        let raw = vec!["create".to_owned(), flag.to_owned(), "2".to_owned()];
        assert!(Args::parse(&raw).is_err(), "{flag}");
    }
}

#[test]
fn cloud_computer_create_terms_come_from_the_scope_balance() {
    let rows = vec![row("cmp_1", "weave-host", Some("org_acme"))];
    let team = create_terms(&rows, Some("org_acme"), ROLE_WEAVE_HOST);
    assert_eq!(team.billing, Some("free"));
    assert_eq!(team.free_hours_left, Some(494.8));
    assert_eq!(team.usd_per_hour, Some(0.09));
    assert_eq!(team.topup_usd, Some(12.4));
    assert_eq!(
        create_terms(&rows, Some("org_acme"), "general").billing,
        Some("billed")
    );
    // No listed computer in the scope: unknown, never invented.
    let first = create_terms(&rows, Some("org_other"), ROLE_WEAVE_HOST);
    assert_eq!(first.billing, None);
    assert_eq!(first.usd_per_hour, None);
    assert_eq!(first.topup_usd, None);
    // Personal is billed, even with no row yet.
    let personal = create_terms(&[], None, ROLE_WEAVE_HOST);
    assert_eq!(personal.billing, Some("billed"));
    assert_eq!(personal.usd_per_hour, None);
}

mod loopback {
    use std::io::{Read, Write};

    use lean_ctx_cloud::transport::CloudClient;

    use super::*;

    /// A loopback LeanCTX Cloud answering `answers` in order; the requests
    /// as text.
    fn serve(answers: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let handle = std::thread::spawn(move || {
            // A test that never sends fails here instead of hanging.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            let mut seen = Vec::new();
            for (status, body) in answers {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "no request came");
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(e) => panic!("accept: {e}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .unwrap();
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
                seen.push(String::from_utf8_lossy(&buf).into_owned());
            }
            seen
        });
        (origin, handle)
    }

    /// The last request the server takes, sent by the test itself: any
    /// request the CLI made after the list would arrive before it.
    fn sentinel(origin: &str) {
        let mut stream =
            std::net::TcpStream::connect(origin.trim_start_matches("http://")).unwrap();
        stream
            .write_all(b"GET /sentinel HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
    }

    fn sent(request: &str) -> Value {
        serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap()
    }

    fn listed() -> String {
        json!({ "computers": [row("cmp_1", "weave-host", Some("org_acme"))] }).to_string()
    }

    fn started() -> String {
        json!({ "id": "cmp_1", "name": "weave-host", "state": "starting", "role": "weave_host" })
            .to_string()
    }

    fn methods(requests: &[String]) -> Vec<String> {
        requests
            .iter()
            .map(|r| r.split(" HTTP/").next().unwrap_or_default().to_owned())
            .collect()
    }

    #[test]
    fn cloud_computer_create_and_start_without_yes_send_no_post() {
        let me = instance("team");
        for (raw, terms) in [
            (
                &["create"][..],
                json!({ "billing": "free", "usd_per_hour": 0.09, "free_hours_left": 494.8, "topup_usd": 12.4 }),
            ),
            (
                &["start", "weave-host"][..],
                json!({ "billing": "free", "usd_per_hour": 0.09, "free_hours_left": 494.8, "topup_usd": 12.4 }),
            ),
        ] {
            let (origin, server) = serve(vec![(200, listed()), (200, "{}".into())]);
            let client = CloudClient::new(&origin).unwrap();
            let exchange = computers::on_client(&client);
            let a = args(raw);
            let ctx = Ctx {
                send: &exchange,
                instance: &me,
                args: &a,
                via: Via::Cli,
                attended: true,
            };
            let refused = if raw[0] == "create" {
                create(&ctx).err()
            } else {
                transition(&ctx, Action::Start).err()
            }
            .expect("refused without --yes");
            sentinel(&origin);
            let requests = server.join().unwrap();
            assert_eq!(
                methods(&requests),
                vec!["GET /v1/computers".to_owned(), "GET /sentinel".to_owned()],
                "{raw:?}: only the list was read, nothing was posted"
            );
            assert_eq!(refused.code, Code::ConfirmationRequired, "{raw:?}");
            assert_eq!(refused.details, Some(terms), "{raw:?}");
            assert!(
                refused.message.contains("Nothing was sent"),
                "{}",
                refused.message
            );
        }
    }

    #[test]
    fn cloud_computer_writes_with_yes_send_confirm_true() {
        let me = instance("team");
        // create --yes: one POST with the design's fields, no list read.
        let row =
            json!({ "id": "cmp_9", "name": "dev", "state": "provisioning", "role": "general" });
        let (origin, server) = serve(vec![(202, row.to_string())]);
        let client = CloudClient::new(&origin).unwrap();
        let exchange = computers::on_client(&client);
        let a = args(&[
            "create",
            "dev",
            "--role",
            "general",
            "--request-id",
            "req_create_01",
            "--yes",
        ]);
        let ctx = Ctx {
            send: &exchange,
            instance: &me,
            args: &a,
            via: Via::Cli,
            attended: true,
        };
        let write = create(&ctx).expect("confirmed");
        assert_eq!(write.preview["confirm"], true);
        let doc = (write.send)().expect("ack");
        let requests = server.join().unwrap();
        assert_eq!(methods(&requests), vec!["POST /v1/computers".to_owned()]);
        assert_eq!(
            sent(&requests[0]),
            json!({ "request_id": "req_create_01", "name": "dev",
                "team": "org_acme", "role": "general", "confirm": true })
        );
        assert_eq!(doc.id, "cmp_9");

        // start --yes: the list, then the start with confirm: true.
        let (origin, server) = serve(vec![(200, listed()), (202, started())]);
        let client = CloudClient::new(&origin).unwrap();
        let exchange = computers::on_client(&client);
        let a = args(&[
            "start",
            "weave-host",
            "--request-id",
            "req_start_001",
            "--yes",
        ]);
        let ctx = Ctx {
            send: &exchange,
            instance: &me,
            args: &a,
            via: Via::Cli,
            attended: true,
        };
        let doc = (transition(&ctx, Action::Start).expect("confirmed").send)().expect("ack");
        let requests = server.join().unwrap();
        assert_eq!(
            methods(&requests),
            vec![
                "GET /v1/computers".to_owned(),
                "POST /v1/computers/cmp_1/start".to_owned()
            ]
        );
        assert_eq!(
            sent(&requests[1]),
            json!({ "request_id": "req_start_001", "confirm": true })
        );
        assert_eq!(doc.state, "starting");
        assert_eq!(serde_json::to_value(&doc.billing).unwrap()["basis"], "free");
    }

    #[test]
    fn cloud_computer_a_cloud_428_after_yes_is_still_refused() {
        let me = instance("team");
        let asked = json!({ "error": { "code": "confirmation_required", "message": "confirm",
            "details": { "billing": "billed", "usd_per_hour": 0.09, "topup_usd": 3.0 } } });
        let (origin, server) = serve(vec![(200, listed()), (428, asked.to_string())]);
        let client = CloudClient::new(&origin).unwrap();
        let exchange = computers::on_client(&client);
        let a = args(&["start", "weave-host", "--yes"]);
        let ctx = Ctx {
            send: &exchange,
            instance: &me,
            args: &a,
            via: Via::Cli,
            attended: true,
        };
        let failure = (transition(&ctx, Action::Start).expect("confirmed").send)()
            .expect_err("the cloud's second guard");
        server.join().unwrap();
        assert_eq!(failure.code, Code::ConfirmationRequired);
        // --yes was given: the advice is not to repeat it.
        assert!(
            !failure.message.contains("repeat with --yes"),
            "{}",
            failure.message
        );
        assert!(
            failure.message.contains("asked for a new confirmation"),
            "{}",
            failure.message
        );
        assert_eq!(
            failure.details,
            Some(json!({ "billing": "billed", "usd_per_hour": 0.09,
                "free_hours_left": null, "topup_usd": 3.0 }))
        );
    }

    /// No provider yet: the list says `available: false`, so `create` and
    /// `start` say "not available yet" and show no terms; a write that
    /// still reaches the cloud gets `503 computer_not_available`, said the
    /// same way. A personal `weave_host` is refused before anything is sent.
    #[test]
    fn cloud_computer_not_available_comes_before_any_terms() {
        let off = json!({ "available": false, "computers": [] }).to_string();
        for (machine, raw) in [
            ("team", &["create"][..]),
            ("personal", &["create", "box"][..]),
            ("team", &["start", "weave-host"][..]),
        ] {
            let me = instance(machine);
            let (origin, server) = serve(vec![(200, off.clone()), (200, "{}".into())]);
            let client = CloudClient::new(&origin).unwrap();
            let exchange = computers::on_client(&client);
            let a = args(raw);
            let ctx = Ctx {
                send: &exchange,
                instance: &me,
                args: &a,
                via: Via::Cli,
                attended: true,
            };
            let failure = if raw[0] == "create" {
                create(&ctx).err()
            } else {
                transition(&ctx, Action::Start).err()
            }
            .expect("not available");
            sentinel(&origin);
            let requests = server.join().unwrap();
            assert_eq!(
                methods(&requests),
                vec!["GET /v1/computers".to_owned(), "GET /sentinel".to_owned()],
                "{raw:?}"
            );
            assert_eq!(failure.code, Code::ComputerNotAvailable, "{raw:?}");
            assert_eq!(failure.message, NOT_AVAILABLE, "{raw:?}");
            assert!(failure.details.is_none(), "{raw:?}: no terms");
        }
        // create --yes: the POST answers 503 computer_not_available.
        let me = instance("team");
        let gone =
            json!({ "error": { "code": "computer_not_available", "message": "no provider" } });
        let (origin, server) = serve(vec![(503, gone.to_string())]);
        let client = CloudClient::new(&origin).unwrap();
        let exchange = computers::on_client(&client);
        let a = args(&["create", "--yes"]);
        let ctx = Ctx {
            send: &exchange,
            instance: &me,
            args: &a,
            via: Via::Cli,
            attended: true,
        };
        let failure = (create(&ctx).expect("confirmed").send)().expect_err("not available");
        server.join().unwrap();
        assert_eq!(failure.code, Code::ComputerNotAvailable);
        assert_eq!(failure.message, NOT_AVAILABLE);
        // A personal weave_host: refused before even the list.
        let me = instance("personal");
        let (origin, server) = serve(vec![(200, listed())]);
        let client = CloudClient::new(&origin).unwrap();
        let exchange = computers::on_client(&client);
        let a = args(&["create", "--role", "weave_host"]);
        let ctx = Ctx {
            send: &exchange,
            instance: &me,
            args: &a,
            via: Via::Cli,
            attended: true,
        };
        let failure = create(&ctx).err().expect("refused");
        sentinel(&origin);
        assert_eq!(
            methods(&server.join().unwrap()),
            vec!["GET /sentinel".to_owned()]
        );
        assert_eq!(failure.code, Code::InvalidArgument);
    }

    /// Owner rule: a paid create or start needs a person. Without a
    /// terminal and not from LeanCTX Code it is refused before anything is
    /// sent, even with --yes; not even the list is read.
    #[test]
    fn cloud_computer_paid_writes_without_a_person_send_nothing() {
        let me = instance("team");
        for raw in [
            &["create", "--yes"][..],
            &["create"][..],
            &["start", "weave-host", "--yes"][..],
            &["start", "weave-host"][..],
        ] {
            let (origin, server) = serve(vec![(200, "{}".into())]);
            let client = CloudClient::new(&origin).unwrap();
            let exchange = computers::on_client(&client);
            let a = args(raw);
            let ctx = Ctx {
                send: &exchange,
                instance: &me,
                args: &a,
                via: Via::Cli,
                attended: attended(Via::Cli, false),
            };
            let refused = if raw[0] == "create" {
                create(&ctx).err()
            } else {
                transition(&ctx, Action::Start).err()
            }
            .expect("refused without a person");
            sentinel(&origin);
            let requests = server.join().unwrap();
            assert_eq!(
                methods(&requests),
                vec!["GET /sentinel".to_owned()],
                "{raw:?}: nothing was sent"
            );
            assert_eq!(refused.code, Code::ConfirmationRequired, "{raw:?}");
            assert_eq!(refused.message, UNATTENDED, "{raw:?}");
            assert_eq!(
                refused.message,
                "Starting a paid computer needs you at a terminal or in LeanCTX Code."
            );
        }
        // stop costs nothing: it runs without a person.
        let (origin, server) = serve(vec![(200, listed())]);
        let client = CloudClient::new(&origin).unwrap();
        let exchange = computers::on_client(&client);
        let a = args(&["stop", "weave-host"]);
        let ctx = Ctx {
            send: &exchange,
            instance: &me,
            args: &a,
            via: Via::Cli,
            attended: false,
        };
        assert!(transition(&ctx, Action::Stop).is_ok());
        assert_eq!(
            methods(&server.join().unwrap()),
            vec!["GET /v1/computers".to_owned()]
        );
    }

    /// LeanCTX Code calls over the 0600 socket: taken as the person, so a
    /// confirmed start is sent without a terminal (a guard against
    /// accidental agent calls, not a security boundary).
    #[test]
    fn cloud_computer_app_calls_count_as_the_person() {
        assert!(attended(Via::Socket, false));
        assert!(attended(Via::Cli, true));
        assert!(!attended(Via::Cli, false));
        let me = instance("team");
        let (origin, server) = serve(vec![(200, listed()), (202, started())]);
        let client = CloudClient::new(&origin).unwrap();
        let exchange = computers::on_client(&client);
        let a = args(&["start", "weave-host", "--json", "--yes"]);
        let ctx = Ctx {
            send: &exchange,
            instance: &me,
            args: &a,
            via: Via::Socket,
            attended: attended(Via::Socket, false),
        };
        let doc = (transition(&ctx, Action::Start)
            .expect("the app's start")
            .send)()
        .expect("ack");
        let requests = server.join().unwrap();
        assert_eq!(
            methods(&requests),
            vec![
                "GET /v1/computers".to_owned(),
                "POST /v1/computers/cmp_1/start".to_owned()
            ]
        );
        assert_eq!(sent(&requests[1])["confirm"], true);
        assert_eq!(doc.state, "starting");
        // Without yes the app gets the terms, and nothing is posted.
        let (origin, server) = serve(vec![(200, listed()), (200, "{}".into())]);
        let client = CloudClient::new(&origin).unwrap();
        let exchange = computers::on_client(&client);
        let a = args(&["create", "--json"]);
        let ctx = Ctx {
            send: &exchange,
            instance: &me,
            args: &a,
            via: Via::Socket,
            attended: attended(Via::Socket, false),
        };
        let refused = create(&ctx).err().expect("terms first");
        sentinel(&origin);
        let requests = server.join().unwrap();
        assert_eq!(
            methods(&requests),
            vec!["GET /v1/computers".to_owned(), "GET /sentinel".to_owned()]
        );
        assert_eq!(refused.code, Code::ConfirmationRequired);
        assert_eq!(refused.details.expect("terms")["billing"], "free");
    }
}
