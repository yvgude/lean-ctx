// SPDX-License-Identifier: Apache-2.0

//! In-process fake of the Hive service's engine API (`/v1`), built strictly
//! on the `lean_ctx_protocol::hive` contract.
//!
//! - Device flow auto-approves after `approve_after_polls` polls.
//! - Keys come from the same test-only seeds as the protocol fixtures, so a
//!   test can pin [`FakeHive::root_public_key_b64`] like the hosted root key.
//! - The clock is injectable ([`FakeHive::set_now`], [`FakeHive::advance`]).
//! - Every request is recorded ([`FakeHive::requests`]) so tests can assert
//!   that no secret ever left the engine.
//!
//! The WebSocket bus arrives with the bus relay package; until then bus events
//! are injected with [`FakeHive::inject_event`] and read over the HTTP
//! fallback routes.

#![allow(dead_code)]

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use lean_ctx_protocol::UtcTimestamp;
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
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Test-only seeds, identical to `lean-ctx-protocol/tests/fixtures/hive/vectors.json`.
pub(crate) const ROOT_SEED: [u8; 32] = [0x52; 32];
pub(crate) const SIGNING_SEED: [u8; 32] = [0x53; 32];
pub(crate) const ORG_SALT: [u8; 32] = [0x0a; 32];
pub(crate) const SIGNING_KID: &str = "hive-2026-10";
pub(crate) const ORG_ID: &str = "org_acme";
pub(crate) const ORG_NAME: &str = "Acme AG";
/// The company the fake's team belongs to (enrollment `org.company`).
pub(crate) const COMPANY_NAME: &str = "Acme Group";
pub(crate) const ORG_SALT_ID: &str = "salt_2026_10";

/// Behaviour knobs.
#[derive(Debug, Clone)]
pub(crate) struct FakeOptions {
    /// Token polls answered with `authorization_pending` before approval.
    pub(crate) approve_after_polls: u32,
    /// `None`: the org has no Hive plan (`402 plan_required`).
    pub(crate) plan: Option<HivePlan>,
    pub(crate) seats_limit: u32,
    pub(crate) rights: Vec<HiveRight>,
    pub(crate) lease_hours: i64,
    pub(crate) grace_days: i64,
    /// Answer every request with `426 upgrade_required`.
    pub(crate) min_instance: Option<String>,
    /// `org_settings` signed into each lease (`None`: an older service).
    pub(crate) org_settings: Option<HiveOrgSettingsV1>,
    /// Refuse `POST /v1/metrics` with `upload_disabled` (the org switched it
    /// off after the current lease was issued).
    pub(crate) refuse_metrics: bool,
    /// At the seat limit, answer as a Pro org whose revoked engine's slot
    /// cools down until this time (`409 quota_exceeded registered_instances`,
    /// `cooldown_days: 7`, API §3 / P7) instead of `instance_limit`.
    pub(crate) slot_cooldown_until: Option<String>,
    /// C0 2.1 device setup: `None` a Hive without it (no `features`);
    /// `Some(share)` it offers the setup and the approval page answered
    /// "Share this project" with `share`.
    pub(crate) device_setup: Option<bool>,
    /// No HTTP request is answered while set (a Hive that does not answer
    /// yet); the waiting requests go on once it is cleared.
    pub(crate) hold_answers: bool,
    /// Answer the instance card (E44) and worker registration (E10). The
    /// card tells the engine that this Hive speaks C0 2.x: its relay
    /// connects to the bus only after it, and its knowledge and message
    /// paths switch to 2.x. Off by default, so the other suites keep the
    /// paths they test.
    pub(crate) instance_card: bool,
    /// Answer `POST /v1/vaults/match` (C0 vault_match): `bound` to the
    /// vault an identity digest of the request was resolved to first, else
    /// `none` (may create). Off by default: without it the engine falls back
    /// to resolve, as against a Hive without the route.
    pub(crate) vault_match: bool,
}

impl Default for FakeOptions {
    fn default() -> Self {
        Self {
            approve_after_polls: 1,
            plan: Some(HivePlan::Team),
            seats_limit: 25,
            // A Team lease as the service issues it: no knowledge.merge
            // (reviewers merge in Hive), receipts.upload is Enterprise-only.
            rights: vec![
                HiveRight::KnowledgeRead,
                HiveRight::KnowledgeDraft,
                HiveRight::MetricsUpload,
                HiveRight::PolicyPull,
                HiveRight::BusRelay,
            ],
            lease_hours: MAX_LEASE_VALIDITY_HOURS,
            grace_days: MAX_LEASE_GRACE_DAYS,
            min_instance: None,
            org_settings: None,
            refuse_metrics: false,
            slot_cooldown_until: None,
            device_setup: None,
            hold_answers: false,
            instance_card: false,
            vault_match: false,
        }
    }
}

/// One request as the fake received it.
#[derive(Debug, Clone)]
pub(crate) struct RecordedRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

struct DeviceGrant {
    polls: u32,
    /// The engine announced its project (C0 2.1 `POST /v1/device/setup`).
    setup: bool,
}

struct Instance {
    public_key: VerifyingKey,
    kx_public_key: String,
    label: String,
    grace_until: DateTime<Utc>,
}

struct Revision {
    revision: u64,
    document: Option<Value>,
    created_at: DateTime<Utc>,
    proposal: Option<u64>,
    state: RevisionState,
}

struct Proposal {
    number: u64,
    instance_id: String,
    state: ProposalState,
    summary: String,
    base_revision: u64,
    created_at: DateTime<Utc>,
    body: Value,
}

struct Project {
    repo: (String, String),
    label: Option<String>,
    revisions: Vec<Revision>,
    proposals: Vec<Proposal>,
    /// `TaskV1` views, number = index + 1 (C0 1.9 §1).
    tasks: Vec<Value>,
    /// X2 (Team): a threshold merge is quarantined.
    quarantine: bool,
}

/// One open bus socket: its instance, where its frames go, and the
/// addresses its latest hello subscribed.
struct BusSocket {
    instance: String,
    sender: tokio::sync::mpsc::UnboundedSender<String>,
    subscribed: BTreeSet<String>,
}

struct BusLease {
    lease_id: String,
    resource: (String, String, String),
    agent_id: String,
    instance_id: String,
    fencing_token: u64,
    expires_at: DateTime<Utc>,
}

struct Inner {
    url: String,
    options: FakeOptions,
    now: DateTime<Utc>,
    next_id: u64,
    devices: HashMap<String, DeviceGrant>,
    instances: BTreeMap<String, Instance>,
    tokens: HashMap<String, (String, DateTime<Utc>)>,
    nonces: HashSet<String>,
    projects: BTreeMap<String, Project>,
    idempotent: HashMap<(String, String), (u16, Value)>,
    metrics: BTreeMap<(String, String, String), Value>,
    /// Engine metrics v2: `(instance, hour)` → `(rev, bucket)`.
    metrics_hours: BTreeMap<(String, String), (u32, Value)>,
    /// The workspace's engine-metrics sharing state (`None`: plan default).
    metrics_sharing: Option<lean_ctx_protocol::hive::metrics_v2::SharingStateV2>,
    receipts: BTreeMap<String, String>,
    policy_bundle: Value,
    /// Bus messages stored so far, as `message.deliver` carries them.
    bus_events: Vec<Value>,
    /// Instance that published each message (aligned with `bus_events`).
    bus_origins: Vec<String>,
    /// Last `seq` per bus address (`#ws/vault`).
    bus_seq: BTreeMap<String, u64>,
    /// `client_msg_id → message.stored`: a republish is answered, not stored again.
    bus_client_ids: HashMap<String, Value>,
    /// Open bus sockets.
    bus_sockets: Vec<BusSocket>,
    /// `(instance, presence entries)` of every `hello` received, in order.
    bus_hellos: Vec<(String, Vec<Value>)>,
    /// `(instance, address, seq)` of every `message.deliver` sent.
    bus_deliveries: Vec<(String, String, u64)>,
    /// `(address, worker id) → (instance, presence.update frame)`, sent to
    /// every later hello that subscribes the address.
    bus_presence: BTreeMap<(String, String), (String, Value)>,
    /// Bus sessions (WebSocket connections) opened so far.
    bus_sessions: usize,
    /// Bus connects still to refuse with 503 (a Hive that is down).
    bus_refusals: usize,
    /// Bumped by `restart_bus`: every open bus socket drops, like a restart.
    bus_restart: tokio::sync::watch::Sender<u64>,
    bus_leases: Vec<BusLease>,
    fencing: BTreeMap<(String, String, String), u64>,
    /// Repos created on the web: (ns, name) → whether this org's instances may contribute.
    web_repos: BTreeMap<(String, String), bool>,
    requests: Vec<RecordedRequest>,
    /// `GET /v1/me/optin` (C0 1.9 §4); no opt-in until a test sets one.
    optin: Value,
}

type Shared = Arc<Mutex<Inner>>;

