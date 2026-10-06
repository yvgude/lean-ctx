// SPDX-License-Identifier: Apache-2.0
//! Local, content-addressed store for Decision Receipts (G4).
//!
//! `<data>/gateway/receipts/<sha256>.json` holds one canonical receipt; the
//! file name is its canonical digest, so any edit is detectable on load.
//! `<data>/gateway/latest/<project>.json` lists the most recent receipt
//! digests of a project, newest first, for `lean-ctx inspect` and the HUD.
//! `<data>/gateway/tasks/<task>.json` lists the digests of a task's receipts,
//! oldest first, so its lineage survives the project index's rotation.
//!
//! Deliberately not the execution ledger: its append verifies the whole chain
//! under an exclusive lock (O(n) per append) and validates task lifecycles, so
//! one event per tool call would degrade every read as the ledger grows. The
//! signed long-term proof chain is an Enterprise concern.

use std::path::{Path, PathBuf};

use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::ContextDecisionReceiptV1;
use serde::{Deserialize, Serialize};

/// Receipts kept in a project's index.
pub(crate) const LATEST_CAPACITY: usize = 64;
/// Receipts a task's index holds. A task that reaches it is reported as
/// incomplete from then on, never as fully known.
pub(crate) const TASK_CAPACITY: usize = 256;
/// Upper bound of an index file: capacity × (64 hex + quotes + comma) + framing.
const MAX_INDEX_BYTES: usize = TASK_CAPACITY * 70 + 256;

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

fn project_key(project_root: &str) -> String {
    let digest = blake3::hash(project_root.as_bytes()).to_hex();
    digest[..16].to_owned()
}

fn receipt_path(dir: &Path, digest: &Sha256Digest) -> PathBuf {
    dir.join("receipts").join(format!("{}.json", digest.hex()))
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

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::core::atomic_fs::try_atomic_write(path, bytes, None).map_err(|e| e.to_string())
}

/// Serializes index read-modify-writes across processes: two calls of one task
/// finishing together must both end up in its index. Waits a bounded time
/// on the tool-call path; `None` when the lock cannot be had.
pub(crate) fn lock_task_files(dir: &Path) -> Option<std::fs::File> {
    use fs2::FileExt;
    std::fs::create_dir_all(dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".index.lock"))
        .ok()?;
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            // No advisory locking on this file system.
            Err(_) => return None,
        }
    }
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
    persist_in(&dir, receipt, project_root, task_scope)
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
    if !path.exists() {
        write_atomic(&path, &bytes)?;
    }

    let Some(_lock) = lock_task_files(dir) else {
        // The receipt itself is stored and keeps its digest for the audit
        // trail; only the indexes are skipped, and the task says so.
        if let (Some(task), Some(scope)) = (&receipt.task, task_scope) {
            let marker = incomplete_marker(dir, scope, task.as_str());
            // Never at the cost of the stored receipt's audit anchor.
            let marked = marker
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&marker, b""));
            if let Err(error) = marked {
                tracing::warn!("task receipt index skipped and not marked incomplete: {error}");
            }
        }
        return Ok(digest);
    };
    let index_path = latest_path(dir, project_root);
    // The project index is a convenience view of recent rounds; a damaged one
    // is rebuilt from this receipt on.
    let mut index = read_index(&index_path).unwrap_or_default();
    index.schema_version = 1;
    index.receipts.retain(|existing| existing != digest.hex());
    index.receipts.insert(0, digest.hex().to_owned());
    index.receipts.truncate(LATEST_CAPACITY);
    write_atomic(
        &index_path,
        &serde_json::to_vec(&index).map_err(|e| e.to_string())?,
    )?;

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
    let bytes = std::fs::read(receipt_path(dir, &expected)).map_err(|_| LoadError::Missing)?;
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

/// The newest `limit` receipts of a project, newest first, each verified.
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
    read_index(&latest_path(dir, project_root))
        .unwrap_or_default()
        .receipts
        .into_iter()
        .take(limit)
        .map(|hex| {
            let loaded = load_in(dir, &hex);
            (hex, loaded)
        })
        .collect()
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
