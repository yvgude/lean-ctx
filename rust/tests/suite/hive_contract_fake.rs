// SPDX-License-Identifier: Apache-2.0

//! The fake Hive service speaks the C0 contract: it accepts the golden
//! request fixtures, answers with bodies that decode into the protocol types,
//! and enforces the contract's errors.
//!
//! `HIVE_CONFORMANCE_URL` (+ `HIVE_CONFORMANCE_ROOT_KEY`, base64 Ed25519)
//! runs the unauthenticated part against a real Hive deployment instead.

use crate::support::fake_hive::{FakeHive, FakeOptions};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use lean_ctx_protocol::hive::bus::*;
use lean_ctx_protocol::hive::enroll::*;
use lean_ctx_protocol::hive::knowledge::*;
use lean_ctx_protocol::hive::lease::*;
use lean_ctx_protocol::hive::policy::*;
use lean_ctx_protocol::hive::telemetry::*;
use lean_ctx_protocol::hive::vault::*;
use lean_ctx_protocol::hive::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

const INSTANCE_SEED: [u8; 32] = [0x49; 32];

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture")).expect("fixture json")
}

struct Client {
    base: String,
    agent: ureq::Agent,
    token: Option<String>,
    instance: Option<String>,
}

static KEYS: AtomicU64 = AtomicU64::new(0);

impl Client {
    fn new(base: &str) -> Self {
        let agent: ureq::Agent = ureq::config::Config::builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        Self {
            base: format!("{base}{HIVE_API_PREFIX}"),
            agent,
            token: None,
            instance: None,
        }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
    ) -> (u16, Value) {
        let url = format!("{}{path}", self.base);
        let key = format!("01jb8z{:020}", KEYS.fetch_add(1, Ordering::Relaxed));
        let mut headers: Vec<(String, String)> = vec![
            (HEADER_PROTOCOL.into(), protocol_header_value()),
            (HEADER_NAMES.into(), NAMES_VAULT.into()),
            ("User-Agent".into(), "lean-ctx/3.12.0".into()),
        ];
        if let (Some(token), Some(instance)) = (&self.token, &self.instance) {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
            headers.push((HEADER_INSTANCE.into(), instance.clone()));
        }
        if method == "POST" {
            headers.push((HEADER_IDEMPOTENCY_KEY.into(), key));
        }
        for (name, value) in extra {
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            if !value.is_empty() {
                headers.push(((*name).into(), (*value).into()));
            }
        }
        let response = match method {
            "GET" | "DELETE" => {
                let mut request = if method == "GET" {
                    self.agent.get(&url)
                } else {
                    self.agent.delete(&url)
                };
                for (name, value) in &headers {
                    request = request.header(name, value);
                }
                request.call()
            }
            _ => {
                let mut request = if method == "PUT" {
                    self.agent.put(&url)
                } else {
                    self.agent.post(&url)
                };
                for (name, value) in &headers {
                    request = request.header(name, value);
                }
                match body {
                    Some(body) => request
                        .header("Content-Type", "application/json")
                        .send(serde_json::to_vec(body).expect("json body").as_slice()),
                    None => request.send_empty(),
                }
            }
        }
        .expect("request reaches the service");
        let status = response.status().as_u16();
        let text = response.into_body().read_to_string().unwrap_or_default();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        self.call("POST", path, Some(body), &[])
    }

    fn get(&self, path: &str) -> (u16, Value) {
        self.call("GET", path, None, &[])
    }
}

fn decode<T: DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap_or_else(|error| panic!("{error}: {value}"))
}

fn error_code(value: &Value) -> HiveErrorCode {
    decode::<HiveErrorBody>(value).error.code
}

fn verify(public_b64: &str, message: &[u8], signature_b64: &str) -> bool {
    let key: [u8; 32] = B64.decode(public_b64).unwrap().try_into().unwrap();
    let signature: [u8; 64] = B64.decode(signature_b64).unwrap().try_into().unwrap();
    VerifyingKey::from_bytes(&key)
        .unwrap()
        .verify(message, &Signature::from_bytes(&signature))
        .is_ok()
}

fn device_token_request(device_code: &str) -> Value {
    let mut request = fixture("device_token.request");
    request["device_code"] = Value::from(device_code);
    request
}

