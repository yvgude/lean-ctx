// SPDX-License-Identifier: Apache-2.0
//! The handoffs this machine offered through Hive.
//!
//! A handoff to another machine's worker is a typed bus message
//! (`handoff`, [`super::messages_v2`]); the addressee answers it with an
//! `answer` that accepts or declines. Hive keeps the thread and its state.
//! This file only remembers which local agent offered which handoff, so a
//! maintainer's veto of it (a control naming the stored message id,
//! [`super::veto`]) reaches the agent that offered it.
//!
//! State in `<data>/hive/bus/handoffs.json`, keyed by the `client_msg_id`
//! of the handoff message.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::HiveError;
use crate::state::{load_or_quarantine, lock_state, write_private};

const HANDOFFS_FILE: &str = "handoffs.json";
/// An offer is remembered this long after it was made (RB8 M14).
const KEPT: chrono::Duration = chrono::Duration::days(7);
/// Offers remembered at most (the newest stay).
const MAX_OFFERS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum OfferState {
    Offered,
    /// A maintainer vetoed it; the offering agent was told once.
    Vetoed {
        reason: String,
    },
}

/// A handoff a local agent offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// The local agent that offered it.
    pub from: String,
    /// The worker it was offered to (`wkr_…`).
    pub to: String,
    /// The message id Hive stored it under (`msg_…`), once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    pub offered_at: String,
    #[serde(flatten)]
    pub state: OfferState,
}

/// `handoffs.json`: `client_msg_id` → offer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoffs {
    #[serde(default)]
    pub offers: BTreeMap<String, Offer>,
}

impl Handoffs {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join("bus").join(HANDOFFS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, data_dir: &Path) -> Result<(), HiveError> {
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
        let cutoff = stamp(now - KEPT);
        self.offers
            .retain(|_, offer| offer.offered_at.as_str() > cutoff.as_str());
        let excess = self.offers.len().saturating_sub(MAX_OFFERS);
        if excess > 0 {
            let mut by_age: Vec<_> = self
                .offers
                .iter()
                .map(|(id, offer)| (offer.offered_at.clone(), id.clone()))
                .collect();
            by_age.sort();
            for (_, id) in by_age.into_iter().take(excess) {
                self.offers.remove(&id);
            }
        }
    }

    /// A veto of the handoff `id` (its message id, or its `client_msg_id`):
    /// the open offers it names become vetoed; the agents that made them.
    pub fn veto(&mut self, id: &str, reason: &str) -> Vec<String> {
        let mut from = Vec::new();
        for (client_msg_id, offer) in &mut self.offers {
            let named = client_msg_id == id || offer.msg_id.as_deref() == Some(id);
            if named && offer.state == OfferState::Offered {
                offer.state = OfferState::Vetoed {
                    reason: reason.to_string(),
                };
                from.push(offer.from.clone());
            }
        }
        from
    }
}

fn stamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Remember that `from` offered the handoff message `client_msg_id` to the
/// worker `to`.
pub fn offered(
    data_dir: &Path,
    client_msg_id: &str,
    from: &str,
    to: &str,
) -> Result<(), HiveError> {
    let offer = Offer {
        from: from.into(),
        to: to.into(),
        msg_id: None,
        offered_at: stamp(chrono::Utc::now()),
        state: OfferState::Offered,
    };
    Handoffs::mutate(data_dir, |handoffs| {
        handoffs.offers.insert(client_msg_id.to_string(), offer);
    })
}

/// Hive stored the message `client_msg_id` as `msg_id`: when it is a
/// handoff offered here, remember that id (a veto names it). `true` when
/// it was one.
#[cfg_attr(not(feature = "http-server"), allow(dead_code))]
pub(crate) fn stored(
    data_dir: &Path,
    client_msg_id: &str,
    msg_id: &str,
) -> Result<bool, HiveError> {
    // Most stored messages are no handoff: read without the lock first.
    if Handoffs::load(data_dir)
        .offers
        .get(client_msg_id)
        .is_none_or(|offer| offer.msg_id.is_some())
    {
        return Ok(false);
    }
    Handoffs::mutate(data_dir, |handoffs| {
        handoffs
            .offers
            .get_mut(client_msg_id)
            .map(|offer| offer.msg_id = Some(msg_id.to_string()))
            .is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_learns_its_message_id_and_a_veto_names_either_id() {
        let dir = tempfile::tempdir().unwrap();
        offered(dir.path(), "01M5NKA2Z8B000000000000004", "alpha", "wkr_b").unwrap();
        assert!(
            !stored(dir.path(), "01OTHER", "msg_1").unwrap(),
            "no handoff"
        );
        assert!(
            stored(
                dir.path(),
                "01M5NKA2Z8B000000000000004",
                "msg_00000000000000a4"
            )
            .unwrap()
        );
        assert!(
            !stored(dir.path(), "01M5NKA2Z8B000000000000004", "msg_x").unwrap(),
            "once"
        );
        let vetoed =
            Handoffs::mutate(dir.path(), |h| h.veto("msg_00000000000000a4", "veto")).unwrap();
        assert_eq!(vetoed, ["alpha"]);
        let again =
            Handoffs::mutate(dir.path(), |h| h.veto("msg_00000000000000a4", "veto")).unwrap();
        assert!(again.is_empty(), "a vetoed offer is vetoed once");
        offered(dir.path(), "01CLIENT", "beta", "wkr_c").unwrap();
        let by_client_id = Handoffs::mutate(dir.path(), |h| h.veto("01CLIENT", "veto")).unwrap();
        assert_eq!(by_client_id, ["beta"]);
    }

    #[test]
    fn a_damaged_file_is_kept_aside_and_never_overwritten() {
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
        offered(dir.path(), "01A", "alpha", "wkr_b").unwrap();
        assert_eq!(
            std::fs::read(bus.join("handoffs.json.damaged-1")).unwrap(),
            b"{ not json"
        );
        assert_eq!(Handoffs::load(dir.path()).offers.len(), 1);
    }

    #[test]
    fn old_offers_age_out_and_the_file_is_capped() {
        let at = chrono::Utc::now();
        let mk = |days_ago: i64, minutes: i64| Offer {
            from: "alpha".into(),
            to: "wkr_b".into(),
            msg_id: None,
            offered_at: stamp(
                at - chrono::Duration::days(days_ago) - chrono::Duration::minutes(minutes),
            ),
            state: OfferState::Offered,
        };
        let mut handoffs = Handoffs::default();
        handoffs.offers.insert("old".into(), mk(8, 0));
        for n in 0..(MAX_OFFERS + 2) {
            let minutes = i64::try_from(MAX_OFFERS + 2 - n).unwrap();
            handoffs.offers.insert(format!("o_{n:04}"), mk(0, minutes));
        }
        handoffs.prune(at);
        assert!(!handoffs.offers.contains_key("old"));
        assert_eq!(handoffs.offers.len(), MAX_OFFERS);
        assert!(!handoffs.offers.contains_key("o_0001"), "oldest went");
        assert!(handoffs.offers.contains_key("o_0002"));
    }
}
