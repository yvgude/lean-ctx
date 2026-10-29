// SPDX-License-Identifier: Apache-2.0
//! Joining, leaving and claiming as a participant (SPEC-ENGINE §3.13).
//!
//! `join` reads the account's pledge (E24 `GET`), asks the person for the
//! `swarm_join` consent over `{swarm, pledge_digest, cli}`, writes the
//! pledge only when none existed (E24 `PUT` with the proof), joins (E23 with
//! the proof) and registers the participant worker as the swarm alias (E10,
//! `place_ref = swarm:<id>`). No step runs before the person approved.

use serde_json::Value;

use super::client;
use super::local::{self, HeldClaim, Participant};
use super::pledge::Pledge;
use super::{ClaimGrant, platform_supported};
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::consent::proof::{self, Choice};
use crate::core::hive::consent::{Broker, ConsentError, Kind};
use crate::core::hive::instance_key::InstanceKeys;
use crate::core::hive::workers::card::{Availability, Budget, WorkerCard};
use crate::core::hive::workers::{attach, default_name, register};

/// Where a `swarm join` stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinStep {
    /// The person approves in a terminal: `lean-ctx hive consent`.
    NeedsConsent {
        consent_id: String,
    },
    /// Only a browser answer proves this one to Hive.
    NeedsWeb {
        consent_id: String,
    },
    Joined(Participant),
}

fn platform() -> Result<(), HiveError> {
    if platform_supported() {
        Ok(())
    } else {
        Err(HiveError::Refused(
            "platform_unsupported: swarms do not run on Windows on ARM".into(),
        ))
    }
}

/// `lean-ctx swarm join <swarm> --cli <cli>`; `wanted` is the pledge to
/// write when the account has none yet.
pub fn join(
    session: &Session,
    broker: &Broker,
    swarm: &str,
    cli: &str,
    wanted: &Pledge,
    allow_local_consent: bool,
    now: u64,
) -> Result<JoinStep, HiveError> {
    platform()?;
    let data = session.store.data_dir();
    if let Some(joined) = local::participant(data, swarm)? {
        return Ok(JoinStep::Joined(joined));
    }
    let existing = session.with_client(|c| client::pledge(c, swarm))?;
    let pledge = if let Some(server) = &existing {
        server.pledge.clone()
    } else {
        wanted
            .check(None)
            .map_err(|field| HiveError::Config(format!("invalid_request: {field}")))?;
        wanted.clone()
    };
    let subject = proof::swarm_join_subject(swarm, &pledge.to_value(), cli)?;
    let record = match broker.require(Kind::SwarmJoin, &subject, now) {
        Ok(record) => record,
        Err(ConsentError::Required { consent_id, .. }) => {
            return Ok(JoinStep::NeedsConsent { consent_id });
        }
        Err(other) => return Err(other.into()),
    };
    let choice = proof::choose(&record, allow_local_consent, now)?;
    if let Choice::NeedsWeb { consent_id } = choice {
        return Ok(JoinStep::NeedsWeb { consent_id });
    }
    if existing.is_none() {
        session.with_client(|c| client::put_pledge(c, swarm, &pledge, Some(&choice)))?;
    }
    session.with_client(|c| client::join(c, swarm, cli, &choice))?;
    let worker_id = register_participant(session, swarm, cli)?;
    let participant = Participant {
        swarm: swarm.into(),
        worker_id,
        cli: cli.into(),
        joined_at: now,
        consent_id: record.consent_id.clone(),
    };
    local::save_participant(data, &participant)?;
    super::pledge_record::approve(&session.store, swarm, &pledge, &record)?;
    Ok(JoinStep::Joined(participant))
}

/// The role of the participant worker (C0 `a_swarm_codex`: `general`).
pub const PARTICIPANT_ROLE: &str = "general";

/// The participant's swarm alias worker id: the id E25 claims and E26
/// events carry (C0 `routes/swarm_claim.v2.request.json`), derived like
/// E10 derives it, so it is known before and without the registration.
pub fn participant_worker_id(
    session: &Session,
    swarm: &str,
    cli: &str,
) -> Result<String, HiveError> {
    let keys = InstanceKeys::load(&session.store)?;
    Ok(
        crate::core::hive::workers::WorkerAliasKey::of(&keys).worker_id(
            cli,
            PARTICIPANT_ROLE,
            &format!("swarm:{swarm}"),
        ),
    )
}

