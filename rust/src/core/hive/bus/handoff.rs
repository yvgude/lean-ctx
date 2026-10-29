// SPDX-License-Identifier: Apache-2.0
//! Handoffs to an agent on another machine, through Hive (C5c).
//!
//! Sender: `ctx_agent handoff` to an agent that Hive reports on another
//! instance posts the summary as a normal bus message (screened and
//! published by the relay), then [`offer`] queues a `handoff.offer` naming
//! that message (`bundle_ref: bus:<client_msg_id>`, deadline 30 min). No
//! network in the tool call. The relay sends the offer once the message is
//! published, so the receiver always has it first (same channel, lower seq).
//! The sender waits as `idle`; `handoff.ack` finishes it, `handoff.reject` or
//! the deadline makes it active again with the reason in its status.
//!
//! Receiver: the relay answers an offer for one of its agents: `ack` once
//! the agent is here and active (ack means durably received, not agreed
//! to), `reject` for a veto, an unknown or an inactive agent. Redelivered
//! offers get the same answer again.
//!
//! State in `<data>/hive/bus/handoffs.json`.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::relay::{GENERAL_CHANNEL, channel, client_msg_id};
use crate::core::agents::{AgentRegistry, AgentStatus};
use crate::core::hive::HiveError;
#[cfg(any(test, feature = "http-server"))]
use crate::core::hive::state::HiveStore;
use crate::core::hive::state::{load_or_quarantine, lock_state, write_private};
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::bus::BusFrameV1;

const HANDOFFS_FILE: &str = "handoffs.json";
/// How long an offer stays open.
pub const DEADLINE_MINUTES: i64 = 30;
/// Decided offers remembered for redeliveries (oldest drop first).
const MAX_DECIDED: usize = 1000;
/// A finished offer (acked, rejected, expired) is kept this long after its
/// deadline for `hive status`, then dropped (RB8 M14).
const FINISHED_KEPT: chrono::Duration = chrono::Duration::days(7);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum OfferState {
    /// Waiting for the summary message to be published.
    Queued,
    Offered,
    Acked {
        by: String,
    },
    Rejected {
        by: String,
        reason: String,
    },
    Expired,
}

/// An offer this machine made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub from: String,
    pub to: String,
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    pub vault_id: String,
    pub org_id: String,
    /// The local bus message carrying the summary.
    pub entry_id: String,
    pub deadline: String,
    #[serde(flatten)]
    pub state: OfferState,
}

/// `handoffs.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoffs {
    #[serde(default)]
    pub offers: BTreeMap<String, Offer>,
    /// Offers this machine answered: handoff id → the answer it sent.
    #[serde(default)]
    pub answered: VecDeque<(String, Answer)>,
}

/// The receiver's answer to an offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Ack,
    Reject(String),
}

impl Handoffs {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join("bus").join(HANDOFFS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec(self).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(&data_dir.join("bus"), HANDOFFS_FILE, &bytes)
    }

    /// Load, change and save under `handoffs.lock` (RB8 M11), shared by the
    /// relay and the tool calls; never held across a request. A damaged or
    /// unknown file is moved aside first and the state starts empty, so
    /// the damaged copy is never overwritten (M12); an unreadable one is an
    /// error. Old entries are pruned on every change (M14).
    pub fn mutate<T>(data_dir: &Path, change: impl FnOnce(&mut Self) -> T) -> Result<T, HiveError> {
        let dir = data_dir.join("bus");
        let _lock = lock_state(&dir, HANDOFFS_FILE)?;
        let path = dir.join(HANDOFFS_FILE);
        let mut state = match load_or_quarantine::<Self>(&path, |_| true) {
            Ok(state) => state.unwrap_or_default(),
            Err(error) if !path.exists() => {
                tracing::warn!("hive bus handoffs: {error}");
                Self::default()
            }
            Err(error) => return Err(error),
        };
        let before = state.clone();
        let out = change(&mut state);
        state.prune(chrono::Utc::now());
        if state != before {
            state.save(data_dir)?;
        }
        Ok(out)
    }

    fn prune(&mut self, now: chrono::DateTime<chrono::Utc>) {
        let cutoff = stamp(now - FINISHED_KEPT);
        self.offers.retain(|_, offer| {
            matches!(offer.state, OfferState::Queued | OfferState::Offered)
                || offer.deadline.as_str() > cutoff.as_str()
        });
        while self.answered.len() > MAX_DECIDED {
            self.answered.pop_front();
        }
    }
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

fn stamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Where a handoff to `to` must go through Hive: this machine is connected,
/// the project is linked, and Hive reports `to` on another instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub data_dir: std::path::PathBuf,
    pub org_id: String,
    pub vault_id: String,
    pub instance_id: String,
}

