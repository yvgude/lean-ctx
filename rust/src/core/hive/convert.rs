// SPDX-License-Identifier: Apache-2.0
//! Conversion of a vault sealed after creation, source `v2_plain`
//! (SPEC-ENGINE §3.16 row "Vault sealed after creation", V13.E3,
//! RESOLUTIONS R85, R90, R97).
//!
//! W25 `{sealed: true}` on a vault with events makes every ordinary write
//! answer `503 conversion_pending` until one member engine has re-written
//! the plaintext log as sealed events. That engine:
//!
//! 1. takes the lease E38a `{source: v2_plain}` (10 min, renewed by asking
//!    again; another holder → `conversion_leased`, wait; the vault converts
//!    from another source → `conversion_source`, an engine bug);
//! 2. reads the plaintext events `seq ≤ conversion_from_seq` from its
//!    verified replica (records `{purged: true}` and withheld stubs are not
//!    re-encrypted; a purge tombstone has no content to copy);
//! 3. writes one sealed copy per event: same op, target, node type and
//!    body, the body sealed under a fresh content key and `ck_wrapped` under
//!    the current epoch key (no plaintext key leaves the engine), signed as
//!    an ordinary class-W event of the place-scoped alias with role
//!    `seal-convert`, **no** `cause`, `event_id = seal_conversion(pk_id,
//!    seq, original_event_id)` (stable across retries);
//! 4. pushes them in original `seq` order, 200 per E5 with
//!    `conversion {vault, source: v2_plain, from_seq, to_seq}`, keeping a
//!    cursor in `<data>/hive/convert/<vault_id>.cursor` (resumable after a
//!    crash or a lost lease; repeats answer `duplicate`);
//! 5. posts E38 with every non-purged `seq ≤ conversion_from_seq` and its
//!    `envelope_digest`, in parts of at most 10 000, each signed by the
//!    instance key over `leanctx.hive.conversion_done.v1\n`.
//!
//! Every engine then folds the replaced plaintext rows as no-ops
//! ([`settle_converted`], and the `{converted: true}` tail stubs).

use std::collections::BTreeMap;
use std::path::Path;

use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::HiveError;
use super::events::build::{EVENT_SCHEMA, Place, Signer};
use super::events::hlc::{self, Stamp};
use super::events::sealed_body::{self, SealKey};
use super::events::{ids, sign};
use super::replica::Replica;
use super::replica::converted::Original;
use super::state::write_private;

pub const SOURCE: &str = "v2_plain";
/// Role of the place-scoped alias that signs the sealed copies.
pub const SEAL_CONVERT_ROLE: &str = "seal-convert";
/// Host app of that alias (`host_app = engine`).
pub const SEAL_CONVERT_HOST: &str = "engine";
pub const DONE_DOMAIN: &str = "leanctx.hive.conversion_done.v1\n";
/// Events per E5 push.
pub const BATCH: usize = 200;
/// `seq_digests` entries per E38 part.
pub const PART: usize = 10_000;
/// The lease lasts 10 min; it is renewed after this long.
const RENEW_AFTER_MS: u64 = 5 * 60 * 1000;

/// The routes a conversion needs, answering status and body even for an
/// error so the `reason` of a `409 invalid_state` can be acted on.
pub trait ConvertApi {
    fn answer(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), HiveError>;
}

impl ConvertApi for crate::core::hive::auth::Session {
    fn answer(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), HiveError> {
        self.with_client(|c| c.send_answer(method, path, body, 8 * 1024 * 1024))
    }
}

/// Who converts and with what.
pub struct Conversion<'a> {
    pub vault_id: &'a str,
    /// The sealed place: `pk_id` from the place secret, `causal_base`.
    pub place: &'a Place,
    pub seal: &'a SealKey,
    /// The `seal-convert` alias: its key, worker id and `hlc.node`.
    pub signer: &'a Signer<'a>,
    /// The instance key that signs E38.
    pub instance_key: &'a SigningKey,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// E38 answered `204`: the vault is sealed and writable again.
    Done { events_written: u64, parts: usize },
    /// Another engine holds the lease until then (ms).
    Waiting { holder_expires_at: Option<u64> },
    /// Nothing to convert: the vault is not (or no longer) pending.
    NotPending,
}

