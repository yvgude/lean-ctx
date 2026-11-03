// SPDX-License-Identifier: Apache-2.0
//! The register of provider-sealed items and of assistant parts, kept across
//! a restart of the proxy (#2177).
//!
//! In memory alone, every update or restart of the proxy turned each sealed
//! item of every running conversation into one of unknown origin, and under
//! a policy that demands full inspection the whole request was withheld. What
//! is kept here is what the in-memory register holds, in the content-free
//! persistence form of the Engine's other registers
//! (`core::archive::register`: owner-only JSON shards, bounded per shard in
//! count and bytes, and in age):
//!
//! - per sealed item (by the digest of its bytes): for each context it was
//!   produced for, the time of that record, the stored form of the authority
//!   its producing request was admitted under (digests, the project root and
//!   the signed horizon — the policy epoch only as its digest) and the lineage
//!   of that request;
//! - per assistant part (by its context-bound key): its lineage.
//!
//! Never stored: a sealed byte, content, the caller's credential, the
//! canonical policy bytes.
//!
//! Loading grants nothing. A loaded producer is held to exactly the checks
//! of one recorded in this process: the same context, its authority verified
//! against the policy, project, role and signed horizon in force NOW, its
//! lineage re-admitted NOW, and its own age. An authority whose source view
//! was invalidated is never stored. Writes are batched on one background
//! thread, off the response path, through a bounded queue; a write that fails
//! or is dropped leaves the item unknown, which is the safe state.
//!
//! Integrity: every record carries a MAC under the Instance's receipt key
//! over its key and its exact stored bytes (`keyed::sealed_record_ref`). A
//! record whose MAC does not verify — edited, forged, moved to another key,
//! or written under another key — is not loaded; without a usable key nothing
//! is stored or loaded. Forging a record is thus as hard as forging a
//! receipt: it needs the key in the gateway store, which the tamper guard
//! keeps out of a governed agent's reach.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use lean_ctx::core::archive::register::{self, Register as Store};
use lean_ctx::core::context_admission::receipt_store::keyed;

use super::{Entry, Lineage, MAX_AGE, Parts, Register, SealedContext, SealedProducer};
use crate::admitted::{Authority, StoredAuthority};

#[allow(clippy::cast_possible_wrap)]
const MAX_AGE_SECONDS: i64 = MAX_AGE.as_secs() as i64;

/// Sealed items: 256 shards of 64 digests.
const PRODUCERS: Store = Store {
    name: "sealed-origin",
    shard_entries: 64,
    max_age_seconds: MAX_AGE_SECONDS,
};

/// Assistant parts: 256 shards of 128 keys.
const PARTS: Store = Store {
    name: "sealed-parts",
    shard_entries: 128,
    max_age_seconds: MAX_AGE_SECONDS,
};

/// Contexts kept for one sealed item; the oldest goes first.
const MAX_PRODUCERS_PER_ITEM: usize = 8;

/// Records waiting for the writer; beyond it a record is dropped (unknown).
#[cfg_attr(test, allow(dead_code))]
const MAX_QUEUED: usize = 4096;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredProducer {
    /// Unix seconds of this producer's own record: a later record of the
    /// same item for another context does not make it younger.
    recorded_at: i64,
    context: SealedContext,
    authority: StoredAuthority,
    lineage: Option<Lineage>,
}

/// One stored record: its exact JSON and the MAC over register key and body.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    body: String,
    mac: String,
}

impl Signed {
    /// `None` without a usable key: nothing is stored.
    fn seal<V: serde::Serialize>(key: &str, value: &V) -> Option<Self> {
        let body = serde_json::to_string(value).ok()?;
        let mac = keyed::sealed_record_ref(key, &body).ok()?;
        Some(Self { body, mac })
    }

