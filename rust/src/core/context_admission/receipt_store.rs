// SPDX-License-Identifier: Apache-2.0
//! Local, content-addressed store for Decision Receipts (G4).
//!
//! `<data>/gateway/receipts/<sha256>.json` holds one canonical receipt; the
//! file name is its canonical digest, so any edit is detectable on load.
//! `<data>/gateway/latest/<project>.json` lists the most recent receipt
//! digests of a project, newest first, for `lean-ctx inspect` and the HUD;
//! `<project>` is the hash of the project's canonical root ([`roots`]).
//! `<data>/gateway/tasks/<task>.json` lists the digests of a task's receipts,
//! oldest first, so its lineage survives the project index's rotation.
//! Receipts are stored packed and kept bounded by [`retention`].
//!
//! Deliberately not the execution ledger: its append verifies the whole chain
//! under an exclusive lock (O(n) per append) and validates task lifecycles, so
//! one event per tool call would degrade every read as the ledger grows. The
//! signed long-term proof chain is an Enterprise concern.

use std::io::Read;
use std::path::{Path, PathBuf};

use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, DeliveryOutcomeV1, EgressAdmissionOutcomeV1,
    EgressAdmissionReceiptV1, EgressTransportFailureV1, EgressTransportObservationV1,
    EgressTransportObserverV1, EgressTransportResultV1, EgressTransportV1,
};
use lean_ctx_protocol::{MAX_EGRESS_ATTEMPTS, Sha256Digest};
use serde::{Deserialize, Serialize};

pub(crate) mod capacity;
pub(crate) mod frame;
pub(crate) mod keyed;
pub(crate) mod observed;
pub(crate) mod references;
pub(crate) mod retention;
pub(crate) mod roots;
pub(crate) mod trace;
pub(crate) mod view;

/// Receipts kept in a project's index.
pub(crate) const LATEST_CAPACITY: usize = 64;
/// Receipts a task's index holds. A task that reaches it is reported as
/// incomplete from then on, never as fully known.
pub(crate) const TASK_CAPACITY: usize = 256;
/// Upper bound of an index file: capacity × (64 hex + quotes + comma) + framing.
const MAX_INDEX_BYTES: usize = TASK_CAPACITY * 70 + 256;
/// Admission, observation, and pointer records contain only bounded metadata.
const MAX_EGRESS_RECORD_BYTES: usize = 16 * 1024;

fn read_egress_record(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_EGRESS_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_EGRESS_RECORD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "egress record exceeds its size limit",
        ));
    }
    Ok(bytes)
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatestIndex {
    schema_version: u32,
    receipts: Vec<String>,
}

fn gateway_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("gateway"))
}

/// What the evidence store holds against its ceiling (G09), for the
/// operator's view (`lean-ctx doctor`). `None` without a data directory.
/// `megabytes` is the ceiling (`0` = none); `Err`: the store cannot be
/// measured completely, so nothing is said about it.
pub(crate) fn store_usage(megabytes: u64) -> Option<Result<capacity::Usage, String>> {
    gateway_dir().map(|dir| capacity::usage_in(&dir, megabytes))
}

fn project_key(project_root: &str) -> String {
    let digest = blake3::hash(project_root.as_bytes()).to_hex();
    digest[..16].to_owned()
}

fn receipt_path(dir: &Path, digest: &Sha256Digest) -> PathBuf {
    dir.join("receipts")
        .join(format!("{}.json.zst", digest.hex()))
}

/// Where receipts were stored before they were packed; still read.
fn plain_receipt_path(dir: &Path, digest: &Sha256Digest) -> PathBuf {
    dir.join("receipts").join(format!("{}.json", digest.hex()))
}

fn egress_admission_path(dir: &Path, digest: &Sha256Digest) -> PathBuf {
    dir.join("egress")
        .join("admissions")
        .join(format!("{}.json", digest.hex()))
}

fn egress_observation_path(dir: &Path, digest: &Sha256Digest) -> PathBuf {
    dir.join("egress")
        .join("observations")
        .join(format!("{}.json", digest.hex()))
}

fn egress_decision_path(dir: &Path, digest: &Sha256Digest) -> PathBuf {
    dir.join("egress")
        .join("decisions")
        .join(format!("{}.json", digest.hex()))
}

fn egress_attempt_path(dir: &Path, admission_digest: &Sha256Digest, attempt: u32) -> PathBuf {
    dir.join("egress")
        .join("attempts")
        .join(admission_digest.hex())
        .join(format!("{attempt}.json"))
}

fn latest_path(dir: &Path, project_root: &str) -> PathBuf {
    dir.join("latest")
        .join(format!("{}.json", project_key(project_root)))
}

/// One task's index, keyed by its tenant/project scope and task id: equal
/// task ids in different projects never share an index. Both parts are
/// hashed, so neither reaches the file system as a path.
fn task_path(dir: &Path, scope: &str, task_id: &str) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(scope.as_bytes());
    hasher.update(&[0]);
    hasher.update(task_id.as_bytes());
    let digest = hasher.finalize().to_hex();
    dir.join("tasks").join(format!("{}.json", &digest[..32]))
}

fn is_digest_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A missing index is empty. An oversized, malformed or foreign one is an
/// error — never silently empty, which would let the next write erase it.
fn read_index(path: &Path) -> Result<LatestIndex, ()> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LatestIndex::default());
        }
        Err(_) => return Err(()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_INDEX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(());
    }
    let index: LatestIndex = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if index.schema_version != 1
        || index.receipts.len() > TASK_CAPACITY
        || !index.receipts.iter().all(|hex| is_digest_hex(hex))
    {
        return Err(());
    }
    Ok(index)
}

/// Write one file of the store under `dir` through the store's one writer
/// ([`capacity::write`], G09): measured, decided against the ceiling,
/// written and booked in one critical section — a new file with its size, a
/// rewritten one (an index) with the difference to what it held. `Err`
/// before anything is written when the change does not fit.
pub(crate) fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic_as(dir, path, bytes, capacity::Kind::FollowUp)
}

/// [`write_atomic`] for the given kind of write: [`capacity::Kind::Entry`]
/// for the first record of a new evidence lifecycle. Unswept: see retention.
fn write_atomic_as(
    dir: &Path,
    path: &Path,
    bytes: &[u8],
    kind: capacity::Kind,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    capacity::write(dir, path, bytes.len() as u64, kind, || {
        crate::core::atomic_fs::try_atomic_write_unswept(path, bytes).map_err(|e| e.to_string())
    })
}