/// A running fake; stops when dropped.
pub(crate) struct FakeHive {
    pub(crate) url: String,
    inner: Shared,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeHive {
    pub(crate) fn start() -> Self {
        Self::start_with(FakeOptions::default())
    }

    pub(crate) fn start_with(options: FakeOptions) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake hive");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let now = Utc::now().with_timezone(&Utc).duration_trunc_seconds();
        let inner = Arc::new(Mutex::new(Inner {
            url: url.clone(),
            options,
            now,
            next_id: 0,
            devices: HashMap::new(),
            instances: BTreeMap::new(),
            tokens: HashMap::new(),
            nonces: HashSet::new(),
            projects: BTreeMap::new(),
            idempotent: HashMap::new(),
            metrics: BTreeMap::new(),
            metrics_hours: BTreeMap::new(),
            metrics_sharing: None,
            receipts: BTreeMap::new(),
            policy_bundle: json!({
                "schema": POLICY_BUNDLE_SCHEMA, "policy_version": 0,
                "org_policy": null, "unpin": null, "filter_packs": [],
            }),
            bus_events: Vec::new(),
            bus_origins: Vec::new(),
            bus_seq: BTreeMap::new(),
            bus_client_ids: HashMap::new(),
            bus_sockets: Vec::new(),
            bus_hellos: Vec::new(),
            bus_deliveries: Vec::new(),
            bus_presence: BTreeMap::new(),
            bus_sessions: 0,
            bus_refusals: 0,
            bus_restart: tokio::sync::watch::channel(0).0,
            bus_leases: Vec::new(),
            fencing: BTreeMap::new(),
            web_repos: BTreeMap::new(),
            requests: Vec::new(),
            optin: serde_json::from_str(include_str!(
                "../../../crates/lean-ctx-protocol/tests/fixtures/hive/instance_optin.none.response.json"
            ))
            .expect("opt-in fixture"),
        }));
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let shared = inner.clone();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
                let app = Router::new()
                    .route("/v1/bus", axum::routing::get(bus_socket))
                    .fallback(handle)
                    .with_state(shared);
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        stopped.await.ok();
                    })
                    .await
                    .expect("serve fake hive");
            });
        });
        Self {
            url,
            inner,
            shutdown: Some(shutdown),
            thread: Some(thread),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Root key an engine pins for this fake (base64 Ed25519).
    pub(crate) fn root_public_key_b64() -> String {
        B64.encode(
            SigningKey::from_bytes(&ROOT_SEED)
                .verifying_key()
                .as_bytes(),
        )
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.lock().now
    }

    pub(crate) fn set_now(&self, now: DateTime<Utc>) {
        self.lock().now = now.duration_trunc_seconds();
    }

    pub(crate) fn advance(&self, by: Duration) {
        let mut inner = self.lock();
        inner.now += by;
    }

    /// Answer no HTTP request until [`Self::release_answers`]: a call that
    /// waited for Hive would run into its whole client budget, so a test can
    /// assert "does not wait" relative to that budget.
    pub(crate) fn hold_answers(&self) {
        self.lock().options.hold_answers = true;
    }

    /// Answer the held requests and every later one.
    pub(crate) fn release_answers(&self) {
        self.lock().options.hold_answers = false;
    }

    pub(crate) fn set_options(&self, options: FakeOptions) {
        self.lock().options = options;
    }

    pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
        self.lock().requests.clone()
    }

    pub(crate) fn seats_used(&self) -> usize {
        self.lock().instances.len()
    }

    pub(crate) fn proposals(&self, project_id: &str) -> Vec<Value> {
        self.lock()
            .projects
            .get(project_id)
            .map(|project| project.proposals.iter().map(|p| p.body.clone()).collect())
            .unwrap_or_default()
    }

    /// A task a person created on the web, `open`; returns its number.
    pub(crate) fn add_task(&self, project_id: &str, title: &str, acceptance: &str) -> u64 {
        let mut inner = self.lock();
        let now = stamp(inner.now);
        let project = inner.projects.get_mut(project_id).expect("project");
        let number = project.tasks.len() as u64 + 1;
        project.tasks.push(json!({
            "schema": lean_ctx_protocol::hive::tasks::TASK_SCHEMA, "vault_id": project_id,
            "number": number, "state": "open", "title": title, "acceptance": acceptance,
            "origin": {"kind": "person"}, "created_at": now,
            "created_by": {"kind": "person", "label": "Ada", "mine": false},
        }));
        number
    }

    /// Gives a task a role: only an agent present on the bus in that role
    /// may claim it (Hive task #17).
    pub(crate) fn set_task_role(&self, project_id: &str, number: u64, role: &str) {
        let mut inner = self.lock();
        let project = inner.projects.get_mut(project_id).expect("project");
        project.tasks[usize::try_from(number - 1).unwrap()]["role"] = role.into();
    }

    /// The engine opt-in Hive answers with (`EngineOptinV1`).
    pub(crate) fn set_optin(&self, optin: Value) {
        self.lock().optin = optin;
    }

    /// Every task of a project, proposals included.
    pub(crate) fn tasks(&self, project_id: &str) -> Vec<Value> {
        self.lock()
            .projects
            .get(project_id)
            .map(|project| project.tasks.clone())
            .unwrap_or_default()
    }

    pub(crate) fn metrics_rows(&self) -> usize {
        self.lock().metrics.len()
    }

    /// Engine metrics v2 stored: `(hour, rev, bucket)`, oldest first.
    pub(crate) fn metrics_hours(&self) -> Vec<(String, u32, Value)> {
        self.lock()
            .metrics_hours
            .iter()
            .map(|((_, hour), (rev, bucket))| (hour.clone(), *rev, bucket.clone()))
            .collect()
    }

    /// Set the workspace's engine-metrics sharing state (as its policy or a
    /// confirmation would).
    pub(crate) fn set_metrics_sharing(
        &self,
        state: Option<lean_ctx_protocol::hive::metrics_v2::SharingStateV2>,
    ) {
        self.lock().metrics_sharing = state;
    }

    /// Publish a revision as if a reviewer merged it in Hive.
    pub(crate) fn publish_revision(&self, project_id: &str, document: Value) -> u64 {
        let mut inner = self.lock();
        let now = inner.now;
        let project = inner.projects.get_mut(project_id).expect("project");
        let revision = project.revisions.last().map_or(0, |r| r.revision) + 1;
        project.revisions.push(Revision {
            revision,
            document: Some(document),
            created_at: now,
            proposal: None,
            state: RevisionState::Released,
        });
        revision
    }

    /// The repo's plan quarantines threshold merges (X2: Team).
    pub(crate) fn set_quarantine(&self, project_id: &str, on: bool) {
        if let Some(project) = self.lock().projects.get_mut(project_id) {
            project.quarantine = on;
        }
    }

    /// The state of proposal `number` (`"draft"`, `"ready"`, `"merged"` …).
    pub(crate) fn proposal_state(&self, project_id: &str, number: u64) -> Option<Value> {
        let inner = self.lock();
        let proposal = inner
            .projects
            .get(project_id)?
            .proposals
            .iter()
            .find(|p| p.number == number)?;
        serde_json::to_value(proposal.state).ok()
    }

    /// Create a repo on the web; `contributable` decides whether instances may bind to it.
    pub(crate) fn create_web_repo(&self, ns: &str, name: &str, contributable: bool) {
        self.lock()
            .web_repos
            .insert((ns.to_string(), name.to_string()), contributable);
    }

    pub(crate) fn set_policy_bundle(&self, bundle: Value) {
        self.lock().policy_bundle = bundle;
    }

    /// Store a bus message as if `worker_id` on another machine published
    /// it to `address` (`#ws/vault`); returns its seq.
    pub(crate) fn inject_event(
        &self,
        address: &str,
        worker_id: &str,
        kind: &str,
        body: &Value,
    ) -> u64 {
        let mut inner = self.lock();
        let now_ms = ms(inner.now);
        let message = json!({
            "schema": lean_ctx_hive::bus::v2::BUS_SCHEMA,
            "client_msg_id": next_id(&mut inner, "cmi"),
            "to": address, "type": kind, "worker_id": worker_id, "body": body,
            "sender": {"kind": "worker", "worker_id": worker_id, "host_app": "codex",
                       "name": "other-box · Codex · dev", "owner_handle": "mara"},
        });
        store_message(&mut inner, "external", message, now_ms)
    }

    /// Bus leases live on the fake clock: `(agent, instance, fencing token)`.
    pub(crate) fn live_bus_leases(&self) -> Vec<(String, String, u64)> {
        let inner = self.lock();
        inner
            .bus_leases
            .iter()
            .filter(|l| l.expires_at > inner.now)
            .map(|l| (l.agent_id.clone(), l.instance_id.clone(), l.fencing_token))
            .collect()
    }

    /// `(instance, address, seq)` of the `message.deliver` frames sent.
    pub(crate) fn bus_deliveries(&self) -> Vec<(String, String, u64)> {
        self.lock().bus_deliveries.clone()
    }

    /// Workers present on the bus: `(address, worker id) → (instance, role)`.
    pub(crate) fn bus_presence(&self) -> BTreeMap<(String, String), (String, String)> {
        self.lock()
            .bus_presence
            .iter()
            .map(|(key, (instance, frame))| {
                let role = frame["role"].as_str().unwrap_or_default().to_string();
                (key.clone(), (instance.clone(), role))
            })
            .collect()
    }

    /// Ids of the projects resolved so far.
    pub(crate) fn project_ids(&self) -> Vec<String> {
        self.lock().projects.keys().cloned().collect()
    }

    /// Instances with an open bus socket.
    pub(crate) fn bus_connected(&self) -> Vec<String> {
        self.lock()
            .bus_sockets
            .iter()
            .map(|socket| socket.instance.clone())
            .collect()
    }

    /// Bus sessions (WebSocket connections) opened so far.
    pub(crate) fn bus_sessions(&self) -> usize {
        self.lock().bus_sessions
    }

    /// Refuse the next `n` bus connects with 503, as while Hive is down.
    pub(crate) fn refuse_bus(&self, n: usize) {
        self.lock().bus_refusals = n;
    }

    /// Drop every open bus socket without a close frame, as a Hive
    /// restart does; relays must reconnect and hello again.
    pub(crate) fn restart_bus(&self) {
        self.lock()
            .bus_restart
            .send_modify(|generation| *generation += 1);
    }

    /// `(instance, presence entries)` of every `hello` received, in order.
    pub(crate) fn bus_hellos(&self) -> Vec<(String, Vec<Value>)> {
        self.lock().bus_hellos.clone()
    }

    /// Bus messages stored so far.
    pub(crate) fn bus_events(&self) -> Vec<Value> {
        self.lock().bus_events.clone()
    }
}