/// `Ok(Some)` when a handoff from `project_root` to `to` is cross-instance,
/// `Ok(None)` when it stays on this machine. Like a direct message (R3), a
/// handoff binds only to the one remote instance that announced `to`; when
/// several did, the name is ambiguous and the handoff is refused (6e's
/// review, M10), never sent to whichever announced it last.
pub fn remote_target(project_root: &Path, to: &str) -> Result<Option<Remote>, String> {
    let Ok(c) = super::leases::connected(project_root) else {
        return Ok(None);
    };
    let own = &c.session.instance.instance_id;
    let presence = super::presence::PresenceMap::load(c.store.data_dir());
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let instance = match presence.instances_of(to, own, &now).as_slice() {
        [] => return Ok(None),
        [one] => one.clone(),
        several => {
            return Err(format!(
                "{to} is announced by several instances ({}); a handoff needs one",
                several.join(", ")
            ));
        }
    };
    // The one instance must still run `to` (a closed agent is no target).
    if presence
        .remote(to, own)
        .is_none_or(|at| at.instance_id != instance)
    {
        return Ok(None);
    }
    Ok(Some(Remote {
        data_dir: c.store.data_dir().to_path_buf(),
        org_id: c.session.lease.lease.org_id.clone(),
        vault_id: c.vault_id,
        instance_id: instance,
    }))
}

/// Queue an offer of `entry_id` from `from` to `to` (on another instance)
/// in `vault_id`. Returns the handoff id.
pub fn offer(
    data_dir: &Path,
    org_id: &str,
    vault_id: &str,
    from: &str,
    to: &str,
    entry_id: &str,
) -> Result<String, HiveError> {
    let handoff_id = format!("hof_{}", uuid::Uuid::new_v4().simple());
    let offer = Offer {
        from: from.into(),
        to: to.into(),
        vault_id: vault_id.into(),
        org_id: org_id.into(),
        entry_id: entry_id.into(),
        deadline: stamp(now() + chrono::Duration::minutes(DEADLINE_MINUTES)),
        state: OfferState::Queued,
    };
    Handoffs::mutate(data_dir, |handoffs| {
        handoffs.offers.insert(handoff_id.clone(), offer);
    })?;
    Ok(handoff_id)
}

/// The tick's half: offers whose message is published become `handoff.offer`
/// frames; offers past their deadline expire (the sender is active again).
pub fn due(handoffs: &mut Handoffs, published: impl Fn(&str) -> bool) -> Vec<BusFrameV1> {
    let now = now();
    let mut frames = Vec::new();
    let mut expired = Vec::new();
    for (id, offer) in &mut handoffs.offers {
        let open = matches!(offer.state, OfferState::Queued | OfferState::Offered);
        let past = chrono::DateTime::parse_from_rfc3339(&offer.deadline)
            .map(|d| d.with_timezone(&chrono::Utc) <= now)
            .unwrap_or(true);
        if open && past {
            offer.state = OfferState::Expired;
            expired.push((offer.from.clone(), offer.to.clone()));
            continue;
        }
        if offer.state == OfferState::Queued && published(&offer.entry_id) {
            let Ok(deadline) = UtcTimestamp::new(&offer.deadline) else {
                continue;
            };
            frames.push(BusFrameV1::HandoffOffer {
                handoff_id: id.clone(),
                channel: channel(&offer.org_id, &offer.vault_id, GENERAL_CHANNEL),
                from: offer.from.clone(),
                to: offer.to.clone(),
                bundle_ref: format!("bus:{}", client_msg_id(&offer.entry_id)),
                deadline,
            });
            offer.state = OfferState::Offered;
        }
    }
    for (from, to) in expired {
        set_sender(
            &from,
            AgentStatus::Active,
            &format!("handoff to {to} expired without an answer"),
        );
    }
    frames
}

fn set_sender(agent: &str, status: AgentStatus, note: &str) {
    let result =
        AgentRegistry::mutate_locked(|registry| registry.set_status(agent, status, Some(note)));
    if let Ok((_, Err(error))) | Err(error) = result {
        tracing::debug!("hive handoff: sender status not updated: {error}");
    }
}

