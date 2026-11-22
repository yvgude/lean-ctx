// SPDX-License-Identifier: Apache-2.0
//! One connected bus session (E14 `GET /v1/bus`, first frame `hello` with
//! `protocol: "2.0"`). Decisions live in `relay_v2`; this loop registers
//! the workers it announces (E10), moves frames, sends queued lease
//! releases (C5) and keeps the state file current.

use std::collections::{BTreeMap, BTreeSet};

use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use super::super::plan_events::{self, PlanOutcome};
use super::super::relay_v2::{self, StateV2};
use super::super::task_events::{self, Origin, TaskEvent};
use super::super::v2::{
    Inbound, PresenceEntry, PresenceIdentity, machine_name, parse_frame, working_on,
};
use super::{SessionEnd, TICK, WAKE_POLL, bus_held, handshake_error, header, open_socket, ws_url};
use crate::CloudError;
use crate::auth::Session;
use crate::config::CloudConfig;
use crate::knowledge::screening::Screen;
use crate::state::CloudStore;
use crate::transport::{PROTOCOL_V2, user_agent};
use crate::vault::VaultMap;
use crate::workers::attach::{self, LocalWorker};
use crate::workers::card::{Availability, Budget, WorkerCard};
use crate::workers::register::ensure_registered;
use lean_ctx::core::agents::{AgentEntry, AgentRegistry, DirectiveStore};
use lean_ctx_protocol::cloud::lease::CloudRight;
use lean_ctx_protocol::cloud::{HEADER_INSTANCE, HEADER_PROTOCOL};

/// The worker a local agent attaches to (host app from its transport or
/// agent type, role from `ctx_agent register`).
fn attach_agent(
    store: &CloudStore,
    instance_id: &str,
    agent: &AgentEntry,
) -> Result<LocalWorker, CloudError> {
    let (host, role) = attach::agent_worker_key(agent);
    Ok(attach::attach(store, instance_id, &host, &role)?.worker)
}

/// The worker of `agent` in the worker book as it is now: by the same
/// `(host_app, role)` key it attaches with ([`attach::agent_worker_key`]),
/// so a session that registered after the relay connected maps to its
/// worker too (#28).
fn book_worker(book: &[LocalWorker], agent: &AgentEntry) -> Option<String> {
    let (host, role) = attach::agent_worker_key(agent);
    book.iter()
        .find(|w| w.host_app == host && w.role == role)
        .map(|w| w.worker_id.clone())
}

/// Register (E10, once) the workers of the active local agents in the
/// subscribed vaults; agent id → registered worker. A failed registration
/// leaves that agent out until the next pass; its error is kept under the
/// agent id for the relay's report.
fn workers_of(
    session: &Session,
    agents: &[AgentEntry],
) -> (BTreeMap<String, LocalWorker>, BTreeMap<String, String>) {
    let instance_id = session.instance.instance_id.clone();
    let mut out = BTreeMap::new();
    let mut failed = BTreeMap::new();
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
            Err(error) => {
                session
                    .store
                    .record_error(&format!("bus worker registration: {error}"));
                failed.insert(agent.agent_id.clone(), error.to_string());
            }
        }
    }
    (out, failed)
}

/// What a hello Cloud acknowledged announced, in the words `cloud bus relay`
/// prints and `bus/announced.txt` keeps (#252): each worker with its role
/// and state on which buses, and for every agent that is not on a bus,
/// why. `linked` tells whether an agent's checkout belongs to a vault this
/// machine resolved; `denied` are the addresses the hello.ack refused.
fn announce_report(
    subscribe: &[String],
    announced: &[PresenceEntry],
    agents: &[AgentEntry],
    workers: &BTreeMap<String, LocalWorker>,
    failed: &BTreeMap<String, String>,
    linked: impl Fn(&str) -> bool,
    denied: &[String],
) -> Vec<String> {
    if agents.is_empty() {
        return vec![
            "bus relay: connected to LeanCTX Cloud, but no agent is active on this machine's local bus: nobody was announced (an agent is announced once it registers with ctx_agent)".into(),
        ];
    }
    let mut lines = vec![if subscribe.is_empty() {
        "bus relay: connected to LeanCTX Cloud, but no active agent works in a checkout linked to a LeanCTX Cloud vault: LeanCTX Cloud shows nobody on a bus (run `lean-ctx cloud connect` in the checkout)".to_string()
    } else {
        format!(
            "bus relay: connected to LeanCTX Cloud; announced on {}:",
            subscribe.join(", ")
        )
    }];
    for agent in agents {
        let role = agent.role.as_deref().unwrap_or_default();
        let shown_role = if role.is_empty() { "no role" } else { role };
        let line = match workers.get(&agent.agent_id) {
            Some(worker) => {
                let state = announced
                    .iter()
                    .find(|e| e.worker_id == worker.worker_id)
                    .and_then(|e| serde_json::to_value(e.state).ok())
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let mut line = format!(
                    "  {} · {shown_role} · {state}  (agent {} in {})",
                    worker.worker_id, agent.agent_id, agent.project_root
                );
                if !linked(&agent.project_root) {
                    line.push_str(
                        " — not on a bus: its checkout is not linked to a LeanCTX Cloud vault",
                    );
                } else if role.is_empty() {
                    line.push_str(" — no role: register with role=… to claim tasks that need one");
                }
                line
            }
            None => format!(
                "  agent {} ({shown_role}) in {}: not announced, its worker could not be registered: {}",
                agent.agent_id,
                agent.project_root,
                failed
                    .get(&agent.agent_id)
                    .map_or("unknown error", String::as_str)
            ),
        };
        lines.push(line);
    }
    for address in denied {
        lines.push(format!(
            "  {address}: refused by LeanCTX Cloud (see `lean-ctx cloud status`)"
        ));
    }
    lines
}