/// Write the NEW file `path` of the store once (`create_new`, synced)
/// through the store's one writer. `Err(AlreadyExists)` when it exists;
/// `Err(Other)` with the store's message when it is refused.
pub(crate) fn write_new(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    let mut exists = false;
    capacity::write(
        dir,
        path,
        bytes.len() as u64,
        capacity::Kind::FollowUp,
        || {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
                .map_err(|error| {
                    exists = error.kind() == std::io::ErrorKind::AlreadyExists;
                    error.to_string()
                })
        },
    )
    .map_err(|message| {
        if exists {
            std::io::ErrorKind::AlreadyExists.into()
        } else {
            std::io::Error::other(message)
        }
    })
}

/// A write-once mapping from one prepared admission attempt to its observation.
/// The pointed-to observation itself is separately content-addressed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EgressAttemptPointerV1 {
    schema_version: u32,
    observation_digest: Sha256Digest,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EgressDecisionPointerV1 {
    schema_version: u32,
    admission_digest: Sha256Digest,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredEgressAdmission {
    pub(crate) digest: Sha256Digest,
    pub(crate) admission: EgressAdmissionReceiptV1,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredEgressObservation {
    pub(crate) digest: Sha256Digest,
    pub(crate) observation: EgressTransportObservationV1,
}

#[derive(Debug, Clone)]
pub(crate) struct EgressLifecycleView {
    /// `None` means a proxy egress decision was stored but its lifecycle link
    /// was not; the legacy V1 `Delivered` outcome is not transport evidence.
    pub(crate) admission: Option<StoredEgressAdmission>,
    pub(crate) observations: Vec<StoredEgressObservation>,
}

/// Stored evidence bytes: compact JSON in struct declaration order. Not the
/// canonical (key-sorted) form — stored records keep these exact bytes.
fn evidence_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, &'static str> {
    let value = serde_json::to_value(value).map_err(|_| "egress evidence serialization failed")?;
    serde_json::to_vec(&value).map_err(|_| "egress evidence serialization failed")
}

fn write_once(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    // Refused by the store's bound (G09), a record leaves nothing behind.
    write_new(dir, path, bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            "egress evidence record already exists"
        } else {
            tracing::warn!("egress evidence record not written: {error}");
            "egress evidence store unavailable"
        }
    })
}

/// Serializes index read-modify-writes across processes: two calls of one task
/// finishing together must both end up in its index. Waits a bounded time
/// on the tool-call path; `None` when the lock cannot be had.
pub(crate) fn lock_task_files(dir: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".index.lock"))
        .ok()?;
    // Contention (including Windows' ERROR_LOCK_VIOLATION) is waited out for
    // `LOCK_WAIT`; any other error means no advisory locking here.
    crate::core::file_lock::acquire_exclusive_timeout(&file, LOCK_WAIT)
        .ok()
        .map(|()| file)
}

const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Marks a task whose receipt could not be indexed: its index can no longer
/// be complete, whatever it lists.
fn incomplete_marker(dir: &Path, scope: &str, task_id: &str) -> PathBuf {
    task_path(dir, scope, task_id).with_extension("incomplete")
}

/// Persist `receipt` and index it under `project_root` and, when it belongs to
/// a task, under the task's `scope`. Returns its digest.
pub(crate) fn persist(
    receipt: &ContextDecisionReceiptV1,
    project_root: &str,
    task_scope: Option<&str>,
) -> Result<Sha256Digest, String> {
    let dir = gateway_dir().ok_or("no lean-ctx data directory")?;
    let persisted = persist_in(&dir, receipt, project_root, task_scope);
    retention::maintain_in_background(dir, false);
    // The caller answers with the content-free `receipt.not_persisted`.
    persisted.inspect_err(|error| tracing::warn!("gateway receipt not persisted: {error}"))
}

pub(crate) fn persist_in(
    dir: &Path,
    receipt: &ContextDecisionReceiptV1,
    project_root: &str,
    task_scope: Option<&str>,
) -> Result<Sha256Digest, String> {
    let digest = receipt.canonical_digest().map_err(|e| e.to_string())?;
    let value = serde_json::to_value(receipt).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    let path = receipt_path(dir, &digest);
    if path.exists() || plain_receipt_path(dir, &digest).exists() {
        // A content-addressed filename alone is not evidence. Reusing a
        // tampered or unreadable object must fail just like a failed write.
        load_in(dir, digest.hex()).map_err(|_| "existing gateway receipt is invalid".to_owned())?;
    } else {
        // A new receipt enters an evidence lifecycle: decided, written, booked
        // and its reserve taken in one step — or refused before any write (G09).
        write_atomic_as(dir, &path, &retention::pack(&bytes)?, capacity::Kind::Entry)?;
    }

    let Some(_lock) = lock_task_files(dir) else {
        // The receipt itself is stored and keeps its digest for the audit
        // trail; only the indexes are skipped, and the task says so.
        if let (Some(task), Some(scope)) = (&receipt.task, task_scope) {
            let marker = incomplete_marker(dir, scope, task.as_str());
            // Never at the cost of the stored receipt's audit anchor.
            if let Err(error) = capacity::mark(dir, &marker) {
                tracing::warn!("task receipt index skipped and not marked incomplete: {error}");
            }
        }
        return Ok(digest);
    };
    // ONE key per project: the canonical root, however the writer spelled it.
    let project_root = roots::canonical(project_root);
    roots::register(dir, &project_root);
    let index_path = latest_path(dir, &project_root);
    // The project index is a convenience view of recent rounds; a damaged one
    // is rebuilt from this receipt on.
    let mut index = read_index(&index_path).unwrap_or_default();
    index.schema_version = 1;
    index.receipts.retain(|existing| existing != digest.hex());
    index.receipts.insert(0, digest.hex().to_owned());
    index.receipts.truncate(LATEST_CAPACITY);
    write_atomic(
        dir,
        &index_path,
        &serde_json::to_vec(&index).map_err(|e| e.to_string())?,
    )?;
    if let Some(output) = &receipt.final_context {
        index_output(dir, output, &digest);
    }

    if let (Some(task), Some(scope)) = (&receipt.task, task_scope) {
        let task_index_path = task_path(dir, scope, task.as_str());
        // A damaged task index is left as it is: readers report the task as
        // incomplete instead of a rewritten, shorter history.
        if let Ok(mut task_index) = read_index(&task_index_path)
            && task_index.receipts.len() < TASK_CAPACITY
            && !task_index.receipts.iter().any(|hex| hex == digest.hex())
        {
            task_index.schema_version = 1;
            task_index.receipts.push(digest.hex().to_owned());
            write_atomic(
                dir,
                &task_index_path,
                &serde_json::to_vec(&task_index).map_err(|e| e.to_string())?,
            )?;
        }
    }
    Ok(digest)
}

