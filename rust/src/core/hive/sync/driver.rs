// SPDX-License-Identifier: Apache-2.0
//! Driving a place's replica from the server (SPEC-ENGINE §3.4.3–§3.4.4):
//! clone (signed manifest, then every segment, each verified before import,
//! refetched once on a mismatch), tail since the cursor, and push of own
//! events. The server log is the only source of truth: accepted pushes stay
//! pending until the tail delivers them.

use serde_json::Value;

use super::{push, records};
use crate::core::hive::HiveError;
use crate::core::hive::replica::Replica;
use crate::core::hive::replica::store::{META_PLACE_NAME, META_SNAPSHOT_REV};

/// Largest segment body read (JSON lines).
const MAX_SEGMENT_BYTES: usize = 256 * 1024 * 1024;
/// Tail pages fetched per call before yielding.
const MAX_TAIL_PAGES: usize = 1_000;

/// The routes a replica needs. [`crate::core::hive::transport::HiveClient`]
/// implements it; tests use an in-memory server.
pub trait PlaceApi {
    fn get(&self, path: &str) -> Result<Value, HiveError>;
    fn get_bytes(&self, path: &str, max: usize) -> Result<Vec<u8>, HiveError>;
    fn post(&self, path: &str, body: &Value) -> Result<Value, HiveError>;
}

impl PlaceApi for crate::core::hive::transport::HiveClient {
    fn get(&self, path: &str) -> Result<Value, HiveError> {
        crate::core::hive::transport::HiveClient::get(self, path)
    }
    fn get_bytes(&self, path: &str, max: usize) -> Result<Vec<u8>, HiveError> {
        crate::core::hive::transport::HiveClient::get_bytes(self, path, max)
    }
    fn post(&self, path: &str, body: &Value) -> Result<Value, HiveError> {
        crate::core::hive::transport::HiveClient::post(self, path, body)
    }
}

/// How a place's events are checked on receive: the Hive snapshot keys and
/// the signer rules of §3.3.5 (class W worker keys, class A Hive keys).
pub struct Verifier<'a> {
    pub snapshot_keys: &'a [[u8; 32]],
    pub signature_ok: &'a dyn Fn(&Value) -> bool,
    /// The place is sealed (purge tombstones carry an unkeyed commit).
    pub sealed: bool,
    /// A sealed place's epoch keys held by this engine: received bodies are
    /// opened with them before their checks (§3.3.4 step 5).
    pub epoch_key: Option<&'a dyn Fn(u32) -> Option<zeroize::Zeroizing<[u8; 32]>>>,
}

fn protocol(what: impl std::fmt::Display) -> HiveError {
    HiveError::Protocol(what.to_string())
}

/// Open the sealed events or records in `items` with the held epoch keys.
fn open_sealed(items: Option<&mut Vec<Value>>, place: &str, verifier: &Verifier<'_>) {
    if let (Some(items), Some(keys)) = (items, verifier.epoch_key) {
        for item in items {
            crate::core::hive::events::sealed_body::open_received(item, place, keys);
        }
    }
}

fn place_path(place: &str) -> String {
    format!("/places/{place}")
}

/// The place id key from the replica (`id_salt` after a clone).
fn pk_id(replica: &Replica) -> Result<[u8; 32], HiveError> {
    replica
        .id_salt()?
        .ok_or_else(|| HiveError::Local("replica has no id_salt: clone it first".into()))
}

/// The zstd frame magic (E3 is served `Content-Encoding: zstd`).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// The JSON lines of a segment, zstd frames decoded (bounded) whether or
/// not the transport already decoded them.
fn segment_records(bytes: &[u8]) -> Result<Vec<Value>, HiveError> {
    use std::io::Read as _;
    let decoded;
    let text = if bytes.starts_with(&ZSTD_MAGIC) {
        let mut out = Vec::new();
        zstd::stream::read::Decoder::new(bytes)
            .map_err(protocol)?
            .take(MAX_SEGMENT_BYTES as u64 + 1)
            .read_to_end(&mut out)
            .map_err(protocol)?;
        if out.len() > MAX_SEGMENT_BYTES {
            return Err(protocol("segment larger than the limit"));
        }
        decoded = out;
        &decoded[..]
    } else {
        bytes
    };
    std::str::from_utf8(text)
        .map_err(protocol)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(protocol))
        .collect()
}