/// True when every agent in `fresh` was announced with the same role (an
/// agent that left needs no new hello; its worker goes idle).
fn attached_alike(fresh: &[AgentEntry], announced: &[AgentEntry]) -> bool {
    let key = |a: &AgentEntry| (a.agent_id.clone(), a.role.clone());
    let announced: BTreeSet<_> = announced.iter().map(key).collect();
    fresh.iter().all(|a| announced.contains(&key(a)))
}

/// Presence entries, one per worker (active if any of its sessions is),
/// each with the worker's name and this Instance (QN0).
fn presence(
    state: &StateV2,
    agents: &[AgentEntry],
    workers: &BTreeMap<String, LocalWorker>,
    instance: &crate::state::InstanceRecord,
) -> Vec<PresenceEntry> {
    let mut by_worker: BTreeMap<String, (String, bool, Option<String>)> = BTreeMap::new();
    for agent in agents {
        if let Some(worker) = workers.get(&agent.agent_id) {
            let active = agent.status == lean_ctx::core::agents::AgentStatus::Active;
            let slot = by_worker
                .entry(worker.worker_id.clone())
                .or_insert_with(|| (worker.role.clone(), false, None));
            slot.1 |= active;
            if slot.2.is_none() {
                slot.2 = agent
                    .display_name
                    .as_deref()
                    .and_then(presence_line)
                    .map(str::to_owned);
            }
        }
    }
    by_worker
        .into_iter()
        .map(|(worker_id, (role, active, display_name))| PresenceEntry {
            state: relay_v2::presence_state(state, &worker_id, active),
            worker_id,
            role,
            display_name,
            instance_id: Some(instance.instance_id.clone()),
            // h7b: the label's short name, never a domain (a raw
            // `gethostname()` on Linux is often the full name).
            instance_name: machine_name(&instance.label),
        })
        .collect()
}

/// `value` as a presence name: a trimmed single line of at most 128
/// characters, else none.
fn presence_line(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= 128 && !value.chars().any(char::is_control))
        .then_some(value)
}

/// What each worker's team sees besides its role and state (h7b): this
/// machine's short name and, redacted, the status line the agent of the
/// worker's first session that has one wrote itself (LeanCTX's lifecycle
/// notes such as "connection closed" are never one, see
/// [`AgentEntry::agent_status_line`]). Empty when the names are off
/// (`[cloud] presence_names = false`) or Cloud did not acknowledge
/// `presence_identity`, so nothing of it leaves the machine then.
fn identities(
    on: bool,
    agents: &[AgentEntry],
    workers: &BTreeMap<String, LocalWorker>,
    instance: &crate::state::InstanceRecord,
) -> BTreeMap<String, PresenceIdentity> {
    let mut out: BTreeMap<String, PresenceIdentity> = BTreeMap::new();
    if !on {
        return out;
    }
    let machine = machine_name(&instance.label);
    for agent in agents {
        let Some(worker) = workers.get(&agent.agent_id) else {
            continue;
        };
        let slot = out
            .entry(worker.worker_id.clone())
            .or_insert_with(|| PresenceIdentity {
                machine_name: machine.clone(),
                working_on: None,
            });
        if slot.working_on.is_none() {
            slot.working_on = agent.agent_status_line().and_then(working_on);
        }
    }
    out
}

/// One `presence.update` frame for `entry` on `address`, with the team's
/// names when there are any.
fn presence_update(
    address: &str,
    entry: &PresenceEntry,
    identity: Option<&PresenceIdentity>,
    at: u64,
) -> Value {
    let mut update = json!({"t": "presence.update", "address": address, "at": at});
    if let Some(frame) = update.as_object_mut() {
        if let Ok(Value::Object(fields)) = serde_json::to_value(entry) {
            frame.extend(fields);
        }
        if let Some(Ok(Value::Object(fields))) = identity.map(serde_json::to_value) {
            frame.extend(fields);
        }
    }
    update
}

/// Active local agents in linked vaults, and the subscriptions they make.
fn local_view(map: &VaultMap) -> (Vec<AgentEntry>, Vec<String>) {
    let registry = AgentRegistry::load_or_create();
    let active: Vec<&AgentEntry> = registry.list_active(None);
    let subscribe = relay_v2::subscriptions(map, &active);
    let agents = active.into_iter().cloned().collect();
    (agents, subscribe)
}

/// Code of the frames that say this instance was revoked (Cloud'
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
fn publish_hold(code: &str, quota: Option<&Value>) -> Option<CloudError> {
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
    Some(CloudError::from_response(status, &body))
}

fn now_ms() -> u64 {
    u64::try_from(lean_ctx_time::UtcDateTime::now().timestamp_millis()).unwrap_or_default()
}

