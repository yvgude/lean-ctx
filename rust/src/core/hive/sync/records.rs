// SPDX-License-Identifier: Apache-2.0
//! Verifying what the server sends for a clone and a tail (SPEC-ENGINE
//! §3.4.3, C0 2.0 section 6 `records`): the signed snapshot manifest, the
//! segment record stream with its envelope Merkle root, and tail pages.
//!
//! Nothing of a segment is imported unless its record count, seq run and
//! envelope root all match the signed manifest. Inside a matching segment
//! an event whose signature or body commitment fails is kept with
//! `sig_invalid` and never folded.

use serde_json::{Map, Value};

use crate::core::hive::events::sign;
use crate::core::hive::replica::log::StoredEvent;
use lean_ctx_protocol::hive::{canonical_json, signing_input_without_signature};

/// Signature domain of a snapshot manifest.
pub const SNAPSHOT_DOMAIN: &str = "leanctx.hive.snapshot.v1\n";

/// Why a segment is refused (refetched once, then `segment_unavailable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentError {
    Shape(String),
    Count { expected: u64, got: u64 },
    SeqRun,
    Root,
}

impl std::fmt::Display for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shape(what) => write!(f, "segment record shape: {what}"),
            Self::Count { expected, got } => {
                write!(f, "segment holds {got} records, manifest says {expected}")
            }
            Self::SeqRun => f.write_str("segment seq run has a gap or does not match"),
            Self::Root => f.write_str("segment envelope root does not match the manifest"),
        }
    }
}

/// `blake3(0x00 ‖ canonical_json(signed envelope))`.
pub fn leaf(envelope: &Value) -> Result<[u8; 32], SegmentError> {
    let canonical = canonical_json(envelope).map_err(|e| SegmentError::Shape(e.to_string()))?;
    let mut h = blake3::Hasher::new();
    h.update(&[0]);
    h.update(&canonical);
    Ok(*h.finalize().as_bytes())
}

/// The Merkle root over leaves in seq order: inner `blake3(0x01 ‖ l ‖ r)`,
/// an odd last node promoted unchanged.
pub fn root(leaves: &[[u8; 32]]) -> Option<[u8; 32]> {
    let mut level = leaves.to_vec();
    if level.is_empty() {
        return None;
    }
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| match pair {
                [l, r] => {
                    let mut h = blake3::Hasher::new();
                    h.update(&[1]);
                    h.update(l);
                    h.update(r);
                    *h.finalize().as_bytes()
                }
                [single] => *single,
                _ => unreachable!("chunks of two"),
            })
            .collect();
    }
    level.first().copied()
}

/// Whether `manifest` carries a valid signature of the Hive snapshot key
/// `public_key` whose key id it names.
pub fn manifest_verifies(manifest: &Value, public_key: &[u8; 32]) -> bool {
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    if manifest.get("key_id").and_then(Value::as_str) != Some(sign::key_id(public_key).as_str()) {
        return false;
    }
    let Some(sig) = manifest
        .get("signature")
        .and_then(Value::as_str)
        .and_then(|s| hex::decode(s).ok())
        .and_then(|b| <[u8; 64]>::try_from(b).ok())
    else {
        return false;
    };
    let (Ok(key), Ok(input)) = (
        VerifyingKey::from_bytes(public_key),
        signing_input_without_signature(SNAPSHOT_DOMAIN, manifest),
    ) else {
        return false;
    };
    key.verify(&input, &Signature::from_bytes(&sig)).is_ok()
}

/// A manifest's segment entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentMeta {
    pub n: u64,
    pub seq_from: u64,
    pub seq_to: u64,
    pub count: u64,
    pub envelope_root: String,
}

pub fn segments(manifest: &Value) -> Result<Vec<SegmentMeta>, SegmentError> {
    let list = manifest
        .get("segments")
        .and_then(Value::as_array)
        .ok_or_else(|| SegmentError::Shape("manifest without segments".into()))?;
    list.iter()
        .map(|s| {
            let n = |k: &str| {
                s.get(k)
                    .and_then(Value::as_u64)
                    .ok_or_else(|| SegmentError::Shape(format!("segment {k}")))
            };
            Ok(SegmentMeta {
                n: n("n")?,
                seq_from: n("seq_from")?,
                seq_to: n("seq_to")?,
                count: n("count")?,
                envelope_root: s
                    .get("envelope_root")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SegmentError::Shape("segment envelope_root".into()))?
                    .to_string(),
            })
        })
        .collect()
}

/// The whole event of a record: envelope, server fields and body merged.
fn event_of(record: &Value) -> Result<Value, SegmentError> {
    let mut event: Map<String, Value> = record
        .get("envelope")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| SegmentError::Shape("record without envelope".into()))?;
    if let Some(fields) = record.get("server_fields").and_then(Value::as_object) {
        for (k, v) in fields {
            event.insert(k.clone(), v.clone());
        }
    }
    if let Some(body) = record.get("body") {
        event.insert("body".into(), body.clone());
    }
    if record.get("purged").and_then(Value::as_bool) == Some(true) {
        event.insert("purged".into(), Value::Bool(true));
    }
    Ok(Value::Object(event))
}