/// What a clone or tail did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub cloned: bool,
    pub stored: usize,
    pub cursor: Option<u64>,
    /// A failover reconcile ran (R48).
    pub reconciled: bool,
    /// Own events re-pushed from the history by it.
    pub repushed: usize,
    /// Local rows it dropped (their `seq` changed or did not come back).
    pub dropped: usize,
}

/// Clone `place` into an empty replica: verified manifest, then every
/// segment. A segment that fails its checks is fetched once more; a second
/// failure stops the clone (`segment_unavailable`) with nothing of it stored.
/// A snapshot revision that rotated out meanwhile (`404 not_found`, only the
/// latest 2 are kept) restarts the clone once from a fresh manifest.
pub fn clone_place(
    api: &dyn PlaceApi,
    replica: &mut Replica,
    place: &str,
    verifier: &Verifier<'_>,
) -> Result<SyncReport, HiveError> {
    match clone_once(api, replica, place, verifier) {
        Err(HiveError::NotFound) => clone_once(api, replica, place, verifier),
        other => other,
    }
}

fn clone_once(
    api: &dyn PlaceApi,
    replica: &mut Replica,
    place: &str,
    verifier: &Verifier<'_>,
) -> Result<SyncReport, HiveError> {
    let base = place_path(place);
    let manifest = api.get(&format!("{base}/snapshot"))?;
    if manifest.get("place").and_then(Value::as_str) != Some(place) {
        return Err(protocol("snapshot manifest names another place"));
    }
    if !verifier
        .snapshot_keys
        .iter()
        .any(|k| records::manifest_verifies(&manifest, k))
    {
        return Err(protocol("snapshot manifest signature does not verify"));
    }
    let rev = manifest
        .get("rev")
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol("manifest without rev"))?;
    // Non-sealed places carry the server's id_salt; sealed places derive
    // pk_id from the place secret (V4.11).
    if let Some(salt) = manifest.get("id_salt").and_then(Value::as_str) {
        let salt: [u8; 32] = hex::decode(salt)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| protocol("manifest id_salt"))?;
        replica.set_id_salt(&salt)?;
    }
    if let Some(epoch) = manifest.get("epoch").and_then(Value::as_u64) {
        replica.set_epoch(epoch)?;
    }
    replica.set_meta(META_PLACE_NAME, place.as_bytes())?;
    let pk = pk_id(replica)?;
    let mut stored = 0;
    for meta in records::segments(&manifest).map_err(protocol)? {
        let path = format!("{base}/snapshot/{rev}/segments/{}", meta.n);
        let mut attempt = 0;
        let events = loop {
            let mut lines = segment_records(&api.get_bytes(&path, MAX_SEGMENT_BYTES)?)?;
            open_sealed(Some(&mut lines), place, verifier);
            match records::import_segment(
                &meta,
                &lines,
                &pk,
                verifier.sealed,
                verifier.signature_ok,
            ) {
                Ok(events) => break events,
                Err(_) if attempt == 0 => attempt += 1,
                Err(e) => return Err(protocol(format!("segment_unavailable: {e}"))),
            }
        };
        stored += replica.insert_events(&events)?;
    }
    replica.set_meta(META_SNAPSHOT_REV, &rev.to_le_bytes())?;
    let mut report = tail(api, replica, place, verifier)?;
    report.cloned = true;
    report.stored += stored;
    Ok(report)
}