    /// The value, only when the MAC verifies for exactly this key and body.
    fn open<V: serde::de::DeserializeOwned>(&self, key: &str) -> Option<V> {
        let expected = keyed::sealed_record_ref(key, &self.body).ok()?;
        if !constant_time_eq(expected.as_bytes(), self.mac.as_bytes()) {
            return None;
        }
        serde_json::from_str(&self.body).ok()
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

enum Job {
    Producer(String, Signed),
    Part(String, Signed),
}

/// Keep what `producer` produced as the sealed item with this digest. An
/// authority that can never verify again is not kept.
pub(super) fn keep_producer(digest: &[u8; 32], producer: &SealedProducer) {
    let Some(authority) = producer.authority.stored() else {
        return;
    };
    let key = hex(digest);
    let stored = StoredProducer {
        recorded_at: register::now(),
        context: producer.context.clone(),
        authority,
        lineage: producer.lineage.as_deref().cloned(),
    };
    if let Some(signed) = Signed::seal(&key, &stored) {
        send(Job::Producer(key, signed));
    }
}

/// Keep the lineage of one assistant part under its context-bound key.
pub(super) fn keep_part(key: &[u8; 32], lineage: &Arc<Lineage>) {
    let key = hex(key);
    if let Some(signed) = Signed::seal(&key, &**lineage) {
        send(Job::Part(key, signed));
    }
}

/// What earlier processes kept for the register. Read without holding the
/// register's lock. Equal lineages share one handle, so the items of one
/// response are re-admitted once per request, as when recorded in-process.
pub(super) fn read_register() -> Vec<Entry> {
    let now = register::now();
    let mut lineages = Interned::default();
    let mut loaded = Vec::new();
    for (key, _, producers) in PRODUCERS.read_all::<Vec<Signed>>(now) {
        let Some(digest) = unhex(&key) else {
            continue;
        };
        for signed in producers {
            let Some(stored) = signed.open::<StoredProducer>(&key) else {
                continue;
            };
            let Some(recorded) = recorded_at(stored.recorded_at, now) else {
                continue;
            };
            loaded.push(Entry {
                digest,
                producer: SealedProducer {
                    context: stored.context,
                    authority: Arc::new(Authority::restore(stored.authority)),
                    lineage: stored.lineage.map(|lineage| lineages.get(lineage)),
                },
                recorded,
            });
        }
    }
    loaded
}

/// Merge what [`read_register`] found into the register, oldest first,
/// within its bounds.
pub(super) fn merge_register(register: &mut Register, mut loaded: Vec<Entry>) {
    loaded.extend(std::mem::take(&mut register.entries));
    loaded.sort_by_key(|entry| entry.recorded);
    let mut entries = VecDeque::from(loaded);
    while entries.len() > register.max_entries {
        entries.pop_front();
    }
    register.entries = entries;
}

/// What earlier processes kept for the parts, oldest first. Read without
/// holding the parts' lock.
pub(super) fn read_parts() -> Vec<([u8; 32], Instant, Arc<Lineage>)> {
    let now = register::now();
    let mut lineages = Interned::default();
    let mut loaded: Vec<_> = PARTS
        .read_all::<Signed>(now)
        .into_iter()
        .filter_map(|(key, at, signed)| {
            let lineage = signed.open::<Lineage>(&key)?;
            Some((unhex(&key)?, recorded_at(at, now)?, lineages.get(lineage)))
        })
        .collect();
    loaded.sort_by_key(|(_, at, _)| *at);
    loaded
}

/// Merge what [`read_parts`] found; a part recorded in this process wins.
pub(super) fn merge_parts(parts: &mut Parts, loaded: Vec<([u8; 32], Instant, Arc<Lineage>)>) {
    for (key, at, lineage) in loaded {
        if !parts.known.contains_key(&key) {
            parts.insert(key, at, lineage);
        }
    }
}

/// One handle per distinct stored lineage.
#[derive(Default)]
struct Interned(HashMap<[u8; 32], Arc<Lineage>>);

impl Interned {
    fn get(&mut self, lineage: Lineage) -> Arc<Lineage> {
        let Ok(encoded) = serde_json::to_vec(&lineage) else {
            return Arc::new(lineage);
        };
        Arc::clone(
            self.0
                .entry(*blake3::hash(&encoded).as_bytes())
                .or_insert_with(|| Arc::new(lineage)),
        )
    }
}

/// The instant a record of unix time `at` was made, on this process's
/// monotonic clock. `None` for a record from the future, one past the age
/// limit, or one older than the clock can express: it is not loaded.
fn recorded_at(at: i64, now: i64) -> Option<Instant> {
    let age = u64::try_from(now.checked_sub(at)?).ok()?;
    let age = Duration::from_secs(age);
    if age >= MAX_AGE {
        return None;
    }
    Instant::now().checked_sub(age)
}

fn hex(bytes: &[u8; 32]) -> String {
    blake3::Hash::from_bytes(*bytes).to_hex().to_string()
}

fn unhex(key: &str) -> Option<[u8; 32]> {
    blake3::Hash::from_hex(key)
        .ok()
        .map(|hash| *hash.as_bytes())
}

/// The producers kept for one item: those of `current` that verify and are
/// of another context than a new one, then the new ones; oldest dropped
/// beyond the bound.
fn merge_producers(key: &str, current: Vec<Signed>, new: Vec<Signed>) -> Vec<Signed> {
    let mut kept: Vec<(StoredProducer, Signed)> = current
        .into_iter()
        .chain(new)
        .filter_map(|signed| Some((signed.open::<StoredProducer>(key)?, signed)))
        .collect();
    // A later record of a context replaces an earlier one of it.
    let mut seen: Vec<SealedContext> = Vec::new();
    kept.reverse();
    kept.retain(|(producer, _)| {
        let first = !seen.contains(&producer.context);
        if first {
            seen.push(producer.context.clone());
        }
        first
    });
    kept.sort_by_key(|(producer, _)| producer.recorded_at);
    let excess = kept.len().saturating_sub(MAX_PRODUCERS_PER_ITEM);
    kept.drain(..excess);
    kept.into_iter().map(|(_, signed)| signed).collect()
}

/// The join of a verifying `current` lineage and the new one, sealed again.
// `current` is owned: the signature is the merge contract of
// `Register::update_many`, whose stored value this replaces.
#[allow(clippy::needless_pass_by_value)]
fn merge_lineage(key: &str, current: Signed, new: Signed) -> Signed {
    let (Some(mut joined), Some(added)) = (current.open::<Lineage>(key), new.open::<Lineage>(key))
    else {
        return new;
    };
    joined.add(&added);
    Signed::seal(key, &joined).unwrap_or(new)
}

fn write(jobs: Vec<Job>) {
    let mut producers = Vec::new();
    let mut parts = Vec::new();
    for job in jobs {
        match job {
            Job::Producer(key, producer) => producers.push((key, vec![producer])),
            Job::Part(key, lineage) => parts.push((key, lineage)),
        }
    }
    let now = register::now();
    if !producers.is_empty() {
        PRODUCERS.update_many(now, producers, merge_producers);
    }
    if !parts.is_empty() {
        PARTS.update_many(now, parts, merge_lineage);
    }
}

/// Tests write on the calling thread: a test's isolated data directory is
/// the one in force while it runs, and a later write must not land in
/// another test's directory or the user's own.
#[cfg(test)]
fn send(job: Job) {
    write(vec![job]);
}

#[cfg(not(test))]
fn send(job: Job) {
    use std::sync::mpsc::{self, SyncSender};
    use std::sync::{LazyLock, Mutex, PoisonError};

    static WRITER: LazyLock<Mutex<Option<SyncSender<Job>>>> = LazyLock::new(|| {
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUED);
        let spawned = std::thread::Builder::new()
            .name("lean-ctx-sealed-store".into())
            .spawn(move || write_loop(&receiver));
        Mutex::new(spawned.ok().map(|_| sender))
    });
    if let Some(sender) = WRITER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        // Full: the record is dropped and the item stays unknown after a
        // restart; the response is never held up for it.
        let _ = sender.try_send(job);
    }
}

/// Everything queued when a write starts goes into that write: one lock and
/// one file per touched shard for all of it.
#[cfg_attr(test, allow(dead_code))]
fn write_loop(receiver: &Receiver<Job>) {
    while let Ok(first) = receiver.recv() {
        let mut jobs = vec![first];
        jobs.extend(receiver.try_iter());
        write(jobs);
    }
}
