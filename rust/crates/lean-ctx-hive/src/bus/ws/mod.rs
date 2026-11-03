// SPDX-License-Identifier: Apache-2.0
//! The relay's WebSocket with `wss://<hive>/v1/bus` (bus v2, C0 2.x):
//! reconnects, backoff and the handshake. One connected session is
//! [`session`].
//!
//! The relay connects only once the instance card (E44) has said that the
//! enrolled Hive speaks C0 2.x (`relay_v2::speaks_v2`); until then it asks
//! for the card again every [`CARD_RETRY`] and waits.

use std::time::Duration;

use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::HiveError;
use crate::config::HiveConfig;
use crate::state::HiveStore;
use lean_ctx_protocol::hive::HIVE_API_PREFIX;

mod session;

const TICK: Duration = Duration::from_secs(2);
/// How often the relay looks for an announce request (`presence::request_announce`).
const WAKE_POLL: Duration = Duration::from_millis(200);
/// How often the relay asks for the instance card while it does not know
/// yet that the enrolled Hive speaks C0 2.x.
const CARD_RETRY: Duration = Duration::from_secs(60);
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
    crate::throttle::holding(store, crate::throttle::Scope::Bus, chrono::Utc::now()).is_some()
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
            crate::revoked::note(store, &instance, error)
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
    /// The subscriptions or the announced workers changed, or Hive asked
    /// for a resync: the relay reconnects at once with a fresh hello.
    Reannounce,
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
    // When the instance card is asked for next (now, at the first pass).
    let mut card_due: Option<tokio::time::Instant> = None;
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
        // Only a Hive whose instance card answered speaks the bus: until
        // then ask for the card now and then, and wait.
        if !super::relay_v2::speaks_v2(&store) {
            if card_due.is_none_or(|at| at <= tokio::time::Instant::now()) {
                card_due = Some(tokio::time::Instant::now() + CARD_RETRY);
                let (card_store, card_config) = (store.clone(), config.clone());
                let card = tokio::task::spawn_blocking(move || {
                    super::instance_card(&card_store, &card_config)
                })
                .await
                .unwrap_or_else(|e| Err(HiveError::Local(e.to_string())));
                match card {
                    Err(error) if fatal(&error) => return Some(stopped(&store, error)),
                    Err(error) => store.record_error(&format!("bus: instance card: {error}")),
                    Ok(()) => {}
                }
                if super::relay_v2::speaks_v2(&store) {
                    continue;
                }
            }
            tokio::select! {
                () = tokio::time::sleep(TICK) => continue,
                () = sleep_until(deadline) => return None,
            }
        }
        let mut connected = false;
        let session = tokio::select! {
            result = session::session_once(&store, &config, &mut connected) => result,
            () = sleep_until(deadline) => return None,
        };
        failures = if connected {
            0
        } else {
            failures.saturating_add(1)
        };
        match session {
            // A fresh hello is due: reconnect right away.
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
/// closed hour while the relay runs (`core::hive::friction`) and stops
/// with it.
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
                    crate::friction::sample_merge_conflicts(&store, &mut seen);
                    crate::friction::emit_due(&store, chrono::Utc::now());
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
    lean_ctx::core::crypto_provider::install_default();
    tokio_tungstenite::connect_async(request).await
}

/// Eight random bytes for the reconnect jitter (zeros if the OS refuses).
fn rand_nonce() -> [u8; 8] {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

    const PROVIDER_CHILD: &str = "LEAN_CTX_TEST_WS_PROVIDER_CHILD";

    /// A revoked engine's relay stops instead of reconnecting forever.
    #[test]
    fn a_revocation_ends_the_relay() {
        assert!(fatal(&HiveError::Revoked(crate::Revocation::default())));
        assert!(!fatal(&HiveError::Offline("reset".into())));
    }

    const REVOKED_BODY: &str = include_str!(
        "../../../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes/instance_revoked.error.json"
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
        let instance = crate::state::InstanceRecord {
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
            .write_config(crate::state::INSTANCE_FILE, &instance)
            .unwrap();
        let config = HiveConfig {
            root_key: Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=".into()),
            ..HiveConfig::default()
        };
        (dir, store, config)
    }

    const REVOKED_LINE: &str = "this instance was revoked by acme (key compromise); reconnect with \
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
            crate::revoked::recorded(&store, &instance),
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
                "bus::ws::tests::a_wss_connect_names_the_crypto_provider_itself",
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