/// Check one received event: signature (by the caller's signer rules,
/// SPEC-ENGINE §3.3.5) and, when it carries a body, the body commitment:
/// keyed with the place key, except a purge tombstone in a sealed place,
/// whose plaintext body the server commits unkeyed (it holds no key).
/// `true` when it may fold.
pub fn event_verifies(
    event: &Value,
    pk_id: &[u8; 32],
    sealed: bool,
    signature_ok: &dyn Fn(&Value) -> bool,
) -> bool {
    if !signature_ok(event) {
        return false;
    }
    let Some(body) = event.get("body").filter(|b| !b.is_null()) else {
        return true;
    };
    let unkeyed = sealed && event.get("op").and_then(Value::as_str) == Some("purge");
    let commit = if unkeyed {
        canonical_json(body)
            .ok()
            .map(|c| blake3::hash(&c).to_hex().to_string())
    } else {
        sign::body_commit(pk_id, body).ok()
    };
    commit.as_deref() == event.get("body_commit").and_then(Value::as_str)
}

/// Verify a segment's records against its manifest entry and turn them into
/// events to store. Withheld and converted stubs (`{seq, withheld|converted:
/// true, leaf}`, R90) count for the root by their leaf and are not stored.
pub fn import_segment(
    meta: &SegmentMeta,
    records: &[Value],
    pk_id: &[u8; 32],
    sealed: bool,
    signature_ok: &dyn Fn(&Value) -> bool,
) -> Result<Vec<StoredEvent>, SegmentError> {
    if records.len() as u64 != meta.count {
        return Err(SegmentError::Count {
            expected: meta.count,
            got: records.len() as u64,
        });
    }
    let mut leaves = Vec::with_capacity(records.len());
    let mut events = Vec::new();
    for (i, record) in records.iter().enumerate() {
        let seq = record
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| SegmentError::Shape("record without seq".into()))?;
        if seq != meta.seq_from + i as u64 {
            return Err(SegmentError::SeqRun);
        }
        let stub = |k: &str| record.get(k).and_then(Value::as_bool) == Some(true);
        if stub("withheld") || stub("converted") {
            let l = record
                .get("leaf")
                .and_then(Value::as_str)
                .and_then(|s| hex::decode(s).ok())
                .and_then(|b| <[u8; 32]>::try_from(b).ok())
                .ok_or_else(|| SegmentError::Shape("stub record without leaf".into()))?;
            leaves.push(l);
            continue;
        }
        let envelope = record
            .get("envelope")
            .ok_or_else(|| SegmentError::Shape("record without envelope".into()))?;
        leaves.push(leaf(envelope)?);
        let event = event_of(record)?;
        let ok = event_verifies(&event, pk_id, sealed, signature_ok);
        events.push(StoredEvent {
            seq,
            event,
            sig_invalid: !ok,
        });
    }
    if records
        .last()
        .and_then(|r| r.get("seq"))
        .and_then(Value::as_u64)
        != Some(meta.seq_to)
    {
        return Err(SegmentError::SeqRun);
    }
    if root(&leaves).map(hex::encode).as_deref() != Some(meta.envelope_root.as_str()) {
        return Err(SegmentError::Root);
    }
    Ok(events)
}

/// One tail page (E4): verified events and where the next page starts.
#[derive(Debug, Clone, PartialEq)]
pub struct TailPage {
    pub events: Vec<StoredEvent>,
    /// `{seq, converted: true}` stubs: plaintext seqs a sealing conversion
    /// replaced (R90); they fold as no-ops and are never stored as events.
    pub converted: Vec<u64>,
    pub next_since: Option<u64>,
    pub head_seq: u64,
    pub epoch: Option<u64>,
}

pub fn tail_page(
    page: &Value,
    pk_id: &[u8; 32],
    sealed: bool,
    signature_ok: &dyn Fn(&Value) -> bool,
) -> Result<TailPage, SegmentError> {
    let list = page
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| SegmentError::Shape("tail without events".into()))?;
    let mut events = Vec::with_capacity(list.len());
    let mut converted = Vec::new();
    for event in list {
        let seq = event
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| SegmentError::Shape("tail event without seq".into()))?;
        if event.get("converted").and_then(Value::as_bool) == Some(true) {
            converted.push(seq);
            continue;
        }
        events.push(StoredEvent {
            seq,
            event: event.clone(),
            sig_invalid: !event_verifies(event, pk_id, sealed, signature_ok),
        });
    }
    Ok(TailPage {
        events,
        converted,
        next_since: page.get("next_since").and_then(Value::as_u64),
        head_seq: page.get("head_seq").and_then(Value::as_u64).unwrap_or(0),
        epoch: page.get("epoch").and_then(Value::as_u64),
    })
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
