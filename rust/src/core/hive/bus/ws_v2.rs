// SPDX-License-Identifier: Apache-2.0
//! The relay's C0 2.0 WebSocket session (E14 `GET /v1/bus`, first frame
//! `hello` with `protocol: "2.0"`). Decisions live in `relay_v2`; this
//! loop registers the workers it announces (E10), moves frames and keeps
//! the state file current.

use std::collections::{BTreeMap, BTreeSet};

use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use super::super::relay_v2::{self, StateV2};
use super::super::v2::{Inbound, PresenceEntry, parse_frame};
use super::{SessionEnd, TICK, bus_held, handshake_error, header, open_socket, ws_url};
use crate::core::agents::{AgentEntry, AgentRegistry};
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::config::HiveConfig;
use crate::core::hive::knowledge::screening::Screen;
use crate::core::hive::state::HiveStore;
use crate::core::hive::transport::{PROTOCOL_V2, user_agent};
use crate::core::hive::vault::VaultMap;
use crate::core::hive::workers::attach::{self, LocalWorker, detect_host_app};
use crate::core::hive::workers::card::{Availability, Budget, WorkerCard};
use crate::core::hive::workers::register::ensure_registered;
use lean_ctx_protocol::hive::lease::HiveRight;
use lean_ctx_protocol::hive::{HEADER_INSTANCE, HEADER_PROTOCOL};

/// The worker a local agent attaches to (host app from its transport or
/// agent type, role from `ctx_agent register`).
fn attach_agent(
    store: &HiveStore,
    instance_id: &str,
    agent: &AgentEntry,
) -> Result<LocalWorker, HiveError> {
    let transport = agent.transport.as_ref();
    let host = detect_host_app(
        transport.map(|t| t.host.as_str()),
        Some(agent.agent_type.as_str()),
        std::iter::empty::<(&str, &str)>(),
        transport.and_then(|t| t.name.as_deref()),
    );
    Ok(attach::attach(
        store,
        instance_id,
        &host,
        agent.role.as_deref().unwrap_or_default(),
    )?
    .worker)
}

/// Register (E10, once) the workers of the active local agents in the
/// subscribed vaults; agent id → registered worker. A failed registration
/// leaves that agent out until the next pass.
fn workers_of(session: &Session, agents: &[AgentEntry]) -> BTreeMap<String, LocalWorker> {
    let instance_id = session.instance.instance_id.clone();
    let mut out = BTreeMap::new();
    for agent in agents {
        let result = attach_agent(&session.store, &instance_id, agent).and_then(|worker| {
            let card = WorkerCard::new(
                &worker.host_app,
                None,
                Vec::new(),
                Budget::default(),
                Availability::Online,
            );
            ensure_registered(session, &worker, &card)
        });
        match result {
            Ok(worker) => {
                out.insert(agent.agent_id.clone(), worker);
            }
            Err(error) => session
                .store
                .record_error(&format!("bus worker registration: {error}")),
        }
    }
    out
}

/// Presence entries, one per worker (active if any of its sessions is).
fn presence(
    state: &StateV2,
    agents: &[AgentEntry],
    workers: &BTreeMap<String, LocalWorker>,
) -> Vec<PresenceEntry> {
    let mut by_worker: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for agent in agents {
        if let Some(worker) = workers.get(&agent.agent_id) {
            let active = agent.status == crate::core::agents::AgentStatus::Active;
            let slot = by_worker
                .entry(worker.worker_id.clone())
                .or_insert_with(|| (worker.role.clone(), false));
            slot.1 |= active;
        }
    }
    by_worker
        .into_iter()
        .map(|(worker_id, (role, active))| PresenceEntry {
            state: relay_v2::presence_state(state, &worker_id, active),
            worker_id,
            role,
        })
        .collect()
}

/// Active local agents in linked vaults, and the subscriptions they make.
fn local_view(map: &VaultMap) -> (Vec<AgentEntry>, Vec<String>) {
    let registry = AgentRegistry::load_or_create();
    let active: Vec<&AgentEntry> = registry.list_active(None);
    let subscribe = relay_v2::subscriptions(map, &active);
    let agents = active.into_iter().cloned().collect();
    (agents, subscribe)
}

/// Code of the frames that say this instance was revoked (Hive'
/// heartbeat sends `place.denied` per place, then closes the socket).
const INSTANCE_REVOKED: &str = "instance_revoked";