/// A receipt loaded back from the store, verified against its digest.
#[derive(Debug, Clone)]
pub(crate) struct StoredReceipt {
    pub(crate) digest: Sha256Digest,
    pub(crate) receipt: ContextDecisionReceiptV1,
}

/// Why a stored receipt could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoadError {
    Missing,
    Unreadable,
    /// The bytes no longer match the digest they are filed under.
    Tampered,
}

pub(crate) fn load_in(dir: &Path, hex: &str) -> Result<StoredReceipt, LoadError> {
    let expected = Sha256Digest::new(format!("sha256:{hex}")).map_err(|_| LoadError::Unreadable)?;
    let bytes = match std::fs::File::open(receipt_path(dir, &expected)) {
        Ok(file) => retention::unpack(file).map_err(|_| LoadError::Unreadable)?,
        Err(_) => {
            std::fs::read(plain_receipt_path(dir, &expected)).map_err(|_| LoadError::Missing)?
        }
    };
    let receipt: ContextDecisionReceiptV1 =
        serde_json::from_slice(&bytes).map_err(|_| LoadError::Unreadable)?;
    let actual = receipt
        .canonical_digest()
        .map_err(|_| LoadError::Tampered)?;
    if actual != expected {
        return Err(LoadError::Tampered);
    }
    Ok(StoredReceipt {
        digest: expected,
        receipt,
    })
}

fn validate_admission_link(
    dir: &Path,
    admission: &EgressAdmissionReceiptV1,
) -> Result<(), &'static str> {
    admission
        .validate()
        .map_err(|_| "egress admission is invalid")?;
    let Some(decision_digest) = admission.decision_receipt_digest.as_ref() else {
        return if admission.outcome == EgressAdmissionOutcomeV1::Failed {
            Ok(())
        } else {
            Err("egress admission is missing its decision receipt")
        };
    };
    let decision = load_in(dir, decision_digest.hex())
        .map_err(|_| "linked egress decision receipt unavailable")?;
    if decision.digest != *decision_digest {
        return Err("linked egress decision receipt digest mismatch");
    }
    let linked = &decision.receipt;
    let consistent = match admission.outcome {
        EgressAdmissionOutcomeV1::Prepared => {
            linked.outcome == DeliveryOutcomeV1::Delivered
                && linked.final_context == admission.prepared_bytes_digest
        }
        EgressAdmissionOutcomeV1::Withheld => {
            matches!(
                linked.outcome,
                DeliveryOutcomeV1::Withheld | DeliveryOutcomeV1::Failed
            ) && linked.final_context.is_none()
        }
        EgressAdmissionOutcomeV1::Failed => {
            linked.outcome == DeliveryOutcomeV1::Failed && linked.final_context.is_none()
        }
    };
    if consistent {
        Ok(())
    } else {
        Err("egress admission does not match its decision receipt")
    }
}

/// Persist an admission record as an immutable content-addressed object and
/// write a bounded decision-to-admission link for decision-centered reads.
pub(crate) fn persist_admission(
    admission: &EgressAdmissionReceiptV1,
) -> Result<Sha256Digest, &'static str> {
    let dir = gateway_dir().ok_or("egress evidence store unavailable")?;
    persist_admission_in(&dir, admission)
}

pub(crate) fn persist_admission_in(
    dir: &Path,
    admission: &EgressAdmissionReceiptV1,
) -> Result<Sha256Digest, &'static str> {
    validate_admission_link(dir, admission)?;
    let digest = admission
        .canonical_digest()
        .map_err(|_| "egress admission is invalid")?;
    let bytes = evidence_bytes(admission)?;
    let path = egress_admission_path(dir, &digest);
    if path.exists() {
        let stored = load_admission_in(dir, &digest)?;
        if stored.admission != *admission {
            return Err("egress admission digest collision");
        }
    } else {
        // A new admission enters an egress evidence lifecycle (G09): refused
        // as a whole when the store is full. The caller treats it like any
        // admission that could not be stored.
        write_atomic_as(dir, &path, &bytes, capacity::Kind::Entry).map_err(|error| {
            tracing::warn!("egress admission not persisted: {error}");
            "egress evidence store unavailable"
        })?;
        let stored = load_admission_in(dir, &digest)?;
        if stored.admission != *admission {
            return Err("egress admission verification failed");
        }
    }

    if let Some(decision_digest) = admission.decision_receipt_digest.as_ref() {
        let Some(_lock) = lock_task_files(dir) else {
            return Err("egress evidence store lock unavailable");
        };
        let pointer_path = egress_decision_path(dir, decision_digest);
        match read_decision_pointer(&pointer_path)? {
            Some(pointer) if pointer.admission_digest == digest => {}
            Some(_) => return Err("decision receipt already has another egress admission"),
            None => {
                let pointer = EgressDecisionPointerV1 {
                    schema_version: 1,
                    admission_digest: digest.clone(),
                };
                write_once(dir, &pointer_path, &evidence_bytes(&pointer)?)?;
            }
        }
    }
    Ok(digest)
}

fn read_decision_pointer(path: &Path) -> Result<Option<EgressDecisionPointerV1>, &'static str> {
    let bytes = match read_egress_record(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("egress evidence store unavailable"),
    };
    let pointer: EgressDecisionPointerV1 =
        serde_json::from_slice(&bytes).map_err(|_| "egress decision link is invalid")?;
    if pointer.schema_version != 1 {
        return Err("egress decision link has an unsupported version");
    }
    Ok(Some(pointer))
}

pub(crate) fn load_admission_in(
    dir: &Path,
    digest: &Sha256Digest,
) -> Result<StoredEgressAdmission, &'static str> {
    let bytes = read_egress_record(&egress_admission_path(dir, digest))
        .map_err(|_| "egress admission unavailable")?;
    let admission: EgressAdmissionReceiptV1 =
        serde_json::from_slice(&bytes).map_err(|_| "egress admission is unreadable")?;
    let actual = admission
        .canonical_digest()
        .map_err(|_| "egress admission is invalid")?;
    if actual != *digest {
        return Err("egress admission digest mismatch");
    }
    validate_admission_link(dir, &admission)?;
    Ok(StoredEgressAdmission {
        digest: actual,
        admission,
    })
}

