// SPDX-License-Identifier: Apache-2.0
//! T-CONSENT-WEB, engine side: pairing code, E35 create and poll, the exact
//! line, and only verified statements stored.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signer as _, SigningKey};

use super::*;
use crate::core::hive::consent::broker::Trust;

const NOW: u64 = 1_792_800_000_000;
const OWNER: &str = "2d4f6a8c-0e1b-4c3d-8e5f-7a9b1c3d5e7f";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn hive_consent_key() -> (String, SigningKey) {
    let key = &fixture("vectors/signing.vectors.json")["keys"]["hive_consent"];
    let mut seed = [0u8; 32];
    hex::decode_to_slice(key["seed_hex"].as_str().unwrap(), &mut seed).unwrap();
    (
        key["key_id"].as_str().unwrap().into(),
        SigningKey::from_bytes(&seed),
    )
}

fn trust() -> Trust {
    let (id, key) = hive_consent_key();
    Trust {
        presence: None,
        consent_keys: std::collections::BTreeMap::from([(id, key.verifying_key())]),
        owner_user_id: Some(OWNER.into()),
    }
}

fn statement(record: &Record, answer: &str, at: u64, signer: &SigningKey) -> Value {
    let (key_id, _) = hive_consent_key();
    let mut doc = json!({
        "schema": "leanctx.hive.consent.v1",
        "consent_id": record.consent_id,
        "subject_digest": record.subject_digest,
        "answer": answer,
        "answered_at": at,
        "user_id": OWNER,
        "key_id": key_id,
    });
    let input =
        lean_ctx_protocol::hive::signing_input(super::super::STATEMENT_DOMAIN, &doc).unwrap();
    doc["signature"] = hex::encode(signer.sign(&input).to_bytes()).into();
    doc
}

type Handler = dyn Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync;

struct Fake {
    origin: String,
    requests: Arc<Mutex<Vec<(String, String, Value)>>>,
}

impl Fake {
    fn start(handler: impl Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync + 'static) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let handler: Arc<Handler> = Arc::new(handler);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorded = Arc::clone(&recorded);
                let handler = Arc::clone(&handler);
                std::thread::spawn(move || serve(stream, &recorded, handler.as_ref()));
            }
        });
        Self { origin, requests }
    }
}

fn serve(
    stream: std::net::TcpStream,
    recorded: &Mutex<Vec<(String, String, Value)>>,
    handler: &Handler,
) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let path = path.strip_prefix("/v1").unwrap_or(&path).to_owned();
        let (status, answer) = handler(&method, &path, &body);
        recorded.lock().unwrap().push((method, path, body));
        let text = answer.to_string();
        let _ = write!(
            writer,
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}",
            text.len()
        );
    }
}

fn pending_request(dir: &tempfile::TempDir) -> (Broker, Record) {
    let store = crate::core::hive::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let broker = Broker::open(&store, trust());
    let subject = fixture("vectors/consent_subject.vectors.json")["cases"][0]["subject"].clone();
    let id = match broker.require(Kind::LinkVault, &subject, NOW) {
        Err(ConsentError::Required { consent_id, .. }) => consent_id,
        other => panic!("{other:?}"),
    };
    let record = broker.get(&id).unwrap();
    (broker, record)
}

#[test]
fn pairing_codes_and_hashes_match_the_vectors() {
    for case in fixture("vectors/consent_code.vectors.json")["cases"]
        .as_array()
        .unwrap()
    {
        let code = case["code"].as_str().unwrap();
        let id = case["consent_id"].as_str().unwrap();
        assert_eq!(code_hash(id, code), case["code_hash"].as_str().unwrap());
        // The displayed form, in any case, hashes the same.
        assert_eq!(
            code_hash(id, case["display"].as_str().unwrap()),
            case["code_hash"].as_str().unwrap()
        );
    }
    let code = new_code().unwrap();
    assert_eq!(code.len(), 8);
    assert!(code.bytes().all(|b| CROCKFORD.contains(&b)), "{code}");
    assert_eq!(display("7KQ2M9XA"), "7KQ2-M9XA");
}

#[test]
fn the_create_body_has_the_contract_shape() {
    let request = fixture("routes/consent_create.request.json");
    let record = Record {
        consent_id: request["consent_id"].as_str().unwrap().into(),
        kind: Kind::SwarmJoin,
        subject: request["subject"].clone(),
        subject_digest: request["subject_digest"].as_str().unwrap().into(),
        created_at: 0,
        expires_at: request["expires_at"].as_u64().unwrap(),
        state: State::Pending,
        channel: None,
        approved_at: None,
        proof: None,
    };
    let now = request["code_expires_at"].as_u64().unwrap() - CODE_TTL_MS;
    assert_eq!(create_body(&record, "7KQ2M9XA", now), request);
    assert_eq!(
        output_line(
            "https://hive.leanctx.com/consent/cns_abcdefgh234567ab/",
            "7KQ2M9XA",
            &record
        ),
        "Confirm it in your browser:\n  https://hive.leanctx.com/consent/cns_abcdefgh234567ab/#k=7KQ2-M9XA\nIf the page asks for a code, it is 7KQ2-M9XA (valid for 10 minutes)."
    );
}