/// One connected session; `Ok(Reannounce)` when the subscriptions or
/// the announced workers changed and a fresh hello is due. `connected` is
/// set once the socket is open, so the caller tells a session that was up
/// from a failed connect.
pub(super) async fn session_once(
    store: &CloudStore,
    config: &CloudConfig,
    connected: &mut bool,
    report: &super::super::Report,
) -> Result<SessionEnd, CloudError> {
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let root = crate::root_cert::trust_root(store, config, &instance.url)?;
    let session = Session::load(store.clone(), root)?;
    if !session.lease.lease.grants(CloudRight::BusRelay) {
        return Err(CloudError::Forbidden);
    }
    let data = store.data_dir().to_path_buf();
    let mut state = StateV2::load(&data);
    state.begin_connection();
    // The hello below announces the agents as they are now, which answers
    // every announce request made so far (a `ctx_agent register` before
    // the relay ran, the one that ended the last session). Left in place,
    // such a request closed the socket right after the hello and dropped
    // the presence while a claim was on its way (Weave, !386).
    let _ = super::super::presence::take_announce(&data);
    let mut map = crate::resolution::current_view(&data);
    let (agents, subscribe) = local_view(&map);
    // Only a cursor of the place its address resolves to now is sent.
    let places = relay_v2::local_places(&map);
    if relay_v2::drop_moved_cursors(&mut state, &places) {
        state.save(&data)?;
    }
    let (session, (workers, failed)) = {
        let agents = agents.clone();
        tokio::task::spawn_blocking(move || {
            // Control keys come from key set v3 (E13); a refused or missing
            // set keeps the last verified one, and without one every
            // control is ignored and counted.
            if let Err(error) = crate::keyset_v3::refresh(&session) {
                session.store.record_error(&format!("key set v3: {error}"));
            }
            let workers = workers_of(&session, &agents);
            (session, workers)
        })
        .await
        .map_err(|e| CloudError::Local(e.to_string()))?
    };
    let keys = crate::keyset_v3::load(store, session.root_key())
        .map(|set| set.keys_of(crate::keyset_v3::Purpose::Control))
        .unwrap_or_default();
    let token = session.bearer_token(false)?;
    let mut request = ws_url(&instance.url)
        .into_client_request()
        .map_err(|e| CloudError::Config(format!("bus URL: {e}")))?;
    let headers = request.headers_mut();
    headers.insert(
        "Authorization",
        header(&format!("Bearer {}", token.as_str()))?,
    );
    headers.insert(HEADER_INSTANCE, header(&instance.instance_id)?);
    // The request header decides the protocol Cloud answers in: this session
    // speaks C0 2.0 (bus v2 frames), so the upgrade says so.
    headers.insert(HEADER_PROTOCOL, header(PROTOCOL_V2)?);
    headers.insert("User-Agent", header(&user_agent())?);
    let (socket, _) = open_socket(request).await.map_err(handshake_error)?;
    *connected = true;
    let (mut sink, mut stream) = socket.split();
    let text = |frame: &Value| Message::Text(frame.to_string().into());
    let offline = |e: tokio_tungstenite::tungstenite::Error| CloudError::Offline(e.to_string());
    let mut announced = presence(&state, &agents, &workers, &instance);
    sink.send(text(&relay_v2::hello(&subscribe, &state, &announced)))
        .await
        .map_err(offline)?;
    let local_workers: BTreeSet<String> = workers.values().map(|w| w.worker_id.clone()).collect();
    let mut screen = Screen::for_installation();
    let mut tick = tokio::time::interval(TICK);
    let mut wake = tokio::time::interval(WAKE_POLL);
    let mut releases: Option<tokio::task::JoinHandle<()>> = None;
    // The task catch-up after the hello (task.event has no replay), over
    // HTTP and off this loop; its events are applied here.
    let mut catchup: Option<tokio::task::JoinHandle<(Vec<TaskEvent>, Vec<String>)>> = None;
    // §37: plan pulls (HTTP) run off this loop and send what they verified
    // here; the agents' answers go back from a background pass whenever the
    // directive records change (retried after a minute when Cloud failed).
    let (plan_tx, mut plan_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<PlanOutcome>>();
    let mut plans_on = false;
    let mut plan_report: Option<tokio::task::JoinHandle<Vec<String>>> = None;
    let mut plan_report_seen: Option<std::time::SystemTime> = None;
    let mut plan_retry_at: Option<std::time::Instant> = None;
    let mut replay = relay_v2::Replay::default();
    // Cloud takes the `policy.applied` frame (its `hello.ack` names it).
    let mut policy_applied = false;
    // Cloud shows team presence names to the team only (its `hello.ack`
    // names `presence_identity`) and this machine shares them (h7b).
    let mut identity_on = false;
    let mut sent_identity: BTreeMap<String, PresenceIdentity> = BTreeMap::new();
    loop {
        tokio::select! {
            caught = async { catchup.as_mut().expect("guarded by is_some").await }, if catchup.is_some() => {
                catchup = None;
                let Ok((events, errors)) = caught else { continue };
                for error in errors {
                    store.record_error(&format!("bus task catch-up: {error}"));
                }
                for event in &events {
                    let applied = AgentRegistry::mutate_locked(|registry| {
                        task_events::apply(registry, &map, &data, &mut state.tasks, event, Origin::CatchUp)
                    });
                    if let Ok((_, Err(error))) = applied {
                        store.record_error(&format!("bus task event: {error}"));
                    }
                }
                state.save(&data)?;
            }
            Some(outcomes) = plan_rx.recv() => {
                let book = attach::list(store, &instance.instance_id).unwrap_or_default();
                let worker_of = |agent: &AgentEntry| {
                    workers
                        .get(&agent.agent_id)
                        .map(|w| w.worker_id.clone())
                        .or_else(|| book_worker(&book, agent))
                };
                for outcome in outcomes {
                    match outcome {
                        // `409 not_ready` (§37.2): a state shown once, not an error.
                        PlanOutcome::Note(note) if note.ends_with(plan_events::PLANS_NOT_READY) => {
                            report.say(plan_events::PLANS_NOT_READY);
                        }
                        PlanOutcome::Note(note) => store.record_error(&format!("bus plan: {note}")),
                        PlanOutcome::Verified { vault, vault_id, root, doc } => {
                            // Recorded as applied only once its directives are out.
                            let mut plans = state.plans.clone();
                            let applied = DirectiveStore::mutate(|registry, directives| {
                                plan_events::apply(
                                    registry,
                                    directives,
                                    &mut plans,
                                    (&vault, &vault_id, &root, &*doc),
                                    &worker_of,
                                )
                            });
                            match applied {
                                Ok(_) => state.plans = plans,
                                Err(error) => store.record_error(&format!("bus plan: {error}")),
                            }
                        }
                    }
                }
                state.save(&data)?;
            }
            // A local agent registered or changed role, or `cloud bus relay`
            // asked: a fresh hello announces this machine's workers again
            // before the next claim, and its record answers the CLI (#252).
            _ = wake.tick() => {
                if plans_on && plan_report.as_ref().is_none_or(tokio::task::JoinHandle::is_finished) {
                    if let Some(job) = plan_report.take()
                        && let Ok(errors) = job.await
                    {
                        plan_retry_at = (!errors.is_empty())
                            .then(|| std::time::Instant::now() + std::time::Duration::from_secs(60));
                        for error in errors {
                            store.record_error(&format!("bus plan: {error}"));
                        }
                    }
                    let modified = lean_ctx::core::agents::directives_modified();
                    let retry = plan_retry_at.is_some_and(|at| std::time::Instant::now() >= at);
                    if modified.is_some() && (modified != plan_report_seen || retry) {
                        plan_report_seen = modified;
                        plan_retry_at = None;
                        let (map, data) = (map.clone(), data.clone());
                        let carried = lean_ctx::core::context_admission::capture::carry();
                        plan_report = Some(tokio::task::spawn_blocking(move || {
                            carried.enter(|| plan_events::report(&map, &data))
                        }));
                    }
                }
                if super::super::presence::take_announce(&data) {
                    state.save(&data)?;
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(SessionEnd::Reannounce);
                }
            }
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
                    Some(Err(error)) => return Err(CloudError::Offline(error.to_string())),
                };
                let Ok(frame) = parse_frame(&raw) else {
                    continue;
                };
                let stamp = lean_ctx_time::UtcDateTime::now().to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true);
                // A pending rebuild (M12) ends once Cloud's replay is complete.
                relay_v2::note_replay(&mut state, &mut replay, &frame, !subscribe.is_empty());
                // A revoked instance: stop at the first frame that says so.
                if revocation_in(&frame) {
                    state.save(&data)?;
                    return Err(CloudError::Revoked(crate::Revocation::default()));
                }
                match frame {
                    Inbound::HelloAck(ack) => {
                        store.record_success("bus", &stamp);
                        // A policy change missed while offline is pulled at
                        // once on every (re)connect (a 304 when none).
                        let policy = ack["policy_version"].as_u64();
                        if policy.is_some() {
                            crate::policy::note_nudges_seen(store.data_dir());
                        }
                        crate::scheduler::nudge_policy(policy);
                        state.person_publish = relay_v2::acks_person_publish(&ack);
                        policy_applied = relay_v2::acks_feature(&ack, relay_v2::POLICY_APPLIED);
                        identity_on = config.presence_names
                            && relay_v2::acks_feature(&ack, relay_v2::PRESENCE_IDENTITY);
                        sent_identity.clear();
                        // A bus without a cursor keeps the replay start Cloud
                        // named, saved now: the next reconnect resumes there
                        // instead of at Cloud's head. A bus of another place
                        // or epoch (or an unbound seq) is reset to 0 and subscribed afresh (a
                        // hello is only a socket's first frame), so Cloud
                        // replays it from its start.
                        let adopted = relay_v2::adopt_ack_cursors(&mut state, &subscribe, &ack);
                        if adopted.changed {
                            state.save(&data)?;
                        }
                        if adopted.resubscribe {
                            let _ = sink.send(Message::Close(None)).await;
                            return Ok(SessionEnd::Replay);
                        }
                        let mut refused = Vec::new();
                        for denied in ack["denied"].as_array().into_iter().flatten() {
                            let line = format!(
                                "{} denied ({})",
                                denied["address"].as_str().unwrap_or("?"),
                                denied["code"].as_str().unwrap_or("?")
                            );
                            store.record_error(&format!("bus: {line}"));
                            refused.push(line);
                        }
                        // What Cloud now shows of this machine (#252).
                        let lines = announce_report(
                            &subscribe,
                            &announced,
                            &agents,
                            &workers,
                            &failed,
                            |root| relay_v2::record_for(&map, root).is_some(),
                            &refused,
                        );
                        super::super::presence::note_announced(&data, &lines);
                        report.say(&lines.join("\n"));
                        // §37: plan versions published while away, pulled once.
                        plans_on = plan_events::frames_taken(&ack);
                        if plans_on && !subscribe.is_empty() {
                            let (map, subscribe, seen, keys, data, tx) = (
                                map.clone(),
                                subscribe.clone(),
                                state.plans.clone(),
                                keys.clone(),
                                data.clone(),
                                plan_tx.clone(),
                            );
                            let carried = lean_ctx::core::context_admission::capture::carry();
                            tokio::task::spawn_blocking(move || {
                                carried.enter(|| {
                                    let (events, errors) = plan_events::catch_up(&map, &subscribe, &seen);
                                    let mut out: Vec<PlanOutcome> = errors
                                        .into_iter()
                                        .map(|e| PlanOutcome::Note(format!("catch-up {e}")))
                                        .collect();
                                    out.extend(plan_events::fetch(&map, &data, &seen, &keys, &events));
                                    let _ = tx.send(out);
                                });
                            });
                        }
                        // Subscribed now: what changed before is read once.
                        if !subscribe.is_empty() && catchup.is_none() {
                            let (map, subscribe) = (map.clone(), subscribe.clone());
                            catchup = Some(tokio::task::spawn_blocking(move || {
                                task_events::catch_up(&map, &subscribe)
                            }));
                        }
                    }
                    Inbound::Stored { client_msg_id, id, .. } => {
                        // A handoff this machine offered: its message id
                        // is what a maintainer's veto names.
                        if let Err(error) = super::super::handoff::stored(&data, &client_msg_id, &id) {
                            store.record_error(&format!("bus handoff: {error}"));
                        }
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
                            let _ = crate::throttle::note(
                                store,
                                crate::throttle::Scope::Bus,
                                refusal,
                                lean_ctx_time::UtcDateTime::now(),
                            );
                        } else {
                            // Remembered for its sender, so no agent passes
                            // the refused message on (process v1 §7).
                            let sender = relay_v2::queued_worker(&data, &id);
                            if let Err(error) = super::super::person_rules::note_refused(
                                &data,
                                &id,
                                sender.as_deref(),
                                &code,
                            ) {
                                store.record_error(&format!("bus refused record: {error}"));
                            }
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
                        // Who a message for one worker reaches is decided now,
                        // from the live registry and the worker book, not from
                        // what this connection saw when it opened (#28).
                        let book = attach::list(store, &instance.instance_id).unwrap_or_default();
                        let mine: BTreeSet<String> = local_workers
                            .iter()
                            .cloned()
                            .chain(book.iter().map(|w| w.worker_id.clone()))
                            .collect();
                        // §40: an exchange from another machine passes the
                        // same rules as an outgoing one and is audited; a
                        // refused one moves the cursor only. A record that
                        // cannot be written leaves the cursor, so the
                        // delivery comes again.
                        let apply = super::super::exchange::admit(
                            &data,
                            &map,
                            &mut state,
                            &message,
                            &mine,
                            lean_ctx_time::UtcDateTime::now(),
                        )
                        .unwrap_or_else(|error| {
                            store.record_error(&format!("bus exchange audit: {error}"));
                            false
                        });
                        if apply {
                            let worker_of = |agent: &AgentEntry| book_worker(&book, agent);
                            let applied = AgentRegistry::mutate_locked(|registry| {
                                relay_v2::apply_delivery(registry, &map, &mut state, &message, &mine, &worker_of)
                            });
                            if let Ok((_, Err(error))) = applied {
                                store.record_error(&format!("bus delivery: {error}"));
                            }
                        }
                    }
                    Inbound::TaskEvent(event) => {
                        let applied = AgentRegistry::mutate_locked(|registry| {
                            task_events::apply(registry, &map, &data, &mut state.tasks, &event, Origin::Frame)
                        });
                        if let Ok((_, Err(error))) = applied {
                            store.record_error(&format!("bus task event: {error}"));
                        }
                    }
                    // §37.1: without `plan_updates` in the hello.ack both
                    // frames are ignored; nothing is pulled or issued.
                    Inbound::PlanAdvanced(_) | Inbound::DecisionChanged(_) if !plans_on => {}
                    // §37: pulled and verified off this loop, applied above.
                    Inbound::PlanAdvanced(event) => {
                        let (map, seen, keys, data, tx) = (
                            map.clone(),
                            state.plans.clone(),
                            keys.clone(),
                            data.clone(),
                            plan_tx.clone(),
                        );
                        let carried = lean_ctx::core::context_admission::capture::carry();
                        tokio::task::spawn_blocking(move || {
                            carried.enter(|| {
                                let _ = tx.send(plan_events::fetch(&map, &data, &seen, &keys, &[*event]));
                            });
                        });
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
                        crate::consent::web::note_answered(&data, &consent_id);
                    }
                    // Who works where: other machines' workers, for
                    // `ctx_agent list` (this machine's own are local).
                    Inbound::Presence { address, worker_id, role, state: presence_state, at, names } => {
                        if !local_workers.contains(&worker_id) {
                            let recorded = super::super::presence::PresenceMap::mutate(&data, |map| {
                                map.apply_named(&address, &worker_id, &role, presence_state, at, names)
                            });
                            if let Err(error) = recorded {
                                store.record_error(&format!("bus presence: {error}"));
                            }
                        }
                    }
                    // Background sync tails the place now (hints only: the
                    // replica verifies whatever it fetches).
                    Inbound::LogAdvanced { place, purge } => {
                        let trigger = if purge {
                            crate::sync::auto::Trigger::BusPurge
                        } else {
                            crate::sync::auto::Trigger::Bus
                        };
                        crate::sync::auto::nudge_place(&place, trigger);
                    }
                    Inbound::TrustAdvanced { place } => {
                        crate::sync::auto::nudge_place(&place, crate::sync::auto::Trigger::Bus);
                    }
                    Inbound::PolicyAdvanced { version } => {
                        crate::policy::note_nudges_seen(store.data_dir());
                        crate::scheduler::nudge_policy(Some(version));
                    }
                    // §37: Cloud sends it only after the person's confirmed
                    // "Send to agents"; delivered here and nowhere else.
                    Inbound::DecisionChanged(event) => {
                        let delivered = DirectiveStore::mutate(|registry, directives| {
                            plan_events::deliver_decision(registry, directives, &map, &event)
                        });
                        if let Err(error) = delivered {
                            store.record_error(&format!("bus decision: {error}"));
                        }
                    }
                    Inbound::Other(_) => {}
                }
                state.save(&data)?;
            }
            _ = tick.tick() => {
                // Switched off in `[cloud]` meanwhile: close; `run` waits.
                if super::super::relay_config().is_none() {
                    state.save(&data)?;
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(SessionEnd::Closed);
                }
                map = crate::resolution::current_view(&data);
                let (fresh_agents, fresh_subscribe) = local_view(&map);
                // A new agent, or one that registered again with another
                // role, attaches to another worker: announce it afresh.
                if fresh_subscribe != subscribe || !attached_alike(&fresh_agents, &agents) {
                    state.save(&data)?;
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(SessionEnd::Reannounce);
                }
                let fresh = presence(&state, &fresh_agents, &workers, &instance);
                let fresh_identity = identities(identity_on, &fresh_agents, &workers, &instance);
                if fresh != announced || fresh_identity != sent_identity {
                    // A worker changed state (held, idle) or what its team
                    // sees of it (h7b): tell each place.
                    let changed: Vec<&PresenceEntry> = fresh
                        .iter()
                        .filter(|e| {
                            !announced.contains(e)
                                || fresh_identity.get(&e.worker_id)
                                    != sent_identity.get(&e.worker_id)
                        })
                        .collect();
                    for address in &subscribe {
                        for entry in &changed {
                            let update = presence_update(
                                address,
                                entry,
                                fresh_identity.get(&entry.worker_id),
                                now_ms(),
                            );
                            sink.send(text(&update)).await.map_err(offline)?;
                        }
                    }
                    announced = fresh;
                    sent_identity = fresh_identity;
                }
                // D4: once per version and digest, tell Cloud which signed
                // policy this Instance applied and how many live sessions
                // enforce it (verified, in force and acked only).
                if policy_applied
                    && let Some((frame, reported)) =
                        crate::policy_sessions::applied_frame(&data, lean_ctx_time::UtcDateTime::now())
                {
                    sink.send(text(&frame)).await.map_err(offline)?;
                    crate::policy_sessions::note_reported(&data, &reported);
                }
                // A bus quota or policy throttle: posts stay local until it ends.
                if !bus_held(store) {
                    let registry = AgentRegistry::load_or_create();
                    let worker_of = |agent: &AgentEntry| workers.get(&agent.agent_id).map(|w| w.worker_id.clone());
                    let records = super::super::person_rules::Records::load(&data);
                    for item in relay_v2::outgoing(&registry, &map, &mut state, &mut screen, &worker_of, &records) {
                        sink.send(text(&item.frame)).await.map_err(offline)?;
                    }
                    for frame in relay_v2::queued_frames(&data, &mut state) {
                        sink.send(text(&frame)).await.map_err(offline)?;
                    }
                }
                // C5: lease releases the tool calls queued, over HTTP and
                // off this loop, one pass at a time.
                if releases.as_ref().is_none_or(tokio::task::JoinHandle::is_finished) {
                    let (store, root) = (store.clone(), *session.root_key());
                    releases = Some(tokio::task::spawn_blocking(move || {
                        if let Ok(session) = Session::load(store.clone(), root) {
                            super::super::leases::send_releases(&store, &session);
                        }
                    }));
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

    /// Cloud' heartbeat on a revoked instance (`place.denied
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

    /// Walk 02.11.: the MCP connection registers as `coder`, then
    /// `ctx_agent register role=tester` changes the role of the same agent.
    /// It attaches to another worker, so the session announces afresh; an
    /// agent that left needs no new hello.
    #[test]
    fn a_role_change_of_an_announced_agent_needs_a_fresh_hello() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let mut registry = AgentRegistry::new();
        registry
            .register("codex", Some("coder"), &root, None)
            .unwrap();
        // One process holds one presence: the second agent is another one's.
        let mut other = registry.agents[0].clone();
        other.agent_id = "claude-2-x".into();
        other.role = Some("dev".into());
        let announced = vec![registry.agents[0].clone(), other];
        assert!(attached_alike(&announced, &announced));
        assert!(attached_alike(&announced[..1], &announced), "one left");

        let mut changed = announced.clone();
        changed[0].role = Some("tester".into());
        assert!(!attached_alike(&changed, &announced), "role changed");
        assert!(!attached_alike(&announced, &announced[..1]), "a new agent");
    }

    /// #28: the relay finds a session's worker in the book as it is now,
    /// by host app and normalized role, so a session that reconnected under
    /// a new agent id still gets its worker's message; another role or
    /// host gets none.
    #[test]
    fn book_worker_maps_sessions_by_host_and_normalized_role() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let mut registry = AgentRegistry::new();
        registry
            .register("claude", Some("Coder"), "/src/p", None)
            .unwrap();
        let coder = registry.agents[0].clone();
        let (host, _) = attach::agent_worker_key(&coder);
        let worker = |id: &str, host: &str, role: &str| LocalWorker {
            worker_id: id.into(),
            host_app: host.into(),
            role: role.into(),
            name: None,
            registered_at: None,
            aliases: BTreeMap::new(),
            last_attached: None,
            per_session: false,
        };
        let book = vec![
            worker("wkr_tester", &host, "tester"),
            worker("wkr_coder", &host, "coder"),
            worker("wkr_other_host", "some-other-host", "reviewer"),
        ];

        assert_eq!(
            book_worker(&book, &coder).as_deref(),
            Some("wkr_coder"),
            "`Coder` maps to the `coder` worker"
        );
        let mut sibling = coder.clone();
        sibling.agent_id = "claude-reconnected".into();
        sibling.role = Some("coder".into());
        assert_eq!(
            book_worker(&book, &sibling).as_deref(),
            Some("wkr_coder"),
            "a second session with the same host and role maps to the same worker"
        );
        let mut other_role = coder.clone();
        other_role.role = Some("security-reviewer".into());
        assert_eq!(book_worker(&book, &other_role), None, "another role");
        let mut other_host = coder.clone();
        other_host.role = Some("reviewer".into());
        assert_ne!(host, "some-other-host");
        assert_eq!(book_worker(&book, &other_host), None, "another host");
    }

    /// #252: `cloud bus relay` said nothing, so nobody saw that the tester
    /// was not on the bus. Its report names each announced worker with role
    /// and state, and for every agent that is not on a bus, why.
    #[test]
    fn the_relay_report_says_who_was_announced_and_why_not() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let mut registry = AgentRegistry::new();
        registry
            .register("claude", Some("tester"), "/src/pounce-cloud", None)
            .unwrap();
        // One process holds one agent: the others are other sessions'.
        let agents: Vec<AgentEntry> = [
            ("tester", "/src/pounce-cloud"),
            ("dev", "/src/unlinked"),
            ("", "/src/pounce-cloud"),
            ("ops", "/src/pounce-cloud"),
        ]
        .iter()
        .enumerate()
        .map(|(n, (role, root))| {
            let mut agent = registry.agents[0].clone();
            agent.agent_id = format!("claude-{n}-x");
            agent.role = Some((*role).to_string()).filter(|r| !r.is_empty());
            agent.project_root = (*root).to_string();
            agent
        })
        .collect();
        let worker = |id: &str, role: &str| LocalWorker {
            worker_id: id.into(),
            host_app: "claude-code".into(),
            role: role.into(),
            name: None,
            registered_at: Some(1),
            aliases: BTreeMap::new(),
            last_attached: None,
            per_session: false,
        };
        let mut workers = BTreeMap::new();
        workers.insert(agents[0].agent_id.clone(), worker("wkr_tester", "tester"));
        workers.insert(agents[1].agent_id.clone(), worker("wkr_dev", "dev"));
        workers.insert(agents[2].agent_id.clone(), worker("wkr_none", ""));
        let mut failed = BTreeMap::new();
        failed.insert(
            agents[3].agent_id.clone(),
            "LeanCTX Cloud answered 503".to_string(),
        );
        let announced = vec![PresenceEntry {
            worker_id: "wkr_tester".into(),
            role: "tester".into(),
            state: super::super::super::v2::PresenceState::Active,
            display_name: None,
            instance_id: None,
            instance_name: None,
        }];
        let subscribe = vec!["#yves/pounce-cloud".to_string()];
        let lines = announce_report(
            &subscribe,
            &announced,
            &agents,
            &workers,
            &failed,
            |root| root == "/src/pounce-cloud",
            &["#yves/secret denied (not_member)".into()],
        );
        let text = lines.join("\n");
        assert_eq!(
            lines[0],
            "bus relay: connected to LeanCTX Cloud; announced on #yves/pounce-cloud:"
        );
        assert!(
            text.contains(&format!(
                "  wkr_tester · tester · active  (agent {} in /src/pounce-cloud)",
                agents[0].agent_id
            )),
            "{text}"
        );
        assert!(
            text.contains("wkr_dev · dev") && text.contains("not linked to a LeanCTX Cloud vault"),
            "{text}"
        );
        assert!(
            text.contains("wkr_none · no role ·") && text.contains("register with role="),
            "{text}"
        );
        assert!(
            text.contains("(ops) in /src/pounce-cloud: not announced, its worker could not be registered: LeanCTX Cloud answered 503"),
            "{text}"
        );
        assert!(
            text.contains("#yves/secret denied (not_member): refused by LeanCTX Cloud"),
            "{text}"
        );

        let none = announce_report(&[], &[], &[], &workers, &failed, |_| true, &[]);
        assert!(none[0].contains("no agent is active"), "{none:?}");
        let unlinked = announce_report(
            &[],
            &announced,
            &agents[..1],
            &workers,
            &failed,
            |_| false,
            &[],
        );
        assert!(
            unlinked[0].contains("no active agent works in a checkout linked"),
            "{unlinked:?}"
        );
    }

    fn coder_worker() -> LocalWorker {
        LocalWorker {
            worker_id: "wkr_coder".into(),
            host_app: "claude-code".into(),
            role: "coder".into(),
            name: None,
            registered_at: Some(1),
            aliases: BTreeMap::new(),
            last_attached: None,
            per_session: false,
        }
    }

    fn instance_labelled(label: &str) -> crate::state::InstanceRecord {
        crate::state::InstanceRecord {
            instance_id: "inst_1".into(),
            org_id: "org_1".into(),
            org_name: "Platform".into(),
            label: label.into(),
            url: "https://cloud.example".into(),
            org_salt_id: "salt".into(),
            enrolled_at: "2026-11-17T00:00:00Z".into(),
            web_url: None,
            org_kind: Some("team".into()),
            company_name: None,
        }
    }

    /// h7b team presence: with the names on and acknowledged, each
    /// worker's `presence.update` carries this machine's short name (never
    /// its domain) and the status line its agent wrote; switched off
    /// (`[cloud] presence_names = false`) or not acknowledged, the frame is
    /// what it was: role and state only.
    #[test]
    fn team_presence_names_ride_only_when_on() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let mut registry = AgentRegistry::new();
        let id = registry
            .register("claude", Some("coder"), "/src/app", None)
            .unwrap();
        registry
            .set_status_by_agent(
                &id,
                lean_ctx::core::agents::AgentStatus::Active,
                Some("Fixing the login page\nstep 2"),
            )
            .unwrap();
        let agent = registry.agents[0].clone();
        let mut idle = agent.clone();
        idle.agent_id = "claude-2-x".into();
        idle.status_message = None;
        let mut workers = BTreeMap::new();
        workers.insert(idle.agent_id.clone(), coder_worker());
        workers.insert(agent.agent_id.clone(), coder_worker());
        let instance = instance_labelled("build-01.corp.example.com");
        let agents = vec![idle, agent];
        let on = identities(true, &agents, &workers, &instance);
        let named = on.get("wkr_coder").expect("one identity per worker");
        assert_eq!(named.machine_name.as_deref(), Some("build-01"));
        assert_eq!(named.working_on.as_deref(), Some("Fixing the login page"));
        assert!(identities(false, &agents, &workers, &instance).is_empty());

        let entry = PresenceEntry {
            worker_id: "wkr_coder".into(),
            role: "coder".into(),
            state: super::super::super::v2::PresenceState::Active,
            display_name: None,
            instance_id: Some("inst_1".into()),
            instance_name: None,
        };
        let with = presence_update("#acme/app", &entry, Some(named), 7);
        assert_eq!(with["machine_name"], "build-01");
        assert_eq!(with["working_on"], "Fixing the login page");
        assert_eq!(with["t"], "presence.update");
        assert_eq!(with["worker_id"], "wkr_coder");
        // A cleared status is sent as an explicit null, so it replaces the
        // old line on a Cloud that merges entries too.
        let cleared = PresenceIdentity {
            machine_name: Some("build-01".into()),
            working_on: None,
        };
        let cleared = presence_update("#acme/app", &entry, Some(&cleared), 8);
        assert!(cleared["working_on"].is_null());
        assert!(cleared.as_object().unwrap().contains_key("working_on"));
        let without = presence_update("#acme/app", &entry, None, 7);
        let keys: BTreeSet<&str> = without
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "t",
                "address",
                "at",
                "worker_id",
                "role",
                "state",
                "instance_id"
            ])
        );
    }

    /// h7b blocking 2: LeanCTX's own lifecycle notes (lease expired,
    /// transport idle, connection closed) are never sent as what a worker
    /// is working on; only a line the agent wrote itself is.
    #[test]
    fn h7b_lifecycle_notes_never_ride_as_working_on() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let mut registry = AgentRegistry::new();
        let id = registry
            .register("claude", Some("coder"), "/src/app", None)
            .unwrap();
        registry
            .set_status(
                &id,
                lean_ctx::core::agents::AgentStatus::Idle,
                Some("worker lease expired; register again before continuing"),
            )
            .unwrap();
        let mut transport_idle = registry.agents[0].clone();
        transport_idle.agent_id = "claude-3-x".into();
        transport_idle.status_message =
            Some("connected; no tool call within the worker lease".into());
        let agents = vec![registry.agents[0].clone(), transport_idle];
        let mut workers = BTreeMap::new();
        for agent in &agents {
            workers.insert(agent.agent_id.clone(), coder_worker());
        }
        let instance = instance_labelled("ada-mac.local");
        let on = identities(true, &agents, &workers, &instance);
        let named = on.get("wkr_coder").expect("one identity per worker");
        assert_eq!(named.working_on, None, "{named:?}");
        assert_eq!(named.machine_name.as_deref(), Some("ada-mac"));
    }

    /// h7b blocking 3: the Instance name in every hello and
    /// `presence.update` is the label's short name, never a domain.
    #[test]
    fn h7b_presence_instance_name_never_carries_a_domain() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let mut registry = AgentRegistry::new();
        registry
            .register("claude", Some("coder"), "/src/app", None)
            .unwrap();
        let agents = vec![registry.agents[0].clone()];
        let mut workers = BTreeMap::new();
        workers.insert(agents[0].agent_id.clone(), coder_worker());
        let state = StateV2::default();
        for (label, shown) in [
            ("build-01.corp.example.com", "build-01"),
            ("Yves's laptop", "Yves's laptop"),
            ("10.0.0.5", "10.0.0.5"),
        ] {
            let entries = presence(&state, &agents, &workers, &instance_labelled(label));
            assert_eq!(entries[0].instance_name.as_deref(), Some(shown), "{label}");
        }
    }

    /// A quota or policy refusal of a publish keeps the post; any other
    /// code is a refusal for good.
    #[test]
    fn quota_and_policy_publish_errors_hold_instead_of_refusing() {
        let quota = publish_hold("quota_exceeded", None).unwrap();
        assert!(
            matches!(&quota, CloudError::QuotaExceeded(q) if q.resource == "bus_messages" && q.waits())
        );
        let now = lean_ctx_time::OffsetDateTime::parse_from_rfc3339("2026-10-26T12:00:00Z")
            .unwrap()
            .to_utc();
        let (_, throttle) =
            crate::throttle::of_error(Some(crate::throttle::Scope::Bus), &quota, now).unwrap();
        assert_eq!(
            throttle.until, "2026-11-01T00:00:00Z",
            "until the month resets"
        );
        assert!(matches!(
            publish_hold("policy_denied", None),
            Some(CloudError::PolicyDenied(_))
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
        let now = lean_ctx_time::OffsetDateTime::parse_from_rfc3339(now)
            .unwrap()
            .to_utc();
        crate::throttle::of_error(Some(crate::throttle::Scope::Bus), &refusal, now)
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
            "{}/crates/lean-ctx-protocol/tests/fixtures/cloud-v2/bus/frame.error.quota_exceeded.json",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
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
        let CloudError::QuotaExceeded(q) = &refusal else {
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
    /// `reset_at`; a frame without them (an older Cloud) or with fields of
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
        assert!(matches!(&refusal, CloudError::QuotaExceeded(q)
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
    /// wait (32 days); it never overflows the time type, so the relay loop lives.
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
                Some(crate::error::RETRY_AFTER_CAP_S),
                "quota_fields caps {huge}"
            );
            assert_eq!(bus_until(&raw), "2026-11-27T12:00:00Z", "{huge}");
        }
    }
}