/// Device flow → verified enrollment → access token. Returns the client and enrollment.
fn connect(fake: &FakeHive) -> (Client, EnrollmentV1) {
    let mut client = Client::new(&fake.url);
    let (status, code) = client.post("/device/code", &fixture("device_code.request"));
    assert_eq!(status, 200, "{code}");
    let code: DeviceCodeV1 = decode(&code);
    code.validate().unwrap();
    let (status, pending) = client.post("/device/token", &device_token_request(&code.device_code));
    assert_eq!(
        (status, error_code(&pending)),
        (400, HiveErrorCode::AuthorizationPending)
    );
    let (status, enrollment) =
        client.post("/device/token", &device_token_request(&code.device_code));
    assert_eq!(status, 200, "{enrollment}");
    let keys_raw = enrollment["keys"].clone();
    let enrollment: EnrollmentV1 = decode(&enrollment);
    enrollment.validate().unwrap();
    assert!(verify(
        &FakeHive::root_public_key_b64(),
        &HiveKeySetV1::signing_input_raw(&keys_raw).unwrap(),
        &enrollment.keys.signature
    ));
    let signer = enrollment
        .keys
        .key(&enrollment.lease.kid)
        .expect("lease kid listed");
    assert!(verify(
        &signer.ed25519_pub,
        &enrollment.lease.signing_input().unwrap(),
        &enrollment.lease.signature
    ));
    let lease = enrollment.lease.lease().unwrap();
    assert_eq!(lease.instance_id, enrollment.instance_id);

    let iat = fake.now().timestamp();
    let nonce = format!("nonce-{}", enrollment.instance_id);
    let assertion = SigningKey::from_bytes(&INSTANCE_SEED).sign(&token_assertion_input(
        &enrollment.instance_id,
        iat,
        &nonce,
    ));
    let request = json!({
        "schema": TOKEN_REQUEST_SCHEMA, "iat": iat, "nonce": nonce,
        "assertion": B64.encode(assertion.to_bytes()),
    });
    let path = format!("/instances/{}/token", enrollment.instance_id);
    let (status, token) = client.post(&path, &request);
    assert_eq!(status, 200, "{token}");
    let token: AccessTokenV1 = decode(&token);
    token.validate().unwrap();
    let (status, replay) = client.call(
        "POST",
        &path,
        Some(&request),
        &[(HEADER_IDEMPOTENCY_KEY, "01jb8zreplayednonce000000")],
    );
    assert_eq!(
        (status, error_code(&replay)),
        (401, HiveErrorCode::Unauthorized),
        "nonce is single use"
    );
    client.token = Some(token.access_token);
    client.instance = Some(enrollment.instance_id.clone());
    (client, enrollment)
}

fn resolve(client: &Client) -> VaultV1 {
    let (status, salt) = client.get("/org/salt");
    assert_eq!(status, 200, "{salt}");
    decode::<OrgSaltV1>(&salt).validate().unwrap();
    let (status, project) = client.post("/vaults/resolve", &fixture("vault_resolve.request"));
    assert_eq!(status, 200, "{project}");
    let project: VaultV1 = decode(&project);
    project.validate().unwrap();
    project
}