/// Fetch the log since the replica's cursor, page by page, store what
/// verifies (and what does not, marked `sig_invalid`), settle own pending
/// events the log now holds, and refold.
pub fn tail(
    api: &dyn PlaceApi,
    replica: &mut Replica,
    place: &str,
    verifier: &Verifier<'_>,
) -> Result<SyncReport, HiveError> {
    let pk = pk_id(replica)?;
    let now = now_ms();
    replica.prune_history(now)?;
    let mut since = replica.cursor()?.unwrap_or(0);
    let mut stored = 0;
    let mut lost = Vec::new();
    let mut converted = Vec::new();
    for _ in 0..MAX_TAIL_PAGES {
        let mut page = api.get(&format!("{}/events?since={since}", place_path(place)))?;
        open_sealed(
            page.get_mut("events").and_then(Value::as_array_mut),
            place,
            verifier,
        );
        let page = records::tail_page(&page, &pk, verifier.sealed, verifier.signature_ok)
            .map_err(protocol)?;
        // Failover (R48): a higher epoch, or a log that lost events below
        // our cursor, is reconciled before anything of this page is kept.
        let known = replica.epoch()?;
        let epoch_up = matches!((page.epoch, known), (Some(e), Some(k)) if e > k);
        let regressed = page.head_seq > 0 && page.head_seq < since;
        if epoch_up || regressed {
            let mut report = reconcile(
                api,
                replica,
                place,
                verifier,
                page.epoch.or(known),
                regressed.then_some(page.head_seq),
            )?;
            report.stored += stored;
            return Ok(report);
        }
        if known.is_none()
            && let Some(epoch) = page.epoch
        {
            replica.set_epoch(epoch)?;
        }
        stored += keep(replica, &page.events, now)?;
        lost.extend(page.events.iter().filter_map(|e| {
            (e.event.get("key_lost").and_then(Value::as_bool) == Some(true))
                .then(|| e.event.get("event_id").and_then(Value::as_str))
                .flatten()
                .map(str::to_string)
        }));
        converted.extend_from_slice(&page.converted);
        let last = page
            .events
            .iter()
            .map(|e| e.seq)
            .chain(page.converted.iter().copied())
            .max();
        let empty = page.events.is_empty() && page.converted.is_empty();
        match page.next_since.or(last) {
            Some(next) if next > since && !empty => since = next,
            _ => break,
        }
    }
    // A sealing conversion replaced these plaintext seqs (R90): their rows
    // fold as no-ops from now on, and the cursor moves past them.
    replica.mark_converted(&converted)?;
    replica.refold()?;
    if let Some(top) = converted.iter().max()
        && replica.cursor()?.is_none_or(|at| at < *top)
    {
        replica.set_cursor(*top)?;
    }
    let repaired = repair_key_lost(api, replica, place, &lost)?;
    if refetch_lost(api, replica, place, &pk, verifier)? > 0 {
        replica.refold()?;
    }
    Ok(SyncReport {
        cloned: false,
        stored,
        cursor: replica.cursor()?,
        repushed: repaired,
        ..SyncReport::default()
    })
}

/// `key_lost` bodies fetched per place and tail (inside E6's 600 per instance).
const MAX_BODY_REFETCH: usize = 100;

/// Fetch the content of `key_lost` rows again (E6); a body that verifies
/// with the stored envelope (signature and `body_commit`) is stored and the
/// row folds. Returns how many came back.
// lean-ctx: non-sealed places only; sealed E6 answers ({ct, nonce,
// ck_wrapped, epoch}) are read with V4.11's place secret.
fn refetch_lost(
    api: &dyn PlaceApi,
    replica: &Replica,
    place: &str,
    pk: &[u8; 32],
    verifier: &Verifier<'_>,
) -> Result<usize, HiveError> {
    if verifier.sealed {
        return Ok(0);
    }
    let mut restored = 0;
    for id in replica.key_lost_ids()?.into_iter().take(MAX_BODY_REFETCH) {
        let answer = api.get(&format!("{}/events/{id}/body", place_path(place)))?;
        let Some(body) = answer.get("body").filter(|b| !b.is_null()) else {
            continue;
        };
        let Some(mut event) = replica.stored_envelope(&id)? else {
            continue;
        };
        event["body"] = body.clone();
        if records::event_verifies(&event, pk, false, verifier.signature_ok) {
            replica.restore_body(&id, body)?;
            restored += 1;
        }
    }
    Ok(restored)
}

/// Re-push own events of `lost` (just served `key_lost`) with the identical
/// signed envelope kept in the history; the place stores the content again
/// and clears the flag (SPEC-ENGINE §3.4.4 "Repair").
// lean-ctx: only from the 24-h history; rebuilding older own events from
// the replica (`envelope_ct` + `body_ct`) follows with V4.11's re-wrap.
fn repair_key_lost(
    api: &dyn PlaceApi,
    replica: &Replica,
    place: &str,
    lost: &[String],
) -> Result<usize, HiveError> {
    let mut own = Vec::new();
    for id in lost {
        if let Some(bytes) = replica.history_event(id)? {
            own.push(serde_json::from_slice::<Value>(&bytes).map_err(protocol)?);
        }
    }
    if !own.is_empty() {
        api.post(
            &format!("{}/events", place_path(place)),
            &push::request(&own, None, &[]),
        )?;
    }
    Ok(own.len())
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0).unsigned_abs()
}

