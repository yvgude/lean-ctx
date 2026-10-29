// SPDX-License-Identifier: Apache-2.0
//! The swarm-scoped replica in a participant's host engine (SPEC-ENGINE
//! §3.4.3 "Swarm replica", V4.8, D10): the clone asks for
//! `scope=subtree:<goal node id>`, so the replica holds only the claimed
//! goal, its subtree and the nodes they reference, and is re-scoped when
//! the worker claims a goal of another subtree. Swarm writes (E26 results)
//! are posted by the swarm client; they wait here as pending own events
//! until the tail delivers them, like vault pushes.

use std::path::Path;

use serde_json::Value;

use super::driver::{self, PlaceApi, Verifier};
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::events::build::Place;
use crate::core::hive::replica::fold::Graph;
use crate::core::hive::replica::{Replica, crypt, store::place_dir};

/// Replica meta key: the scope the replica was cloned with.
const META_SCOPE: &str = "swarm_scope";

/// One swarm place as its participant's engine holds it.
pub struct SwarmPlace {
    /// `swarm:<uuid>`.
    pub place_ref: String,
    pub swarm_id: String,
    /// The swarm's `id_salt` (node ids and body commitments of its claims).
    pub id_salt: [u8; 32],
    /// The newest server `seq` folded here: `causal_base` of new events.
    pub causal_base: u64,
    replica: Replica,
}

impl SwarmPlace {
    /// The [`Place`] events of this swarm are built for.
    pub fn place(&self) -> Place {
        Place {
            kind: "swarm".into(),
            id: self.swarm_id.clone(),
            pk_id: self.id_salt,
            causal_base: self.causal_base,
        }
    }

    /// Keep own signed events (an E26 result and its `result_of` edge) as
    /// pending until the tail delivers them; posting them is E26's job.
    pub fn append(&self, events: &[Value]) -> Result<(), HiveError> {
        for event in events {
            let id = event
                .get("event_id")
                .and_then(Value::as_str)
                .ok_or_else(|| HiveError::Local("swarm event without event_id".into()))?;
            let wall = event.pointer("/hlc/wall_ms").and_then(Value::as_u64);
            let counter = event.pointer("/hlc/counter").and_then(Value::as_u64);
            let (Some(wall), Some(counter)) = (wall, counter.and_then(|c| u16::try_from(c).ok()))
            else {
                return Err(HiveError::Local(format!("swarm event {id} without hlc")));
            };
            let bytes = serde_json::to_vec(event).map_err(|e| HiveError::Local(e.to_string()))?;
            self.replica.add_pending(id, wall, counter, None, &bytes)?;
        }
        Ok(())
    }

    /// The folded graph of the scoped log.
    pub fn graph(&mut self) -> Result<Graph, HiveError> {
        self.replica.refold()
    }
}

/// The replica key of a swarm (`<data>/hive/replica/swarm-<id>`).
fn replica_key(swarm_id: &str) -> String {
    format!("swarm-{swarm_id}")
}

/// Open the scoped replica of `swarm_id` for `goal` (`g_…`/`n_…` node id):
/// clone with the scope when there is none yet or the scope moved (the old
/// subtree is dropped, own pending events are kept), tail otherwise.
pub fn open_scoped(
    api: &dyn PlaceApi,
    data_dir: &Path,
    mk: &[u8; 32],
    swarm_id: &str,
    goal: &str,
    verifier: &Verifier<'_>,
) -> Result<SwarmPlace, HiveError> {
    if !goal.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') || goal.is_empty() {
        return Err(HiveError::Config(format!("{goal:?} is not a goal node id")));
    }
    let key = replica_key(swarm_id);
    let place_ref = format!("swarm:{swarm_id}");
    let scope = format!("subtree:{goal}");
    let (mut replica, _) = Replica::open(data_dir, &key, mk)?;
    let same_scope =
        replica.meta(META_SCOPE)?.as_deref().map(Vec::as_slice) == Some(scope.as_bytes());
    if replica.cursor()?.is_some() && !same_scope {
        replica = rescope(data_dir, mk, &key, replica)?;
    }
    if replica.cursor()?.is_none() {
        driver::clone_scoped(api, &mut replica, &place_ref, Some(&scope), verifier)?;
        replica.set_meta(META_SCOPE, scope.as_bytes())?;
    } else {
        driver::tail(api, &mut replica, &place_ref, verifier)?;
    }
    let id_salt = replica
        .id_salt()?
        .ok_or_else(|| HiveError::Protocol("swarm snapshot without id_salt".into()))?;
    Ok(SwarmPlace {
        place_ref,
        swarm_id: swarm_id.to_string(),
        id_salt,
        causal_base: replica.cursor()?.unwrap_or(0),
        replica,
    })
}