fn read_attempt_pointer(path: &Path) -> Result<Option<EgressAttemptPointerV1>, &'static str> {
    let bytes = match read_egress_record(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("egress evidence store unavailable"),
    };
    let pointer: EgressAttemptPointerV1 =
        serde_json::from_slice(&bytes).map_err(|_| "egress attempt link is invalid")?;
    if pointer.schema_version != 1 {
        return Err("egress attempt link has an unsupported version");
    }
    Ok(Some(pointer))
}

fn load_observation_in(
    dir: &Path,
    digest: &Sha256Digest,
    admission: &EgressAdmissionReceiptV1,
) -> Result<StoredEgressObservation, &'static str> {
    let bytes = read_egress_record(&egress_observation_path(dir, digest))
        .map_err(|_| "egress observation unavailable")?;
    let observation: EgressTransportObservationV1 =
        serde_json::from_slice(&bytes).map_err(|_| "egress observation is unreadable")?;
    let actual = observation
        .canonical_digest()
        .map_err(|_| "egress observation is invalid")?;
    if actual != *digest {
        return Err("egress observation digest mismatch");
    }
    observation
        .validate_for(admission)
        .map_err(|_| "egress observation does not match its admission")?;
    Ok(StoredEgressObservation {
        digest: actual,
        observation,
    })
}

fn persist_observation_object_in(
    dir: &Path,
    observation: &EgressTransportObservationV1,
) -> Result<Sha256Digest, &'static str> {
    let digest = observation
        .canonical_digest()
        .map_err(|_| "egress observation is invalid")?;
    let bytes = evidence_bytes(observation)?;
    let path = egress_observation_path(dir, &digest);
    if path.exists() {
        let admission = load_admission_in(dir, &observation.admission_receipt_digest)?;
        let stored = load_observation_in(dir, &digest, &admission.admission)?;
        if stored.observation != *observation {
            return Err("egress observation digest collision");
        }
    } else {
        write_atomic(dir, &path, &bytes).map_err(|_| "egress evidence store unavailable")?;
    }
    Ok(digest)
}

fn append_observation_in(
    dir: &Path,
    observation: &EgressTransportObservationV1,
) -> Result<Sha256Digest, &'static str> {
    if observation.attempt > MAX_EGRESS_ATTEMPTS {
        return Err("egress observation attempt exceeds the limit");
    }
    let Some(_lock) = lock_task_files(dir) else {
        return Err("egress evidence store lock unavailable");
    };
    let admission = load_admission_in(dir, &observation.admission_receipt_digest)?;
    observation
        .validate_for(&admission.admission)
        .map_err(|_| "egress observation does not match a prepared admission")?;
    let digest = observation
        .canonical_digest()
        .map_err(|_| "egress observation is invalid")?;
    let path = egress_attempt_path(
        dir,
        &observation.admission_receipt_digest,
        observation.attempt,
    );
    if let Some(pointer) = read_attempt_pointer(&path)? {
        let stored = load_observation_in(dir, &pointer.observation_digest, &admission.admission)?;
        if stored.observation == *observation && stored.digest == digest {
            Ok(digest)
        } else {
            Err("egress attempt already has a different observation")
        }
    } else {
        persist_observation_object_in(dir, observation)?;
        let pointer = EgressAttemptPointerV1 {
            schema_version: 1,
            observation_digest: digest.clone(),
        };
        write_once(dir, &path, &evidence_bytes(&pointer)?)?;
        Ok(digest)
    }
}

#[cfg(feature = "http-server")]
pub(crate) fn append_engine_observation(
    observation: &EgressTransportObservationV1,
) -> Result<(), &'static str> {
    let dir = gateway_dir().ok_or("egress evidence store unavailable")?;
    append_engine_observation_in(&dir, observation)
}

#[cfg(feature = "http-server")]
pub(crate) fn append_engine_observation_in(
    dir: &Path,
    observation: &EgressTransportObservationV1,
) -> Result<(), &'static str> {
    if observation.observer != EgressTransportObserverV1::EngineObserved {
        return Err("Engine observations must be EngineObserved");
    }
    append_observation_in(dir, observation).map(|_| ())
}

pub(crate) fn append_host_observation(
    admission_digest: &Sha256Digest,
    attempt: u32,
    transport: EgressTransportV1,
    result: EgressTransportResultV1,
    http_status: Option<u16>,
    failure: Option<EgressTransportFailureV1>,
) -> Result<(Sha256Digest, EgressTransportObservationV1), &'static str> {
    let dir = gateway_dir().ok_or("egress evidence store unavailable")?;
    append_host_observation_in(
        &dir,
        admission_digest,
        attempt,
        transport,
        result,
        http_status,
        failure,
    )
}

pub(crate) fn append_host_observation_in(
    dir: &Path,
    admission_digest: &Sha256Digest,
    attempt: u32,
    transport: EgressTransportV1,
    result: EgressTransportResultV1,
    http_status: Option<u16>,
    failure: Option<EgressTransportFailureV1>,
) -> Result<(Sha256Digest, EgressTransportObservationV1), &'static str> {
    let admission = load_admission_in(dir, admission_digest)?;
    if admission.admission.outcome != EgressAdmissionOutcomeV1::Prepared {
        return Err("host reports require a prepared admission");
    }
    let observation = EgressTransportObservationV1 {
        schema_version: 1,
        admission_receipt_digest: admission_digest.clone(),
        attempt,
        transport,
        observer: EgressTransportObserverV1::HostReported,
        result,
        http_status,
        failure,
    };
    let digest = append_observation_in(dir, &observation)?;
    Ok((digest, observation))
}

/// Resolve a decision receipt's egress record without scanning directories.
/// Non-MCP receipts with a missing lifecycle pointer return a view with no
/// admission, preserving the unknown state of an interrupted write.
pub(crate) fn load_egress_for_decision(
    decision_digest: &Sha256Digest,
) -> Result<Option<EgressLifecycleView>, &'static str> {
    let dir = gateway_dir().ok_or("egress evidence store unavailable")?;
    load_egress_for_decision_in(&dir, decision_digest)
}