#[test]
fn engine_flow_round_trips_every_contract_fixture() {
    let fake = FakeHive::start_with(FakeOptions {
        rights: vec![
            HiveRight::KnowledgeRead,
            HiveRight::KnowledgeDraft,
            HiveRight::KnowledgeMerge,
            HiveRight::MetricsUpload,
            HiveRight::ReceiptsUpload,
            HiveRight::PolicyPull,
            HiveRight::BusRelay,
        ],
        ..FakeOptions::default()
    });
    let (client, _) = connect(&fake);
    let project = resolve(&client);
    let pid = project.vault_id.clone();
    assert_eq!(resolve(&client).vault_id, pid, "resolution is stable");

    let (status, revision) = client.get(&format!("/vaults/{pid}/knowledge"));
    assert_eq!(status, 200, "{revision}");
    let revision: KnowledgeRevisionV1 = decode(&revision);
    revision.validate().unwrap();
    assert_eq!(revision.revision, 0);
    let (status, _) = client.call(
        "GET",
        &format!("/vaults/{pid}/knowledge"),
        None,
        &[("If-None-Match", "\"0\"")],
    );
    assert_eq!(status, 304);

    let mut put = fixture("knowledge_put.request");
    let (status, conflict) =
        client.call("PUT", &format!("/vaults/{pid}/knowledge"), Some(&put), &[]);
    let detail = decode::<HiveErrorBody>(&conflict).error;
    assert_eq!(
        (status, detail.code, detail.expected, detail.actual),
        (409, HiveErrorCode::RevisionConflict, Some(3), Some(0))
    );
    put["expected_revision"] = Value::from(0);
    let (status, merged) = client.call("PUT", &format!("/vaults/{pid}/knowledge"), Some(&put), &[]);
    assert_eq!(status, 200, "{merged}");
    assert_eq!(decode::<KnowledgeRevisionV1>(&merged).revision, 1);
    for _ in 0..2 {
        fake.publish_revision(&pid, put["document"].clone());
    }

    let (status, ack) = client.post(
        &format!("/vaults/{pid}/proposals"),
        &fixture("proposal.request"),
    );
    assert_eq!(status, 200, "{ack}");
    assert_eq!(decode::<ProposalAckV1>(&ack).state, ProposalState::Draft);
    let (status, list) = client.get(&format!("/vaults/{pid}/proposals?state=draft&mine=1"));
    assert_eq!(status, 200);
    let list: ProposalListV1 = decode(&list);
    assert_eq!(list.proposals.len(), 1);
    assert!(list.proposals[0].mine);
    let (status, revisions) = client.get(&format!(
        "/vaults/{pid}/knowledge/revisions?after=1&limit=1"
    ));
    assert_eq!(status, 200);
    let revisions: RevisionListV1 = decode(&revisions);
    revisions.validate().unwrap();
    assert_eq!(
        (revisions.revisions[0].revision, revisions.next_after),
        (2, Some(2))
    );

    let mut metrics = fixture("metrics.request");
    metrics["days"][0]["vault"]["id"] = Value::from(pid.clone());
    for _ in 0..2 {
        let (status, ack) = client.post("/metrics", &metrics);
        assert_eq!((status, decode::<MetricsAckV1>(&ack).accepted), (200, 1));
    }
    assert_eq!(
        fake.metrics_rows(),
        1,
        "metrics replace per device, day and project"
    );

    let mut receipts = fixture("receipts.request");
    receipts["receipts"][0]["vault_id"] = Value::from(pid.clone());
    let (_, first) = client.post("/receipts", &receipts);
    let (_, second) = client.post("/receipts", &receipts);
    assert_eq!(decode::<ReceiptsAckV1>(&first).accepted, 1);
    assert_eq!(decode::<ReceiptsAckV1>(&second).duplicates, 1);

    let (status, bundle) = client.get("/policy/bundle?since=0");
    assert_eq!(status, 200);
    decode::<PolicyBundleV1>(&bundle).validate().unwrap();

    let mut lease_request = fixture("bus_lease.request");
    lease_request["vault_id"] = Value::from(pid.clone());
    let (_, granted) = client.post("/bus/leases", &lease_request);
    let granted: BusLeaseV1 = decode(&granted);
    granted.validate().unwrap();
    assert!(granted.granted);
    lease_request["owner_agent_id"] = Value::from("codex-85");
    let (_, refused) = client.post("/bus/leases", &lease_request);
    let refused: BusLeaseV1 = decode(&refused);
    refused.validate().unwrap();
    assert_eq!(
        refused.holder.map(|h| h.agent_id).as_deref(),
        Some("claude-3e")
    );
    fake.advance(chrono::Duration::minutes(11));
    let (_, after_expiry) = client.post("/bus/leases", &lease_request);
    let after_expiry: BusLeaseV1 = decode(&after_expiry);
    assert!(after_expiry.granted, "the service clock expires leases");
    assert!(after_expiry.fencing_token > granted.fencing_token);

    let channel = format!("org:org_acme/vault:{pid}/general");
    fake.inject_event(&channel, "codex-85", "status", &json!({"text": "hello"}));
    let (status, events) = client.get(&format!(
        "/bus/channels/{}/events?after_seq=0",
        urlencoding::encode(&channel)
    ));
    assert_eq!(status, 200, "{events}");
    decode::<BusEventsV1>(&events).validate().unwrap();
    let (_, poll) = client.get("/bus/poll?cursor=c_0");
    assert_eq!(decode::<BusPollV1>(&poll).frames.len(), 1);

    let (status, renewed) = client.post(
        &format!(
            "/instances/{}/lease/renew",
            client.instance.as_ref().unwrap()
        ),
        &json!({}),
    );
    assert_eq!(status, 200, "{renewed}");
    decode::<HiveLeaseEnvelopeV1>(&renewed).lease().unwrap();
}

