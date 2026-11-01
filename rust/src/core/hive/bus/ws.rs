// SPDX-License-Identifier: Apache-2.0
//! The relay's WebSocket session with `wss://<hive>/v1/bus`.

use std::collections::HashMap;
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use super::relay::{RelayState, hello, outgoing};
use crate::core::agents::AgentRegistry;
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::config::HiveConfig;
use crate::core::hive::knowledge::screening::Screen;
use crate::core::hive::state::HiveStore;
use crate::core::hive::transport::user_agent;
use crate::core::hive::vault::VaultMap;
use lean_ctx_protocol::hive::bus::BusFrameV1;
use lean_ctx_protocol::hive::lease::HiveRight;
use lean_ctx_protocol::hive::{
    HEADER_INSTANCE, HEADER_NAMES, HEADER_PROTOCOL, HIVE_API_PREFIX, NAMES_VAULT,
    protocol_header_value,
};

#[path = "ws_v2.rs"]
mod v2_session;

const TICK: Duration = Duration::from_secs(2);
/// How often the relay looks for an announce request (`presence::request_announce`).
const WAKE_POLL: Duration = Duration::from_millis(200);
/// How long after a re-hello an `error` without a message id is taken as
/// its refusal (an older Hive): the relay then reconnects instead.
const REHELLO_ANSWER: Duration = Duration::from_secs(10);
/// The longest an open socket goes without announcing its agents again.
const REHELLO_EVERY: Duration = Duration::from_secs(300);
/// The first reconnect after a session that was up. A Hive restart drops
/// every socket at once and presence only returns with the next hello, so
/// the relay comes back within about a second (24.10. prod deploy: the
/// backoff of an earlier outage had kept relays away for ~5 min).
const RECONNECT_FIRST: Duration = Duration::from_secs(1);
/// Random extra wait on every reconnect, so instances do not storm Hive.
const RECONNECT_JITTER_MS: u64 = 3_000;
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// The wait before the next connect: [`RECONNECT_FIRST`] after a session
/// that was up, doubled for every connect that failed since, at most
/// [`BACKOFF_MAX`], plus `jitter_ms`.
fn reconnect_delay(failures: u32, jitter_ms: u64) -> Duration {
    let base = RECONNECT_FIRST
        .saturating_mul(1u32 << failures.min(16))
        .min(BACKOFF_MAX);
    base + Duration::from_millis(jitter_ms.min(RECONNECT_JITTER_MS))
}

fn jitter_ms() -> u64 {
    u64::from_le_bytes(rand_nonce()) % (RECONNECT_JITTER_MS + 1)
}

/// `wss://host/v1/bus` (`ws://` only for loopback origins).
fn ws_url(origin: &str) -> String {
    let rest = origin
        .strip_prefix("https://")
        .map(|host| format!("wss://{host}"))
        .or_else(|| {
            origin
                .strip_prefix("http://")
                .map(|host| format!("ws://{host}"))
        })
        .unwrap_or_else(|| origin.to_string());
    format!("{rest}{HIVE_API_PREFIX}/bus")
}

/// Whether a quota or policy throttle holds bus publishes right now.
fn bus_held(store: &HiveStore) -> bool {
    crate::core::hive::throttle::holding(
        store,
        crate::core::hive::throttle::Scope::Bus,
        chrono::Utc::now(),
    )
    .is_some()
}

/// The error a refused WebSocket handshake means. Hive answers a revoked
/// instance's upgrade with `401 instance_revoked` and its statement (E12),
/// a quota or policy refusal with its own body: the body decides; a 401 or
/// 403 without a contract body keeps its old meaning.
fn handshake_error(error: tokio_tungstenite::tungstenite::Error) -> HiveError {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            let body = response
                .body()
                .as_deref()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                .unwrap_or(serde_json::Value::Null);
            match HiveError::from_response(status, &body) {
                HiveError::Server { .. } if status == 401 => HiveError::Unauthorized,
                HiveError::Server { .. } if status == 403 => HiveError::Forbidden,
                HiveError::Server { status, code } if status < 500 && status != 429 => {
                    HiveError::Offline(format!("bus: Hive answered {status} ({code})"))
                }
                mapped => mapped,
            }
        }
        other => HiveError::Offline(format!(
            "bus: {}",
            other.to_string().chars().take(100).collect::<String>()
        )),
    }
}

