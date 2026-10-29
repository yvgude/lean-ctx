// SPDX-License-Identifier: Apache-2.0
//! The approved pledge of a joined swarm (SPEC-ENGINE §3.14.1 "Approved
//! pledge digest", RESOLUTIONS R83): `<config>/hive/pledges/<swarm>.json`.
//!
//! The web and any token holder can write the server pledge, so the host
//! engine never enforces a server value it has not approved itself. The
//! record is valid only when its consent (`swarm_join` or `pledge_change`)
//! verifies, that consent's digest equals `approved_pledge_digest`, and
//! `values` is nowhere looser than `consent_values`.

use chrono::{Datelike as _, TimeZone as _, Timelike as _};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::pledge::{DAYS, Pledge, Window};
use crate::core::hive::consent::{Broker, ConsentError, Kind, Record, State, Trust};
use crate::core::hive::state::write_private;
use crate::core::hive::{HiveError, HiveStore};

const DIR: &str = "pledges";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PledgeRecord {
    pub swarm: String,
    pub approved_pledge_digest: String,
    /// What the approving consent named.
    pub consent_values: Pledge,
    /// What this engine enforces now (equal or tighter per field).
    pub values: Pledge,
    pub consent_id: String,
    pub approved_at: u64,
}

fn path_name(swarm: &str) -> Result<String, HiveError> {
    if !super::is_swarm_id(swarm) {
        return Err(HiveError::Config(format!("not a swarm id: {swarm}")));
    }
    Ok(format!("{swarm}.json"))
}

fn write(store: &HiveStore, record: &PledgeRecord) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(
        &store.config_dir().join(DIR),
        &path_name(&record.swarm)?,
        &bytes,
    )
}

fn broker(store: &HiveStore) -> Broker {
    let config = crate::core::config::Config::load().hive.clone();
    Broker::open(store, Trust::load(store, &config))
}

/// The digest an approving consent committed to.
fn approved_digest(consent: &Record) -> Option<&str> {
    match consent.kind {
        Kind::SwarmJoin => consent.subject["pledge_digest"].as_str(),
        Kind::PledgeChange => consent.subject["after_digest"].as_str(),
        _ => None,
    }
}

/// After an approved `swarm_join` or `pledge_change`: the approved values.
pub fn approve(
    store: &HiveStore,
    swarm: &str,
    pledge: &Pledge,
    consent: &Record,
) -> Result<PledgeRecord, HiveError> {
    let digest = pledge.digest()?;
    if consent.state != State::Approved || approved_digest(consent) != Some(digest.as_str()) {
        return Err(ConsentError::RecordInvalid.into());
    }
    let record = PledgeRecord {
        swarm: swarm.into(),
        approved_pledge_digest: digest,
        consent_values: pledge.clone(),
        values: pledge.clone(),
        consent_id: consent.consent_id.clone(),
        approved_at: consent.approved_at.unwrap_or_default(),
    };
    write(store, &record)?;
    Ok(record)
}

/// The record, checked against its consent; `None` when absent.
pub fn load(
    store: &HiveStore,
    broker: &Broker,
    swarm: &str,
) -> Result<Option<PledgeRecord>, HiveError> {
    let Some(record) = store
        .read_config::<PledgeRecord>(&format!("{DIR}/{}", path_name(swarm)?))
        .ok()
        .flatten()
    else {
        return Ok(None);
    };
    let consent = broker.get(&record.consent_id).ok();
    let valid = record.swarm == swarm
        && consent.as_ref().is_some_and(|c| {
            c.state == State::Approved
                && approved_digest(c) == Some(record.approved_pledge_digest.as_str())
        })
        && record.consent_values.digest().ok().as_deref()
            == Some(record.approved_pledge_digest.as_str())
        && !record.consent_values.loosened_by(&record.values);
    if valid {
        Ok(Some(record))
    } else {
        tracing::warn!("pledge record of swarm {swarm} failed its check");
        Err(ConsentError::RecordInvalid.into())
    }
}

/// What the engine enforces before a claim: the valid record's values. No
/// valid record: no claim until a pledge is approved (§3.14.1 test).
pub fn enforced(store: &HiveStore, swarm: &str) -> Result<Pledge, HiveError> {
    match load(store, &broker(store), swarm)? {
        Some(record) => Ok(record.values),
        None => Err(HiveError::Refused(format!(
            "pledge_unapproved: approve a pledge for this swarm first: lean-ctx swarm join {swarm}"
        ))),
    }
}