#[test]
fn contract_errors_are_enforced() {
    let fake = FakeHive::start();
    let (client, _) = connect(&fake);

    let mut unknown = fixture("vault_resolve.request");
    unknown["unexpected_field"] = Value::from(true);
    let (status, body) = client.post("/vaults/resolve", &unknown);
    assert_eq!(
        (status, error_code(&body)),
        (400, HiveErrorCode::InvalidRequest)
    );

    let (status, body) = client.call(
        "POST",
        "/vaults/resolve",
        Some(&fixture("vault_resolve.request")),
        &[(HEADER_IDEMPOTENCY_KEY, "")],
    );
    assert_eq!(
        (status, error_code(&body)),
        (400, HiveErrorCode::InvalidRequest),
        "Idempotency-Key is required"
    );

    let (status, body) = client.call("GET", "/org/salt", None, &[(HEADER_PROTOCOL, "2.0")]);
    assert_eq!(
        (status, error_code(&body)),
        (426, HiveErrorCode::UpgradeRequired)
    );

    let (status, body) = client.call(
        "GET",
        "/org/salt",
        None,
        &[("Authorization", "Bearer nope")],
    );
    assert_eq!(
        (status, error_code(&body)),
        (401, HiveErrorCode::Unauthorized)
    );

    let project = resolve(&client);
    let (status, body) = client.call(
        "PUT",
        &format!("/vaults/{}/knowledge", project.vault_id),
        Some(&fixture("knowledge_put.request")),
        &[],
    );
    assert_eq!(
        (status, error_code(&body)),
        (403, HiveErrorCode::ForbiddenRight),
        "default lease has no knowledge.merge"
    );

    let key = [(HEADER_IDEMPOTENCY_KEY, "01jb8zsamekey00000000000000")];
    let proposal = fixture("proposal.request");
    let path = format!("/vaults/{}/proposals", project.vault_id);
    let (status, ahead) = client.post(&path, &proposal);
    assert_eq!(
        (status, error_code(&ahead)),
        (409, HiveErrorCode::RevisionConflict),
        "a draft cannot be based on a revision the service has not issued"
    );
    for _ in 0..3 {
        fake.publish_revision(
            &project.vault_id,
            fixture("knowledge_put.request")["document"].clone(),
        );
    }
    let (status, first) = client.call("POST", &path, Some(&proposal), &key);
    assert_eq!(status, 200, "{first}");
    let (_, replay) = client.call("POST", &path, Some(&proposal), &key);
    assert_eq!(
        first, replay,
        "a replayed Idempotency-Key returns the first answer"
    );
    assert_eq!(fake.proposals(&project.vault_id).len(), 1);

    fake.advance(chrono::Duration::hours(2));
    let (status, body) = client.get("/org/salt");
    assert_eq!(
        (status, error_code(&body)),
        (401, HiveErrorCode::Unauthorized),
        "access tokens expire"
    );
}

#[test]
fn resolve_binds_to_a_web_repo_only_when_the_instance_may_contribute() {
    let fake = FakeHive::start();
    let (client, _) = connect(&fake);
    let request = fixture("vault_resolve.ref.request");
    let (status, body) = client.post("/vaults/resolve", &request);
    assert_eq!(
        (status, error_code(&body)),
        (403, HiveErrorCode::ForbiddenRight)
    );
    fake.create_web_repo("acme", "payments", false);
    let (status, _) = client.post("/vaults/resolve", &request);
    assert_eq!(status, 403, "a read-only repo cannot be bound");
    fake.create_web_repo("acme", "payments", true);
    let (status, body) = client.post("/vaults/resolve", &request);
    assert_eq!(status, 200, "{body}");
    let project: VaultV1 = decode(&body);
    project.validate().unwrap();
    assert_eq!(
        (project.vault.ns.as_str(), project.vault.name.as_str()),
        ("acme", "payments")
    );
}