/// Progress kept across runs: `<data>/hive/convert/<vault_id>.cursor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cursor {
    /// Last original `seq` whose sealed copy the place answered.
    pub last_seq: u64,
    pub events_written: u64,
    /// `conversion_digest_mismatch` answers per seq.
    pub mismatches: BTreeMap<u64, u32>,
    /// `conversion_incomplete` answers; the second one stops the run.
    pub incomplete: u32,
}

fn cursor_path(data_dir: &Path, vault_id: &str) -> std::path::PathBuf {
    data_dir.join("convert").join(format!("{vault_id}.cursor"))
}

pub fn load_cursor(data_dir: &Path, vault_id: &str) -> Cursor {
    std::fs::read(cursor_path(data_dir, vault_id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_cursor(data_dir: &Path, vault_id: &str, cursor: &Cursor) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec(cursor).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(
        &data_dir.join("convert"),
        &format!("{vault_id}.cursor"),
        &bytes,
    )
}

fn local(e: impl std::fmt::Display) -> HiveError {
    HiveError::Local(e.to_string())
}

fn reason(body: &Value) -> &str {
    body["error"]["reason"]
        .as_str()
        .or_else(|| body["error"]["code"].as_str())
        .unwrap_or_default()
}

fn refused(status: u16, body: &Value) -> HiveError {
    HiveError::Refused(format!(
        "conversion refused: {} ({status})",
        match reason(body) {
            "" => "unknown",
            r => r,
        }
    ))
}

enum Lease {
    Held { from_seq: u64 },
    Waiting(Option<u64>),
    NotPending,
}

fn take_lease(api: &dyn ConvertApi, vault_id: &str) -> Result<Lease, HiveError> {
    let (status, body) = api.answer(
        "POST",
        &format!("/vaults/{vault_id}/conversion/lease"),
        Some(&json!({ "source": SOURCE })),
    )?;
    match (status, reason(&body)) {
        (200..=299, _) => {
            if body["source"].as_str() != Some(SOURCE) {
                return Err(HiveError::Protocol(
                    "conversion lease for another source".into(),
                ));
            }
            body["conversion_from_seq"]
                .as_u64()
                .map(|from_seq| Lease::Held { from_seq })
                .ok_or_else(|| HiveError::Protocol("conversion lease without from_seq".into()))
        }
        (409, "conversion_leased") => {
            Ok(Lease::Waiting(body["error"]["holder_expires_at"].as_u64()))
        }
        (409, "conversion_done") => Ok(Lease::NotPending),
        _ => Err(refused(status, &body)),
    }
}

/// Whether an original has content to copy: purged records, failed
/// signatures and server purge tombstones are not re-encrypted.
fn copyable(o: &Original) -> bool {
    !o.purged && !o.sig_invalid && o.op != "purge" && o.body.is_some()
}

/// The sealed copy of one plaintext event.
pub fn sealed_copy(
    c: &Conversion<'_>,
    original: &Original,
    stamp: Stamp,
) -> Result<Value, HiveError> {
    let env = original
        .envelope
        .as_object()
        .ok_or_else(|| local("stored envelope is not an object"))?;
    let body = original
        .body
        .clone()
        .ok_or_else(|| local("no content to copy"))?;
    let mut e = Map::new();
    e.insert("schema".into(), EVENT_SCHEMA.into());
    e.insert(
        "event_id".into(),
        ids::seal_conversion_event_id(&c.place.pk_id, original.seq, &original.event_id).into(),
    );
    e.insert(
        "place".into(),
        json!({"kind": c.place.kind, "id": c.place.id}),
    );
    e.insert("op".into(), original.op.clone().into());
    for key in ["target", "node_type"] {
        if let Some(v) = env.get(key) {
            e.insert(key.into(), v.clone());
        }
    }
    e.insert(
        "body_commit".into(),
        sign::body_commit(&c.place.pk_id, &body)
            .map_err(local)?
            .into(),
    );
    e.insert("body".into(), body.clone());
    e.insert(
        "hlc".into(),
        json!({"wall_ms": stamp.wall_ms, "counter": stamp.counter, "node": c.signer.hlc_node}),
    );
    e.insert("causal_base".into(), c.place.causal_base.into());
    e.insert("actor".into(), json!({"kind": "worker"}));
    e.insert("worker_id".into(), c.signer.worker_id.clone().into());
    e.insert("provenance_claim".into(), "workspace".into());
    e.insert(
        "capture".into(),
        env.get("capture").cloned().unwrap_or_else(|| {
            json!({
                "gate_version": 0,
                "evidence": "observed",
                "reasons_passed_hash": blake3::hash(b"leanctx.hive.seal-convert.v1")
                    .to_hex()
                    .to_string(),
            })
        }),
    );
    e.insert(
        "screening".into(),
        env.get("screening").cloned().unwrap_or_else(|| {
            json!({
                "counts": {"forbidden": 0, "local_path": 0, "pii": 0, "redaction": 0, "secret": 0},
                "scanner_version": crate::core::hive::knowledge::screening::SCANNER_VERSION,
            })
        }),
    );
    let commands = body
        .get("commands")
        .and_then(Value::as_array)
        .is_some_and(|c| !c.is_empty());
    e.insert(
        "flags".into(),
        env.get("flags")
            .cloned()
            .unwrap_or_else(|| json!({ "commands": commands })),
    );
    // No `cause` and no `origin`: the copy is an ordinary class-W event.
    let mut event = Value::Object(e);
    sign::sign(&mut event, c.signer.key).map_err(local)?;
    sealed_body::seal_event(&mut event, c.seal, c.signer.key)?;
    Ok(event)
}

/// `{seq, envelope_digest}` of every non-purged original, in seq order.
pub fn seq_digests(originals: &[Original]) -> Result<Vec<(u64, String)>, HiveError> {
    originals
        .iter()
        .filter(|o| !o.purged)
        .map(|o| Ok((o.seq, sign::envelope_digest(&o.envelope).map_err(local)?)))
        .collect()
}

/// The signed E38 bodies for `digests`, in parts of at most [`PART`].
pub fn done_bodies(
    key: &SigningKey,
    events_written: u64,
    last_seq: u64,
    digests: &[(u64, String)],
) -> Result<Vec<Value>, HiveError> {
    let chunks: Vec<&[(u64, String)]> = if digests.is_empty() {
        vec![&[]]
    } else {
        digests.chunks(PART).collect()
    };
    let parts = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut body = json!({
                "source": SOURCE,
                "events_written": events_written,
                "last_seq": last_seq,
                "seq_digests": chunk
                    .iter()
                    .map(|(seq, digest)| json!({"seq": seq, "envelope_digest": digest}))
                    .collect::<Vec<_>>(),
                "part": i + 1,
                "parts": parts,
                "key_id": sign::key_id(key.verifying_key().as_bytes()),
            });
            let input =
                lean_ctx_protocol::hive::signing_input_without_signature(DONE_DOMAIN, &body)
                    .map_err(local)?;
            body["signature"] = hex::encode(key.sign(&input).to_bytes()).into();
            Ok(body)
        })
        .collect()
}