/// The sender's half of an answer from Hive; `true` when it was ours.
pub fn answered(handoffs: &mut Handoffs, frame: &BusFrameV1) -> bool {
    // An answer from another instance is checked like a client frame
    // (ids, reason length) before anything of it is kept (M8).
    if frame.validate().is_err() {
        return false;
    }
    let (handoff_id, state) = match frame {
        BusFrameV1::HandoffAck { handoff_id, by } => {
            (handoff_id, OfferState::Acked { by: by.clone() })
        }
        BusFrameV1::HandoffReject {
            handoff_id,
            by,
            reason,
        } => (
            handoff_id,
            OfferState::Rejected {
                by: by.clone(),
                reason: reason.clone(),
            },
        ),
        _ => return false,
    };
    let Some(offer) = handoffs.offers.get_mut(handoff_id) else {
        return false;
    };
    if !matches!(offer.state, OfferState::Queued | OfferState::Offered) {
        return true;
    }
    // The sender's status (shown to agents) names only what this machine
    // knows: the agent it addressed. `by` and the reason came from another
    // party; they stay in handoffs.json and never reach a status line (X5).
    match &state {
        OfferState::Acked { .. } => set_sender(
            &offer.from,
            AgentStatus::Finished,
            &format!("handed off to {} (Hive)", offer.to),
        ),
        OfferState::Rejected { .. } => set_sender(
            &offer.from,
            AgentStatus::Active,
            &format!("handoff to {} rejected by its instance", offer.to),
        ),
        _ => {}
    }
    offer.state = state;
    true
}

/// The receiver's answer to an offer for `to`, or `None` when `to` is not
/// an agent of this machine (another instance answers).
#[cfg(any(test, feature = "http-server"))]
pub(super) fn answer_offer(
    store: &HiveStore,
    handoffs: &mut Handoffs,
    registry: &AgentRegistry,
    frame: &BusFrameV1,
) -> Option<BusFrameV1> {
    let BusFrameV1::HandoffOffer {
        handoff_id,
        from,
        to,
        ..
    } = frame
    else {
        return None;
    };
    let agent = registry.agents.iter().find(|a| a.agent_id == *to)?;
    let known = handoffs
        .answered
        .iter()
        .find(|(id, _)| id == handoff_id)
        .map(|(_, answer)| answer.clone());
    let answer = known.unwrap_or_else(|| {
        let vetoed = super::control::current(store)
            .vetoed(Some(to), handoff_id)
            .or_else(|| {
                super::control::current(store).vetoed(Some(to), &format!("handoff:{from}:{to}"))
            });
        let answer = if vetoed.is_some() {
            Answer::Reject("veto".into())
        } else if agent.status == AgentStatus::Finished {
            Answer::Reject("inactive".into())
        } else {
            Answer::Ack
        };
        handoffs
            .answered
            .push_back((handoff_id.clone(), answer.clone()));
        while handoffs.answered.len() > MAX_DECIDED {
            handoffs.answered.pop_front();
        }
        answer
    });
    Some(match answer {
        Answer::Ack => BusFrameV1::HandoffAck {
            handoff_id: handoff_id.clone(),
            by: to.clone(),
        },
        Answer::Reject(reason) => BusFrameV1::HandoffReject {
            handoff_id: handoff_id.clone(),
            by: to.clone(),
            reason,
        },
    })
}

#[cfg(test)]
mod vault_rename_tests {
    use super::*;