/// E10 for the swarm alias of this engine's participant worker.
fn register_participant(session: &Session, swarm: &str, cli: &str) -> Result<String, HiveError> {
    let keys = InstanceKeys::load(&session.store)?;
    let place = format!("swarm:{swarm}");
    let name = default_name(
        &session.instance.label,
        &attach::host_display(cli),
        PARTICIPANT_ROLE,
    );
    let card = WorkerCard::new(
        cli,
        None,
        Vec::new(),
        Budget::default(),
        Availability::Online,
    );
    let reg = register::Registration {
        instance_id: &session.instance.instance_id,
        host_app: cli,
        role: PARTICIPANT_ROLE,
        name: &name,
        card: &card,
        alias_place: Some(&place),
    };
    Ok(session
        .with_client(|c| register::register(c, &keys, &reg))?
        .id)
}

/// `lean-ctx swarm leave <swarm>`: release the claims, leave (E23
/// `DELETE`), end the consent coverage and delete the swarm's local data.
pub fn leave(session: &Session, broker: &Broker, swarm: &str) -> Result<usize, HiveError> {
    let data = session.store.data_dir();
    let held = local::claims(data, swarm).unwrap_or_default();
    for (claim_id, claim) in &held {
        let _ =
            session.with_client(|c| client::release(c, swarm, claim_id, claim.grant.fencing_token));
    }
    session.with_client(|c| client::leave(c, swarm))?;
    broker.withdraw(
        &[Kind::SwarmJoin, Kind::PledgeChange, Kind::UnattendedStart],
        "swarm",
        swarm,
    )?;
    super::pledge_record::forget(&session.store, swarm)?;
    local::forget(data, swarm)?;
    Ok(held.len())
}

fn participant(session: &Session, swarm: &str) -> Result<Participant, HiveError> {
    local::participant(session.store.data_dir(), swarm)?.ok_or_else(|| {
        HiveError::Refused(format!(
            "not_participant: join the swarm first: lean-ctx swarm join {swarm}"
        ))
    })
}

/// E22 for the participant.
pub fn goal(session: &Session, swarm: &str, scope: Option<&str>) -> Result<super::Goal, HiveError> {
    let p = participant(session, swarm)?;
    session.with_client(|c| client::goal(c, swarm, &p.worker_id, scope))
}

/// E25 claim of `goal`, after the local pledge checks (§3.14.1).
pub fn claim(session: &Session, swarm: &str, goal: &str, now: u64) -> Result<HeldClaim, HiveError> {
    platform()?;
    let p = participant(session, swarm)?;
    let data = session.store.data_dir();
    let enforced = super::pledge_record::enforced(&session.store, swarm)?;
    let open = local::claims(data, swarm)?.len();
    if u32::try_from(open).unwrap_or(u32::MAX) >= enforced.max_sessions {
        return Err(HiveError::Refused(format!(
            "pledge: {open} open claim(s) already; your pledge allows {} parallel session(s)",
            enforced.max_sessions
        )));
    }
    super::pledge_record::claim_allowed(&enforced, now)?;
    let grant: ClaimGrant =
        session.with_client(|c| client::claim(c, swarm, goal, &p.worker_id, None))?;
    let held = HeldClaim {
        goal: goal.into(),
        worker_id: p.worker_id,
        grant,
    };
    local::with_claims(data, swarm, |claims| {
        claims.insert(held.grant.claim_id.clone(), held.clone());
    })?;
    Ok(held)
}

fn held(session: &Session, swarm: &str, claim_id: &str) -> Result<HeldClaim, HiveError> {
    local::claims(session.store.data_dir(), swarm)?
        .remove(claim_id)
        .ok_or_else(|| {
            HiveError::Refused(format!("no_claim: this engine holds no claim {claim_id}"))
        })
}