/// Push the copies of every original past the cursor (E5 with
/// `conversion`), saving progress after each batch.
#[allow(clippy::too_many_arguments)]
fn push_copies(
    api: &dyn ConvertApi,
    c: &Conversion<'_>,
    originals: &[Original],
    cursor: &mut Cursor,
    data_dir: &Path,
    now: &dyn Fn() -> u64,
    lease_at: &mut u64,
) -> Result<(), HiveError> {
    let todo: Vec<&Original> = originals
        .iter()
        .filter(|o| o.seq > cursor.last_seq && copyable(o))
        .collect();
    let mut clock = Stamp::default();
    let place_ref = format!("{}:{}", c.place.kind, c.place.id);
    for batch in todo.chunks(BATCH) {
        if now().saturating_sub(*lease_at) > RENEW_AFTER_MS {
            match take_lease(api, c.vault_id)? {
                Lease::Held { .. } => *lease_at = now(),
                _ => return Err(HiveError::Refused("conversion lease lost".into())),
            }
        }
        let mut events = Vec::with_capacity(batch.len());
        for original in batch {
            clock = hlc::send(clock, now());
            events.push(sealed_copy(c, original, clock)?);
        }
        let (from, to) = (batch[0].seq, batch[batch.len() - 1].seq);
        let request = json!({
            "events": events,
            "conversion": {"vault": c.vault_id, "source": SOURCE, "from_seq": from, "to_seq": to},
        });
        let (status, body) = api.answer(
            "POST",
            &format!("/places/{place_ref}/events"),
            Some(&request),
        )?;
        if !(200..300).contains(&status) {
            return Err(refused(status, &body));
        }
        let (results, _) = super::sync::push::results(&body).map_err(HiveError::Protocol)?;
        for (id, outcome) in &results {
            match outcome {
                super::sync::push::Outcome::Accepted { .. }
                | super::sync::push::Outcome::Duplicate { .. }
                | super::sync::push::Outcome::Held { .. } => cursor.events_written += 1,
                super::sync::push::Outcome::Refused { code, reason, .. } => {
                    return Err(HiveError::Refused(format!(
                        "conversion push refused {id}: {code}{}",
                        reason
                            .as_deref()
                            .map(|r| format!(" {r}"))
                            .unwrap_or_default()
                    )));
                }
            }
        }
        cursor.last_seq = to;
        save_cursor(data_dir, c.vault_id, cursor)?;
    }
    Ok(())
}