pub(crate) fn load_egress_for_decision_in(
    dir: &Path,
    decision_digest: &Sha256Digest,
) -> Result<Option<EgressLifecycleView>, &'static str> {
    let decision =
        load_in(dir, decision_digest.hex()).map_err(|_| "decision receipt unavailable")?;
    let pointer_path = egress_decision_path(dir, decision_digest);
    let Some(pointer) = read_decision_pointer(&pointer_path)? else {
        if decision.receipt.destination.provider.as_str() == "mcp-host" {
            return Ok(None);
        }
        return Ok(Some(EgressLifecycleView {
            admission: None,
            observations: Vec::new(),
        }));
    };
    let admission = load_admission_in(dir, &pointer.admission_digest)?;
    if admission.admission.decision_receipt_digest.as_ref() != Some(decision_digest) {
        return Err("egress decision link does not match its admission");
    }
    let mut observations = Vec::new();
    if admission.admission.outcome == EgressAdmissionOutcomeV1::Prepared {
        for attempt in 1..=MAX_EGRESS_ATTEMPTS {
            let path = egress_attempt_path(dir, &admission.digest, attempt);
            let Some(pointer) = read_attempt_pointer(&path)? else {
                continue;
            };
            let stored =
                load_observation_in(dir, &pointer.observation_digest, &admission.admission)?;
            if stored.observation.attempt != attempt {
                return Err("egress attempt link names a different attempt");
            }
            observations.push(stored);
        }
    }
    Ok(Some(EgressLifecycleView {
        admission: Some(admission),
        observations,
    }))
}

/// The newest `limit` receipts indexed under one project root, newest first,
/// each verified: its canonical key and the spelling given ([`roots`]). The
/// receipts of the whole repository: `roots::latest_of_repository`.
pub(crate) fn latest(
    project_root: &str,
    limit: usize,
) -> Vec<(String, Result<StoredReceipt, LoadError>)> {
    let Some(dir) = gateway_dir() else {
        return Vec::new();
    };
    latest_in(&dir, project_root, limit)
}

pub(crate) fn latest_in(
    dir: &Path,
    project_root: &str,
    limit: usize,
) -> Vec<(String, Result<StoredReceipt, LoadError>)> {
    roots::latest_in(dir, project_root, limit)
}

/// A task's receipts and whether they are all of them.
#[derive(Debug, Clone)]
pub(crate) struct TaskReceipts {
    /// Oldest first, each verified; a receipt naming another task is tampered.
    pub(crate) entries: Vec<(String, Result<StoredReceipt, LoadError>)>,
    /// False when the index is damaged or full: deliveries may be missing.
    pub(crate) index_complete: bool,
}

impl TaskReceipts {
    /// Every delivery is known and verified.
    pub(crate) fn complete(&self) -> bool {
        self.index_complete && self.entries.iter().all(|(_, loaded)| loaded.is_ok())
    }

    pub(crate) fn verified(&self) -> impl Iterator<Item = &ContextDecisionReceiptV1> {
        self.entries
            .iter()
            .filter_map(|(_, loaded)| loaded.as_ref().ok().map(|stored| &stored.receipt))
    }
}

pub(crate) fn for_task(scope: &str, task_id: &str) -> TaskReceipts {
    match gateway_dir() {
        Some(dir) => for_task_in(&dir, scope, task_id),
        None => TaskReceipts {
            entries: Vec::new(),
            index_complete: true,
        },
    }
}

pub(crate) fn for_task_in(dir: &Path, scope: &str, task_id: &str) -> TaskReceipts {
    let Ok(index) = read_index(&task_path(dir, scope, task_id)) else {
        return TaskReceipts {
            entries: Vec::new(),
            index_complete: false,
        };
    };
    let index_complete =
        index.receipts.len() < TASK_CAPACITY && !incomplete_marker(dir, scope, task_id).exists();
    let entries = index
        .receipts
        .into_iter()
        .map(|hex| {
            let loaded = load_in(dir, &hex).and_then(|stored| {
                if stored
                    .receipt
                    .task
                    .as_ref()
                    .map(lean_ctx_protocol::TaskId::as_str)
                    == Some(task_id)
                {
                    Ok(stored)
                } else {
                    Err(LoadError::Tampered)
                }
            });
            (hex, loaded)
        })
        .collect();
    TaskReceipts {
        entries,
        index_complete,
    }
}

/// A stored receipt by its digest, verified.
pub(crate) fn load(hex: &str) -> Result<StoredReceipt, LoadError> {
    let dir = gateway_dir().ok_or(LoadError::Missing)?;
    if !is_digest_hex(hex) {
        return Err(LoadError::Unreadable);
    }
    load_in(&dir, hex)
}

// ─── Lookup by delivered output ─────────────────────────────────────────────

/// Receipts one output digest lists. The same bytes can be delivered more
/// than once; the newest deliveries are kept.
const OUTPUT_CAPACITY: usize = 16;

/// `<data>/gateway/outputs/<sha256 of the delivered bytes>.json`: the receipts
/// whose `final_context` is that digest, newest first. The name is a digest a
/// receipt already holds; the file holds receipt digests only.
fn output_path(dir: &Path, output: &Sha256Digest) -> PathBuf {
    dir.join("outputs").join(format!("{}.json", output.hex()))
}

/// A convenience view for `lean-ctx inspect --output`: never at the cost of
/// the stored receipt, and a damaged index is left as it is (the reader then
/// falls back to the project index).
fn index_output(dir: &Path, output: &Sha256Digest, receipt: &Sha256Digest) {
    let path = output_path(dir, output);
    let Ok(mut index) = read_index(&path) else {
        return;
    };
    if index.receipts.iter().any(|hex| hex == receipt.hex()) {
        return;
    }
    index.schema_version = 1;
    index.receipts.insert(0, receipt.hex().to_owned());
    index.receipts.truncate(OUTPUT_CAPACITY);
    if let Ok(bytes) = serde_json::to_vec(&index)
        && let Err(error) = write_atomic(dir, &path, &bytes)
    {
        tracing::warn!("gateway receipt not indexed by its output: {error}");
    }
}

/// The SHA-256 digest a receipt's `final_context` holds for `bytes`.
pub(crate) fn output_digest(bytes: &[u8]) -> Sha256Digest {
    use sha2::{Digest, Sha256};
    let hex = crate::core::agent_identity::hex_encode(&Sha256::digest(bytes));
    Sha256Digest::new(format!("sha256:{hex}")).expect("SHA-256 digest is canonical")
}

