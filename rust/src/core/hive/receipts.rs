// SPDX-License-Identifier: Apache-2.0
//! `POST /v1/receipts` (receipts v2, C0 2.5 K2 M3): the Engine receipts of
//! this machine's Hive vaults.
//!
//! The tool path only notes a receipt: [`note_engine_receipt`] checks one
//! marker file and, when uploads are on, turns the local v1 receipt into a
//! closed `leanctx.engine-receipt/v2` document and queues it in the durable
//! outbox. No lease, network or config work happens in a tool call. The
//! scheduler keeps the marker in step with what allows receipts — the
//! release switch, `[hive] receipts_upload`, the lease's `receipts.upload`
//! right (Enterprise), the workspace's `org_settings.receipts_upload` and no
//! `upload_disabled` answer under the current lease — and sends the queue
//! in signed batches (`leanctx.hive.receipts/v2`, instance key).
//!
//! What leaves the engine (aggregates and opaque values only):
//! - input and output digests as HMAC-SHA256 under an engine-local key
//!   (`receipts.hmac-key`, created here, never uploaded): Hive cannot test
//!   a guess against them, the engine can re-derive them from its receipt;
//! - the invocation and receipt ids as HMAC-derived hex, never the local id;
//! - source references as a count; failure recovery references dropped;
//! - measurements with a contract name and unit only;
//! - no policy reference: the Hive policy digest is not bound to a local
//!   invocation, so `policy_digest` is `null`.
//!
//! The checkout path stays on this machine: it only selects the Hive
//! vault, and a receipt of a checkout Hive does not know is dropped.

use std::path::Path;

use chrono::{DateTime, Utc};
use ed25519_dalek::Signer as _;
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::{Value, json};
use sha2::Sha256;

use super::auth::Session;
use super::config::HiveConfig;
use super::instance_key::InstanceKeys;
use super::metrics_v2::gate::Sharing;
use super::outbox::{CHANNEL_RECEIPTS, Outbox, OutboxItem};
use super::state::{InstanceRecord, write_private};
use super::vault::VaultMap;
use super::{HiveError, HiveStore};
use lean_ctx_protocol::hive::HiveUploadKind;
use lean_ctx_protocol::hive::lease::{HiveLeaseV1, HiveRight};
use lean_ctx_protocol::hive::metrics_v2::{KeyId, SignatureHex, TimeStamp, VaultId};
use lean_ctx_protocol::hive::receipts_v2::{
    AdmissionDecision, CapabilityIdConst, Classification, EngineIdConst, EngineReceiptSchema,
    EngineReceiptV2, FailureCode, FailureV2, HMAC_PREFIX, InvocationIdV2, MAX_MEASUREMENTS,
    MAX_RECEIPTS_BODY_BYTES, MAX_RECEIPTS_V2, MAX_SOURCE_COUNT, MeasurementName, MeasurementUnit,
    MeasurementV2, ObservationStatus, PolicyAdmissionV2, RECEIPTS_ROUTE, ReceiptEngineV2,
    ReceiptHmac, ReceiptIdV2, ReceiptInvocationV2, ReceiptItemV2, ReceiptObservationV2,
    ReceiptOperationV2, ReceiptSemver, ReceiptsAckV2, ReceiptsBodyV2, ReceiptsRequestV2,
    ReceiptsSchema, SchemaVersionOne,
};
use lean_ctx_protocol::{
    EngineFailureCodeV1, EngineInvocationV1, EngineObservationStatusV1, EngineObservationV1,
    EnginePolicyDecisionV1, EngineValueClassificationV1,
};

const ENABLED_FILE: &str = "receipts.enabled";
const DISABLED_FILE: &str = "receipts.disabled-lease";
/// The engine-local receipt HMAC key (32 random bytes, hex). Never uploaded.
const KEY_FILE: &str = "receipts.hmac-key";