/// Convert the vault `c.vault_id` (one run; call it again after
/// `Waiting`). `replica` must have folded the log at least up to
/// `conversion_from_seq`.
pub fn run(
    api: &dyn ConvertApi,
    replica: &Replica,
    data_dir: &Path,
    c: &Conversion<'_>,
    now: &dyn Fn() -> u64,
) -> Result<Outcome, HiveError> {
    let from_seq = match take_lease(api, c.vault_id)? {
        Lease::Held { from_seq } => from_seq,
        Lease::Waiting(at) => {
            return Ok(Outcome::Waiting {
                holder_expires_at: at,
            });
        }
        Lease::NotPending => {
            let _ = std::fs::remove_file(cursor_path(data_dir, c.vault_id));
            return Ok(Outcome::NotPending);
        }
    };
    let mut lease_at = now();
    if replica.cursor()?.unwrap_or(0) < from_seq {
        return Err(HiveError::Local(format!(
            "the replica holds the log only to seq {}, conversion needs {from_seq}: sync first",
            replica.cursor()?.unwrap_or(0)
        )));
    }
    let originals = replica.originals_upto(from_seq)?;
    let mut cursor = load_cursor(data_dir, c.vault_id);
    loop {
        push_copies(
            api,
            c,
            &originals,
            &mut cursor,
            data_dir,
            now,
            &mut lease_at,
        )?;
        let digests = seq_digests(&originals)?;
        let bodies = done_bodies(c.instance_key, cursor.events_written, from_seq, &digests)?;
        let parts = bodies.len();
        let mut retry = false;
        for body in &bodies {
            let (status, answer) = api.answer(
                "POST",
                &format!("/vaults/{}/conversion/done", c.vault_id),
                Some(body),
            )?;
            match (status, reason(&answer)) {
                (204, _) => {
                    let _ = std::fs::remove_file(cursor_path(data_dir, c.vault_id));
                    return Ok(Outcome::Done {
                        events_written: cursor.events_written,
                        parts,
                    });
                }
                (200..=299, _) => continue,
                (409, "conversion_done") => {
                    let _ = std::fs::remove_file(cursor_path(data_dir, c.vault_id));
                    return Ok(Outcome::NotPending);
                }
                (409, "conversion_incomplete") => {
                    // Re-push every copy (repeats answer duplicate), once.
                    cursor.incomplete += 1;
                    if cursor.incomplete > 1 {
                        save_cursor(data_dir, c.vault_id, &cursor)?;
                        return Err(refused(status, &answer));
                    }
                    cursor.last_seq = 0;
                    cursor.events_written = 0;
                    retry = true;
                }
                (409, "conversion_digest_mismatch") => {
                    let seq = answer["error"]["seq"].as_u64().unwrap_or(0);
                    let seen = cursor.mismatches.entry(seq).or_insert(0);
                    *seen += 1;
                    if *seen > 1 {
                        save_cursor(data_dir, c.vault_id, &cursor)?;
                        return Err(HiveError::Refused(format!(
                            "conversion refused at seq {seq}"
                        )));
                    }
                    retry = true;
                }
                (409, "conversion_leased") => match take_lease(api, c.vault_id)? {
                    Lease::Held { .. } => {
                        lease_at = now();
                        retry = true;
                    }
                    Lease::Waiting(at) => {
                        return Ok(Outcome::Waiting {
                            holder_expires_at: at,
                        });
                    }
                    Lease::NotPending => return Ok(Outcome::NotPending),
                },
                _ => return Err(refused(status, &answer)),
            }
            if retry {
                break;
            }
        }
        save_cursor(data_dir, c.vault_id, &cursor)?;
        if !retry {
            return Err(HiveError::Protocol(
                "conversion done: the last part was not answered 204".into(),
            ));
        }
    }
}

/// Replica meta key: the vault needs no conversion any more on this engine.
const META_SETTLED: &str = "conversion_settled";