/// Deletions while the month's transfer is used up (E2E-FINAL). E4 then
/// answers only the `purge` tombstones of purges still waiting for acks,
/// with `next_since` = `since` (deleting is protected capacity). They fold
/// on top of the replica's graph without being stored, so the cursor never
/// moves past events not received yet, and the bodies they purge are
/// shredded at once; the caller denies, removes local text and acks.
/// `None`: the answer was an ordinary page (it moved on, or held other
/// events), so the quota is back and a full sync is due.
pub fn deletions(
    api: &dyn PlaceApi,
    replica: &mut Replica,
    place: &str,
    verifier: &Verifier<'_>,
) -> Result<Option<crate::core::hive::replica::fold::Graph>, HiveError> {
    use crate::core::hive::replica::fold::FoldEvent;
    let Some(since) = replica.cursor()? else {
        // Never cloned: there is nothing to delete here yet.
        return Ok(Some(crate::core::hive::replica::fold::Graph::default()));
    };
    let pk = pk_id(replica)?;
    let mut page = api.get(&format!("{}/events?since={since}", place_path(place)))?;
    open_sealed(
        page.get_mut("events").and_then(Value::as_array_mut),
        place,
        verifier,
    );
    let page =
        records::tail_page(&page, &pk, verifier.sealed, verifier.signature_ok).map_err(protocol)?;
    let str_of = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
    let tombstones_only = page.converted.is_empty()
        && page
            .events
            .iter()
            .all(|e| e.event.get("op").and_then(Value::as_str) == Some("purge"));
    if page.next_since.is_some_and(|n| n > since) || !tombstones_only {
        return Ok(None);
    }
    let mut graph = replica.refold()?;
    let events: Vec<FoldEvent> = page
        .events
        .iter()
        .filter(|e| e.seq > since)
        .map(|e| FoldEvent {
            seq: e.seq,
            event_id: str_of(&e.event, "event_id").unwrap_or_default(),
            op: "purge".into(),
            target: None,
            node_type: None,
            body: e.event.get("body").filter(|b| !b.is_null()).cloned(),
            sig_invalid: e.sig_invalid,
            provenance: str_of(&e.event, "provenance"),
            review_state: str_of(&e.event, "review_state"),
        })
        .collect();
    graph.fold(&events).map_err(protocol)?;
    replica.shred_events(&graph.purged_events)?;
    Ok(Some(graph))
}

/// Send signed purge acks alone (an ack-only E5 push, never metered).
pub fn push_acks(api: &dyn PlaceApi, place: &str, acks: &[Value]) -> Result<(), HiveError> {
    if acks.is_empty() {
        return Ok(());
    }
    api.post(
        &format!("{}/events", place_path(place)),
        &push::request(&[], None, acks),
    )
    .map(|_| ())
}

/// Store tailed events: settle own pending ones, note when each arrived.
fn keep(
    replica: &mut Replica,
    events: &[crate::core::hive::replica::log::StoredEvent],
    now: u64,
) -> Result<usize, HiveError> {
    for e in events {
        if let Some(id) = e.event.get("event_id").and_then(Value::as_str) {
            replica.settle_pending(id)?;
        }
    }
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    replica.note_received(&seqs, now)?;
    replica.insert_events(events)
}