#[test]
fn states_parse() {
    assert_eq!(
        parse_state(&fixture("routes/consent_statement.pending.response.json")).unwrap(),
        Polled::Pending
    );
    let answered = fixture("routes/consent_statement.response.json");
    assert!(matches!(
        parse_state(&answered).unwrap(),
        Polled::Answered(_)
    ));
    assert!(parse_state(&json!({"state": "approved"})).is_err());
}

/// What a `--web` run left: its result, the printed lines, the requests
/// Hive saw and the stored record.
type WebOutcome = (
    Result<Record, ConsentError>,
    Vec<String>,
    Vec<(String, String, Value)>,
    Record,
);

fn run_against(answer: impl Fn(&Record, u64) -> Value + Send + Sync + 'static) -> WebOutcome {
    let dir = tempfile::tempdir().unwrap();
    let (broker, record) = pending_request(&dir);
    let polls = Arc::new(AtomicU64::new(0));
    let seen = record.clone();
    let counter = Arc::clone(&polls);
    let fake = Fake::start(move |method, path, _| match method {
        "POST" => (
            201,
            json!({"url": format!("https://hive.example/consent/{}/", seen.consent_id)}),
        ),
        _ if path.ends_with(&seen.consent_id) => {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            (200, answer(&seen, n))
        }
        _ => (404, json!({"error": {"code": "not_found"}})),
    });
    let client = HiveClient::new(&fake.origin).unwrap();
    let clock = AtomicU64::new(NOW + 1);
    let now = || clock.load(Ordering::SeqCst);
    let sleep = |d: Duration| {
        clock.fetch_add(u64::try_from(d.as_millis()).unwrap(), Ordering::SeqCst);
    };
    let data = dir.path().join("d");
    let run = WebRun {
        broker: &broker,
        data_dir: &data,
        now: &now,
        sleep: &sleep,
        open: &|_| {},
    };
    let mut printed = Vec::new();
    let result = run.run(&client, &record.consent_id, &mut |line| {
        printed.push(line.to_string());
    });
    let requests = fake.requests.lock().unwrap().clone();
    let stored = broker.list().unwrap().remove(0).record;
    (result, printed, requests, stored)
}

#[test]
fn an_allow_statement_is_stored_after_polling() {
    let (_, signer) = hive_consent_key();
    let (result, printed, requests, stored) = run_against(move |record, n| {
        if n < 2 {
            json!({"state": "pending"})
        } else {
            json!({"state": "approved", "statement": statement(record, "allow", NOW + 5_000, &signer)})
        }
    });
    let record = result.unwrap();
    assert_eq!(record.state, State::Approved);
    assert_eq!(record.channel, Some(super::super::Channel::Web));
    assert_eq!(record.approved_at, Some(NOW + 5_000));
    assert_eq!(stored, record);
    assert_eq!(printed.len(), 1);
    // The link carries the pairing code in its fragment (the browser fills it in).
    assert!(printed[0].starts_with("Confirm it in your browser:\n  "));
    assert!(printed[0].contains("#k="), "{}", printed[0]);
    // 161b0175f5: one GET first (a browser answer from an interrupted
    // wait is taken as is), then the create.
    assert_eq!(
        requests[0].0, "GET",
        "an earlier answer is looked for first"
    );
    let created = &requests
        .iter()
        .find(|r| r.0 == "POST")
        .expect("the request is created")
        .2;
    assert_eq!(
        created["subject"], record.subject,
        "the stored subject is sent"
    );
    assert_eq!(created["subject_digest"], record.subject_digest.as_str());
    assert_eq!(requests.iter().filter(|r| r.0 == "GET").count(), 3);
}

#[test]
fn a_statement_by_another_key_is_refused_and_nothing_is_stored() {
    let forger = SigningKey::from_bytes(&[7u8; 32]);
    let (result, _, _, stored) = run_against(
        move |record, _| json!({"state": "approved", "statement": statement(record, "allow", NOW + 5_000, &forger)}),
    );
    assert_eq!(result, Err(ConsentError::RecordInvalid));
    assert_eq!(stored.state, State::Pending);
}