/// Errors after which retrying cannot help until the user acts.
fn fatal(error: &HiveError) -> bool {
    matches!(
        error,
        HiveError::NotConnected
            | HiveError::LeaseLapsed
            | HiveError::Revoked(_)
            | HiveError::Forbidden
            | HiveError::Config(_)
            | HiveError::UpgradeRequired { .. }
    )
}

/// The relay ended for good on `error`: a revocation is recorded (every
/// path then fails closed, `revoked.rs`) and named; the line says why, in
/// the words `hive status` and every CLI path use.
fn stopped(store: &HiveStore, error: HiveError) -> HiveError {
    let error = match (&error, store.instance()) {
        (HiveError::Revoked(_), Ok(Some(instance))) => {
            crate::core::hive::revoked::note(store, &instance, error)
        }
        _ => error,
    };
    store.record_error(&format!("bus: {error}"));
    tracing::warn!("hive bus relay stopped: {error}");
    error
}

/// Why a session ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
    /// The service closed it.
    Closed,
    /// An older Hive refused a later hello on the open socket: the relay
    /// reconnects at once so a fresh hello announces the new agents.
    Reannounce,
}

/// The agents a `hello` announces.
fn announced(hello: &BusFrameV1) -> std::collections::BTreeSet<(String, String)> {
    match hello {
        BusFrameV1::Hello { agents, .. } => agents
            .iter()
            .map(|a| (a.agent_id.clone(), a.vault_id.clone()))
            .collect(),
        _ => std::collections::BTreeSet::new(),
    }
}

/// Every agent of a `hello` with its vault and role.
fn announced_roles(hello: &BusFrameV1) -> std::collections::BTreeSet<(String, String, String)> {
    match hello {
        BusFrameV1::Hello { agents, .. } => agents
            .iter()
            .map(|a| (a.agent_id.clone(), a.vault_id.clone(), a.role.clone()))
            .collect(),
        _ => std::collections::BTreeSet::new(),
    }
}

/// Whether a tick's `fresh` hello must go out on the open socket after
/// `last` was sent `since` ago: it announces an agent, a vault or a role
/// `last` did not (a role is set by `ctx_agent register` after the MCP
/// start announced the default one, and task claims check it), or the
/// last one is older than [`REHELLO_EVERY`]. The periodic one restores
/// presence Hive closed meanwhile, e.g. when another socket of this
/// instance ended, and closes agents that left.
fn needs_rehello(last: &BusFrameV1, fresh: &BusFrameV1, since: Duration) -> bool {
    let (last, fresh) = (announced_roles(last), announced_roles(fresh));
    !fresh.is_subset(&last) || (since >= REHELLO_EVERY && !(fresh.is_empty() && last.is_empty()))
}