/// The engine-local key that makes input/output values opaque to Hive.
#[derive(Clone)]
pub(crate) struct ReceiptKey([u8; 32]);

impl ReceiptKey {
    /// The key in `hive_data`, created on first use.
    pub(crate) fn load_or_create(hive_data: &Path) -> Option<Self> {
        if let Some(key) = Self::load(hive_data) {
            return Some(key);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).ok()?;
        write_private(hive_data, KEY_FILE, hex::encode(bytes).as_bytes()).ok()?;
        // Another process may have written first: the file wins.
        Self::load(hive_data)
    }

    fn load(hive_data: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(hive_data.join(KEY_FILE)).ok()?;
        let bytes = hex::decode(text.trim()).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// HMAC-SHA256(key, `label` ‖ `\n` ‖ `data`), lowercase hex.
    fn mac_hex(&self, label: &str, data: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC takes any key length");
        mac.update(label.as_bytes());
        mac.update(b"\n");
        mac.update(data.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn hmac(&self, label: &str, data: &str) -> Option<ReceiptHmac> {
        ReceiptHmac::new(&format!("{HMAC_PREFIX}{}", self.mac_hex(label, data)))
    }
}

fn unit_of(unit: &str) -> Option<MeasurementUnit> {
    // The local receipt says `token`; the contract `tokens`.
    MeasurementUnit::of(if unit == "token" { "tokens" } else { unit })
}

fn count(n: usize, floor: u16) -> u16 {
    u16::try_from(n)
        .unwrap_or(MAX_SOURCE_COUNT)
        .clamp(floor, MAX_SOURCE_COUNT)
}

/// The v2 receipt id and document of a local receipt, or `None` when it
/// cannot be expressed in the closed contract (it is then not sent).
pub(crate) fn to_v2(
    key: &ReceiptKey,
    receipt_id: &str,
    invocation: &EngineInvocationV1,
    observation: &EngineObservationV1,
) -> Option<(ReceiptIdV2, EngineReceiptV2)> {
    let rcpt = ReceiptIdV2::new(&format!(
        "rcpt_{}",
        &key.mac_hex("receipt", receipt_id)[..32]
    ))?;
    let invocation_id =
        InvocationIdV2::new(&key.mac_hex("invocation", invocation.invocation_id.as_str())[..32])?;
    let measurements = observation
        .measurements
        .iter()
        .filter_map(|m| {
            Some(MeasurementV2 {
                name: MeasurementName::new(&m.name)?,
                unit: unit_of(&m.unit)?,
                classification: match m.classification {
                    EngineValueClassificationV1::Measured => Classification::Measured,
                    EngineValueClassificationV1::Estimated => Classification::Estimated,
                    EngineValueClassificationV1::Unavailable => Classification::Unavailable,
                },
                value: m.value,
            })
        })
        .take(MAX_MEASUREMENTS)
        .collect();
    let document = EngineReceiptV2 {
        schema: EngineReceiptSchema,
        invocation: ReceiptInvocationV2 {
            schema_version: SchemaVersionOne,
            invocation_id: invocation_id.clone(),
            engine: ReceiptEngineV2 {
                engine_id: EngineIdConst,
                engine_version: ReceiptSemver::new(invocation.engine.engine_version.as_str())?,
            },
            operation: ReceiptOperationV2 {
                capability_id: (invocation.operation.capability_id.as_str()
                    == lean_ctx_protocol::hive::receipts_v2::RECEIPT_CAPABILITY_ID)
                    .then_some(CapabilityIdConst)?,
                capability_version: ReceiptSemver::new(
                    invocation.operation.capability_version.as_str(),
                )?,
            },
            input_hmac: key.hmac("input", invocation.input_digest.as_str())?,
            source_count: count(invocation.source_refs.len(), 1),
            policy_admission: PolicyAdmissionV2 {
                decision: match invocation.policy_admission.decision {
                    EnginePolicyDecisionV1::Admitted => AdmissionDecision::Admitted,
                    EnginePolicyDecisionV1::Rejected => AdmissionDecision::Rejected,
                },
                policy_digest: None,
            },
        },
        observation: ReceiptObservationV2 {
            schema_version: SchemaVersionOne,
            invocation_id,
            status: match observation.status {
                EngineObservationStatusV1::Succeeded => ObservationStatus::Succeeded,
                EngineObservationStatusV1::Degraded => ObservationStatus::Degraded,
                EngineObservationStatusV1::Rejected => ObservationStatus::Rejected,
                EngineObservationStatusV1::Failed => ObservationStatus::Failed,
            },
            output_hmac: match &observation.output_digest {
                Some(digest) => Some(key.hmac("output", digest.as_str())?),
                None => None,
            },
            source_count: count(observation.source_lineage.len(), 0),
            measurements,
            failure: observation.failure.as_ref().map(|f| FailureV2 {
                code: match f.code {
                    EngineFailureCodeV1::PolicyRejected => FailureCode::PolicyRejected,
                    EngineFailureCodeV1::SourceUnavailable => FailureCode::SourceUnavailable,
                    EngineFailureCodeV1::SourceIntegrityMismatch => {
                        FailureCode::SourceIntegrityMismatch
                    }
                    EngineFailureCodeV1::ResourceLimit => FailureCode::ResourceLimit,
                    EngineFailureCodeV1::UnsupportedOperation => FailureCode::UnsupportedOperation,
                    EngineFailureCodeV1::Internal => FailureCode::Internal,
                },
                retryable_by_host: f.retryable_by_host,
            }),
        },
    };
    document.validate().ok()?;
    Some((rcpt, document))
}

/// Queue an Engine receipt for Hive, if receipt uploads are on. Cheap and
/// silent: a failure here never fails the Engine call that produced it.
pub fn note_engine_receipt(
    root: &Path,
    receipt_id: &str,
    invocation: &EngineInvocationV1,
    observation: &EngineObservationV1,
    now: DateTime<Utc>,
) {
    let Ok(store) = HiveStore::open() else {
        return;
    };
    // N1: never while disconnected or revoked, whatever a stale marker says.
    // NEW-3: nor with the [hive] switches off or a lapsed lease.
    if !store.data_dir().join(ENABLED_FILE).is_file() || !store.may_record() {
        return;
    }
    let config = crate::core::config::Config::load().hive.clone();
    if !config.receipts_upload || !store.recording_allowed(&config) {
        return;
    }
    let Some(key) = ReceiptKey::load_or_create(store.data_dir()) else {
        return;
    };
    let Some((rcpt, document)) = to_v2(&key, receipt_id, invocation, observation) else {
        return;
    };
    let Ok(document) = serde_json::to_value(&document) else {
        return;
    };
    // `root` stays in the local outbox: it only selects the vault.
    let payload = json!({
        "receipt_id": rcpt.as_str(),
        "root": root.to_string_lossy(),
        "created_at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "document": document,
    });
    let _ = Outbox::at(store.data_dir()).enqueue(CHANNEL_RECEIPTS, payload, now);
}

/// Why receipts are not sent, or `None` when everything allows them.
pub(crate) fn blocked_by(
    released: bool,
    config: &HiveConfig,
    lease: &HiveLeaseV1,
    disabled_under_lease: Option<&str>,
) -> Option<&'static str> {
    if !config.receipts_upload {
        Some("switched off on this engine ([hive] receipts_upload = false)")
    } else if !lease.grants(HiveRight::ReceiptsUpload) {
        Some("the lease does not include receipts.upload (Enterprise)")
    } else if !lease.org_allows_receipts_upload() {
        Some("switched off by the workspace")
    } else if disabled_under_lease == Some(lease.lease_id.as_str()) {
        Some("switched off by the workspace (Hive refused the upload)")
    } else if !released {
        Some(NOT_RELEASED)
    } else {
        None
    }
}

/// Receipts v2 follow the engine-metrics release switch (owner go).
pub(crate) const NOT_RELEASED: &str = "receipts v2 wait for the engine telemetry release";

fn released(origin: &str) -> bool {
    super::metrics_v2::upload::released(origin)
}

fn disabled_lease(store: &HiveStore) -> Option<String> {
    std::fs::read_to_string(store.data_dir().join(DISABLED_FILE))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Owner decision (K2 review 2, N2): receipts fall under the same rule as
/// engine metrics, "Share engine metrics" — policy off, not confirmed
/// (Personal), no valid bundle in force or revoked: nothing is
/// collected or sent.
pub(crate) const SHARING_OFF: &str =
    "off under the workspace rule \"Share engine metrics\" (see Engine metrics)";

fn sharing(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
    lease: &HiveLeaseV1,
    now: DateTime<Utc>,
) -> Sharing {
    super::metrics_v2::gate::decide(store, config, instance, lease, now)
}

/// Why receipts are not sent now, for `hive status` (local only).
pub fn status(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
    lease: &HiveLeaseV1,
) -> Option<&'static str> {
    // The receipts switches, then the shared rule, then the release.
    blocked_by(true, config, lease, disabled_lease(store).as_deref())
        .or_else(|| {
            (!sharing(store, config, instance, lease, Utc::now()).is_on()).then_some(SHARING_OFF)
        })
        .or_else(|| (!released(&instance.url)).then_some(NOT_RELEASED))
}

/// Stop everything this engine records for Hive on this machine (K2
/// review 2, N1): the engine-metrics buffer, state and switches (secret
/// hits and K10/K12/K13 signals) and the receipts marker and queue. Called
/// when the engine disconnects, is revoked, its scheduler stops or Hive is
/// disabled; the next connected pass turns recording on again only when
/// every gate allows it.
pub(crate) fn stop_local_recording(hive_data: &Path) {
    super::metrics_v2::buffer::forget(hive_data);
    super::metrics_v2::buffer::note_local_off(hive_data);
    stop_receipts(hive_data);
}

/// The receipts half: the marker and every queued receipt.
fn stop_receipts(hive_data: &Path) {
    let _ = std::fs::remove_file(hive_data.join(ENABLED_FILE));
    let outbox = Outbox::at(hive_data);
    for item in outbox.list(CHANNEL_RECEIPTS) {
        let _ = outbox.remove(&item);
    }
}

/// K2 review 3 (NEW-2): `config set` of an `[hive]` off switch applies at
/// once. `enabled = false` and `metrics_upload = false` stop everything
/// (receipts follow the metrics rule, N2); `receipts_upload = false` stops
/// receipts.
pub(crate) fn apply_off_switches(config: &HiveConfig) {
    let Ok(store) = HiveStore::open() else {
        return;
    };
    apply_off_switches_in(store.data_dir(), config);
}

fn apply_off_switches_in(hive_data: &Path, config: &HiveConfig) {
    if config.enabled == Some(false) || !config.metrics_upload {
        stop_local_recording(hive_data);
    } else if !config.receipts_upload {
        stop_receipts(hive_data);
    }
}

/// Turn queueing on or off to match what allows receipts now; when off,
/// whatever is queued is dropped (it would never be sent).
fn set_enabled(store: &HiveStore, outbox: &Outbox, on: bool) -> Result<(), HiveError> {
    let marker = store.data_dir().join(ENABLED_FILE);
    if on {
        if !marker.is_file() {
            write_private(store.data_dir(), ENABLED_FILE, b"1")?;
        }
    } else {
        let _ = std::fs::remove_file(&marker);
        for item in outbox.list(CHANNEL_RECEIPTS) {
            outbox.remove(&item)?;
        }
    }
    Ok(())
}

/// The Hive vault id of a checkout Hive resolved on this machine.
fn vault_of(map: &VaultMap, root: &str) -> Option<VaultId> {
    map.vaults
        .values()
        .find(|record| record.roots.iter().any(|known| known == root))
        .and_then(|record| VaultId::new(&record.vault_id))
}

/// The wire item of a queued receipt, or `None` when it cannot be sent (a
/// checkout Hive does not know, or a v1 item queued by an older engine).
fn wire_item(map: &VaultMap, item: &OutboxItem) -> Option<ReceiptItemV2> {
    let payload = &item.payload;
    let document: EngineReceiptV2 = serde_json::from_value(payload["document"].clone()).ok()?;
    ReceiptItemV2::new(
        ReceiptIdV2::new(payload["receipt_id"].as_str()?)?,
        vault_of(map, payload["root"].as_str()?)?,
        TimeStamp::new(payload["created_at"].as_str()?)?,
        document,
    )
    .ok()
}

/// Sign a batch with the instance key (as engine metrics).
pub(crate) fn sign(
    keys: &InstanceKeys,
    body: ReceiptsBodyV2,
) -> Result<ReceiptsRequestV2, HiveError> {
    let signer = keys.signing_key();
    let key_id = KeyId::new(&super::workers::key_id(signer))
        .ok_or_else(|| HiveError::Local("instance key id".into()))?;
    let input = body
        .signing_input(&key_id)
        .map_err(|e| HiveError::Local(e.to_string()))?;
    let signature = SignatureHex::new(&hex::encode(signer.sign(&input).to_bytes()))
        .ok_or_else(|| HiveError::Local("signature".into()))?;
    Ok(body.signed(key_id, signature))
}

/// Batches of at most 256 items and 1 MiB (signature included).
fn batches(items: Vec<(ReceiptItemV2, OutboxItem)>) -> Vec<Vec<(ReceiptItemV2, OutboxItem)>> {
    // `key_id` and `signature` with their JSON framing.
    const SIGNED_EXTRA: usize = 200;
    let mut out = Vec::new();
    let mut current: Vec<(ReceiptItemV2, OutboxItem)> = Vec::new();
    let mut size = 64usize;
    for (wire, item) in items {
        let len = serde_json::to_vec(&wire).map_or(usize::MAX, |v| v.len() + 1);
        if !current.is_empty()
            && (current.len() >= MAX_RECEIPTS_V2
                || size.saturating_add(len) + SIGNED_EXTRA > MAX_RECEIPTS_BODY_BYTES)
        {
            out.push(std::mem::take(&mut current));
            size = 64;
        }
        size = size.saturating_add(len);
        current.push((wire, item));
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The same batch always carries the same key, so a retry is idempotent.
fn idempotency_key(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(&Sha256::digest(bytes)[..16])
}

/// What a receipts run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptsRun {
    Off(&'static str),
    Sent {
        accepted: u64,
        duplicates: u64,
        dropped: u64,
    },
}

/// Bring the marker in step with the gates and send what is queued.
pub fn run(
    session: &Session,
    config: &HiveConfig,
    now: DateTime<Utc>,
) -> Result<ReceiptsRun, HiveError> {
    let store = &session.store;
    let lease = &session.lease.lease;
    let outbox = Outbox::at(store.data_dir());
    let mut refused = disabled_lease(store);
    // A newer lease that allows receipts lifts an earlier refusal.
    if refused
        .as_deref()
        .is_some_and(|id| id != lease.lease_id && lease.org_allows_receipts_upload())
    {
        let _ = std::fs::remove_file(store.data_dir().join(DISABLED_FILE));
        refused = None;
    }
    if let Some(reason) = blocked_by(true, config, lease, refused.as_deref()) {
        set_enabled(store, &outbox, false)?;
        return Ok(ReceiptsRun::Off(reason));
    }
    let Sharing::On {
        vaults_off, since, ..
    } = sharing(store, config, &session.instance, lease, now)
    else {
        set_enabled(store, &outbox, false)?;
        return Ok(ReceiptsRun::Off(SHARING_OFF));
    };
    if !released(&session.instance.url) {
        set_enabled(store, &outbox, false)?;
        return Ok(ReceiptsRun::Off(NOT_RELEASED));
    }
    set_enabled(store, &outbox, true)?;
    let map = VaultMap::load(store.data_dir());
    let (mut accepted, mut duplicates, mut dropped) = (0u64, 0u64, 0u64);
    let mut ready = Vec::new();
    for item in outbox
        .list(CHANNEL_RECEIPTS)
        .into_iter()
        .filter(|item| item.due(now))
    {
        // Nothing from before the sharing period, nothing of a vault whose
        // own policy turns the rule off.
        let allowed = |wire: &ReceiptItemV2| {
            since.is_none_or(|since| wire.created_at.at() >= since)
                && !vaults_off.contains(wire.vault_id.as_str())
        };
        if let Some(wire) = wire_item(&map, &item).filter(allowed) {
            ready.push((wire, item));
        } else {
            outbox.remove(&item)?;
            dropped += 1;
        }
    }
    if ready.is_empty() {
        return Ok(ReceiptsRun::Sent {
            accepted,
            duplicates,
            dropped,
        });
    }
    let keys = InstanceKeys::load(store)?;
    for batch in batches(ready) {
        let (wires, sent): (Vec<ReceiptItemV2>, Vec<OutboxItem>) = batch.into_iter().unzip();
        let body = ReceiptsBodyV2 {
            schema: ReceiptsSchema,
            receipts: wires,
        };
        if body.validate().is_err() {
            // Malformed (a duplicate id): Hive would refuse the batch as a
            // whole, so it is dropped rather than retried forever.
            for item in &sent {
                outbox.remove(item)?;
                dropped += 1;
            }
            continue;
        }
        let request = sign(&keys, body)?;
        let bytes = serde_json::to_vec(&request).map_err(|e| HiveError::Local(e.to_string()))?;
        let wire: Value =
            serde_json::from_slice(&bytes).map_err(|e| HiveError::Local(e.to_string()))?;
        let key = idempotency_key(&bytes);
        match session.with_client(|client| client.post_idempotent(RECEIPTS_ROUTE, &wire, &key)) {
            Ok(answer) => {
                let ack: ReceiptsAckV2 = serde_json::from_value(answer)
                    .map_err(|e| HiveError::Protocol(format!("receipts ack: {e}")))?;
                accepted += u64::from(ack.accepted);
                duplicates += u64::from(ack.duplicates);
                for item in &sent {
                    outbox.remove(item)?;
                }
            }
            Err(HiveError::UploadDisabled(HiveUploadKind::Receipts | HiveUploadKind::Unknown)) => {
                write_private(store.data_dir(), DISABLED_FILE, lease.lease_id.as_bytes())?;
                set_enabled(store, &outbox, false)?;
                return Ok(ReceiptsRun::Off(
                    "switched off by the workspace (Hive refused the upload)",
                ));
            }
            Err(error) if error.is_transient() => {
                // 429: Retry-After / retry_after_s, or a quota's reset_at.
                let retry_after = error.retry_after_at(chrono::Utc::now());
                for item in &sent {
                    let mut item = item.clone();
                    outbox.retry_later(&mut item, &error.to_string(), retry_after, now)?;
                }
                return Err(error);
            }
            Err(error) => {
                for item in &sent {
                    let mut item = item.clone();
                    outbox.fail(&mut item, &error.to_string())?;
                }
                return Err(error);
            }
        }
    }
    Ok(ReceiptsRun::Sent {
        accepted,
        duplicates,
        dropped,
    })
}

#[cfg(test)]
#[path = "receipts_tests.rs"]
mod tests;