    /// `Offer` of the engine before the VA rename.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct PreVaOffer {
        from: String,
        to: String,
        project_id: String,
        org_id: String,
        entry_id: String,
        deadline: String,
        #[serde(flatten)]
        state: OfferState,
    }

    #[test]
    fn handoff_offers_keep_pre_va_names() {
        let old = r#"{"from":"a","to":"b","project_id":"v1","org_id":"org_1","entry_id":"e1","deadline":"2026-10-22T10:00:00Z","state":"queued"}"#;
        let read = crate::core::hive::vault::assert_pre_va_round_trip::<Offer, PreVaOffer>(
            old,
            "project_id",
            "vault_id",
        );
        assert_eq!(
            (read.vault_id.as_str(), &read.state),
            ("v1", &OfferState::Queued)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(handoffs: &Handoffs, id: &str) -> OfferState {
        handoffs.offers[id].state.clone()
    }

    #[test]
    fn an_offer_waits_for_its_message_then_is_sent_once() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let id = offer(
            dir.path(),
            "org_a",
            "0123456789abcdef0123456789abcdef",
            "alpha",
            "beta",
            "e1",
        )
        .unwrap();
        let mut handoffs = Handoffs::load(dir.path());
        assert!(
            due(&mut handoffs, |_| false).is_empty(),
            "message not published yet"
        );
        let frames = due(&mut handoffs, |entry| entry == "e1");
        assert_eq!(frames.len(), 1);
        let BusFrameV1::HandoffOffer {
            handoff_id,
            channel,
            bundle_ref,
            to,
            ..
        } = &frames[0]
        else {
            panic!("{frames:?}");
        };
        assert_eq!(handoff_id, &id);
        assert_eq!(
            channel,
            "org:org_a/vault:0123456789abcdef0123456789abcdef/general"
        );
        assert_eq!(bundle_ref, &format!("bus:{}", client_msg_id("e1")));
        assert_eq!(to, "beta");
        assert!(frames[0].validate().is_ok());
        assert_eq!(offered(&handoffs, &id), OfferState::Offered);
        assert!(due(&mut handoffs, |_| true).is_empty(), "sent once");
    }

    #[test]
    fn answers_and_the_deadline_close_an_offer_once() {
        // Closing an offer updates the sender in the (isolated) agent registry.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let mut handoffs = Handoffs::default();
        let mk = |deadline: chrono::DateTime<chrono::Utc>| Offer {
            from: "alpha".into(),
            to: "beta".into(),
            vault_id: "p".into(),
            org_id: "o".into(),
            entry_id: "e".into(),
            deadline: stamp(deadline),
            state: OfferState::Offered,
        };
        handoffs
            .offers
            .insert("hof_a".into(), mk(now() + chrono::Duration::minutes(5)));
        handoffs
            .offers
            .insert("hof_r".into(), mk(now() + chrono::Duration::minutes(5)));
        handoffs
            .offers
            .insert("hof_x".into(), mk(now() - chrono::Duration::minutes(1)));
        assert!(answered(
            &mut handoffs,
            &BusFrameV1::HandoffAck {
                handoff_id: "hof_a".into(),
                by: "beta".into()
            }
        ));
        assert!(answered(
            &mut handoffs,
            &BusFrameV1::HandoffReject {
                handoff_id: "hof_r".into(),
                by: "hive".into(),
                reason: "veto".into()
            }
        ));
        // A late ack after a reject changes nothing.
        answered(
            &mut handoffs,
            &BusFrameV1::HandoffAck {
                handoff_id: "hof_r".into(),
                by: "beta".into(),
            },
        );
        assert!(!answered(
            &mut handoffs,
            &BusFrameV1::HandoffAck {
                handoff_id: "hof_other".into(),
                by: "beta".into()
            }
        ));
        due(&mut handoffs, |_| true);
        assert_eq!(
            offered(&handoffs, "hof_a"),
            OfferState::Acked { by: "beta".into() }
        );
        assert_eq!(
            offered(&handoffs, "hof_r"),
            OfferState::Rejected {
                by: "hive".into(),
                reason: "veto".into()
            }
        );
        assert_eq!(offered(&handoffs, "hof_x"), OfferState::Expired);
    }

    #[test]
    fn a_damaged_file_is_kept_aside_and_never_overwritten() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let bus = dir.path().join("bus");
        std::fs::create_dir_all(&bus).unwrap();
        std::fs::write(bus.join(HANDOFFS_FILE), b"{ not json").unwrap();
        assert!(Handoffs::load(dir.path()).offers.is_empty());
        assert_eq!(
            std::fs::read(bus.join(HANDOFFS_FILE)).unwrap(),
            b"{ not json",
            "a reader changes nothing"
        );
        offer(dir.path(), "org_a", "p", "alpha", "beta", "e1").unwrap();
        assert_eq!(
            std::fs::read(bus.join("handoffs.json.damaged-1")).unwrap(),
            b"{ not json"
        );
        assert_eq!(Handoffs::load(dir.path()).offers.len(), 1);
    }

    #[test]
    fn finished_offers_age_out_and_answers_are_capped() {
        let mut handoffs = Handoffs::default();
        let at = now();
        let mk = |state: OfferState, days_ago: i64| Offer {
            from: "alpha".into(),
            to: "beta".into(),
            vault_id: "p".into(),
            org_id: "o".into(),
            entry_id: "e".into(),
            deadline: stamp(at - chrono::Duration::days(days_ago)),
            state,
        };
        handoffs
            .offers
            .insert("old_expired".into(), mk(OfferState::Expired, 8));
        handoffs.offers.insert(
            "old_acked".into(),
            mk(OfferState::Acked { by: "beta".into() }, 8),
        );
        handoffs
            .offers
            .insert("recent_expired".into(), mk(OfferState::Expired, 6));
        handoffs
            .offers
            .insert("old_queued".into(), mk(OfferState::Queued, 8));
        handoffs
            .offers
            .insert("old_offered".into(), mk(OfferState::Offered, 8));
        for n in 0..(MAX_DECIDED + 5) {
            handoffs
                .answered
                .push_back((format!("hof_{n}"), Answer::Ack));
        }
        handoffs.prune(at);
        assert_eq!(
            handoffs
                .offers
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["old_offered", "old_queued", "recent_expired"]
        );
        assert_eq!(handoffs.answered.len(), MAX_DECIDED);
        assert_eq!(handoffs.answered[0].0, "hof_5", "oldest went");
    }
}