/// `swarm pledge` lowering (and a tighter server pledge): only `values`
/// changes; no consent is needed.
pub fn tighten(store: &HiveStore, swarm: &str, lower: &Pledge) -> Result<PledgeRecord, HiveError> {
    let mut record = load(store, &broker(store), swarm)?
        .ok_or_else(|| HiveError::Refused("pledge_unapproved: no approved pledge".into()))?;
    if record.values.loosened_by(lower) {
        return Err(HiveError::Refused(
            "pledge_change: this raises a limit; it needs your approval".into(),
        ));
    }
    record.values = lower.clone();
    write(store, &record)?;
    Ok(record)
}

/// What a server pledge means for this engine (§3.14.1 "Compare").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compared {
    /// Equal to `values`.
    Same,
    /// Tighter: applied and written to `values`.
    Tightened(Pledge),
    /// Looser: `pledge_change` is open; enforce the tighter of both.
    Looser { consent_id: String, enforce: Pledge },
}

/// On every E24 `GET`: compare the server pledge with `values`.
pub fn compare(
    store: &HiveStore,
    broker: &Broker,
    swarm: &str,
    server: &Pledge,
    now: u64,
) -> Result<Compared, HiveError> {
    let mut record = load(store, broker, swarm)?
        .ok_or_else(|| HiveError::Refused("pledge_unapproved: no approved pledge".into()))?;
    if &record.values == server {
        return Ok(Compared::Same);
    }
    if !record.values.loosened_by(server) {
        record.values = server.clone();
        write(store, &record)?;
        return Ok(Compared::Tightened(server.clone()));
    }
    let subject = json!({
        "swarm": swarm,
        "before_digest": record.values.digest()?,
        "after_digest": server.digest()?,
    });
    match broker.require(Kind::PledgeChange, &subject, now) {
        // Approved: the server values become the approved ones.
        Ok(consent) => {
            let approved = approve(store, swarm, server, &consent)?;
            Ok(Compared::Tightened(approved.values))
        }
        Err(ConsentError::Required { consent_id, .. }) => Ok(Compared::Looser {
            consent_id,
            enforce: record.values.tighter(server),
        }),
        Err(ConsentError::Denied) => Ok(Compared::Looser {
            consent_id: String::new(),
            enforce: record.values.tighter(server),
        }),
        Err(other) => Err(other.into()),
    }
}

pub fn forget(store: &HiveStore, swarm: &str) -> Result<(), HiveError> {
    let path = store.config_dir().join(DIR).join(path_name(swarm)?);
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(HiveError::Local(e.to_string())),
    }
}

fn minutes(hhmm: &str) -> Option<u32> {
    let (h, m) = hhmm.split_once(':')?;
    Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
}

/// Whether `window` is open at `now_ms`, evaluated in its own `tz` (never
/// the reading engine's zone). `to < from` spans midnight: the minutes
/// after midnight belong to the day the window started.
pub fn window_open(window: &Window, now_ms: u64) -> Result<bool, HiveError> {
    let tz: chrono_tz::Tz = window
        .tz
        .parse()
        .map_err(|_| HiveError::Config("invalid_request: window.tz".into()))?;
    let (Some(from), Some(to)) = (minutes(&window.from), minutes(&window.to)) else {
        return Err(HiveError::Config("invalid_request: window".into()));
    };
    let Some(local) = tz
        .timestamp_millis_opt(i64::try_from(now_ms).unwrap_or(i64::MAX))
        .single()
    else {
        return Ok(false);
    };
    let now = local.hour() * 60 + local.minute();
    let day =
        |offset: u32| DAYS[((local.weekday().num_days_from_monday() + 7 - offset) % 7) as usize];
    let listed = |d: &str| window.days.iter().any(|x| x == d);
    Ok(if from <= to {
        listed(day(0)) && now >= from && now < to
    } else {
        (listed(day(0)) && now >= from) || (listed(day(1)) && now < to)
    })
}

/// Pledge checks before a claim that the server cannot make (window).
pub fn claim_allowed(enforced: &Pledge, now_ms: u64) -> Result<(), HiveError> {
    if let Some(window) = &enforced.window
        && !window_open(window, now_ms)?
    {
        return Err(HiveError::Refused(
            "pledge_window_closed: your pledge's time window is closed now".into(),
        ));
    }
    Ok(())
}