#[test]
fn seats_plans_and_lease_lapse() {
    let fake = FakeHive::start_with(FakeOptions {
        seats_limit: 1,
        ..FakeOptions::default()
    });
    let (first, enrollment) = connect(&fake);
    let second = Client::new(&fake.url);
    let (_, code) = second.post("/device/code", &fixture("device_code.request"));
    let code: DeviceCodeV1 = decode(&code);
    second.post("/device/token", &device_token_request(&code.device_code));
    let (status, body) = second.post("/device/token", &device_token_request(&code.device_code));
    let detail = decode::<HiveErrorBody>(&body).error;
    assert_eq!(
        (status, detail.code, detail.limit, detail.used),
        (409, HiveErrorCode::InstanceLimit, Some(1), Some(1))
    );

    let (status, _) = first.call(
        "DELETE",
        &format!("/instances/{}", enrollment.instance_id),
        None,
        &[],
    );
    assert_eq!(status, 204);
    assert_eq!(fake.seats_used(), 0, "disconnect frees the seat");

    let (_, enrollment) = connect(&fake);
    let lease = enrollment.lease.lease().unwrap();
    let grace_end = chrono::DateTime::parse_from_rfc3339(lease.grace_until.as_str()).unwrap();
    fake.set_now(grace_end.with_timezone(&chrono::Utc));
    let iat = fake.now().timestamp();
    let assertion = SigningKey::from_bytes(&INSTANCE_SEED).sign(&token_assertion_input(
        &enrollment.instance_id,
        iat,
        "nonce-after-grace",
    ));
    let (status, body) = second.post(
        &format!("/instances/{}/token", enrollment.instance_id),
        &json!({"schema": TOKEN_REQUEST_SCHEMA, "iat": iat, "nonce": "nonce-after-grace", "assertion": B64.encode(assertion.to_bytes())}),
    );
    assert_eq!(
        (status, error_code(&body)),
        (401, HiveErrorCode::LeaseExpired)
    );

    let no_plan = FakeHive::start_with(FakeOptions {
        plan: None,
        approve_after_polls: 0,
        ..FakeOptions::default()
    });
    let client = Client::new(&no_plan.url);
    let (_, code) = client.post("/device/code", &fixture("device_code.request"));
    let code: DeviceCodeV1 = decode(&code);
    let (status, body) = client.post("/device/token", &device_token_request(&code.device_code));
    assert_eq!(
        (status, error_code(&body)),
        (402, HiveErrorCode::PlanRequired)
    );

    let old = FakeHive::start_with(FakeOptions {
        min_engine: Some("3.12.1".into()),
        ..FakeOptions::default()
    });
    let (status, body) =
        Client::new(&old.url).post("/device/code", &fixture("device_code.request"));
    let detail = decode::<HiveErrorBody>(&body).error;
    assert_eq!(
        (status, detail.min_engine.as_deref()),
        (426, Some("3.12.1"))
    );
}