/// Failover reconcile (SPEC-ENGINE §3.4.4, RESOLUTIONS R48), once per
/// trigger: re-push the kept own batches unchanged (the place answers
/// `duplicate` for what it still holds and logs the lost ones again),
/// re-tail from the lowest `seq` received in the last 24 h (or the place's
/// `head_seq` after a restore), drop local rows whose `seq` came back with
/// another event or not at all, refold, then take the new epoch.
pub fn reconcile(
    api: &dyn PlaceApi,
    replica: &mut Replica,
    place: &str,
    verifier: &Verifier<'_>,
    epoch: Option<u64>,
    head_seq: Option<u64>,
) -> Result<SyncReport, HiveError> {
    let pk = pk_id(replica)?;
    let now = now_ms();
    let mut report = SyncReport {
        reconciled: true,
        ..SyncReport::default()
    };
    for batch in replica.history_batches()? {
        let events: Vec<Value> = batch
            .iter()
            .map(|bytes| serde_json::from_slice(bytes).map_err(protocol))
            .collect::<Result<_, _>>()?;
        api.post(
            &format!("{}/events", place_path(place)),
            &push::request(&events, None, &[]),
        )?;
        report.repushed += events.len();
    }
    let cursor = replica.cursor()?.unwrap_or(0);
    let mut from = replica.rewind_seq(now)?.unwrap_or(cursor + 1);
    if let Some(head) = head_seq {
        from = from.min(head + 1);
    }
    let from = from.max(1);
    let mut since = from - 1;
    let mut fetched = Vec::new();
    for _ in 0..MAX_TAIL_PAGES {
        let mut page = api.get(&format!("{}/events?since={since}", place_path(place)))?;
        open_sealed(
            page.get_mut("events").and_then(Value::as_array_mut),
            place,
            verifier,
        );
        let page = records::tail_page(&page, &pk, verifier.sealed, verifier.signature_ok)
            .map_err(protocol)?;
        let last = page.events.iter().map(|e| e.seq).max();
        let empty = page.events.is_empty();
        fetched.extend(page.events);
        match page.next_since.or(last) {
            Some(next) if next > since && !empty => since = next,
            _ => break,
        }
    }
    let kept: Vec<(u64, String)> = fetched
        .iter()
        .filter_map(|e| {
            let id = e.event.get("event_id").and_then(Value::as_str)?;
            Some((e.seq, id.to_string()))
        })
        .collect();
    report.dropped = replica.drop_changed_from(from, &kept)?;
    report.stored = keep(replica, &fetched, now)?;
    replica.refold()?;
    if let Some(epoch) = epoch {
        replica.set_epoch(epoch)?;
    }
    report.cursor = replica.cursor()?;
    Ok(report)
}

/// What a push did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushReport {
    pub sent: usize,
    pub logged: usize,
    pub held: usize,
    /// Event ids to stamp again (`hlc_skew`).
    pub restamp: Vec<String>,
    /// Event ids to retry later (rate limits, migrations).
    pub retry: Vec<String>,
    /// Event ids that move to the failed queue, with the refusal: code,
    /// reason and failing member (`invalid_event shape /body/statement`).
    pub failed: Vec<(String, String)>,
    /// The purge acks went out with an accepted request.
    pub acked: bool,
}

/// Push the replica's pending own events in batches of
/// [`push::MAX_BATCH`], with the signed purge acks riding on the first
/// request (one is sent even without events). Failed events leave the
/// pending queue; everything else stays pending until the tail delivers
/// it. `report.acked` is true when the acks reached the place.
pub fn push_pending(
    api: &dyn PlaceApi,
    replica: &Replica,
    place: &str,
    purge_acks: &[Value],
) -> Result<PushReport, HiveError> {
    let pending = replica.pending()?;
    let mut report = PushReport::default();
    let mut batches: Vec<&[(String, zeroize::Zeroizing<Vec<u8>>)]> =
        pending.chunks(push::MAX_BATCH).collect();
    if batches.is_empty() && !purge_acks.is_empty() {
        batches.push(&[]);
    }
    for (i, batch) in batches.into_iter().enumerate() {
        let events: Vec<Value> = batch
            .iter()
            .map(|(_, bytes)| serde_json::from_slice(bytes).map_err(protocol))
            .collect::<Result<_, _>>()?;
        let acks = if i == 0 { purge_acks } else { &[] };
        let response = api.post(
            &format!("{}/events", place_path(place)),
            &push::request(&events, None, acks),
        )?;
        report.acked |= !acks.is_empty();
        // The place answered: keep the batch for a failover re-push (R48).
        let answered: Vec<(&str, &[u8])> = batch
            .iter()
            .map(|(id, bytes)| (id.as_str(), bytes.as_slice()))
            .collect();
        if !answered.is_empty() {
            replica.record_history(
                &crate::core::hive::events::ids::new_random_id("b")?,
                now_ms(),
                &answered,
            )?;
        }
        report.sent += events.len();
        let (results, _) = push::results(&response).map_err(protocol)?;
        for (id, outcome) in results {
            if outcome.logged() {
                report.logged += 1;
                continue;
            }
            match outcome.next() {
                None => report.held += 1,
                Some(push::Next::Restamp) => report.restamp.push(id),
                Some(push::Next::RetryLater) => report.retry.push(id),
                Some(push::Next::Fail) => {
                    let code = match &outcome {
                        push::Outcome::Refused {
                            code,
                            reason,
                            field,
                            ..
                        } => [Some(code.as_str()), reason.as_deref(), field.as_deref()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    };
                    replica.settle_pending(&id)?;
                    report.failed.push((id, code));
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