/// Reconnect until a fatal error (returned, see [`stopped`]), or until
/// `limit` has passed (`None`). `[hive]` as it was at start is not kept:
/// every connect reads it again.
pub(super) async fn run(
    store: HiveStore,
    _started_with: HiveConfig,
    limit: Option<Duration>,
) -> Option<HiveError> {
    let _friction = FrictionEmitter::start(store.clone());
    let deadline = limit.map(|limit| tokio::time::Instant::now() + limit);
    // Connects that failed since the last session that was up.
    let mut failures: u32 = 0;
    loop {
        // `[hive]` is read again before every connect: `bus = "off"` or
        // `enabled = false` pauses the relay without a daemon restart, and
        // switching it back on resumes it.
        let Some(config) = super::relay_config() else {
            tokio::select! {
                () = tokio::time::sleep(TICK) => continue,
                () = sleep_until(deadline) => return None,
            }
        };
        // A C0 2.0 Hive (its instance card answered) gets the v2 hello.
        let v2 = super::relay_v2::speaks_v2(&store);
        let mut connected = false;
        let session = if v2 {
            tokio::select! {
                result = v2_session::session_once(&store, &config, &mut connected) => result,
                () = sleep_until(deadline) => return None,
            }
        } else {
            tokio::select! {
                result = session_once(&store, &config, &mut connected) => result,
                () = sleep_until(deadline) => return None,
            }
        };
        failures = if connected {
            0
        } else {
            failures.saturating_add(1)
        };
        match session {
            // New local agents: announce them in a fresh hello right away.
            Ok(SessionEnd::Reannounce) => continue,
            Ok(SessionEnd::Closed) => {}
            Err(error) if fatal(&error) => return Some(stopped(&store, error)),
            Err(error) => store.record_error(&format!("bus: {error}")),
        }
        tokio::select! {
            () = tokio::time::sleep(reconnect_delay(failures, jitter_ms())) => {}
            () = sleep_until(deadline) => return None,
        }
    }
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// How often the relay looks for closed hours to report (V16.1).
const FRICTION_EVERY: Duration = Duration::from_secs(60);

/// Samples merge conflicts and queues the collaboration findings of each
/// closed hour while the relay runs (`core::hive::friction`; nothing is
/// queued on a 1.x Hive) and stops with it.
struct FrictionEmitter(tokio::task::JoinHandle<()>);

impl FrictionEmitter {
    fn start(store: HiveStore) -> Self {
        Self(tokio::spawn(async move {
            let mut every = tokio::time::interval(FRICTION_EVERY);
            let mut conflicted = std::collections::BTreeSet::new();
            loop {
                every.tick().await;
                let store = store.clone();
                let mut seen = std::mem::take(&mut conflicted);
                conflicted = tokio::task::spawn_blocking(move || {
                    crate::core::hive::friction::sample_merge_conflicts(&store, &mut seen);
                    crate::core::hive::friction::emit_due(&store, chrono::Utc::now());
                    seen
                })
                .await
                .unwrap_or_default();
            }
        }))
    }
}

impl Drop for FrictionEmitter {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn header(value: &str) -> Result<HeaderValue, HiveError> {
    HeaderValue::from_str(value).map_err(|_| HiveError::Local("header value".into()))
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The bus WebSocket. Its TLS client comes from the process-default rustls
/// provider, which must be named first (two providers are compiled in).
async fn open_socket(
    request: tokio_tungstenite::tungstenite::handshake::client::Request,
) -> Result<
    (
        Socket,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    crate::core::crypto_provider::install_default();
    tokio_tungstenite::connect_async(request).await
}

/// One connected session; `Ok` when the service closed it normally or new
/// local agents need a fresh hello. `connected` turns true once the socket
/// is open, so a session that was up is told apart from a failed connect.
async fn session_once(
    store: &HiveStore,
    config: &HiveConfig,
    connected: &mut bool,
) -> Result<SessionEnd, HiveError> {
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let session = Session::load(
        store.clone(),
        crate::core::hive::root_cert::trust_root(store, config, &instance.url)?,
    )?;
    if !session.lease.lease.grants(HiveRight::BusRelay) {
        return Err(HiveError::Forbidden);
    }
    let token = session.bearer_token(false)?;
    let mut request = ws_url(&instance.url)
        .into_client_request()
        .map_err(|e| HiveError::Config(format!("bus URL: {e}")))?;
    let headers = request.headers_mut();
    headers.insert(
        "Authorization",
        header(&format!("Bearer {}", token.as_str()))?,
    );
    headers.insert(HEADER_INSTANCE, header(&instance.instance_id)?);
    headers.insert(HEADER_PROTOCOL, header(&protocol_header_value())?);
    headers.insert(HEADER_NAMES, header(NAMES_VAULT)?);
    headers.insert("User-Agent", header(&user_agent())?);
    let (socket, _) = open_socket(request).await.map_err(handshake_error)?;
    *connected = true;
    let (mut sink, mut stream) = socket.split();
    let data = store.data_dir().to_path_buf();
    let mut state = RelayState::load(&data);
    let mut map = VaultMap::load(&data);
    let mut screen = Screen::for_installation();
    let mut pending: HashMap<String, String> = HashMap::new();
    let mut messages_in_flight = std::collections::BTreeSet::new();
    let send = |frame: &BusFrameV1| {
        serde_json::to_string(frame)
            .map(|text| Message::Text(text.into()))
            .map_err(|e| HiveError::Local(e.to_string()))
    };
    let registry = AgentRegistry::load_or_create();
    let first = hello(&registry, &map, &instance.instance_id, &state);
    let mut announced_agents = announced(&first);
    // When the last re-hello was sent: an `error` without a message id
    // right after it means an older Hive that allows one hello only.
    let mut rehello_sent: Option<tokio::time::Instant> = None;
    let mut last_hello_at = tokio::time::Instant::now();
    sink.send(send(&first)?)
        .await
        .map_err(|e| HiveError::Offline(e.to_string()))?;
    let mut barriers = Barriers::default();
    let barrier = u64::from_le_bytes(rand_nonce());
    sink.send(send(&BusFrameV1::Ping { nonce: barrier })?)
        .await
        .map_err(|e| HiveError::Offline(e.to_string()))?;
    barriers.sent(barrier, announced_agents.clone());
    let mut last_hello = first;
    let mut tick = tokio::time::interval(TICK);
    let mut wake = tokio::time::interval(WAKE_POLL);
    let mut releases: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        tokio::select! {
            // A local agent registered or changed role: tick now, so its
            // hello goes out before the agent's first claim.
            _ = wake.tick() => {
                if super::presence::take_announce(&data) {
                    tick.reset_immediately();
                }
            }
            incoming = stream.next() => {
                let text = match incoming {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = sink.send(Message::Pong(payload)).await;
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(SessionEnd::Closed),
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => return Err(HiveError::Offline(error.to_string())),
                };
                let Ok(frame) = serde_json::from_str::<BusFrameV1>(&text) else {
                    continue;
                };
                // A revoked instance: stop at the first frame that says so.
                if let BusFrameV1::Error { error, .. } = &frame
                    && error.code == lean_ctx_protocol::hive::HiveErrorCode::InstanceRevoked
                {
                    state.save(&data)?;
                    return Err(HiveError::Revoked(crate::core::hive::Revocation::default()));
                }
                if let BusFrameV1::Pong { nonce } = &frame
                    && let Some(confirmed) = barriers.pong(*nonce)
                {
                    // Every project this socket announces must be confirmed.
                    let projects = announced_agents.iter().map(|(_, p)| p.clone()).collect();
                    if let Err(error) = super::control::rebuilt(store, &confirmed, &projects) {
                        store.record_error(&format!("bus control: {error}"));
                    }
                    continue;
                }
                let refused_hello = matches!(&frame, BusFrameV1::Error { client_msg_id: None, .. });
                if refused_hello {
                    barriers.refused();
                }
                if refused_hello
                    && rehello_sent.is_some_and(|at| at.elapsed() < REHELLO_ANSWER)
                {
                    state.save(&data)?;
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(SessionEnd::Reannounce);
                }
                let reply = super::frames::apply(&frame, store, &map, &mut state, &mut pending);
                // Cursor first, ack second: an ack Hive has seen is never
                // replayed after a crash (the hello carries the saved cursor).
                state.save(&data)?;
                if let Some(reply) = reply {
                    sink.send(send(&reply)?).await.map_err(|e| HiveError::Offline(e.to_string()))?;
                }
            }
            _ = tick.tick() => {
                // Switched off in `[hive]` meanwhile: close; `run` waits.
                if super::relay_config().is_none() {
                    state.save(&data)?;
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(SessionEnd::Closed);
                }
                map = VaultMap::load(&data);
                let registry = AgentRegistry::load_or_create();
                // An agent registered after the hello cannot send until a
                // hello announces it: send a fresh one on this socket (C0 1.9
                // allows a later hello; it replaces the announced agents).
                let fresh = hello(&registry, &map, &instance.instance_id, &state);
                if needs_rehello(&last_hello, &fresh, last_hello_at.elapsed()) {
                    sink.send(send(&fresh)?).await.map_err(|e| HiveError::Offline(e.to_string()))?;
                    let barrier = u64::from_le_bytes(rand_nonce());
                    sink.send(send(&BusFrameV1::Ping { nonce: barrier })?)
                        .await
                        .map_err(|e| HiveError::Offline(e.to_string()))?;
                    let fresh_agents = announced(&fresh);
                    barriers.sent(barrier, fresh_agents.clone());
                    announced_agents = fresh_agents;
                    let now = tokio::time::Instant::now();
                    rehello_sent = Some(now);
                    last_hello_at = now;
                    last_hello = fresh;
                }
                // A bus quota or policy throttle: posts stay local until it ends.
                let outgoing_items = if bus_held(store) {
                    Vec::new()
                } else {
                    outgoing(&registry, &map, &session.lease.lease.org_id, &mut state, &mut screen)
                };
                for item in outgoing_items {
                    if pending.values().any(|entry| entry == &item.entry_id) {
                        continue;
                    }
                    if let BusFrameV1::Publish { client_msg_id, .. } = &item.frame {
                        pending.insert(client_msg_id.clone(), item.entry_id.clone());
                    }
                    sink.send(send(&item.frame)?).await.map_err(|e| HiveError::Offline(e.to_string()))?;
                }
                // I1: queued agent messages (once per session) and receipts.
                // (`due` drains receipts, so it is not called while held.)
                let due = if bus_held(store) {
                    Vec::new()
                } else {
                    super::messages::due(&data, &messages_in_flight)
                };
                for frame in due {
                    if let BusFrameV1::Message(message) = &frame {
                        messages_in_flight.insert(message.client_msg_id.clone());
                    }
                    sink.send(send(&frame)?).await.map_err(|e| HiveError::Offline(e.to_string()))?;
                }
                // C5c: handoff offers whose message is out; queued lease releases.
                for frame in super::frames::due_offers(store, &state) {
                    sink.send(send(&frame)?).await.map_err(|e| HiveError::Offline(e.to_string()))?;
                }
                if releases.as_ref().is_none_or(tokio::task::JoinHandle::is_finished) {
                    let (store, root) = (store.clone(), *session.root_key());
                    releases = Some(tokio::task::spawn_blocking(move || {
                        if let Ok(session) = Session::load(store.clone(), root) {
                            super::leases::send_releases(&store, &session);
                        }
                    }));
                }
                state.save(&data)?;
            }
        }
    }
}

/// The barrier ping after every hello (RB8 M12). Hive answers a ping only
/// after it sent the replay of the active controls for the projects that
/// hello newly announced, on this socket, in order; its pong therefore says
/// those agents' controls are complete. A hello Hive refuses is answered
/// with an `error` without a message id, strictly before the pong of its
/// barrier, and replays nothing: that barrier is dropped, so its agents
/// stay held (c9).
#[derive(Default)]
struct Barriers {
    /// Nonce → the agents (agent id, project id) of that hello.
    open: HashMap<u64, std::collections::BTreeSet<(String, String)>>,
    /// The barrier of the newest hello, while unanswered.
    last: Option<u64>,
}

impl Barriers {
    fn sent(&mut self, nonce: u64, agents: std::collections::BTreeSet<(String, String)>) {
        self.open.insert(nonce, agents);
        self.last = Some(nonce);
    }

    /// An `error` without a message id: the newest hello was refused.
    fn refused(&mut self) {
        if let Some(nonce) = self.last.take() {
            self.open.remove(&nonce);
        }
    }

    /// The agents a pong confirms (once), `None` for any other pong.
    fn pong(&mut self, nonce: u64) -> Option<std::collections::BTreeSet<(String, String)>> {
        if self.last == Some(nonce) {
            self.last = None;
        }
        self.open.remove(&nonce)
    }
}

/// Eight random bytes for the barrier ping (zeros if the OS refuses: the
/// barrier then still works, it only becomes guessable).
fn rand_nonce() -> [u8; 8] {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDER_CHILD: &str = "LEAN_CTX_TEST_WS_PROVIDER_CHILD";

    /// A revoked engine's relay stops instead of reconnecting forever.
    #[test]
    fn a_revocation_ends_the_relay() {
        assert!(fatal(&HiveError::Revoked(
            crate::core::hive::Revocation::default()
        )));
        assert!(!fatal(&HiveError::Offline("reset".into())));
    }

    const REVOKED_BODY: &str = include_str!(
        "../../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes/instance_revoked.error.json"
    );

    fn refused_upgrade(status: u16, body: Option<&str>) -> HiveError {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(status)
            .body(body.map(|b| b.as_bytes().to_vec()))
            .unwrap();
        handshake_error(tokio_tungstenite::tungstenite::Error::Http(Box::new(
            response,
        )))
    }

    /// Hive answers a revoked instance's upgrade with `401
    /// instance_revoked`: the relay reads the body and stops at once,
    /// instead of taking it for an expired token and reconnecting.
    #[test]
    fn a_refused_upgrade_is_read_by_its_body() {
        let revoked = refused_upgrade(401, Some(REVOKED_BODY));
        assert!(
            matches!(&revoked, HiveError::Revoked(r) if r.reason.as_deref() == Some("key_compromise"))
        );
        assert!(fatal(&revoked));
        assert_eq!(refused_upgrade(401, None), HiveError::Unauthorized);
        assert_eq!(refused_upgrade(403, Some("<html>")), HiveError::Forbidden);
        let denied = refused_upgrade(
            403,
            Some(
                r#"{"error":{"code":"policy_denied","message":"x","member":"sharing.allowed_vaults","level":"org"}}"#,
            ),
        );
        assert!(matches!(denied, HiveError::PolicyDenied(_)));
        assert!(
            !fatal(&denied),
            "a policy may change: the relay keeps reconnecting with its backoff"
        );
        let busy = refused_upgrade(503, None);
        assert!(busy.is_transient() && !fatal(&busy));
        let quota = refused_upgrade(
            429,
            Some(
                r#"{"error":{"code":"quota_exceeded","message":"x","resource":"transfer_bytes","retry_after_s":60}}"#,
            ),
        );
        assert!(quota.is_transient() && !fatal(&quota));
        assert!(matches!(refused_upgrade(404, None), HiveError::Offline(_)));
    }

    fn revoked_store() -> (tempfile::TempDir, HiveStore, HiveConfig) {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let instance = crate::core::hive::state::InstanceRecord {
            instance_id: "inst_8e3c0d5f2b4e4f609b8c1d2e3f4a5b6c".into(),
            org_id: "4b6d8f0a-2c4e-4f6a-8b0c-2e4f6a8c0e1a".into(),
            org_name: "acme".into(),
            label: "laptop".into(),
            url: lean_ctx_protocol::hive::HIVE_DEFAULT_URL.into(),
            org_salt_id: "s".into(),
            enrolled_at: "2026-10-25T10:00:00Z".into(),
            web_url: None,
            org_kind: None,
            company_name: None,
        };
        store
            .write_config(crate::core::hive::state::INSTANCE_FILE, &instance)
            .unwrap();
        let config = HiveConfig {
            root_key: Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=".into()),
            ..HiveConfig::default()
        };
        (dir, store, config)
    }

    const REVOKED_LINE: &str = "this engine was revoked by acme (key compromise); reconnect with \
         `lean-ctx hive connect` after an admin allows it";

    /// A revocation frame ends the session; the relay records it (every
    /// path then fails closed) and says so in the CLI's own words.
    #[test]
    fn a_stopped_relay_records_the_revocation_and_names_it() {
        let (_dir, store, _) = revoked_store();
        let shown = stopped(&store, refused_upgrade(401, Some(REVOKED_BODY)));
        assert_eq!(shown.to_string(), REVOKED_LINE);
        let instance = store.instance().unwrap().unwrap();
        assert_eq!(
            crate::core::hive::revoked::recorded(&store, &instance),
            Some(shown.clone())
        );
        assert_eq!(
            store.activity().last_error.as_deref(),
            Some(format!("bus: {REVOKED_LINE}").as_str())
        );
    }

    /// Once recorded, the relay stops at its first connect, without
    /// waiting out its deadline and without any network call.
    #[test]
    fn a_recorded_revocation_stops_the_relay_at_once() {
        let (_dir, store, config) = revoked_store();
        let _ = stopped(&store, refused_upgrade(401, Some(REVOKED_BODY)));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let started = std::time::Instant::now();
        let end = runtime.block_on(run(store, config, Some(Duration::from_secs(60))));
        assert_eq!(end.map(|e| e.to_string()).as_deref(), Some(REVOKED_LINE));
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no reconnect loop"
        );
    }

    /// The relay's wss:// connect must not panic on the rustls provider. In a
    /// fresh process (this test re-runs itself, so no other test installed a
    /// default first) the TLS handshake against a peer that closes at once
    /// ends in an error, never in "Could not automatically determine the
    /// process-level CryptoProvider".
    #[test]
    fn a_wss_connect_names_the_crypto_provider_itself() {
        if std::env::var_os(PROVIDER_CHILD).is_some() {
            assert!(
                rustls::crypto::CryptoProvider::get_default().is_none(),
                "fresh process"
            );
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listen");
            let port = listener.local_addr().expect("addr").port();
            std::thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    drop(stream);
                }
            });
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let request = format!("wss://127.0.0.1:{port}/v1/bus")
                .into_client_request()
                .expect("request");
            assert!(runtime.block_on(open_socket(request)).is_err());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "core::hive::bus::ws::tests::a_wss_connect_names_the_crypto_provider_itself",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROVIDER_CHILD, "1")
            .output()
            .expect("child test");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(!stderr.contains("CryptoProvider"), "{stderr}");
    }

    fn hello_with(agents: &[(&str, &str)]) -> BusFrameV1 {
        let agents: Vec<_> = agents.iter().map(|(a, v)| (*a, *v, "dev")).collect();
        hello_with_roles(&agents)
    }

    fn hello_with_roles(agents: &[(&str, &str, &str)]) -> BusFrameV1 {
        BusFrameV1::Hello {
            instance_id: "inst_a".into(),
            agents: agents
                .iter()
                .map(
                    |(agent, vault, role)| lean_ctx_protocol::hive::bus::BusAgentV1 {
                        agent_id: (*agent).into(),
                        role: (*role).into(),
                        vault_id: (*vault).into(),
                    },
                )
                .collect(),
            cursors: std::collections::BTreeMap::new(),
        }
    }

    /// Dogfooding (RC blocker #4): an MCP process announces its default
    /// role at start and `ctx_agent register role=web` changes it later;
    /// Hive checks that role on a claim, so the change must go out on the
    /// open socket. And presence Hive closed meanwhile (another socket of
    /// the instance ended) comes back within REHELLO_EVERY.
    #[test]
    fn a_changed_role_and_a_quiet_interval_send_a_fresh_hello() {
        let v = "a".repeat(32);
        let at_start = hello_with_roles(&[("mcp-1", &v, "coder"), ("mcp-2", &v, "lead")]);
        let same = hello_with_roles(&[("mcp-1", &v, "coder"), ("mcp-2", &v, "lead")]);
        let quiet = Duration::from_secs(5);
        assert!(!needs_rehello(&at_start, &same, quiet));
        let registered = hello_with_roles(&[("mcp-1", &v, "web"), ("mcp-2", &v, "lead")]);
        assert!(needs_rehello(&at_start, &registered, quiet), "a new role");
        let left = hello_with_roles(&[("mcp-2", &v, "lead")]);
        assert!(
            !needs_rehello(&at_start, &left, quiet),
            "an agent that left waits"
        );
        assert!(
            needs_rehello(&at_start, &same, REHELLO_EVERY),
            "the periodic one"
        );
        let nobody = hello_with_roles(&[]);
        assert!(
            needs_rehello(&at_start, &nobody, REHELLO_EVERY),
            "the last one left"
        );
        assert!(
            !needs_rehello(&nobody, &nobody, REHELLO_EVERY),
            "nothing to announce"
        );
    }

    /// The re-hello rule: a tick's hello that announces an agent (or an
    /// agent in another project) the socket's hello did not is a reason to
    /// send it; one that only lost agents is not. (A real late registration
    /// in another process runs end to end in the suite:
    /// an_agent_registered_after_the_relay_connected_is_announced_and_can_send.)
    #[test]
    fn only_a_newly_announced_agent_needs_a_fresh_hello() {
        let p = "a".repeat(32);
        let q = "b".repeat(32);
        let at_connect = announced(&hello_with(&[("claude-1", &p)]));
        assert!(announced(&hello_with(&[("claude-1", &p)])).is_subset(&at_connect));
        let late = announced(&hello_with(&[("claude-1", &p), ("codex-2", &p)]));
        assert!(!late.is_subset(&at_connect), "a new agent is announced");
        let moved = announced(&hello_with(&[("claude-1", &q)]));
        assert!(
            !moved.is_subset(&at_connect),
            "a new project of an agent too"
        );
        let finished = announced(&hello_with(&[("codex-2", &p)]));
        assert!(finished.is_subset(&late), "an agent that left is no reason");
        assert!(announced(&BusFrameV1::Ping { nonce: 1 }).is_empty());
    }

    /// RB8 M12 (c9): a pong confirms the agents of its hello once; a hello
    /// Hive refused (its `error` comes before the pong) confirms nobody.
    #[test]
    fn a_refused_hello_confirms_no_agent() {
        let p = "0123456789abcdef0123456789abcdef".to_string();
        let first = announced(&hello_with(&[("claude-1", &p)]));
        let late = announced(&hello_with(&[("claude-1", &p), ("codex-2", &p)]));
        let mut barriers = Barriers::default();
        barriers.sent(1, first.clone());
        assert_eq!(barriers.pong(1), Some(first));
        assert_eq!(barriers.pong(1), None, "once");
        assert_eq!(barriers.pong(7), None, "an unknown pong");
        // A re-hello refused: error, then its pong.
        barriers.sent(2, late.clone());
        barriers.refused();
        assert_eq!(barriers.pong(2), None, "nothing was replayed");
        // An error with no hello pending changes nothing later.
        barriers.refused();
        barriers.sent(3, late.clone());
        assert_eq!(barriers.pong(3), Some(late));
    }

    /// 24.10. prod deploy: after a Hive restart, presence came back only
    /// ~5 min later, because a session that ended with an error kept the
    /// backoff of an earlier outage. After a session that was up the relay
    /// reconnects within a second plus at most 3 s of jitter; only failed
    /// connects double the wait, up to the cap.
    #[test]
    fn a_relay_that_was_up_reconnects_within_seconds() {
        assert_eq!(reconnect_delay(0, 0), Duration::from_secs(1));
        assert_eq!(reconnect_delay(0, 3_000), Duration::from_secs(4));
        assert_eq!(
            reconnect_delay(0, 999_999),
            Duration::from_secs(4),
            "jitter is bounded"
        );
        assert_eq!(reconnect_delay(1, 0), Duration::from_secs(2));
        assert_eq!(reconnect_delay(3, 0), Duration::from_secs(8));
        assert_eq!(reconnect_delay(9, 0), BACKOFF_MAX);
        assert_eq!(reconnect_delay(u32::MAX, 0), BACKOFF_MAX);
        for _ in 0..100 {
            assert!(jitter_ms() <= RECONNECT_JITTER_MS);
        }
    }

    #[test]
    fn bus_urls_follow_the_origin_scheme() {
        assert_eq!(
            ws_url("https://hive.leanctx.com"),
            "wss://hive.leanctx.com/v1/bus"
        );
        assert_eq!(
            ws_url("http://127.0.0.1:8100"),
            "ws://127.0.0.1:8100/v1/bus"
        );
    }
}