/// Runs the contract against a deployed Hive when `HIVE_CONFORMANCE_URL`
/// is set (`HIVE_CONFORMANCE_ROOT_KEY`: base64 root key). With
/// `HIVE_CONFORMANCE_APPROVE` (a shell command; `{user_code}` is replaced)
/// it also enrolls an instance, runs every authenticated area and frees the
/// seat. Prints pass/fail per area; fails if any area failed.
#[test]
fn conformance_against_deployed_hive() {
    let Ok(url) = std::env::var("HIVE_CONFORMANCE_URL") else {
        return;
    };
    let root = std::env::var("HIVE_CONFORMANCE_ROOT_KEY").expect("HIVE_CONFORMANCE_ROOT_KEY");
    let approve = std::env::var("HIVE_CONFORMANCE_APPROVE").ok();
    let mut client = Client::new(url.trim_end_matches('/'));
    let mut report: Vec<(&str, Result<(), String>)> = Vec::new();
    let mut area = |name: &'static str, run: &mut dyn FnMut()| {
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).map_err(|panic| {
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_default()
            });
        let ok = outcome.is_ok();
        report.push((name, outcome));
        ok
    };

    area("keys", &mut || {
        let (status, keys_raw) = client.get("/keys");
        assert_eq!(status, 200, "{keys_raw}");
        let keys: HiveKeySetV1 = decode(&keys_raw);
        keys.validate().unwrap();
        assert!(
            verify(
                &root,
                &HiveKeySetV1::signing_input_raw(&keys_raw).unwrap(),
                &keys.signature
            ),
            "key set does not verify against the root key"
        );
    });
    area("errors", &mut || {
        let mut unknown = fixture("device_code.request");
        unknown["unexpected_field"] = Value::from(true);
        let (status, body) = client.post("/device/code", &unknown);
        assert_eq!(
            (status, error_code(&body)),
            (400, HiveErrorCode::InvalidRequest),
            "unknown field"
        );
        let (status, body) = client.get("/org/salt");
        assert_eq!(
            (status, error_code(&body)),
            (401, HiveErrorCode::Unauthorized),
            "no token"
        );
        let (status, body) = client.call(
            "POST",
            "/device/code",
            Some(&fixture("device_code.request")),
            &[(HEADER_PROTOCOL, "2.0")],
        );
        assert_eq!(
            (status, error_code(&body)),
            (426, HiveErrorCode::UpgradeRequired),
            "protocol 2.0"
        );
    });

    // A fresh instance key per run: the service refuses to enroll a known key twice.
    let mut instance_seed = [0u8; 32];
    getrandom::fill(&mut instance_seed).expect("random instance seed");
    let instance_key = SigningKey::from_bytes(&instance_seed);
    let token_request = |device_code: &str| {
        let mut request = device_token_request(device_code);
        request["instance_public_key"] =
            Value::from(B64.encode(instance_key.verifying_key().as_bytes()));
        request
    };
    let mut enrollment: Option<EnrollmentV1> = None;
    let enrolled = approve.is_some()
        && area("enrollment", &mut || {
            let (status, code) = client.post("/device/code", &fixture("device_code.request"));
            assert_eq!(status, 200, "{code}");
            let code: DeviceCodeV1 = decode(&code);
            code.validate().unwrap();
            let (status, pending) = client.post("/device/token", &token_request(&code.device_code));
            assert_eq!(
                (status, error_code(&pending)),
                (400, HiveErrorCode::AuthorizationPending),
                "{pending}"
            );
            let command = approve
                .as_ref()
                .unwrap()
                .replace("{user_code}", &code.user_code);
            let approved = std::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .status()
                .unwrap();
            assert!(approved.success(), "approve command failed");
            std::thread::sleep(std::time::Duration::from_secs(u64::from(code.interval)));
            let (status, body) = client.post("/device/token", &token_request(&code.device_code));
            assert_eq!(status, 200, "{body}");
            let keys_raw = body["keys"].clone();
            let enrolled: EnrollmentV1 = decode(&body);
            enrolled.validate().unwrap();
            assert!(verify(
                &root,
                &HiveKeySetV1::signing_input_raw(&keys_raw).unwrap(),
                &enrolled.keys.signature
            ));
            let signer = enrolled
                .keys
                .key(&enrolled.lease.kid)
                .expect("lease kid listed");
            assert!(
                verify(
                    &signer.ed25519_pub,
                    &enrolled.lease.signing_input().unwrap(),
                    &enrolled.lease.signature
                ),
                "lease signature"
            );
            assert_eq!(
                enrolled.lease.lease().unwrap().instance_id,
                enrolled.instance_id
            );

            let iat = chrono::Utc::now().timestamp();
            let nonce = format!("conformance-{iat}-nonce");
            let assertion =
                instance_key.sign(&token_assertion_input(&enrolled.instance_id, iat, &nonce));
            let request = json!({
                "schema": TOKEN_REQUEST_SCHEMA, "iat": iat, "nonce": nonce,
                "assertion": B64.encode(assertion.to_bytes()),
            });
            let path = format!("/instances/{}/token", enrolled.instance_id);
            let (status, token) = client.post(&path, &request);
            assert_eq!(status, 200, "{token}");
            let token: AccessTokenV1 = decode(&token);
            token.validate().unwrap();
            let (status, _) = client.post(&path, &request);
            assert_eq!(status, 401, "nonce must be single use");
            client.token = Some(token.access_token);
            client.instance = Some(enrolled.instance_id.clone());
            enrollment = Some(enrolled);
        });

    if enrolled {
        let enrolled = enrollment.clone().unwrap();
        let lease = enrolled.lease.lease().unwrap();
        let instance_id = enrolled.instance_id.clone();
        let forbidden_or = |right: HiveRight, status: u16, body: &Value| {
            if lease.grants(right) {
                assert!(
                    (200..300).contains(&status),
                    "{right:?} granted but {status}: {body}"
                );
            } else {
                assert_eq!(
                    (status, error_code(body)),
                    (403, HiveErrorCode::ForbiddenRight),
                    "{right:?} not granted"
                );
            }
        };
        area("lease", &mut || {
            let (status, renewed) =
                client.post(&format!("/instances/{instance_id}/lease/renew"), &json!({}));
            assert_eq!(status, 200, "{renewed}");
            let envelope: HiveLeaseEnvelopeV1 = decode(&renewed);
            envelope.validate_shape().unwrap();
            let signer = enrolled.keys.key(&envelope.kid).expect("kid listed");
            assert!(verify(
                &signer.ed25519_pub,
                &envelope.signing_input().unwrap(),
                &envelope.signature
            ));
            envelope.lease().unwrap();
        });
        let mut project_id = String::new();
        area("resolve", &mut || {
            let (status, salt) = client.get("/org/salt");
            assert_eq!(status, 200, "{salt}");
            let salt: OrgSaltV1 = decode(&salt);
            salt.validate().unwrap();
            let mut request = fixture("vault_resolve.request");
            request["org_salt_id"] = Value::from(salt.org_salt_id.clone());
            let (status, body) = client.post("/vaults/resolve", &request);
            assert_eq!(status, 200, "{body}");
            let project: VaultV1 = decode(&body);
            project.validate().unwrap();
            let (_, again) = client.post("/vaults/resolve", &request);
            assert_eq!(
                decode::<VaultV1>(&again).vault_id,
                project.vault_id,
                "stable"
            );
            project_id = project.vault_id;
            let mut bind = fixture("vault_resolve.ref.request");
            bind["org_salt_id"] = Value::from(salt.org_salt_id);
            bind["identity_digest"] = Value::from("ab".repeat(32));
            bind["vault"] = json!({"ns": "conformance", "name": "does-not-exist"});
            let (status, body) = client.post("/vaults/resolve", &bind);
            assert_eq!(
                status, 403,
                "repo the instance may not contribute to: {body}"
            );
        });
        area("knowledge", &mut || {
            let (status, body) = client.get(&format!("/vaults/{project_id}/knowledge"));
            forbidden_or(HiveRight::KnowledgeRead, status, &body);
            if status != 200 {
                return;
            }
            let revision: KnowledgeRevisionV1 = decode(&body);
            revision.validate().unwrap();
            let etag = format!("\"{}\"", revision.revision);
            let (status, _) = client.call(
                "GET",
                &format!("/vaults/{project_id}/knowledge"),
                None,
                &[("If-None-Match", &etag)],
            );
            assert_eq!(status, 304, "conditional GET");
            let (status, body) = client.get(&format!(
                "/vaults/{project_id}/knowledge/revisions?after=0&limit=10"
            ));
            assert_eq!(status, 200, "{body}");
            decode::<RevisionListV1>(&body).validate().unwrap();
            let mut draft = fixture("proposal.request");
            draft["context_patch"]["base_revision"] = Value::from(revision.revision);
            // Identical drafts map to the first sender's proposal; make this run's unique.
            let run = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            draft["context_patch"]["ops"][0]["fact"]["statement"] =
                Value::from(format!("Conformance run {run} drafts a fact."));
            draft["session_ref"] = Value::from(format!("ses_conformance_{run}"));
            let (status, body) = client.post(&format!("/vaults/{project_id}/proposals"), &draft);
            forbidden_or(HiveRight::KnowledgeDraft, status, &body);
            if status == 200 {
                assert_eq!(decode::<ProposalAckV1>(&body).state, ProposalState::Draft);
                let (status, list) = client.get(&format!(
                    "/vaults/{project_id}/proposals?state=draft&mine=1"
                ));
                assert_eq!(status, 200, "{list}");
                let raw = list.clone();
                let list: ProposalListV1 = decode(&list);
                assert!(
                    !list.proposals.is_empty() && list.proposals.iter().all(|p| p.mine),
                    "mine=1 must list only this instance's drafts: {raw}"
                );
            }
        });
        area("metrics+receipts", &mut || {
            let mut metrics = fixture("metrics.request");
            metrics["days"][0]["vault"]["id"] = Value::from(project_id.clone());
            metrics["days"][0]["date"] =
                Value::from(chrono::Utc::now().format("%Y-%m-%d").to_string());
            let (status, body) = client.post("/metrics", &metrics);
            forbidden_or(HiveRight::MetricsUpload, status, &body);
            if status == 200 {
                decode::<MetricsAckV1>(&body);
            }
            let mut receipts = fixture("receipts.request");
            receipts["receipts"][0]["vault_id"] = Value::from(project_id.clone());
            let (status, body) = client.post("/receipts", &receipts);
            forbidden_or(HiveRight::ReceiptsUpload, status, &body);
        });
        area("policy", &mut || {
            let (status, body) = client.get("/policy/bundle?since=0");
            forbidden_or(HiveRight::PolicyPull, status, &body);
            if status == 200 {
                decode::<PolicyBundleV1>(&body).validate().unwrap();
            }
        });
        area("bus", &mut || {
            let mut lease_request = fixture("bus_lease.request");
            lease_request["vault_id"] = Value::from(project_id.clone());
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            lease_request["resource_ref"] = Value::from(format!("{nanos:064x}"));
            let (status, body) = client.post("/bus/leases", &lease_request);
            forbidden_or(HiveRight::BusRelay, status, &body);
            if status != 200 {
                return;
            }
            let granted: BusLeaseV1 = decode(&body);
            granted.validate().unwrap();
            assert!(granted.granted, "{body}");
            lease_request["owner_agent_id"] = Value::from("codex-85");
            let (_, body) = client.post("/bus/leases", &lease_request);
            let refused: BusLeaseV1 = decode(&body);
            refused.validate().unwrap();
            assert!(
                !refused.granted && refused.holder.is_some(),
                "contention: {body}"
            );
            let lease_id = granted.lease_id.clone().unwrap();
            let token = granted.fencing_token.unwrap();
            let renew = json!({
                "schema": BUS_LEASE_RENEW_REQUEST_SCHEMA, "fencing_token": token, "duration_ms": 60_000,
            });
            let (status, body) = client.post(&format!("/bus/leases/{lease_id}/renew"), &renew);
            assert_eq!(status, 200, "renew: {body}");
            let release = format!("/bus/leases/{lease_id}?fencing_token={token}");
            let (status, _) = client.call("DELETE", &release, None, &[]);
            assert_eq!(status, 204, "release");
            let channel = format!("org:{}/vault:{project_id}/general", lease.org_id);
            let (status, body) = client.get(&format!(
                "/bus/channels/{}/events?after_seq=0",
                urlencoding::encode(&channel)
            ));
            assert_eq!(status, 200, "{body}");
            decode::<BusEventsV1>(&body).validate().unwrap();
            let (status, body) = client.get("/bus/poll");
            assert_eq!(status, 200, "{body}");
            decode::<BusPollV1>(&body);
        });
        area("disconnect", &mut || {
            let (status, body) =
                client.call("DELETE", &format!("/instances/{instance_id}"), None, &[]);
            assert_eq!(status, 204, "{body}");
        });
    }

    for (name, outcome) in &report {
        match outcome {
            Ok(()) => eprintln!("hive conformance {name}: pass"),
            Err(why) => eprintln!("hive conformance {name}: FAIL: {why}"),
        }
    }
    assert!(
        report.iter().all(|(_, outcome)| outcome.is_ok()),
        "conformance failures"
    );
}