/// The receipts whose final context is exactly `output`, newest first, each
/// verified. A receipt listed under an output it does not name is tampered.
/// Receipts stored before the output index existed are found through the
/// recent receipts of `project_root`.
pub(crate) fn by_output(
    project_root: &str,
    output: &Sha256Digest,
) -> Vec<(String, Result<StoredReceipt, LoadError>)> {
    let Some(dir) = gateway_dir() else {
        return Vec::new();
    };
    by_output_in(&dir, project_root, output)
}

pub(crate) fn by_output_in(
    dir: &Path,
    project_root: &str,
    output: &Sha256Digest,
) -> Vec<(String, Result<StoredReceipt, LoadError>)> {
    let indexed: Vec<_> = read_index(&output_path(dir, output))
        .unwrap_or_default()
        .receipts
        .into_iter()
        .map(|hex| {
            let loaded = load_in(dir, &hex).and_then(|stored| {
                if stored.receipt.final_context.as_ref() == Some(output) {
                    Ok(stored)
                } else {
                    Err(LoadError::Tampered)
                }
            });
            (hex, loaded)
        })
        .collect();
    if !indexed.is_empty() {
        return indexed;
    }
    latest_in(dir, project_root, LATEST_CAPACITY)
        .into_iter()
        .filter(|(_, loaded)| {
            loaded
                .as_ref()
                .is_ok_and(|stored| stored.receipt.final_context.as_ref() == Some(output))
        })
        .collect()
}

// ─── One delivery's receipt: build, persist, anchor ─────────────────────────

/// The typed reason of a delivery that was not released because its receipt
/// could not be stored while the store is mandatory.
pub(crate) const NOT_PERSISTED_CODE: &str = "receipt.not_persisted";

/// The ONE "receipt store is mandatory" predicate for MCP, CLI and proxy,
/// decided under the policy snapshot the caller already holds: protection is
/// active (`stored::source_authority_required_under` — a project policy
/// pack, the gateway in governed or sovereign mode, or a configuration that
/// could not be loaded), or — for the proxy, which is the only caller that
/// knows it — a destination policy is active. Reads the request's project:
/// call it inside the request scope.
pub(crate) fn mandatory_under(
    policy: &super::AdmissionPolicy,
    destination_policy_active: bool,
) -> bool {
    super::stored::source_authority_required_under(policy) || destination_policy_active
}

/// The receipt duty of one delivery, from the call's ONE policy snapshot
/// (`capture::CallSnapshot`): whether the store is mandatory, and the mode
/// and gateway policy digest a receipt is bound to when no admission bound
/// the capture. The snapshot is the one taken at the call's entry — the same
/// rules the release point of the call used — never a later read.
#[derive(Debug, Clone)]
pub(crate) struct Duty {
    pub(crate) mandatory: bool,
    snapshot: super::capture::CallSnapshot,
}

impl Duty {
    /// Take the call's snapshot now and decide the duty under it: the entry
    /// of a call. Call inside the request scope.
    pub(crate) fn at_entry() -> Self {
        Self::under(super::capture::CallSnapshot::take())
    }

    /// The duty of the call `capture` was opened for, under the snapshot the
    /// capture carries. A capture without one (not opened with
    /// `AdmissionCapture::for_call`) is decided under the rules configured
    /// now; call inside the request scope then.
    pub(crate) fn of(capture: &super::capture::AdmissionCapture) -> Self {
        capture
            .snapshot()
            .cloned()
            .map_or_else(Self::at_entry, Self::under)
    }

    fn under(snapshot: super::capture::CallSnapshot) -> Self {
        Self {
            mandatory: snapshot.protected(),
            snapshot,
        }
    }

    /// The policy snapshot this duty was decided under.
    pub(crate) fn snapshot(&self) -> &super::capture::CallSnapshot {
        &self.snapshot
    }
}

/// The closed set of results that carry no context to the host and therefore
/// owe no receipt under protection. Being content-free is this positive,
/// typed fact — never the mere absence of a recorded source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentFree {
    /// The result has no bytes (for the CLI: nothing but the final newline).
    NoBytes,
    /// The result is one typed refusal notice the gateway itself emits:
    /// `[lean-ctx gateway: content withheld — <reason codes>]`.
    GatewayNotice,
}

impl ContentFree {
    fn of(text: &str) -> Option<Self> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        if text.is_empty() {
            return Some(Self::NoBytes);
        }
        let codes = text
            .strip_prefix("[lean-ctx gateway: content withheld — ")?
            .strip_suffix(']')?;
        let typed = !codes.is_empty()
            && codes
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b',' | b' '));
        typed.then_some(Self::GatewayNotice)
    }
}

/// Who a delivery's receipt is recorded for.
pub(crate) struct ReceiptSubject<'a> {
    pub(crate) tool: &'a str,
    pub(crate) agent_id: Option<&'a str>,
    /// Content-free reference of a shown presenter capability, if any.
    pub(crate) presenter_ref: Option<&'a str>,
    pub(crate) project_root: &'a str,
    /// Transport and channel of the delivery, for its trace sidecar.
    pub(crate) delivery: &'a trace::Delivery,
}

/// What became of one delivery's receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReceiptRecord {
    /// No protection, and the call admitted no source: nothing to receipt.
    NotOwed,
    /// Under protection, and the result is in the content-free set.
    ContentFree(ContentFree),
    /// Built, validated and stored under `digest`. `prepared`: where the
    /// store is mandatory, the handle through which — and only through
    /// which — the hand-over of THIS receipt's bytes is observed.
    Stored {
        digest: Sha256Digest,
        security: bool,
        prepared: Option<observed::Prepared>,
    },
    /// Owed, but not built or not stored.
    Missing { security: bool },
}

impl ReceiptRecord {
    /// Whether a security event entered the audit trail for this delivery.
    pub(crate) fn security(&self) -> bool {
        match self {
            Self::NotOwed | Self::ContentFree(_) => false,
            Self::Stored { security, .. } | Self::Missing { security } => *security,
        }
    }

    /// The handle of the prepared delivery, to be confirmed by whoever
    /// hands its bytes over. `None`: nothing was sealed (no protection, no
    /// receipt), so there is nothing to observe.
    pub(crate) fn into_prepared(self) -> Option<observed::Prepared> {
        match self {
            Self::Stored { prepared, .. } => prepared,
            Self::NotOwed | Self::ContentFree(_) | Self::Missing { .. } => None,
        }
    }