/// Drop the replica of the old scope and open an empty one that keeps the
/// own events still waiting for the log.
fn rescope(
    data_dir: &Path,
    mk: &[u8; 32],
    key: &str,
    replica: Replica,
) -> Result<Replica, HiveError> {
    let pending: Vec<(String, Vec<u8>)> = replica
        .pending()?
        .into_iter()
        .map(|(id, bytes)| (id, bytes.to_vec()))
        .collect();
    drop(replica);
    std::fs::remove_dir_all(place_dir(data_dir, key))
        .map_err(|e| HiveError::Local(e.to_string()))?;
    let (fresh, _) = Replica::open(data_dir, key, mk)?;
    for (id, bytes) in pending {
        let event: Value =
            serde_json::from_slice(&bytes).map_err(|e| HiveError::Local(e.to_string()))?;
        let wall = event
            .pointer("/hlc/wall_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let counter = event
            .pointer("/hlc/counter")
            .and_then(Value::as_u64)
            .and_then(|c| u16::try_from(c).ok())
            .unwrap_or(0);
        fresh.add_pending(&id, wall, counter, None, &bytes)?;
    }
    Ok(fresh)
}

/// The swarm place of `swarm_id` scoped to `goal`, for the participant's
/// host engine (ENGINE-2's E26 result submit builds its events on
/// [`SwarmPlace::place`] and keeps them with [`SwarmPlace::append`]).
pub fn swarm_place_handle(
    session: &Session,
    swarm_id: &str,
    goal: &str,
) -> Result<SwarmPlace, HiveError> {
    use crate::core::hive::keyset_v3::{self, Purpose};
    let store = &session.store;
    let (mk, _) = crypt::master_key(store.config_dir(), false)?;
    if keyset_v3::refresh(session).is_err() && keyset_v3::load(store, session.root_key()).is_none()
    {
        return Err(HiveError::Protocol("no verified Hive key set v3".into()));
    }
    let now = chrono::Utc::now().timestamp_millis().unsigned_abs();
    let snapshot_keys: Vec<[u8; 32]> =
        keyset_v3::hive_keys(store, session.root_key(), Purpose::Snapshot, now)
            .into_iter()
            .map(|(_, k)| k)
            .collect();
    let place_ref = format!("swarm:{swarm_id}");
    let check = |e: &Value| super::place::signature_ok(session, &place_ref, e);
    let verifier = Verifier {
        snapshot_keys: &snapshot_keys,
        signature_ok: &check,
        sealed: false,
        epoch_key: None,
    };
    open_scoped(
        &super::place::SessionApi(session),
        store.data_dir(),
        &mk,
        swarm_id,
        goal,
        &verifier,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_rescope_keeps_own_pending_events_and_drops_the_old_subtree() {
        let data = tempfile::tempdir().unwrap();
        let mk = [3u8; 32];
        let key = replica_key("5b0c9f7e-2a41-4c3d-9e8f-1a2b3c4d5e6f");
        let (replica, _) = Replica::open(data.path(), &key, &mk).unwrap();
        replica.set_cursor(41).unwrap();
        replica.set_meta(META_SCOPE, b"subtree:g_old").unwrap();
        let event = json!({"event_id": "01M5NKA2Z8006000000000W001",
                           "hlc": {"wall_ms": 1_792_800_000_000u64, "counter": 2}});
        let place = SwarmPlace {
            place_ref: "swarm:x".into(),
            swarm_id: "x".into(),
            id_salt: [0; 32],
            causal_base: 41,
            replica,
        };
        place.append(std::slice::from_ref(&event)).unwrap();
        let fresh = rescope(data.path(), &mk, &key, place.replica).unwrap();
        assert_eq!(fresh.cursor().unwrap(), None, "the old subtree is gone");
        assert!(fresh.meta(META_SCOPE).unwrap().is_none());
        let pending = fresh.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, "01M5NKA2Z8006000000000W001");
    }

    #[test]
    fn a_goal_id_is_checked_before_any_request() {
        struct NoApi;
        impl PlaceApi for NoApi {
            fn get(&self, _: &str) -> Result<Value, HiveError> {
                unreachable!()
            }
            fn get_bytes(&self, _: &str, _: usize) -> Result<Vec<u8>, HiveError> {
                unreachable!()
            }
            fn post(&self, _: &str, _: &Value) -> Result<Value, HiveError> {
                unreachable!()
            }
        }
        let data = tempfile::tempdir().unwrap();
        let ok = |_: &Value| true;
        let verifier = Verifier {
            snapshot_keys: &[],
            signature_ok: &ok,
            sealed: false,
            epoch_key: None,
        };
        let err = open_scoped(
            &NoApi,
            data.path(),
            &[1; 32],
            "s",
            "g_x&scope=all",
            &verifier,
        )
        .err()
        .unwrap();
        assert!(err.to_string().contains("goal node id"), "{err}");
    }
}