/// `GET /v1/bus`: the bus v2 socket (C0 2.x E14). A message one instance
/// publishes reaches every other socket that subscribed its address, never
/// the sender; `hello` cursors replay what an instance missed.
async fn bus_socket(
    State(shared): State<Shared>,
    headers: HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let major = header(HEADER_PROTOCOL)
        .and_then(lean_ctx_protocol::hive::parse_protocol_header)
        .map(|(major, _)| major);
    if major != Some(2) {
        let (status, body) = error(HiveErrorCode::UpgradeRequired, "Unsupported Hive protocol.");
        return reply(status, body);
    }
    let instance = {
        let mut inner = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.bus_refusals > 0 {
            inner.bus_refusals -= 1;
            return reply(
                503,
                json!({"error": {"code": "unavailable", "message": "Hive is restarting."}}),
            );
        }
        authenticate(&inner, header("authorization"), header(HEADER_INSTANCE))
            .and_then(|id| require(&inner, HiveRight::BusRelay).map(|()| id))
    };
    match instance {
        Ok(instance) => upgrade.on_upgrade(move |socket| bus_session(shared, instance, socket)),
        Err((status, body)) => reply(status, body),
    }
}

fn ms(time: DateTime<Utc>) -> u64 {
    u64::try_from(time.timestamp_millis()).unwrap_or_default()
}

/// Store `message` (a publish, without `t`) as Hive does: the next `seq` of
/// its address, an id, its thread; deliver it to every other socket that
/// follows the address. Returns its `seq`.
fn store_message(inner: &mut Inner, origin: &str, mut message: Value, now_ms: u64) -> u64 {
    let address = message["to"].as_str().unwrap_or_default().to_string();
    let seq = {
        let seq = inner.bus_seq.entry(address.clone()).or_insert(0);
        *seq += 1;
        *seq
    };
    let id = format!("msg_{seq:016x}");
    message["id"] = id.clone().into();
    message["seq"] = seq.into();
    let thread = message["reply_to"].as_str().unwrap_or(&id).to_string();
    message["thread_id"] = thread.into();
    message["received_at"] = now_ms.into();
    message["provenance"] = "workspace".into();
    inner.bus_events.push(message.clone());
    inner.bus_origins.push(origin.to_string());
    message["t"] = "message.deliver".into();
    let text = message.to_string();
    let mut sent = Vec::new();
    for socket in &inner.bus_sockets {
        if socket.instance != origin && socket.subscribed.contains(&address) {
            let _ = socket.sender.send(text.clone());
            sent.push((socket.instance.clone(), address.clone(), seq));
        }
    }
    inner.bus_deliveries.extend(sent);
    seq
}