/// Whether `frame` says this instance was revoked: a `place.denied`, a
/// denied hello address or an `error` with code `instance_revoked`.
fn revocation_in(frame: &Inbound) -> bool {
    match frame {
        Inbound::PlaceDenied { code, .. } | Inbound::Error { code, .. } => code == INSTANCE_REVOKED,
        Inbound::HelloAck(ack) => ack["denied"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|d| d["code"] == INSTANCE_REVOKED),
        _ => false,
    }
}

/// The refusal a publish `error` frame of this code means when it is no
/// refusal for good: a used-up quota (`quota_exceeded`; the bus counts
/// `bus_messages`) or a policy hold (`policy_denied`). A quota frame's own
/// `resource, limit, used, reset_at, retry_after_s` are kept, so the bus
/// throttle waits `retry_after_s`, else until `reset_at`; a frame with
/// only the code waits for the calendar month's reset (API §8.1).
fn publish_hold(code: &str, quota: Option<&Value>) -> Option<HiveError> {
    let body = match code {
        "quota_exceeded" => {
            let mut error = json!({"code": code, "message": "", "resource": "bus_messages"});
            if let (Some(fields), Some(error)) =
                (quota.and_then(Value::as_object), error.as_object_mut())
            {
                error.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            json!({ "error": error })
        }
        "policy_denied" => json!({"error": {"code": code, "message": ""}}),
        _ => return None,
    };
    let status = if code == "policy_denied" { 403 } else { 429 };
    Some(HiveError::from_response(status, &body))
}

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default()
}