    /// One content-free line for the log: what the receipt duty came to.
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::NotOwed => "not owed".to_owned(),
            Self::ContentFree(reason) => format!("not owed, content-free result ({reason:?})"),
            Self::Stored { digest, .. } => format!("stored {}", digest.as_str()),
            Self::Missing { .. } => "owed and missing".to_owned(),
        }
    }

    /// The fail-closed rule: where the store is mandatory, a delivery whose
    /// owed receipt is missing must not leave.
    pub(crate) fn withholds(&self, mandatory: bool) -> bool {
        mandatory && matches!(self, Self::Missing { .. })
    }
}

/// The content-free notice that stands in for a delivery withheld by
/// [`ReceiptRecord::withholds`].
pub(crate) fn not_persisted_notice() -> String {
    super::carrier::Withheld::new(&[NOT_PERSISTED_CODE]).notice()
}

/// One audit entry for a mandatory receipt and its sidecar: the receipt
/// digest and the digest of the sidecar's stored bytes, in the hashed
/// `action` of a signed, chained entry. Same prefix as
/// `security_events::anchor_receipt`, so one reader finds both forms.
/// Returns the hash of that entry.
fn anchor_pair(
    tool: &str,
    agent_id: &str,
    receipt: &Sha256Digest,
    trace_hex: &str,
) -> Result<String, String> {
    let session = crate::core::value::current_session()
        .map(|id| format!("|session={id}"))
        .unwrap_or_default();
    crate::core::audit_trail::try_record(crate::core::audit_trail::AuditEntryData {
        agent_id: agent_id.to_owned(),
        tool: tool.to_owned(),
        action: Some(format!(
            "gateway_receipt:{}|trace={trace_hex}{session}",
            receipt.hex()
        )),
        input_hash: String::new(),
        output_tokens: 0,
        role: crate::core::roles::active_role_name(),
        event_type: crate::core::audit_trail::AuditEventType::ToolCall,
    })
    .map(|entry| entry.entry_hash)
    .map_err(|error| format!("gateway receipt not anchored: {error}"))
}

/// What sealing a mandatory receipt needs besides the receipt itself.
pub(super) struct Sealing<'a> {
    pub(super) capture: &'a super::capture::AdmissionCapture,
    pub(super) delivery: &'a trace::Delivery,
    pub(super) delivered: &'a super::capture::Delivered<'a>,
    pub(super) tool: &'a str,
    pub(super) audit_agent: &'a str,
}

/// Seal the mandatory receipt stored under `digest`, for MCP, CLI and proxy
/// alike: it reads back verified, gets its trace sidecar, and the pair is
/// anchored in one signed audit entry. A step that fails AFTER the receipt
/// was stored leaves the receipt in the store (it has no delete and needs
/// none) and marks it there and then as a failed preparation
/// (`observed::preparation_failed`), so that it is never read as a delivery.
///
/// A sealed receipt says PREPARED. The returned handle is the only way an
/// observation of this receipt's hand-over is written; it carries the hash
/// of the audit entry that anchors the pair.
pub(super) fn seal(
    digest: &Sha256Digest,
    receipt: &ContextDecisionReceiptV1,
    sealing: &Sealing<'_>,
) -> Result<observed::Prepared, String> {
    use observed::PreparationStep as Step;
    load(digest.hex())
        .map_err(|_| {
            (
                Step::Receipt,
                "stored gateway receipt does not verify".to_owned(),
            )
        })
        .and_then(|_| {
            trace::build(
                digest,
                receipt,
                sealing.capture,
                sealing.delivery,
                sealing.delivered,
            )
            .and_then(|sidecar| trace::persist(&sidecar))
            .map_err(|error| (Step::Sidecar, error))
        })
        .and_then(|trace_hex| {
            anchor_pair(sealing.tool, sealing.audit_agent, digest, &trace_hex)
                .map_err(|error| (Step::Anchor, error))
        })
        .map(|anchor_entry| {
            observed::Prepared::sealed(
                digest,
                anchor_entry,
                sealing.delivery.channel(),
                sealing.delivery.prepared_bytes(sealing.delivered),
            )
        })
        .map_err(|(step, error)| {
            observed::preparation_failed(digest, step);
            error
        })
}

/// Turn `capture` into the receipt of the delivery `delivered`, store it and
/// anchor its digest in the signed audit trail — BEFORE the delivery leaves:
/// the caller releases `delivered.text` unchanged afterwards, or, when
/// [`ReceiptRecord::withholds`] says so, the notice instead. A withheld
/// delivery has no receipt of its own (the store that would hold it is the
/// one that failed); it is recorded as a `content_withheld` audit event.
///
/// Where the store is mandatory (`duty`):
/// - a result with bytes owes a receipt unless it is [`ContentFree`]; one
///   that no admission decided on gets a receipt whose single decision says
///   so (`delivery.no_source_decision`);
/// - the receipt gets a trace sidecar ([`trace`]) and the pair is anchored
///   in ONE signed audit entry — every mandatory receipt, not only those
///   with a security action. A sidecar or an anchor that cannot be written
///   counts like a receipt that cannot be stored, and the receipt that was
///   already stored is marked as a failed preparation ([`seal`]).
///
/// A stored receipt, its sidecar and its anchor say PREPARED: they exist
/// before a byte leaves and prove no hand-over. The observation is a second
/// record written by whoever hands the bytes over ([`observed`]).
///
/// Without protection nothing of that applies: a receipt for admitted
/// sources, anchored when a security action was recorded, as before.
pub(crate) fn record_delivery(
    capture: &super::capture::AdmissionCapture,
    subject: &ReceiptSubject<'_>,
    delivered: &super::capture::Delivered<'_>,
    duty: &Duty,
) -> ReceiptRecord {
    let mandatory = duty.mandatory;
    if !capture.describes_sources() {
        if !mandatory {
            return ReceiptRecord::NotOwed;
        }
        if let Some(reason) = ContentFree::of(delivered.text) {
            return ReceiptRecord::ContentFree(reason);
        }
        let policy = duty.snapshot.admission().admission_policy();
        capture.record_unsourced(
            output_digest(delivered.text.as_bytes()),
            policy.gateway.mode,
            super::fingerprint_digest(&policy.fingerprint()),
        );
    }
    let audit_agent = subject.agent_id.unwrap_or("unknown");
    let tally = capture.security_tally();
    let identity = super::capture::CallIdentity {
        agent_id: subject.agent_id,
        destination: None,
    };
    let stored =
        super::capture::finish_for_presenter(capture, &identity, subject.presenter_ref, delivered)
            .ok_or_else(|| "no valid receipt could be built".to_owned())
            .and_then(|receipt| {
                capture.record_security(subject.tool, audit_agent);
                let digest = persist(
                    &receipt,
                    subject.project_root,
                    capture.task_scope().as_deref(),
                )?;
                if !mandatory {
                    return Ok((digest, None));
                }
                // A mandatory receipt is only claimed once it reads back
                // verified, has its sidecar and the pair is anchored.
                let sealing = Sealing {
                    capture,
                    delivery: subject.delivery,
                    delivered,
                    tool: subject.tool,
                    audit_agent,
                };
                // PREPARED. Whether these bytes are handed over is observed
                // afterwards, by the caller, through this receipt's handle.
                let prepared = seal(&digest, &receipt, &sealing)?;
                Ok((digest, Some(prepared)))
            });
    match stored {
        Ok((digest, prepared)) => {
            // The digest enters the signed, hash-chained audit trail: a
            // stored receipt can later be checked against it.
            if prepared.is_none() && !tally.is_empty() {
                crate::core::security_events::anchor_receipt(
                    subject.tool,
                    audit_agent,
                    digest.hex(),
                );
            }
            ReceiptRecord::Stored {
                digest,
                security: !tally.is_empty(),
                prepared,
            }
        }
        Err(error) => {
            tracing::warn!("context gateway receipt not persisted: {error}");
            if mandatory {
                crate::core::security_events::record_one(
                    subject.tool,
                    audit_agent,
                    crate::core::security_events::SecurityKind::ContentWithheld,
                );
            }
            ReceiptRecord::Missing {
                security: mandatory || !tally.is_empty(),
            }
        }
    }
}