async fn bus_session(shared: Shared, instance: String, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;
    use futures::{SinkExt as _, StreamExt as _};
    let (mut sink, mut stream) = socket.split();
    let (sender, mut outbox) = tokio::sync::mpsc::unbounded_channel::<String>();
    fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Inner> {
        shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    let mut restart = {
        let mut inner = lock(&shared);
        inner.bus_sessions += 1;
        inner.bus_sockets.push(BusSocket {
            instance: instance.clone(),
            sender: sender.clone(),
            subscribed: BTreeSet::new(),
        });
        inner.bus_restart.subscribe()
    };
    let writer = tokio::spawn(async move {
        while let Some(text) = outbox.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });
    // Workers the hello on this socket announced: like Hive, a publish
    // from any other worker is refused.
    let mut announced: BTreeSet<String> = BTreeSet::new();
    loop {
        let message = tokio::select! {
            next = stream.next() => match next {
                Some(Ok(message)) => message,
                _ => break,
            },
            // A restart: the socket drops without a close frame.
            _ = restart.changed() => break,
        };
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let mut inner = lock(&shared);
        let now_ms = ms(inner.now);
        match frame["t"].as_str().unwrap_or_default() {
            "hello" => {
                let subscribe: BTreeSet<String> = frame["subscribe"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect();
                let presence: Vec<Value> =
                    frame["presence"].as_array().cloned().unwrap_or_default();
                announced = presence
                    .iter()
                    .filter_map(|p| p["worker_id"].as_str().map(str::to_string))
                    .collect();
                inner.bus_hellos.push((instance.clone(), presence.clone()));
                if let Some(socket) = inner
                    .bus_sockets
                    .iter_mut()
                    .find(|s| s.sender.same_channel(&sender))
                {
                    socket.subscribed.clone_from(&subscribe);
                }
                let _ = sender.send(
                    json!({"t": "hello.ack", "server_time": now_ms, "subscribed": subscribe, "denied": [], "epochs": {}})
                        .to_string(),
                );
                // What this instance missed on its addresses.
                let mut replayed = Vec::new();
                for (message, origin) in inner.bus_events.iter().zip(&inner.bus_origins) {
                    let address = message["to"].as_str().unwrap_or_default();
                    let seq = message["seq"].as_u64().unwrap_or(0);
                    let after = frame["cursors"][address].as_u64().unwrap_or(0);
                    if origin != &instance && subscribe.contains(address) && seq > after {
                        let mut deliver = message.clone();
                        deliver["t"] = "message.deliver".into();
                        let _ = sender.send(deliver.to_string());
                        replayed.push((instance.clone(), address.to_string(), seq));
                    }
                }
                inner.bus_deliveries.extend(replayed);
                // Presence: the hello's workers to everyone on the address,
                // everyone else's to the newcomer.
                for address in &subscribe {
                    for entry in &presence {
                        let update = json!({
                            "t": "presence.update", "address": address,
                            "worker_id": entry["worker_id"], "role": entry["role"],
                            "state": entry["state"], "at": now_ms,
                        });
                        for socket in &inner.bus_sockets {
                            if socket.instance != instance && socket.subscribed.contains(address) {
                                let _ = socket.sender.send(update.to_string());
                            }
                        }
                        let worker = entry["worker_id"].as_str().unwrap_or_default().to_string();
                        inner
                            .bus_presence
                            .insert((address.clone(), worker), (instance.clone(), update));
                    }
                }
                for ((address, _), (owner, update)) in &inner.bus_presence {
                    if owner != &instance && subscribe.contains(address) {
                        let _ = sender.send(update.to_string());
                    }
                }
            }
            "place.publish" => {
                let client_msg_id = frame["client_msg_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let worker = frame["worker_id"].as_str().unwrap_or_default();
                if !announced.contains(worker) {
                    let _ = sender.send(
                        json!({"t": "error", "client_msg_id": client_msg_id, "code": "invalid_request"})
                            .to_string(),
                    );
                    continue;
                }
                if let Some(stored) = inner.bus_client_ids.get(&client_msg_id) {
                    let _ = sender.send(stored.to_string());
                    continue;
                }
                let mut message = frame.clone();
                if let Some(fields) = message.as_object_mut() {
                    fields.remove("t");
                }
                message["sender"] = json!({"kind": "worker", "worker_id": worker,
                    "host_app": "claude-code", "name": "box · Claude Code", "owner_handle": "yves"});
                let seq = store_message(&mut inner, &instance, message, now_ms);
                let stored = inner.bus_events.last().cloned().unwrap_or_default();
                let answer = json!({
                    "t": "message.stored", "client_msg_id": client_msg_id,
                    "id": stored["id"], "seq": seq, "thread_id": stored["thread_id"],
                });
                inner.bus_client_ids.insert(client_msg_id, answer.clone());
                let _ = sender.send(answer.to_string());
            }
            "presence.update" => {
                let address = frame["address"].as_str().unwrap_or_default().to_string();
                let worker = frame["worker_id"].as_str().unwrap_or_default().to_string();
                for socket in &inner.bus_sockets {
                    if socket.instance != instance && socket.subscribed.contains(&address) {
                        let _ = socket.sender.send(frame.to_string());
                    }
                }
                inner
                    .bus_presence
                    .insert((address, worker), (instance.clone(), frame.clone()));
            }
            "ping" => {
                let _ = sender.send(json!({"t": "pong"}).to_string());
            }
            _ => {}
        }
    }
    {
        let mut inner = lock(&shared);
        inner
            .bus_sockets
            .retain(|socket| !socket.sender.same_channel(&sender));
        // Like Hive: a worker is present only while the socket that
        // announced it is connected (#252).
        if !inner.bus_sockets.iter().any(|s| s.instance == instance) {
            inner
                .bus_presence
                .retain(|_, (owner, _)| owner != &instance);
        }
    }
    writer.abort();
}

impl Drop for FakeHive {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            shutdown.send(()).ok();
        }
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

trait TruncSeconds {
    fn duration_trunc_seconds(self) -> Self;
}

impl TruncSeconds for DateTime<Utc> {
    fn duration_trunc_seconds(self) -> Self {
        DateTime::from_timestamp(self.timestamp(), 0).expect("timestamp in range")
    }
}

fn stamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn timestamp(time: DateTime<Utc>) -> UtcTimestamp {
    UtcTimestamp::new(stamp(time)).expect("canonical timestamp")
}

/// The protocol every answer names, like the deployed Hive (C0 2.0).
const ANSWER_PROTOCOL: &str = "2.0";

fn reply(status: u16, body: Value) -> Response {
    let mut response = (
        StatusCode::from_u16(status).expect("status"),
        axum::Json(body),
    )
        .into_response();
    response
        .headers_mut()
        .insert(HEADER_PROTOCOL, HeaderValue::from_static(ANSWER_PROTOCOL));
    response
}

fn empty(status: u16) -> Response {
    let mut response = StatusCode::from_u16(status)
        .expect("status")
        .into_response();
    response
        .headers_mut()
        .insert(HEADER_PROTOCOL, HeaderValue::from_static(ANSWER_PROTOCOL));
    response
}

/// The workspace's engine-metrics sharing state: the one set, else the plan
/// default (Team and Business on since enrolment, Community and Pro waiting).
fn metrics_sharing(inner: &Inner) -> lean_ctx_protocol::hive::metrics_v2::SharingStateV2 {
    use lean_ctx_protocol::hive::metrics_v2::{
        PlanDefault, SharingStateV2, SharingValue, TimeStamp,
    };
    if let Some(state) = &inner.metrics_sharing {
        return state.clone();
    }
    let default_on = matches!(
        inner.options.plan,
        Some(HivePlan::Team | HivePlan::Enterprise)
    );
    SharingStateV2 {
        state: if default_on {
            SharingValue::On
        } else {
            SharingValue::NeedsConfirmation
        },
        plan_default: if default_on {
            PlanDefault::On
        } else {
            PlanDefault::Off
        },
        policy: None,
        confirmed_at: None,
        since: default_on
            .then(|| TimeStamp::new(&stamp(inner.now - Duration::days(400))))
            .flatten(),
    }
}

fn error(code: HiveErrorCode, message: &str) -> (u16, Value) {
    error_with(HiveErrorDetailV1::new(code, message))
}

fn error_with(detail: HiveErrorDetailV1) -> (u16, Value) {
    let status = detail.code.http_status();
    (
        status,
        serde_json::to_value(HiveErrorBody { error: detail }).expect("error body"),
    )
}

type Outcome = Result<(u16, Value), (u16, Value)>;

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, (u16, Value)> {
    serde_json::from_slice(body)
        .map_err(|e| error(HiveErrorCode::InvalidRequest, &format!("invalid body: {e}")))
}

fn valid(result: Result<(), lean_ctx_protocol::ValidationError>) -> Result<(), (u16, Value)> {
    result.map_err(|e| error(HiveErrorCode::InvalidRequest, &e.to_string()))
}

fn query_param<'a>(query: Option<&'a str>, name: &str) -> Option<&'a str> {
    query?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

async fn handle(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    while shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .options
        .hold_answers
    {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let mut inner = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    inner.requests.push(RecordedRequest {
        method: method.to_string(),
        path: uri.path().to_string(),
        query: uri.query().map(str::to_string),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(min) = inner.options.min_instance.clone() {
        let mut detail = HiveErrorDetailV1::new(HiveErrorCode::UpgradeRequired, "Update lean-ctx.");
        detail.min_instance = Some(min.clone());
        let (status, body) = error_with(detail);
        let mut response = reply(status, body);
        response.headers_mut().insert(
            HEADER_MIN_ENGINE,
            HeaderValue::from_str(&min).expect("header"),
        );
        return response;
    }
    // The deployed guard: only major 2 passes, on every route, enrollment
    // included; 1.x (retired), a missing or malformed header or another
    // major is `426`.
    let major = header(HEADER_PROTOCOL)
        .and_then(lean_ctx_protocol::hive::parse_protocol_header)
        .map(|(major, _)| major);
    if major != Some(2) {
        let (status, body) = error(HiveErrorCode::UpgradeRequired, "Unsupported Hive protocol.");
        return reply(status, body);
    }
    // This fake speaks only the vault form; without the header the real
    // Hive would answer in the old names until the 3.12 release day.
    if header(HEADER_NAMES) != Some(NAMES_VAULT) {
        let (status, body) = error(
            HiveErrorCode::InvalidRequest,
            "Hive-Names: vault header is required.",
        );
        return reply(status, body);
    }
    let Some(path) = uri.path().strip_prefix(HIVE_API_PREFIX) else {
        let (status, body) = error(HiveErrorCode::NotFound, "Unknown route.");
        return reply(status, body);
    };
    let segments: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .map(|s| {
            urlencoding::decode(s)
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_default()
        })
        .collect();
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let query = uri.query();

    let public_route = matches!(
        (method.as_str(), segments.as_slice()),
        (
            "POST",
            ["device", "code" | "token" | "setup"] | ["instances", _, "token"]
        ) | ("GET", ["keys" | "root-cert"])
    );
    let instance_id = if public_route {
        None
    } else {
        match authenticate(&inner, header("authorization"), header(HEADER_INSTANCE)) {
            Ok(id) => Some(id),
            Err((status, body)) => return reply(status, body),
        }
    };

    // E10 is idempotent by the worker id it registers.
    let idempotency = if method == Method::POST && segments.as_slice() != ["workers"] {
        let Some(key) = header(HEADER_IDEMPOTENCY_KEY) else {
            let (status, body) = error(
                HiveErrorCode::InvalidRequest,
                "Idempotency-Key is required.",
            );
            return reply(status, body);
        };
        let scope = instance_id.clone().unwrap_or_else(|| "public".into());
        let slot = (scope, key.to_string());
        if let Some((status, body)) = inner.idempotent.get(&slot) {
            return reply(*status, body.clone());
        }
        Some(slot)
    } else {
        None
    };

    let outcome = route(
        &mut inner,
        &method,
        &segments,
        query,
        &headers,
        &body,
        instance_id.as_deref(),
    );
    let (status, body) = match outcome {
        Ok(ok) => ok,
        Err(err) => err,
    };
    if let Some(slot) = idempotency
        && status < 500
        && body != Value::Null
        && !matches!(status, 400 | 401 | 429)
    {
        inner.idempotent.insert(slot, (status, body.clone()));
    }
    match status {
        204 | 304 => empty(status),
        _ => {
            let etag = if body["schema"] == POLICY_BUNDLE_SCHEMA {
                Some(policy_etag(&body))
            } else {
                body.get("revision")
                    .and_then(Value::as_u64)
                    .map(|revision| format!("\"{revision}\""))
            };
            let mut response = reply(status, body);
            if let Some(etag) = etag {
                response
                    .headers_mut()
                    .insert("etag", HeaderValue::from_str(&etag).expect("etag"));
            }
            response
        }
    }
}

fn authenticate(
    inner: &Inner,
    authorization: Option<&str>,
    instance_header: Option<&str>,
) -> Result<String, (u16, Value)> {
    let token = authorization
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| error(HiveErrorCode::Unauthorized, "Bearer token required."))?;
    let (instance_id, expires) = inner
        .tokens
        .get(token)
        .ok_or_else(|| error(HiveErrorCode::Unauthorized, "Unknown token."))?;
    if *expires <= inner.now {
        return Err(error(HiveErrorCode::Unauthorized, "Token expired."));
    }
    if instance_header != Some(instance_id.as_str()) {
        return Err(error(
            HiveErrorCode::Unauthorized,
            "Hive-Instance does not match the token.",
        ));
    }
    let instance = inner
        .instances
        .get(instance_id)
        .ok_or_else(|| error(HiveErrorCode::Unauthorized, "Unknown instance."))?;
    if instance.grace_until <= inner.now {
        return Err(error(
            HiveErrorCode::LeaseExpired,
            "Lease lapsed; reconnect.",
        ));
    }
    Ok(instance_id.clone())
}

fn require(inner: &Inner, right: HiveRight) -> Result<(), (u16, Value)> {
    if inner.options.rights.contains(&right) {
        Ok(())
    } else {
        Err(error(
            HiveErrorCode::ForbiddenRight,
            "This lease lacks the right for this route.",
        ))
    }
}

fn next_id(inner: &mut Inner, prefix: &str) -> String {
    inner.next_id += 1;
    format!("{prefix}_{:08}", inner.next_id)
}

fn keyset() -> Value {
    let signing = SigningKey::from_bytes(&SIGNING_SEED);
    let mut keyset = json!({
        "schema": KEYSET_SCHEMA,
        "keys": [{
            "kid": SIGNING_KID,
            "ed25519_pub": B64.encode(signing.verifying_key().as_bytes()),
            "not_after": "2099-01-01T00:00:00Z",
        }],
    });
    let input = HiveKeySetV1::signing_input_raw(&keyset).expect("keyset input");
    keyset["signature"] =
        Value::from(B64.encode(SigningKey::from_bytes(&ROOT_SEED).sign(&input).to_bytes()));
    keyset
}

fn issue_lease(inner: &mut Inner, instance_id: &str) -> Value {
    let now = inner.now;
    let expires = now + Duration::hours(inner.options.lease_hours);
    let grace = expires + Duration::days(inner.options.grace_days);
    let lease = HiveLeaseV1 {
        schema: LEASE_SCHEMA.into(),
        lease_id: next_id(inner, "lse"),
        org_id: ORG_ID.into(),
        instance_id: instance_id.into(),
        plan: inner.options.plan.unwrap_or(HivePlan::Team),
        seats_limit: inner.options.seats_limit,
        rights: inner.options.rights.clone(),
        issued_at: timestamp(now),
        expires_at: timestamp(expires),
        grace_until: timestamp(grace),
        server_time: timestamp(now),
        org_settings: inner.options.org_settings,
    };
    if let Some(instance) = inner.instances.get_mut(instance_id) {
        instance.grace_until = grace;
    }
    let input = HiveLeaseEnvelopeV1::signing_input_for(&lease).expect("lease input");
    let signature = B64.encode(
        SigningKey::from_bytes(&SIGNING_SEED)
            .sign(&input)
            .to_bytes(),
    );
    serde_json::to_value(HiveLeaseEnvelopeV1::seal(&lease, SIGNING_KID, signature).expect("seal"))
        .expect("envelope")
}

/// Content tag of a policy bundle: a new bundle is a new tag even at the same version.
fn policy_etag(bundle: &Value) -> String {
    let digest = Sha256::digest(serde_json::to_vec(bundle).unwrap_or_default());
    format!("\"{}\"", hex::encode(&digest[..16]))
}

fn project_id_for(digest: &str) -> String {
    hex::encode(&Sha256::digest(digest.as_bytes())[..16])
}

/// The delivered head (the newest released revision before the first
/// quarantined one: delivery is linear) and the quarantined count after it.
fn delivered(project: &Project) -> (u64, Option<Value>, u64) {
    let mut head = None;
    for revision in &project.revisions {
        match revision.state {
            RevisionState::Quarantined => break,
            RevisionState::Released => head = Some(revision),
            _ => {}
        }
    }
    let after = head.map_or(0, |r| r.revision);
    let quarantined = project
        .revisions
        .iter()
        .filter(|r| r.revision > after && r.state == RevisionState::Quarantined)
        .count() as u64;
    match head {
        Some(r) => (r.revision, r.document.clone(), quarantined),
        None => (
            0,
            Some(json!({"format": RESEARCH_CONTEXT_FORMAT, "facts": [], "relations": []})),
            quarantined,
        ),
    }
}

#[allow(clippy::too_many_lines)]
fn route(
    inner: &mut Inner,
    method: &Method,
    segments: &[&str],
    query: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
    instance: Option<&str>,
) -> Outcome {
    let now = inner.now;
    match (method.as_str(), segments) {
        ("POST", ["device", "code"]) => {
            let request: DeviceCodeRequestV1 = parse(body)?;
            valid(request.validate())?;
            let device_code = next_id(inner, "dc");
            inner.devices.insert(
                device_code.clone(),
                DeviceGrant {
                    polls: 0,
                    setup: false,
                },
            );
            let user_code = format!("FAKE-{:04}", inner.next_id);
            let mut code = json!({
                "schema": DEVICE_CODE_SCHEMA,
                "device_code": device_code,
                "user_code": user_code,
                "verification_uri": format!("{}/device", inner.url),
                "verification_uri_complete": format!("{}/device?user_code={user_code}", inner.url),
                "interval": 1,
                "expires_in": 900,
            });
            // C0 2.1: only a Hive with the device setup lists it.
            if inner.options.device_setup.is_some() {
                code["features"] = json!(["device_setup"]);
            }
            Ok((200, code))
        }
        ("POST", ["device", "token"]) => {
            let request: DeviceTokenRequestV1 = parse(body)?;
            valid(request.validate())?;
            let approve_after = inner.options.approve_after_polls;
            let grant = inner
                .devices
                .get_mut(&request.device_code)
                .ok_or_else(|| error(HiveErrorCode::ExpiredToken, "Device code expired."))?;
            grant.polls += 1;
            if grant.polls <= approve_after {
                return Err(error(
                    HiveErrorCode::AuthorizationPending,
                    "Waiting for approval.",
                ));
            }
            if inner.options.plan.is_none() {
                return Err(error(
                    HiveErrorCode::PlanRequired,
                    "Hive is part of Personal, Team and Enterprise.",
                ));
            }
            let limit = inner.options.seats_limit;
            let used = u32::try_from(inner.instances.len()).unwrap_or(u32::MAX);
            if used >= limit
                && let Some(reset) = inner.options.slot_cooldown_until.clone()
            {
                let mut detail = HiveErrorDetailV1::new(
                    HiveErrorCode::QuotaExceeded,
                    format!("This workspace has used all its engine slots ({used} of {limit})."),
                );
                detail.resource = Some("registered_instances".into());
                detail.limit = Some(u64::from(limit));
                detail.used = Some(u64::from(used));
                detail.reset_at = Some(reset);
                detail.cooldown_days = Some(7);
                return Err((
                    409,
                    serde_json::to_value(HiveErrorBody { error: detail }).expect("error body"),
                ));
            }
            if used >= limit {
                let mut detail = HiveErrorDetailV1::new(
                    HiveErrorCode::InstanceLimit,
                    format!("All {limit} instance seats are in use."),
                );
                detail.limit = Some(u64::from(limit));
                detail.used = Some(u64::from(used));
                return Err(error_with(detail));
            }
            let announced = inner
                .devices
                .remove(&request.device_code)
                .is_some_and(|g| g.setup);
            let key: [u8; 32] = B64
                .decode(&request.instance_public_key)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| error(HiveErrorCode::InvalidRequest, "instance_public_key"))?;
            let public_key = VerifyingKey::from_bytes(&key)
                .map_err(|_| error(HiveErrorCode::InvalidRequest, "instance_public_key"))?;
            let instance_id = next_id(inner, "inst");
            inner.instances.insert(
                instance_id.clone(),
                Instance {
                    public_key,
                    kx_public_key: request.instance_kx_public_key,
                    label: request.instance_label,
                    grace_until: now,
                },
            );
            let lease = issue_lease(inner, &instance_id);
            let mut enrollment = json!({
                "schema": ENROLLMENT_SCHEMA,
                "instance_id": instance_id,
                // Hive C line: the workspace's kind and its company (additive).
                "org": {"id": ORG_ID, "name": ORG_NAME, "kind": "team", "company": {"name": COMPANY_NAME}},
                "lease": lease,
                "keys": keyset(),
                "org_salt_id": ORG_SALT_ID,
                "web_url": inner.url.clone(),
            });
            // C0 2.1: only to an engine that announced a setup.
            if announced {
                enrollment["sharing"] = json!({ "share": inner.options.device_setup });
            }
            Ok((200, enrollment))
        }
        ("POST", ["device", "setup"]) => {
            if inner.options.device_setup.is_none() {
                return Err(error(HiveErrorCode::NotFound, "No such route."));
            }
            let code = serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|b| b["device_code"].as_str().map(str::to_string))
                .unwrap_or_default();
            let grant = inner
                .devices
                .get_mut(&code)
                .filter(|g| !g.setup)
                .ok_or_else(|| error(HiveErrorCode::InvalidRequest, "No pending code."))?;
            grant.setup = true;
            Ok((204, Value::Null))
        }
        ("POST", ["device", "setup", "result"]) => Ok((204, Value::Null)),
        ("POST", ["instances", id, "token"]) => {
            let request: InstanceTokenRequestV1 = parse(body)?;
            valid(request.validate())?;
            let instance = inner
                .instances
                .get(*id)
                .ok_or_else(|| error(HiveErrorCode::Unauthorized, "Unknown instance."))?;
            if (request.iat - now.timestamp()).abs() > MAX_ASSERTION_SKEW_SECONDS {
                return Err(error(
                    HiveErrorCode::Unauthorized,
                    "Assertion time is out of range.",
                ));
            }
            let signature: [u8; 64] = B64
                .decode(&request.assertion)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| error(HiveErrorCode::Unauthorized, "Bad assertion."))?;
            instance
                .public_key
                .verify(
                    &token_assertion_input(id, request.iat, &request.nonce),
                    &Signature::from_bytes(&signature),
                )
                .map_err(|_| error(HiveErrorCode::Unauthorized, "Bad assertion."))?;
            if instance.grace_until <= now {
                return Err(error(
                    HiveErrorCode::LeaseExpired,
                    "Lease lapsed; reconnect.",
                ));
            }
            if !inner.nonces.insert(format!("{id}:{}", request.nonce)) {
                return Err(error(HiveErrorCode::Unauthorized, "Nonce was used."));
            }
            let token = format!("atl_fake_{}", next_id(inner, "tok"));
            inner.tokens.insert(
                token.clone(),
                (
                    id.to_string(),
                    now + Duration::seconds(i64::from(MAX_ACCESS_TOKEN_SECONDS)),
                ),
            );
            Ok((
                200,
                json!({"schema": ACCESS_TOKEN_SCHEMA, "access_token": token, "expires_in": MAX_ACCESS_TOKEN_SECONDS}),
            ))
        }
        ("POST", ["instances", id, "lease", "renew"]) => {
            if Some(*id) != instance {
                return Err(error(HiveErrorCode::Unauthorized, "Not this instance."));
            }
            Ok((200, issue_lease(inner, id)))
        }
        ("DELETE", ["instances", id]) => {
            if Some(*id) != instance {
                return Err(error(HiveErrorCode::Unauthorized, "Not this instance."));
            }
            inner.instances.remove(*id);
            inner.tokens.retain(|_, (owner, _)| owner != id);
            Ok((204, Value::Null))
        }
        // E44: only a C0 2.x Hive has the instance card.
        ("PATCH", ["instances", id, "card"]) if inner.options.instance_card => {
            if Some(*id) != instance {
                return Err(error(HiveErrorCode::Unauthorized, "Not this instance."));
            }
            let mut card: Value = serde_json::from_str(include_str!(
                "../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes/instance_card.response.json"
            ))
            .expect("instance card fixture");
            card["presence_key"] = Value::Null;
            Ok((200, card))
        }
        // E10: the worker as registered (idempotent by its id).
        ("POST", ["workers"]) if inner.options.instance_card => {
            let request: Value = parse(body)?;
            Ok((
                200,
                json!({"worker": {
                    "id": request["worker_id"], "name": request["name"], "place": null,
                    "status": "online", "registered_at": ms(inner.now),
                    "host_app": request["host_app"], "role": request["role"], "card": request["card"],
                }}),
            ))
        }
        ("GET", ["keys"]) => Ok((200, keyset())),
        // The fake is hosted-like: no root certificate.
        ("GET", ["root-cert"]) => Err(error(HiveErrorCode::NotFound, "No root certificate.")),
        ("GET", ["org", "salt"]) => Ok((
            200,
            json!({
                "schema": ORG_SALT_SCHEMA, "org_salt_id": ORG_SALT_ID, "salt": B64.encode(ORG_SALT),
            }),
        )),
        ("POST", ["vaults", "match"]) if inner.options.vault_match => {
            let request: Value = parse(body)?;
            if request["org_salt_id"] != ORG_SALT_ID {
                return Err(error(HiveErrorCode::InvalidRequest, "Unknown org_salt_id."));
            }
            // Like Hive's vault_bindings: the first resolve of a digest
            // bound it; candidates are tried strongest first.
            let bound = request["candidates"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["digest"].as_str())
                .map(project_id_for)
                .find_map(|id| inner.projects.get(&id).map(|p| (id, p.repo.clone())));
            Ok((
                200,
                match bound {
                    Some((vault_id, (ns, name))) => json!({
                        "schema": "leanctx.hive.vault-match/v1", "match": "bound",
                        "vault": {
                            "schema": VAULT_SCHEMA, "vault_id": vault_id,
                            "vault": {"ns": ns, "name": name},
                            "sealed": false, "sealed_conversion": "none",
                        },
                    }),
                    None => json!({
                        "schema": "leanctx.hive.vault-match/v1", "match": "none", "can_create": true,
                    }),
                },
            ))
        }
        ("POST", ["vaults", "resolve"]) => {
            let request: VaultResolveRequestV1 = parse(body)?;
            valid(request.validate())?;
            if request.org_salt_id != ORG_SALT_ID {
                return Err(error(HiveErrorCode::InvalidRequest, "Unknown org_salt_id."));
            }
            let project_id = project_id_for(&request.identity_digest);
            let repo = match &request.vault {
                Some(repo) => {
                    let key = (repo.ns.clone(), repo.name.clone());
                    if inner.web_repos.get(&key) != Some(&true) {
                        return Err(error(
                            HiveErrorCode::ForbiddenRight,
                            "This instance may not contribute to that repo.",
                        ));
                    }
                    key
                }
                None => ("acme".to_string(), format!("p-{}", &project_id[..8])),
            };
            let project = inner.projects.entry(project_id.clone()).or_insert(Project {
                repo,
                label: request.label.clone(),
                revisions: Vec::new(),
                proposals: Vec::new(),
                tasks: Vec::new(),
                quarantine: false,
            });
            let mut response = json!({
                "schema": VAULT_SCHEMA, "vault_id": project_id,
                "vault": {"ns": project.repo.0, "name": project.repo.1},
                "sealed": false, "sealed_conversion": "none",
            });
            if let Some(label) = &project.label {
                response["label"] = Value::from(label.clone());
            }
            Ok((200, response))
        }
        ("GET", ["vaults", pid, "knowledge"]) => {
            require(inner, HiveRight::KnowledgeRead)?;
            let project = inner
                .projects
                .get(*pid)
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Unknown project."))?;
            // Only the delivered head is served.
            let (revision, document, quarantined) = delivered(project);
            let if_none_match = headers.get("if-none-match").and_then(|v| v.to_str().ok());
            if quarantined == 0 && if_none_match == Some(format!("\"{revision}\"").as_str()) {
                return Ok((304, Value::Null));
            }
            let mut response = json!({
                "schema": KNOWLEDGE_REVISION_SCHEMA, "vault_id": pid, "revision": revision,
                "updated_at": stamp(project.revisions.last().map_or(now, |r| r.created_at)),
            });
            if quarantined > 0 {
                response["quarantined_after"] = json!(quarantined);
            }
            if let Some(document) = document {
                response["document"] = document;
            }
            Ok((200, response))
        }
        ("POST", ["vaults", pid, "proposals"]) => {
            require(inner, HiveRight::KnowledgeDraft)?;
            let request: DraftProposalV1 = parse(body)?;
            valid(request.validate())?;
            let instance_id = instance.unwrap_or_default().to_string();
            let project = inner
                .projects
                .get_mut(*pid)
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Unknown project."))?;
            let current = project.revisions.last().map_or(0, |r| r.revision);
            if request.context_patch.base_revision > current {
                let mut detail = HiveErrorDetailV1::new(
                    HiveErrorCode::RevisionConflict,
                    "Unknown base revision.",
                );
                detail.expected = Some(request.context_patch.base_revision);
                detail.actual = Some(current);
                return Err(error_with(detail));
            }
            let number = project.proposals.len() as u64 + 1;
            project.proposals.push(Proposal {
                number,
                instance_id,
                state: ProposalState::Draft,
                summary: request.summary.clone().unwrap_or_default(),
                base_revision: request.context_patch.base_revision,
                created_at: now,
                body: serde_json::from_slice(body).unwrap_or_default(),
            });
            Ok((
                200,
                json!({"schema": PROPOSAL_ACK_SCHEMA, "number": number, "state": "draft"}),
            ))
        }
        ("GET", ["vaults", pid, "proposals", number]) => {
            require(inner, HiveRight::KnowledgeRead)?;
            let project = inner
                .projects
                .get(*pid)
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Unknown project."))?;
            let wanted: u64 = number
                .parse()
                .map_err(|_| error(HiveErrorCode::NotFound, "Unknown proposal."))?;
            let proposal = project
                .proposals
                .iter()
                .find(|p| p.number == wanted)
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Unknown proposal."))?;
            let mut view = json!({
                "schema": PROPOSAL_VIEW_SCHEMA, "number": proposal.number, "state": proposal.state,
                "created_at": stamp(proposal.created_at),
                "mine": Some(proposal.instance_id.as_str()) == instance,
                "draft": proposal.body,
            });
            if let Some(revision) = project
                .revisions
                .iter()
                .find(|r| r.proposal == Some(proposal.number))
                .map(|r| r.revision)
            {
                view["merged_revision"] = json!(revision);
            }
            Ok((200, view))
        }
        ("GET", ["vaults", pid, "proposals"]) => {
            require(inner, HiveRight::KnowledgeRead)?;
            let project = inner
                .projects
                .get(*pid)
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Unknown project."))?;
            let state = query_param(query, "state");
            let mine_only = query_param(query, "mine") == Some("1");
            let proposals: Vec<Value> = project
                .proposals
                .iter()
                .filter(|p| !mine_only || Some(p.instance_id.as_str()) == instance)
                .filter(|p| {
                    state.is_none_or(|s| serde_json::to_value(p.state).ok() == Some(Value::from(s)))
                })
                .map(|p| {
                    json!({
                        "number": p.number, "state": p.state, "summary": p.summary,
                        "base_revision": p.base_revision, "created_at": stamp(p.created_at),
                        "mine": Some(p.instance_id.as_str()) == instance,
                    })
                })
                .collect();
            Ok((
                200,
                json!({"schema": PROPOSAL_LIST_SCHEMA, "vault_id": pid, "proposals": proposals}),
            ))
        }
        ("PUT", ["vaults", pid, "code-graph"]) => {
            use lean_ctx_protocol::hive::codegraph::{
                CODE_GRAPH_ACK_SCHEMA, CodeGraphAckV1, CodeGraphPutV1,
            };
            require(inner, HiveRight::KnowledgeDraft)?;
            let request: CodeGraphPutV1 = parse(body)?;
            valid(request.validate())?;
            if !inner.projects.contains_key(*pid) {
                return Err(error(HiveErrorCode::NotFound, "Unknown project."));
            }
            let graph = request.graph;
            let (nodes, edges, revision_hint) = (
                graph.nodes.len() as u64,
                graph.edges.len() as u64,
                graph.revision_hint,
            );
            Ok((
                200,
                serde_json::to_value(CodeGraphAckV1 {
                    schema: CODE_GRAPH_ACK_SCHEMA.into(),
                    nodes,
                    edges,
                    revision_hint,
                })
                .expect("ack json"),
            ))
        }
        ("GET", ["metrics", "sharing"]) => {
            require(inner, HiveRight::MetricsUpload)?;
            Ok((
                200,
                serde_json::to_value(metrics_sharing(inner)).unwrap_or_default(),
            ))
        }
        ("POST", ["metrics", "sharing", "confirm"]) => {
            use lean_ctx_protocol::hive::metrics_v2::{PlanDefault, SharingValue, TimeStamp};
            require(inner, HiveRight::MetricsUpload)?;
            let mut state = metrics_sharing(inner);
            if state.plan_default == PlanDefault::Off
                && state.state == SharingValue::NeedsConfirmation
            {
                let at = TimeStamp::new(&stamp(now));
                state.state = SharingValue::On;
                state.confirmed_at.clone_from(&at);
                state.since = at;
            }
            inner.metrics_sharing = Some(state.clone());
            Ok((200, serde_json::to_value(state).unwrap_or_default()))
        }
        ("DELETE", ["metrics", "hours"]) => {
            let caller = instance.unwrap_or_default().to_string();
            let before = inner.metrics_hours.len();
            inner.metrics_hours.retain(|(owner, _), _| *owner != caller);
            Ok((
                200,
                json!({"deleted_buckets": before - inner.metrics_hours.len(), "instances": 1}),
            ))
        }
        ("POST", ["metrics", "hours"]) => {
            use lean_ctx_protocol::hive::metrics_v2::{
                ENGINE_METRICS_ACK_SCHEMA, HoursRequestV2, SharingValue,
            };
            require(inner, HiveRight::MetricsUpload)?;
            let caller = instance.unwrap_or_default().to_string();
            let request: HoursRequestV2 = parse(body)?;
            valid(request.validate())?;
            // The batch is signed by the bearer's instance key.
            let signed = inner.instances.get(&caller).is_some_and(|known| {
                let input = request
                    .body()
                    .signing_input(&request.key_id)
                    .unwrap_or_default();
                hex::decode(request.signature.as_str())
                    .ok()
                    .and_then(|b| Signature::from_slice(&b).ok())
                    .is_some_and(|s| known.public_key.verify(&input, &s).is_ok())
            });
            if !signed {
                return Err(error(HiveErrorCode::InvalidRequest, "signature"));
            }
            let sharing = metrics_sharing(inner);
            if sharing.state != SharingValue::On {
                return Ok((
                    200,
                    json!({
                        "schema": ENGINE_METRICS_ACK_SCHEMA,
                        "accepted": 0, "unchanged": 0, "refused": [],
                        "sharing": sharing,
                    }),
                ));
            }
            let (mut accepted, mut unchanged) = (0u32, 0u32);
            for bucket in &request.buckets {
                let key = (caller.clone(), bucket.hour.as_str().to_string());
                match inner.metrics_hours.get(&key) {
                    Some((rev, _)) if *rev >= bucket.rev => unchanged += 1,
                    _ => {
                        inner.metrics_hours.insert(
                            key,
                            (bucket.rev, serde_json::to_value(bucket).unwrap_or_default()),
                        );
                        accepted += 1;
                    }
                }
            }
            Ok((
                200,
                json!({
                    "schema": ENGINE_METRICS_ACK_SCHEMA,
                    "accepted": accepted, "unchanged": unchanged, "refused": [],
                    "sharing": sharing,
                }),
            ))
        }
        ("POST", ["metrics"]) => {
            require(inner, HiveRight::MetricsUpload)?;
            if inner.options.refuse_metrics
                || inner
                    .options
                    .org_settings
                    .is_some_and(|settings| !settings.metrics_upload)
            {
                let mut detail = HiveErrorDetailV1::new(
                    HiveErrorCode::UploadDisabled,
                    "The organization has switched metrics upload off.",
                );
                detail.kind = Some(HiveUploadKind::Metrics);
                return Err(error_with(detail));
            }
            let request: MetricsUploadV1 = parse(body)?;
            valid(request.validate())?;
            let accepted = request.days.len();
            for day in request.days {
                inner.metrics.insert(
                    (
                        request.device.id.clone(),
                        day.date.clone(),
                        day.vault.id.clone(),
                    ),
                    serde_json::to_value(&day).unwrap_or_default(),
                );
            }
            Ok((
                200,
                json!({"schema": METRICS_ACK_SCHEMA, "accepted": accepted}),
            ))
        }
        ("POST", ["receipts"]) => {
            require(inner, HiveRight::ReceiptsUpload)?;
            if inner
                .options
                .org_settings
                .is_some_and(|settings| !settings.receipts_upload)
            {
                let mut detail = HiveErrorDetailV1::new(
                    HiveErrorCode::UploadDisabled,
                    "The organization has switched receipts upload off.",
                );
                detail.kind = Some(HiveUploadKind::Receipts);
                return Err(error_with(detail));
            }
            // C0 2.5: the engine sends receipts v2 (signed, closed
            // documents); the v1 form stays for the 1.x contract fixtures.
            let v2 = serde_json::from_slice::<Value>(body).is_ok_and(|v| {
                v["schema"] == lean_ctx_protocol::hive::receipts_v2::RECEIPTS_V2_SCHEMA
            });
            let items: Vec<(String, String)> = if v2 {
                let request: lean_ctx_protocol::hive::receipts_v2::ReceiptsRequestV2 = parse(body)?;
                valid(request.validate())?;
                request
                    .receipts
                    .into_iter()
                    .map(|r| {
                        (
                            r.receipt_id.as_str().to_string(),
                            r.digest.as_str().to_string(),
                        )
                    })
                    .collect()
            } else {
                let request: ReceiptsUploadV1 = parse(body)?;
                valid(request.validate())?;
                request
                    .receipts
                    .into_iter()
                    .map(|r| (r.receipt_id, r.digest.as_str().to_string()))
                    .collect()
            };
            let (mut accepted, mut duplicates) = (0u32, 0u32);
            for (receipt_id, digest) in items {
                match inner.receipts.get(&receipt_id) {
                    Some(stored) if *stored == digest => duplicates += 1,
                    Some(_) => {
                        return Err(error(
                            HiveErrorCode::InvalidRequest,
                            "receipt_id reused with other content.",
                        ));
                    }
                    None => {
                        inner.receipts.insert(receipt_id, digest);
                        accepted += 1;
                    }
                }
            }
            let ack_schema = if v2 {
                lean_ctx_protocol::hive::receipts_v2::RECEIPTS_V2_ACK_SCHEMA
            } else {
                RECEIPTS_ACK_SCHEMA
            };
            Ok((
                200,
                json!({"schema": ack_schema, "accepted": accepted, "duplicates": duplicates}),
            ))
        }
        ("GET", ["policy", "bundle"]) => {
            require(inner, HiveRight::PolicyPull)?;
            let if_none_match = headers.get("if-none-match").and_then(|v| v.to_str().ok());
            if if_none_match == Some(policy_etag(&inner.policy_bundle).as_str()) {
                return Ok((304, Value::Null));
            }
            Ok((200, inner.policy_bundle.clone()))
        }
        ("POST", ["bus", "leases"]) => {
            require(inner, HiveRight::BusRelay)?;
            let request: BusLeaseRequestV1 = parse(body)?;
            valid(request.validate())?;
            let resource = (
                request.vault_id.clone(),
                request.resource_kind.clone(),
                request.resource_ref.clone(),
            );
            inner.bus_leases.retain(|l| l.expires_at > now);
            if let Some(holder) = inner.bus_leases.iter().find(|l| l.resource == resource) {
                return Ok((
                    200,
                    json!({
                        "schema": BUS_LEASE_SCHEMA, "granted": false,
                        "holder": {"agent_id": holder.agent_id, "instance_id": holder.instance_id, "expires_at": stamp(holder.expires_at)},
                        "server_time": stamp(now),
                    }),
                ));
            }
            let token = {
                let token = inner.fencing.entry(resource.clone()).or_insert(0);
                *token += 1;
                *token
            };
            let lease_id = next_id(inner, "bl");
            let expires_at =
                now + Duration::milliseconds(i64::try_from(request.duration_ms).unwrap_or(0));
            inner.bus_leases.push(BusLease {
                lease_id: lease_id.clone(),
                resource,
                agent_id: request.owner_agent_id,
                instance_id: instance.unwrap_or_default().to_string(),
                fencing_token: token,
                expires_at,
            });
            Ok((
                200,
                json!({
                    "schema": BUS_LEASE_SCHEMA, "granted": true, "lease_id": lease_id,
                    "fencing_token": token, "expires_at": stamp(expires_at), "server_time": stamp(now),
                }),
            ))
        }
        ("POST", ["bus", "leases", lease_id, "renew"]) => {
            require(inner, HiveRight::BusRelay)?;
            let request: BusLeaseRenewRequestV1 = parse(body)?;
            valid(request.validate())?;
            let lease = inner
                .bus_leases
                .iter_mut()
                .find(|l| {
                    l.lease_id == *lease_id
                        && l.expires_at > now
                        && l.fencing_token == request.fencing_token
                })
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Lease expired or superseded."))?;
            lease.expires_at =
                now + Duration::milliseconds(i64::try_from(request.duration_ms).unwrap_or(0));
            Ok((
                200,
                json!({
                    "schema": BUS_LEASE_SCHEMA, "granted": true, "lease_id": lease.lease_id,
                    "fencing_token": lease.fencing_token, "expires_at": stamp(lease.expires_at), "server_time": stamp(now),
                }),
            ))
        }
        ("DELETE", ["bus", "leases", lease_id]) => {
            require(inner, HiveRight::BusRelay)?;
            let token = query_param(query, "fencing_token").and_then(|v| v.parse::<u64>().ok());
            inner
                .bus_leases
                .retain(|l| !(l.lease_id == *lease_id && Some(l.fencing_token) == token));
            Ok((204, Value::Null))
        }
        ("GET", ["vaults", pid, "tasks"]) => {
            require(inner, HiveRight::TasksRead)?;
            let project = inner
                .projects
                .get(*pid)
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Unknown project."))?;
            let state = query_param(query, "state");
            let tasks: Vec<Value> = project
                .tasks
                .iter()
                .filter(|task| state.is_none_or(|state| task["state"] == state))
                .map(|task| {
                    json!({"number": task["number"], "state": task["state"], "title": task["title"],
                        "origin": task["origin"]["kind"], "updated_at": task["created_at"], "mine": false})
                })
                .collect();
            Ok((
                200,
                json!({"schema": lean_ctx_protocol::hive::tasks::TASK_LIST_SCHEMA, "vault_id": pid, "tasks": tasks}),
            ))
        }
        ("GET", ["vaults", pid, "tasks", number]) => {
            require(inner, HiveRight::TasksRead)?;
            let index = number.parse::<usize>().ok().and_then(|n| n.checked_sub(1));
            inner
                .projects
                .get(*pid)
                .and_then(|project| index.and_then(|i| project.tasks.get(i)))
                .cloned()
                .map(|task| (200, task))
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Unknown task."))
        }
        ("GET", ["me", "optin"]) => Ok((200, inner.optin.clone())),
        ("POST", ["vaults", pid, "tasks", number, "claim"]) => {
            require(inner, HiveRight::TasksWork)?;
            let request: lean_ctx_protocol::hive::tasks::TaskClaimRequestV1 = parse(body)?;
            valid(request.validate())?;
            if !inner.projects.contains_key(*pid) {
                return Err(error(HiveErrorCode::NotFound, "Unknown project."));
            }
            // Hive admits claims only under the engine's active opt-in for
            // the repo, exactly as section 4 answers otherwise.
            let wanted: Option<u64> = number.parse().ok();
            let refuse = |code: HiveErrorCode, reason: &str, message: &str| -> Outcome {
                let mut detail = HiveErrorDetailV1::new(code, message);
                detail.reason = Some(reason.into());
                detail.number = wanted;
                Err(error_with(detail))
            };
            let optin = &inner.optin;
            let Some(sent) = request.optin_id.as_deref() else {
                let mut detail =
                    HiveErrorDetailV1::new(HiveErrorCode::InvalidRequest, "optin_id is required.");
                detail.field = Some("optin_id".into());
                return Err(error_with(detail));
            };
            if optin["active"] != true || optin["optin_id"].is_null() {
                return refuse(
                    HiveErrorCode::InvalidState,
                    "optin_inactive",
                    "This instance is not opted in.",
                );
            }
            if optin["optin_id"].as_str() != Some(sent) {
                return refuse(
                    HiveErrorCode::InvalidState,
                    "optin_changed",
                    "The opt-in changed; read it again before pulling.",
                );
            }
            if !optin["vaults"]
                .as_array()
                .is_some_and(|vaults| vaults.iter().any(|r| r == *pid))
            {
                return refuse(
                    HiveErrorCode::ForbiddenRight,
                    "optin_scope",
                    "The opt-in does not cover this project.",
                );
            }
            // Hive task #17: a task with a role is claimed only by an agent
            // present on the vault's bus address in that role.
            let project = &inner.projects[*pid];
            let task_role = wanted
                .and_then(|n| project.tasks.get(usize::try_from(n).ok()?.checked_sub(1)?))
                .and_then(|task| task["role"].as_str().map(str::to_string));
            if let Some(role) = task_role {
                let address = format!("#{}/{}", project.repo.0, project.repo.1);
                let present = inner
                    .bus_presence
                    .get(&(address, request.agent.clone()))
                    .map(|(_, frame)| frame["role"].as_str().unwrap_or_default().to_string());
                let (check, agent_role) = match present {
                    None => ("presence", None),
                    Some(present) if present != role => ("role", Some(present)),
                    Some(_) => ("", None),
                };
                if !check.is_empty() {
                    let mut detail = HiveErrorDetailV1::new(
                        HiveErrorCode::ForbiddenRight,
                        "The agent is not present on the bus with the task's role.",
                    );
                    detail.reason = Some("not_permitted".into());
                    detail.number = wanted;
                    detail.check = Some(check.into());
                    detail.role = Some(role);
                    detail.agent_role = agent_role;
                    return Err(error_with(detail));
                }
            }
            Ok((
                200,
                json!({
                    "schema": lean_ctx_protocol::hive::tasks::TASK_CLAIM_ACK_SCHEMA,
                    "number": wanted.unwrap_or(0),
                    "claim": {"lease_id": next_id(inner, "lse"), "fencing_token": 1},
                    "until": stamp(now + Duration::minutes(i64::from(request.minutes))),
                }),
            ))
        }
        ("POST", ["vaults", pid, "tasks"]) => {
            require(inner, HiveRight::TasksPropose)?;
            let request: lean_ctx_protocol::hive::tasks::TaskProposeRequestV1 = parse(body)?;
            valid(request.validate())?;
            let created = stamp(now);
            let project = inner
                .projects
                .get_mut(*pid)
                .ok_or_else(|| error(HiveErrorCode::NotFound, "Unknown project."))?;
            let number = project.tasks.len() as u64 + 1;
            let mut task = json!({
                "schema": lean_ctx_protocol::hive::tasks::TASK_SCHEMA, "vault_id": pid,
                "number": number, "state": "open", "origin": request.origin,
                "created_at": created,
                "created_by": {"kind": "instance", "label": "engine", "mine": true},
            });
            for (field, value) in [
                ("title", request.title.map(Value::from)),
                ("goal", request.goal.map(Value::from)),
                ("acceptance", request.acceptance.map(Value::from)),
                ("role", request.role.map(Value::from)),
            ] {
                if let Some(value) = value {
                    task[field] = value;
                }
            }
            project.tasks.push(task);
            Ok((
                200,
                json!({"schema": lean_ctx_protocol::hive::tasks::TASK_ACK_SCHEMA, "number": number, "state": "open"}),
            ))
        }
        _ => Err(error(HiveErrorCode::NotFound, "Unknown route.")),
    }
}