/// One connected v2 session; `Ok(Reannounce)` when the subscriptions or
/// the announced workers changed and a fresh hello is due. `connected` is
/// set once the socket is open, so the caller tells a session that was up
/// from a failed connect.
pub(super) async fn session_once(
    store: &HiveStore,
    config: &HiveConfig,
    connected: &mut bool,
) -> Result<SessionEnd, HiveError> {
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(store, config, &instance.url)?;
    let session = Session::load(store.clone(), root)?;
    if !session.lease.lease.grants(HiveRight::BusRelay) {
        return Err(HiveError::Forbidden);
    }
    let data = store.data_dir().to_path_buf();
    let mut state = StateV2::load(&data);
    // Nothing is in flight on a fresh socket: what the last one sent without
    // `message.stored` goes again (Hive deduplicates by client_msg_id).
    state.in_flight.clear();
    let mut map = VaultMap::load(&data);
    let (agents, subscribe) = local_view(&map);
    let (session, workers) = {
        let agents = agents.clone();
        tokio::task::spawn_blocking(move || {
            // Control keys come from key set v3 (E13); a refused or missing
            // set keeps the last verified one, and without one every
            // control is ignored and counted.
            if let Err(error) = crate::core::hive::keyset_v3::refresh(&session) {
                session.store.record_error(&format!("key set v3: {error}"));
            }
            let workers = workers_of(&session, &agents);
            (session, workers)
        })
        .await
        .map_err(|e| HiveError::Local(e.to_string()))?
    };
    let keys = crate::core::hive::keyset_v3::load(store, session.root_key())
        .map(|set| set.keys_of(crate::core::hive::keyset_v3::Purpose::Control))
        .unwrap_or_default();
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
    // The request header decides the protocol Hive answers in: this session
    // speaks C0 2.0 (bus v2 frames), so the upgrade says so.
    headers.insert(HEADER_PROTOCOL, header(PROTOCOL_V2)?);
    headers.insert("User-Agent", header(&user_agent())?);
    let (socket, _) = open_socket(request).await.map_err(handshake_error)?;
    *connected = true;
    let (mut sink, mut stream) = socket.split();
    let text = |frame: &Value| Message::Text(frame.to_string().into());
    let offline = |e: tokio_tungstenite::tungstenite::Error| HiveError::Offline(e.to_string());
    let mut announced = presence(&state, &agents, &workers);
    sink.send(text(&relay_v2::hello(&subscribe, &state, &announced)))
        .await
        .map_err(offline)?;
    let local_workers: BTreeSet<String> = workers.values().map(|w| w.worker_id.clone()).collect();
    let mut screen = Screen::for_installation();
    let mut tick = tokio::time::interval(TICK);
    loop {
        tokio::select! {
            incoming = stream.next() => {
                let raw = match incoming {
                    Some(Ok(Message::Text(raw))) => raw.to_string(),
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = sink.send(Message::Pong(payload)).await;
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        state.save(&data)?;
                        return Ok(SessionEnd::Closed);
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => return Err(HiveError::Offline(error.to_string())),
                };
                let Ok(frame) = parse_frame(&raw) else {
                    continue;
                };
                let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                // A revoked instance: stop at the first frame that says so.
                if revocation_in(&frame) {
                    state.save(&data)?;
                    return Err(HiveError::Revoked(crate::core::hive::Revocation::default()));
                }
                match frame {
                    Inbound::HelloAck(ack) => {
                        store.record_success("bus", &stamp);
                        for denied in ack["denied"].as_array().into_iter().flatten() {
                            store.record_error(&format!(
                                "bus: {} denied ({})",
                                denied["address"].as_str().unwrap_or("?"),
                                denied["code"].as_str().unwrap_or("?")
                            ));
                        }
                    }
                    Inbound::Stored { client_msg_id, .. } => {
                        state.stored(&client_msg_id);
                        relay_v2::settle_queued(&data, &client_msg_id)?;
                        store.record_success("bus", &stamp);
                    }
                    Inbound::Error { client_msg_id: Some(id), code, quota, .. } => {
                        if let Some(refusal) = publish_hold(&code, quota.as_deref()) {
                            // A used-up bus quota or a policy hold: the post
                            // is not refused, it stays queued and goes again
                            // once the throttle ends (nothing is published
                            // meanwhile).
                            state.in_flight.remove(&id);
                            store.record_error(&format!("bus: {refusal}"));
                            let _ = crate::core::hive::throttle::note(
                                store,
                                crate::core::hive::throttle::Scope::Bus,
                                refusal,
                                chrono::Utc::now(),
                            );
                        } else {
                            state.refused(&id);
                            relay_v2::settle_queued(&data, &id)?;
                            store.record_error(&format!("bus publish refused: {code}"));
                        }
                    }
                    Inbound::Error { client_msg_id: None, code, reason, .. } => {
                        store.record_error(&format!(
                            "bus: {code}{}",
                            reason.map(|r| format!(" ({r})")).unwrap_or_default()
                        ));
                    }
                    Inbound::Deliver(message) => {
                        let applied = AgentRegistry::mutate_locked(|registry| {
                            relay_v2::apply_delivery(registry, &map, &mut state, &message, &local_workers)
                        });
                        if let Ok((_, Err(error))) = applied {
                            store.record_error(&format!("bus delivery: {error}"));
                        }
                    }
                    Inbound::Control { lifted, control } => {
                        // A verified veto of a host lock or handoff drops it
                        // here and refuses its holder once (§3.7.4, V9.5).
                        let veto = (!lifted
                            && control.action == super::super::v2::ControlAction::Veto)
                            .then(|| (*control).clone());
                        if state.apply_control(lifted, *control, &keys)
                            && let Some(control) = veto
                        {
                            let worker_of = |agent: &str| {
                                workers.get(agent).map(|w| w.worker_id.clone())
                            };
                            if let Err(error) =
                                super::super::veto::apply(store, &control, &worker_of)
                            {
                                store.record_error(&format!("bus veto: {error}"));
                            }
                        }
                    }
                    Inbound::PlaceDenied { place, code } => {
                        store.record_error(&format!("bus: {place} denied ({code})"));
                        state.denied.insert(place, code);
                    }
                    // A cleared queue: reconnect, the hello resumes from the cursors.
                    Inbound::Resync { .. } => {
                        state.save(&data)?;
                        return Ok(SessionEnd::Reannounce);
                    }
                    Inbound::Ping => {
                        sink.send(text(&json!({"t": "pong"}))).await.map_err(offline)?;
                    }
                    Inbound::ConsentAnswered { consent_id } => {
                        crate::core::hive::consent::web::note_answered(&data, &consent_id);
                    }
                    Inbound::Presence { .. } | Inbound::Other(_) => {}
                }
                state.save(&data)?;
            }
            _ = tick.tick() => {
                // Switched off in `[hive]` meanwhile: close; `run` waits.
                if super::super::relay_config().is_none() {
                    state.save(&data)?;
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(SessionEnd::Closed);
                }
                map = VaultMap::load(&data);
                let (fresh_agents, fresh_subscribe) = local_view(&map);
                let fresh_ids: BTreeSet<&str> = fresh_agents.iter().map(|a| a.agent_id.as_str()).collect();
                let known_ids: BTreeSet<&str> = agents.iter().map(|a| a.agent_id.as_str()).collect();
                if fresh_subscribe != subscribe || !fresh_ids.is_subset(&known_ids) {
                    state.save(&data)?;
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(SessionEnd::Reannounce);
                }
                let fresh = presence(&state, &fresh_agents, &workers);
                if fresh != announced {
                    // A worker changed state (held, idle): tell each place.
                    for address in &subscribe {
                        for entry in fresh.iter().filter(|e| !announced.contains(e)) {
                            let update = json!({
                                "t": "presence.update", "address": address,
                                "worker_id": entry.worker_id, "role": entry.role,
                                "state": entry.state, "at": now_ms(),
                            });
                            sink.send(text(&update)).await.map_err(offline)?;
                        }
                    }
                    announced = fresh;
                }
                // A bus quota or policy throttle: posts stay local until it ends.
                if !bus_held(store) {
                    let registry = AgentRegistry::load_or_create();
                    let worker_of = |agent: &AgentEntry| workers.get(&agent.agent_id).map(|w| w.worker_id.clone());
                    for item in relay_v2::outgoing(&registry, &map, &mut state, &mut screen, &worker_of) {
                        sink.send(text(&item.frame)).await.map_err(offline)?;
                    }
                    for frame in relay_v2::queued_frames(&data, &mut state) {
                        sink.send(text(&frame)).await.map_err(offline)?;
                    }
                }
                state.save(&data)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: &Value) -> Inbound {
        parse_frame(&value.to_string()).unwrap()
    }

    /// Hive' heartbeat on a revoked instance (`place.denied
    /// instance_revoked` per place, then close): the first such frame
    /// stops the relay; other denials do not.
    #[test]
    fn the_first_revocation_frame_stops_the_session() {
        assert!(revocation_in(&frame(
            &json!({"t": "place.denied", "place": "vault:x", "code": "instance_revoked"})
        )));
        assert!(revocation_in(&frame(
            &json!({"t": "error", "code": "instance_revoked"})
        )));
        assert!(revocation_in(&frame(&json!({"t": "hello.ack", "denied": [
            {"address": "#a/b", "code": "not_member"},
            {"address": "#a/c", "code": "instance_revoked"}
        ]}))));
        assert!(!revocation_in(&frame(
            &json!({"t": "place.denied", "place": "vault:x", "code": "not_member"})
        )));
        assert!(!revocation_in(&frame(
            &json!({"t": "hello.ack", "denied": []})
        )));
    }

    /// A quota or policy refusal of a publish keeps the post; any other
    /// code is a refusal for good.
    #[test]
    fn quota_and_policy_publish_errors_hold_instead_of_refusing() {
        let quota = publish_hold("quota_exceeded", None).unwrap();
        assert!(
            matches!(&quota, HiveError::QuotaExceeded(q) if q.resource == "bus_messages" && q.waits())
        );
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-26T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let (_, throttle) = crate::core::hive::throttle::of_error(
            Some(crate::core::hive::throttle::Scope::Bus),
            &quota,
            now,
        )
        .unwrap();
        assert_eq!(
            throttle.until, "2026-11-01T00:00:00Z",
            "until the month resets"
        );
        assert!(matches!(
            publish_hold("policy_denied", None),
            Some(HiveError::PolicyDenied(_))
        ));
        assert!(publish_hold("screen_blocked", None).is_none());
        assert!(publish_hold("invalid_state", None).is_none());
    }

    /// The bus throttle `until` a `quota_exceeded` publish frame sets at
    /// `now` (2026-10-26 12:00 UTC).
    fn bus_until(raw: &Value) -> String {
        bus_until_at(raw, "2026-10-26T12:00:00Z")
    }

    fn bus_until_at(raw: &Value, now: &str) -> String {
        let Inbound::Error { code, quota, .. } = frame(raw) else {
            panic!("not an error frame: {raw}");
        };
        let refusal = publish_hold(&code, quota.as_deref()).unwrap();
        let now = chrono::DateTime::parse_from_rfc3339(now)
            .unwrap()
            .with_timezone(&chrono::Utc);
        crate::core::hive::throttle::of_error(
            Some(crate::core::hive::throttle::Scope::Bus),
            &refusal,
            now,
        )
        .unwrap()
        .1
        .until
    }

    /// C0 `bus/frame.error.quota_exceeded.json` (c/follow-final 24d7ecab):
    /// every quota member is read, and its `retry_after_s` (5.5 days) sets
    /// the wait, not its `reset_at`.
    #[test]
    fn the_c0_quota_frame_sets_the_bus_wait_from_its_own_fields() {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/bus/frame.error.quota_exceeded.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw: Value = serde_json::from_slice(&std::fs::read(&path).expect(&path)).unwrap();
        let Inbound::Error {
            client_msg_id,
            code,
            quota,
            ..
        } = frame(&raw)
        else {
            panic!("not an error frame");
        };
        assert_eq!(client_msg_id.as_deref(), raw["client_msg_id"].as_str());
        assert_eq!(code, "quota_exceeded");
        let refusal = publish_hold(&code, quota.as_deref()).unwrap();
        let HiveError::QuotaExceeded(q) = &refusal else {
            panic!("not a quota: {refusal:?}");
        };
        assert_eq!(q.resource, raw["resource"].as_str().unwrap());
        assert_eq!(q.limit, raw["limit"].as_u64());
        assert_eq!(q.used, raw["used"].as_u64());
        assert_eq!(q.reset_at.as_deref(), raw["reset_at"].as_str());
        assert_eq!(q.retry_after, raw["retry_after_s"].as_u64());
        assert!(q.waits());
        // 475 200 s after 2026-10-30 00:00 is 2026-11-04 12:00, past the
        // frame's reset_at (2026-11-01): retry_after_s wins.
        assert_eq!(
            bus_until_at(&raw, "2026-10-30T00:00:00Z"),
            "2026-11-04T12:00:00Z"
        );
    }

    /// A quota frame's `retry_after_s` sets the bus wait, else its
    /// `reset_at`; a frame without them (an older Hive) or with fields of
    /// another type keeps the calendar-month fallback.
    #[test]
    fn a_quota_frame_waits_for_its_own_retry_after_or_reset_at() {
        let error = |extra: Value| {
            let mut f = json!({"t": "error", "client_msg_id": "01M5NKA2Z8B000000000000012",
                "code": "quota_exceeded"});
            f.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            f
        };
        let full = error(
            json!({"resource": "bus_messages", "limit": 10_000, "used": 10_000,
            "reset_at": "2026-11-01T00:00:00Z", "retry_after_s": 600}),
        );
        assert_eq!(
            bus_until(&full),
            "2026-10-26T12:10:00Z",
            "retry_after_s first"
        );
        let Inbound::Error { quota, .. } = frame(&full) else {
            unreachable!()
        };
        let refusal = publish_hold("quota_exceeded", quota.as_deref()).unwrap();
        assert!(matches!(&refusal, HiveError::QuotaExceeded(q)
            if q.resource == "bus_messages" && q.limit == Some(10_000) && q.used == Some(10_000)));
        assert_eq!(
            bus_until(&error(json!({"reset_at": "2026-10-28T06:30:00Z"}))),
            "2026-10-28T06:30:00Z",
            "then reset_at"
        );
        assert_eq!(
            bus_until(&error(json!({}))),
            "2026-11-01T00:00:00Z",
            "no fields: the month's reset"
        );
        assert_eq!(
            bus_until(&error(
                json!({"limit": "many", "retry_after_s": -5, "reset_at": 7})
            )),
            "2026-11-01T00:00:00Z",
            "odd types are ignored, not fatal"
        );
        assert_eq!(
            bus_until(&error(json!({"used": 3, "retry_after_s": 90}))),
            "2026-10-26T12:01:30Z",
            "a good field survives an absent one"
        );
    }

    /// A hostile frame's huge `retry_after_s` holds the bus for the longest
    /// wait (32 days); it never overflows chrono, so the relay loop lives.
    #[test]
    fn a_huge_retry_after_in_a_quota_frame_is_capped_not_a_panic() {
        for huge in [u64::MAX, i64::MAX.unsigned_abs()] {
            let raw = json!({"t": "error", "client_msg_id": "01M5NKA2Z8B000000000000012",
                "code": "quota_exceeded", "retry_after_s": huge});
            let Inbound::Error { quota, .. } = frame(&raw) else {
                unreachable!()
            };
            assert_eq!(
                quota.as_deref().and_then(|q| q["retry_after_s"].as_u64()),
                Some(crate::core::hive::error::RETRY_AFTER_CAP_S),
                "quota_fields caps {huge}"
            );
            assert_eq!(bus_until(&raw), "2026-11-27T12:00:00Z", "{huge}");
        }
    }
}
