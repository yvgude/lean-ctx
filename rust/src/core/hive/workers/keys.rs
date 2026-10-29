// SPDX-License-Identifier: Apache-2.0
//! Worker keys of a place (E8 `GET /v1/places/{p}/worker-keys?since=`,
//! C0 2.0 §3.17.2): the instance and alias keys that sign class-W events,
//! cached per place so the replica verifies offline.
//!
//! A key verifies an event received while it was valid:
//! `created_at ≤ received_at` and (`revoked_at` null or `received_at <
//! revoked_at`). An unknown key id is fetched once more since the cursor.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use serde::{Deserialize, Serialize};

use super::super::HiveError;
use super::super::auth::Session;
use super::super::state::{HiveStore, load_or_quarantine, lock_state, write_private};

const CACHE_FILE: &str = "worker_keys.json";
/// Pages one refresh follows at most (1 000 keys each).
const MAX_PAGES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    Instance,
    Alias,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerKey {
    pub key_id: String,
    pub kind: KeyKind,
    pub public_key: String,
    pub worker_ids: Vec<String>,
    pub created_at: u64,
    pub revoked_at: Option<u64>,
}

impl WorkerKey {
    /// The raw key when it was valid at `received_at`.
    pub fn valid_at(&self, received_at: u64) -> Option<[u8; 32]> {
        if received_at < self.created_at || self.revoked_at.is_some_and(|at| received_at >= at) {
            return None;
        }
        B64URL
            .decode(&self.public_key)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    keys: Vec<WorkerKey>,
    next_since: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PlaceKeys {
    since: u64,
    keys: BTreeMap<String, WorkerKey>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Cache {
    places: BTreeMap<String, PlaceKeys>,
}

fn read(store: &HiveStore) -> Cache {
    load_or_quarantine::<Cache>(&store.data_dir().join(CACHE_FILE), |_| true)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Merge one page into a place's keys (a later page replaces a key's row,
/// e.g. when it was revoked meanwhile).
fn merge(place: &mut PlaceKeys, page: Page) -> bool {
    let more = !page.keys.is_empty() && page.next_since > place.since;
    for key in page.keys {
        place.keys.insert(key.key_id.clone(), key);
    }
    place.since = place.since.max(page.next_since);
    more
}

/// The cached key `key_id` of `place_ref` for an event received at
/// `received_at` (ms), with the workers it signs for.
pub fn worker_key(
    store: &HiveStore,
    place_ref: &str,
    key_id: &str,
    received_at: u64,
) -> Option<([u8; 32], Vec<String>)> {
    let cache = read(store);
    let key = cache.places.get(place_ref)?.keys.get(key_id)?;
    Some((key.valid_at(received_at)?, key.worker_ids.clone()))
}

/// Fetch the place's keys since the cached cursor and store them.
pub fn fetch(session: &Session, place_ref: &str) -> Result<(), HiveError> {
    if !place_ref.starts_with("vault:") && !place_ref.starts_with("swarm:") {
        return Err(HiveError::Config(format!("{place_ref} is not a place")));
    }
    let store = &session.store;
    let mut place = read(store).places.remove(place_ref).unwrap_or_default();
    for _ in 0..MAX_PAGES {
        let path = format!("/places/{place_ref}/worker-keys?since={}", place.since);
        let body = session.with_client(|client| client.get(&path))?;
        let page: Page = serde_json::from_value(body)
            .map_err(|e| HiveError::Protocol(format!("worker keys: {e}")))?;
        if !merge(&mut place, page) {
            break;
        }
    }
    let dir = store.data_dir();
    let _lock = lock_state(dir, CACHE_FILE)?;
    let mut cache = read(store);
    cache.places.insert(place_ref.to_string(), place);
    let bytes = serde_json::to_vec(&cache).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(dir, CACHE_FILE, &bytes)
}

/// [`worker_key`], fetching once when the key id is not cached yet.
pub fn worker_key_or_fetch(
    session: &Session,
    place_ref: &str,
    key_id: &str,
    received_at: u64,
) -> Result<Option<([u8; 32], Vec<String>)>, HiveError> {
    let known = read(&session.store)
        .places
        .get(place_ref)
        .is_some_and(|p| p.keys.contains_key(key_id));
    if !known {
        fetch(session, place_ref)?;
    }
    Ok(worker_key(&session.store, place_ref, key_id, received_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(name: &str) -> Page {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn keys_merge_and_verify_only_while_valid() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let mut place = PlaceKeys::default();
        assert!(merge(&mut place, page("worker_keys.vault.response.json")));
        assert_eq!(place.since, 1_792_720_000_000);
        assert!(!merge(
            &mut place,
            Page {
                keys: Vec::new(),
                next_since: 1_792_720_000_000
            }
        ));
        let cache = Cache {
            places: BTreeMap::from([("vault:v".to_string(), place)]),
        };
        std::fs::create_dir_all(store.data_dir()).unwrap();
        write_private(
            store.data_dir(),
            CACHE_FILE,
            &serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();

        let (key, workers) =
            worker_key(&store, "vault:v", "47983724220c8521", 1_792_800_000_000).unwrap();
        assert_eq!(
            hex::encode(key),
            "772c8a442b7db06e166cfbc1ccbcbcde6f3eba76a4e98ef3ffc519502237d6ef"
        );
        assert_eq!(workers, ["wkr_vk4oelpv5lzjmev44hun"]);
        // Revoked at 1792800500000: valid before, refused after.
        assert!(worker_key(&store, "vault:v", "685e652d8892a354", 1_792_800_400_000).is_some());
        assert!(worker_key(&store, "vault:v", "685e652d8892a354", 1_792_800_500_000).is_none());
        // Before it was created, unknown id, unknown place.
        assert!(worker_key(&store, "vault:v", "47983724220c8521", 1_792_713_599_999).is_none());
        assert!(worker_key(&store, "vault:v", "ffffffffffffffff", 1_792_800_000_000).is_none());
        assert!(worker_key(&store, "vault:w", "47983724220c8521", 1_792_800_000_000).is_none());
    }

    #[test]
    fn swarm_pages_carry_only_alias_keys() {
        let swarm = page("worker_keys.response.json");
        assert!(swarm.keys.iter().all(|k| k.kind == KeyKind::Alias));
        assert_eq!(swarm.keys[0].worker_ids, ["wkr_76aa2uwmpczt3cqkhfed"]);
    }
}