#[test]
fn a_deny_statement_is_stored_as_denied() {
    let (_, signer) = hive_consent_key();
    let (result, _, _, stored) = run_against(
        move |record, _| json!({"state": "denied", "statement": statement(record, "deny", NOW + 9_000, &signer)}),
    );
    assert_eq!(result.unwrap().state, State::Denied);
    assert_eq!(stored.state, State::Denied);
}

#[test]
fn no_answer_within_ten_minutes_times_out_and_stays_pending() {
    let (result, _, requests, stored) = run_against(|_, _| json!({"state": "pending"}));
    assert_eq!(result, Err(ConsentError::WebTimeout));
    assert_eq!(stored.state, State::Pending);
    let polls = requests.iter().filter(|r| r.0 == "GET").count();
    assert!(
        (290..=310).contains(&polls),
        "every 2 s for 10 min: {polls}"
    );
}

#[test]
fn api_keys_never_reach_the_relay() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::core::hive::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let broker = Broker::open(&store, trust());
    let id = match broker.require(
        Kind::ApiKeySet,
        &json!({"cli": "codex", "vendor": "openai"}),
        NOW,
    ) {
        Err(ConsentError::Required { consent_id, .. }) => consent_id,
        other => panic!("{other:?}"),
    };
    let fake = Fake::start(|_, _, _| (500, json!({})));
    let client = HiveClient::new(&fake.origin).unwrap();
    let now = || NOW + 1;
    let sleep = |_: Duration| {};
    let data = dir.path().join("d");
    let run = WebRun {
        broker: &broker,
        data_dir: &data,
        now: &now,
        sleep: &sleep,
        open: &|_| {},
    };
    assert_eq!(
        run.run(&client, &id, &mut |_| {}),
        Err(ConsentError::TerminalOnly)
    );
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[test]
fn an_answered_frame_leaves_a_marker_for_the_waiting_command() {
    let dir = tempfile::tempdir().unwrap();
    let frame = fixture("bus/frame.consent_answered.json");
    let Ok(crate::core::hive::bus::v2::Inbound::ConsentAnswered { consent_id }) =
        crate::core::hive::bus::v2::parse_frame(&frame.to_string())
    else {
        panic!("consent.answered parses");
    };
    note_answered(dir.path(), &consent_id);
    let marker = answered_marker(dir.path(), &consent_id).unwrap();
    assert!(marker.exists());
    note_answered(dir.path(), "../../x");
    assert!(answered_marker(dir.path(), "../../x").is_none());
}

/// C0 2.1: a first share bound to the device approval page carries no
/// pairing code (the contract fixture's shape), waits for that page and
/// stores only the Hive-signed answer.
#[test]
fn a_device_bound_first_share_has_no_code_and_waits_for_the_page() {
    let dir = tempfile::tempdir().unwrap();
    let (broker, record) = pending_request(&dir);
    let body = device_body(&record);
    let fixture = fixture("routes/consent_create.device_binding.request.json");
    let keys = |v: &Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&body), keys(&fixture));
    assert_eq!(body["binding"], "device");
    let (_, signer) = hive_consent_key();
    let polls = Arc::new(AtomicU64::new(0));
    let seen = record.clone();
    let counter = Arc::clone(&polls);
    let fake = Fake::start(move |method, path, _| match method {
        "POST" => (
            201,
            json!({"url": format!("https://hive.example/consent/{}/", seen.consent_id)}),
        ),
        _ if path.ends_with(&seen.consent_id) => {
            if counter.fetch_add(1, Ordering::SeqCst) < 1 {
                (200, json!({"state": "pending"}))
            } else {
                (
                    200,
                    json!({"state": "approved", "statement": statement(&seen, "allow", NOW + 3_000, &signer)}),
                )
            }
        }
        _ => (404, json!({"error": {"code": "not_found"}})),
    });
    let client = HiveClient::new(&fake.origin).unwrap();
    let clock = AtomicU64::new(NOW + 1);
    let now = || clock.load(Ordering::SeqCst);
    let sleep = |d: Duration| {
        clock.fetch_add(u64::try_from(d.as_millis()).unwrap(), Ordering::SeqCst);
    };
    let data = dir.path().join("d");
    let run = WebRun {
        broker: &broker,
        data_dir: &data,
        now: &now,
        sleep: &sleep,
        open: &|_| {},
    };
    let mut waited = 0;
    let stored = run
        .run_device(&client, &record.consent_id, &mut || waited += 1)
        .unwrap();
    assert_eq!(stored.state, State::Approved);
    assert_eq!(waited, 1, "said once that the page has a question");
    let requests = fake.requests.lock().unwrap().clone();
    let created = &requests[0].2;
    assert_eq!(created["binding"], "device");
    assert!(created.get("code_hash").is_none(), "no pairing code");
}