/// E25 renew; the new expiry and fencing token are kept.
pub fn renew(session: &Session, swarm: &str, claim_id: &str) -> Result<HeldClaim, HiveError> {
    let mut claim = held(session, swarm, claim_id)?;
    claim.grant =
        session.with_client(|c| client::renew(c, swarm, claim_id, claim.grant.fencing_token))?;
    local::with_claims(session.store.data_dir(), swarm, |claims| {
        claims.insert(claim_id.into(), claim.clone());
    })?;
    Ok(claim)
}

/// E25 release.
pub fn release(session: &Session, swarm: &str, claim_id: &str) -> Result<(), HiveError> {
    let claim = held(session, swarm, claim_id)?;
    session.with_client(|c| client::release(c, swarm, claim_id, claim.grant.fencing_token))?;
    local::with_claims(session.store.data_dir(), swarm, |claims| {
        claims.remove(claim_id);
    })
}

/// `reasons_passed_hash` of a participant result: the result passed the
/// engine scanner before it left (capture gate version, rule `swarm_result`).
pub fn result_reasons_hash() -> String {
    blake3::hash(
        format!(
            "leanctx.hive.capture.v{}:swarm_result",
            crate::core::hive::capture::GATE_VERSION
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string()
}

/// E26 for a held claim: screens the summary, builds the result node and
/// its `result_of` edge in the swarm place (scoped replica of the claimed
/// goal, ENGINE-1's V4.8 handle), signs them with the swarm alias key and
/// keeps them pending in the replica until the tail delivers them.
pub fn submit(
    session: &Session,
    swarm: &str,
    claim_id: &str,
    summary: &str,
    artefacts: Vec<Value>,
    usage: Option<super::result::Usage>,
    now: u64,
) -> Result<Value, HiveError> {
    use super::result::{Capture, Ids, ResultDraft, events};
    use crate::core::hive::events::build::Signer;
    use crate::core::hive::events::hlc::Stamp;

    let p = participant(session, swarm)?;
    let claim = held(session, swarm, claim_id)?;
    let mut screen = crate::core::hive::knowledge::screening::Screen::for_installation();
    screen.check(summary).map_err(|reason| {
        HiveError::Refused(format!("screened: the summary was not sent ({reason})"))
    })?;
    let tree = session.with_client(|c| client::goal(c, swarm, &p.worker_id, Some(&claim.goal)))?;
    let goal_node = tree
        .node(&claim.goal)
        .map(|n| n.node.clone())
        .ok_or_else(|| HiveError::Protocol(format!("goal {} is not in the tree", claim.goal)))?;
    let handle = crate::core::hive::sync::swarm::swarm_place_handle(session, swarm, &claim.goal)?;
    let keys = InstanceKeys::load(&session.store)?;
    let place_ref = format!("swarm:{swarm}");
    let key = crate::core::hive::workers::alias_signing_key(&keys, &place_ref);
    let signer = Signer::alias(&key, &keys, &place_ref, &p.worker_id);
    let ids = Ids::fresh(
        Stamp {
            wall_ms: now,
            counter: 0,
        },
        Stamp {
            wall_ms: now,
            counter: 1,
        },
    )?;
    let draft = ResultDraft {
        goal_node,
        summary: summary.into(),
        artefacts,
        usage,
    };
    let reasons = result_reasons_hash();
    let capture = Capture {
        gate_version: crate::core::hive::capture::GATE_VERSION,
        reasons_passed_hash: &reasons,
    };
    let signed = events(
        &draft,
        &ids,
        &handle.place(),
        &signer,
        &capture,
        crate::core::hive::knowledge::screening::SCANNER_VERSION,
    )?;
    let answer = submit_events(session, swarm, claim_id, &signed)?;
    handle.append(&signed)?;
    Ok(answer)
}

/// E26 for a held claim: `events` are the signed result events.
pub fn submit_events(
    session: &Session,
    swarm: &str,
    claim_id: &str,
    events: &[Value],
) -> Result<Value, HiveError> {
    let claim = held(session, swarm, claim_id)?;
    let answer = session
        .with_client(|c| client::results(c, swarm, claim_id, claim.grant.fencing_token, events))?;
    local::with_claims(session.store.data_dir(), swarm, |claims| {
        claims.remove(claim_id);
    })?;
    Ok(answer)
}