/// The sync's half for a sealed vault, before anything is written to it:
/// when the vault waits for its `v2_plain` conversion and this engine gets
/// the lease, it creates the epoch key and place secret (only the lease
/// holder does, §3.3.4), registers its `seal-convert` alias and converts.
/// `None` when nothing was attempted (done before, or no plaintext log
/// here to convert from).
pub fn sync_conversion(
    session: &crate::core::hive::auth::Session,
    record: &crate::core::hive::vault::VaultRecord,
    replica: &Replica,
) -> Result<Option<Outcome>, HiveError> {
    use crate::core::hive::workers::card::{Availability, Budget, WorkerCard};
    use crate::core::hive::workers::{self, register};
    if replica.meta(META_SETTLED)?.is_some() || replica.cursor()?.is_none() {
        return Ok(None);
    }
    let vault_id = record.vault_id.as_str();
    match take_lease(session, vault_id)? {
        Lease::NotPending => {
            replica.set_meta(META_SETTLED, b"1")?;
            return Ok(Some(Outcome::NotPending));
        }
        Lease::Waiting(at) => {
            return Ok(Some(Outcome::Waiting {
                holder_expires_at: at,
            }));
        }
        Lease::Held { .. } => {}
    }
    let store = &session.store;
    let crate::core::hive::sealed::SealingKey::Ready(epoch, key) =
        crate::core::hive::sealed::sealing_key(session, vault_id, 0)?
    else {
        return Err(HiveError::Refused("sealed_key_missing".into()));
    };
    let secret = crate::core::hive::sealed::ensure_place_secret(store, vault_id)?
        .ok_or_else(|| HiveError::Refused("sealed_key_missing".into()))?;
    let pk_id = ids::sealed_pk_id(&secret);
    let place_ref = format!("vault:{vault_id}");
    let keys = crate::core::hive::instance_key::InstanceKeys::load(store)?;
    let alias = workers::WorkerAliasKey::of(&keys);
    let card = WorkerCard::new(
        SEAL_CONVERT_HOST,
        None,
        Vec::new(),
        Budget::default(),
        Availability::Online,
    );
    let name = workers::default_name(
        &session.instance.label,
        &workers::attach::host_display(SEAL_CONVERT_HOST),
        SEAL_CONVERT_ROLE,
    );
    let reg = register::Registration {
        instance_id: &session.instance.instance_id,
        host_app: SEAL_CONVERT_HOST,
        role: SEAL_CONVERT_ROLE,
        name: &name,
        card: &card,
        alias_place: Some(&place_ref),
    };
    match session.with_client(|client| register::register(client, &keys, &reg)) {
        Ok(_) | Err(HiveError::Conflict { .. }) => {}
        Err(error) => return Err(error),
    }
    let alias_key = workers::alias_signing_key(&keys, &place_ref);
    let signer = Signer::alias(
        &alias_key,
        &keys,
        &place_ref,
        &alias.worker_id(SEAL_CONVERT_HOST, SEAL_CONVERT_ROLE, &place_ref),
    );
    let place = Place {
        kind: "vault".into(),
        id: vault_id.to_string(),
        pk_id,
        causal_base: replica.cursor()?.unwrap_or(0),
    };
    let seal = SealKey { epoch, key };
    let conversion = Conversion {
        vault_id,
        place: &place,
        seal: &seal,
        signer: &signer,
        instance_key: keys.signing_key(),
    };
    let now = || chrono::Utc::now().timestamp_millis().unsigned_abs();
    let outcome = run(session, replica, store.data_dir(), &conversion, &now)?;
    if matches!(outcome, Outcome::Done { .. } | Outcome::NotPending) {
        replica.set_meta(META_SETTLED, b"1")?;
    }
    Ok(Some(outcome))
}

/// Every engine of a sealed vault: plaintext rows whose sealed copy is in
/// the log fold as no-ops from now on and lose their plaintext body.
/// Returns how many rows it marked.
pub fn settle_converted(replica: &Replica, pk_id: &[u8; 32]) -> Result<usize, HiveError> {
    let mut done = Vec::new();
    for (seq, id) in replica.unconverted_ids()? {
        if replica.has_event(&ids::seal_conversion_event_id(pk_id, seq, &id))? {
            done.push(seq);
        }
    }
    replica.mark_converted(&done)
}

#[cfg(test)]
#[path = "convert_tests.rs"]
mod tests;