// ─── Audit-trail anchors ────────────────────────────────────────────────────

/// The `gateway_receipt:<digest>` entries of the signed audit trail, read
/// once, with the result of verifying the trail's hash chain.
pub(crate) struct AuditAnchors {
    /// The trail file exists and is readable.
    pub(crate) trail_present: bool,
    /// The trail could not be read at this moment (an append held its lock
    /// past the reader's bound, or the file is unreadable): nothing about
    /// anchors or the chain was CHECKED. Not an absent trail and not a broken
    /// chain — readers must say "not checked", never "not found" or "invalid".
    pub(crate) trail_unavailable: bool,
    /// Every hash link of the trail verifies.
    pub(crate) chain_valid: bool,
    /// Entries the trail holds, as far as the chain check counted them.
    pub(crate) entries_total: usize,
    /// The hash chain verifies from the first entry through this one
    /// (1-based); equal to `entries_total` when the whole chain verifies.
    pub(crate) verified_through: usize,
    /// Receipt digest (hex) → its anchor.
    entries: std::collections::HashMap<String, Anchor>,
}

/// What checking an audit entry's Ed25519 signature showed. The signature is
/// over the entry hash; that the hash covers the entry's fields is what the
/// chain check establishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignatureState {
    /// Verified with the public key of this machine's audit identity.
    Verified,
    /// A signature is there, but it cannot be checked here: the reason.
    Unverifiable(&'static str),
    /// The entry carries no signature.
    Absent,
    /// The signature does not verify with the trail's public key.
    Invalid,
}

/// One `gateway_receipt:` entry of the audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub(crate) entry_hash: String,
    /// 1-based position of the entry in the trail.
    pub(crate) position: usize,
    /// The hash chain verifies up to and including this entry.
    pub(crate) chain_verified: bool,
    pub(crate) signature: SignatureState,
    /// SHA-256 (hex) of the trace sidecar's stored bytes, when the entry
    /// anchors a pair.
    pub(crate) trace: Option<String>,
}

impl AuditAnchors {
    pub(crate) fn load() -> Self {
        match crate::core::audit_trail::default_trail_path() {
            Some(path) => Self::load_at(&path),
            None => Self {
                trail_present: false,
                trail_unavailable: false,
                chain_valid: false,
                entries_total: 0,
                verified_through: 0,
                entries: std::collections::HashMap::new(),
            },
        }
    }

    pub(crate) fn load_at(path: &Path) -> Self {
        use crate::core::agent_identity;
        let chain = crate::core::audit_trail::verify_chain_at(path);
        let verified_through = if chain.valid {
            chain.total_entries
        } else {
            chain.first_invalid_at.unwrap_or(0)
        };
        // The trail is signed by this installation's audit identity.
        let public_key = agent_identity::load_public_key("lean-ctx").ok();
        let mut entries = std::collections::HashMap::new();
        for (index, entry) in crate::core::audit_trail::load_all_at(path)
            .into_iter()
            .enumerate()
        {
            let Some(rest) = entry
                .action
                .as_deref()
                .and_then(|action| action.strip_prefix("gateway_receipt:"))
            else {
                continue;
            };
            let mut parts = rest.split('|');
            let Some(hex) = parts.next().filter(|hex| is_digest_hex(hex)) else {
                continue;
            };
            let trace = parts
                .find_map(|part| part.strip_prefix("trace="))
                .filter(|hex| is_digest_hex(hex))
                .map(str::to_owned);
            let signature = match (entry.signature.as_deref(), public_key.as_ref()) {
                (None, _) => SignatureState::Absent,
                (Some(_), None) => SignatureState::Unverifiable(
                    "the public key of the audit identity is not available on this machine",
                ),
                (Some(signature), Some(key)) => match agent_identity::hex_decode(signature) {
                    Ok(bytes)
                        if agent_identity::verify_signature(
                            key.as_bytes(),
                            entry.entry_hash.as_bytes(),
                            &bytes,
                        ) =>
                    {
                        SignatureState::Verified
                    }
                    _ => SignatureState::Invalid,
                },
            };
            let anchor = Anchor {
                position: index + 1,
                chain_verified: index < verified_through,
                signature,
                trace,
                entry_hash: entry.entry_hash,
            };
            entries.insert(hex.to_owned(), anchor);
        }
        Self {
            trail_present: chain.source_present,
            // `audit_trail` reports an unreadable or contended trail as "not
            // present and not valid"; an absent trail is "not present, valid".
            trail_unavailable: !chain.source_present && !chain.valid,
            chain_valid: chain.valid,
            entries_total: chain.total_entries,
            verified_through,
            entries,
        }
    }

    /// The anchor of the receipt stored under `hex`. `None`: no entry of the
    /// trail names it.
    pub(crate) fn of(&self, hex: &str) -> Option<&Anchor> {
        self.entries.get(hex)
    }
}

#[cfg(test)]
#[path = "receipt_store_egress_size_tests.rs"]
mod egress_record_size_tests;
