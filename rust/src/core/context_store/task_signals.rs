// SPDX-License-Identifier: Apache-2.0
//! Per-task runtime signals that say whether a compressed read served the
//! model: a full re-read of the same file (bounce), an edit that failed after
//! a compressed read, or an expansion of a file handle that was read
//! compressed.
//!
//! Each event is attributed to the task whose compressed read caused it — not
//! to the task that happens to be running when it is noticed. Nothing missing
//! may read as clean: an event whose causing read is unknown (another process,
//! before a restart), an event that could not be recorded, and a task served
//! by more than one process (whose bounces no single process can see) all
//! leave that task's signals unknown. Written once per process and task and
//! when an event happens, never per read; counts only, no paths.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_FILE_BYTES: u64 = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    Bounce,
    EditFailure,
    Expand,
    Unattributed,
    /// This process serves reads for the task.
    Observed,
}

/// Counts of one task's events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskSignals {
    pub(crate) schema_version: u32,
    pub bounce: u32,
    pub edit_failure: u32,
    pub expand: u32,
    pub unattributed: u32,
    /// The process that served the task's reads, while only one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) observer: Option<String>,
}

/// Identifies this process among the ones that serve a task's reads.
fn process_nonce() -> &'static str {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NONCE.get_or_init(|| {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!("{}-{started}", std::process::id())
    })
}

/// The tenant/project scope and task id a read ran under.
pub(crate) type Origin = (String, String);

/// The scope and task of the code running now, if it runs inside a task.
pub(crate) fn current_origin() -> Option<Origin> {
    let envelope = crate::core::task_spine::TaskSpine::current()?;
    Some((
        super::task_scope(envelope.tenant_id.as_ref(), &envelope.project_id),
        envelope.task_id.as_str().to_owned(),
    ))
}

fn gateway_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("gateway"))
}

fn signals_path(dir: &Path, (scope, task): &(String, String)) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(scope.as_bytes());
    hasher.update(&[0]);
    hasher.update(task.as_bytes());
    let digest = hasher.finalize().to_hex();
    dir.join("tasks")
        .join(format!("{}.signals.json", &digest[..32]))
}

/// Marks a task whose event could not be recorded; written without the lock.
fn incomplete_marker(path: &Path) -> PathBuf {
    path.with_extension("incomplete")
}

fn read(path: &Path) -> Option<TaskSignals> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    serde_json::from_slice::<TaskSignals>(&std::fs::read(path).ok()?)
        .ok()
        .filter(|signals| signals.schema_version == 1)
}

pub(crate) fn record_in(dir: &Path, origin: &Origin, signal: Signal) -> Result<(), String> {
    record_as(dir, origin, signal, process_nonce())
}

fn record_as(dir: &Path, origin: &Origin, signal: Signal, process: &str) -> Result<(), String> {
    let path = signals_path(dir, origin);
    let _lock = crate::core::context_admission::receipt_store::lock_task_files(dir)
        .ok_or("task index lock unavailable")?;
    // A damaged file is replaced by a state that says nothing is known.
    let mut signals = match std::fs::metadata(&path) {
        Ok(_) => read(&path).unwrap_or(TaskSignals {
            unattributed: 1,
            ..TaskSignals::default()
        }),
        Err(_) => TaskSignals::default(),
    };
    signals.schema_version = 1;
    let counter = match signal {
        Signal::Bounce => Some(&mut signals.bounce),
        Signal::EditFailure => Some(&mut signals.edit_failure),
        Signal::Expand => Some(&mut signals.expand),
        Signal::Unattributed => Some(&mut signals.unattributed),
        Signal::Observed => match signals.observer.as_deref() {
            Some(observer) if observer == process => return Ok(()),
            // A second process: neither sees the other's bounces.
            Some(_) => Some(&mut signals.unattributed),
            None => {
                signals.observer = Some(process.to_owned());
                None
            }
        },
    };
    if let Some(counter) = counter {
        *counter = counter.saturating_add(1);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec(&signals).map_err(|e| e.to_string())?;
    crate::core::atomic_fs::try_atomic_write(&path, &bytes, None).map_err(|e| e.to_string())
}

/// Record `signal` for the task whose read caused it. An event that cannot
/// be recorded marks the task's signals unknown instead of leaving them clean.
pub(crate) fn record(origin: &Origin, signal: Signal) {
    let Some(dir) = gateway_dir() else {
        return;
    };
    if let Err(error) = record_in(&dir, origin, signal) {
        tracing::debug!("task signal not recorded: {error}");
        let marker = incomplete_marker(&signals_path(&dir, origin));
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(marker);
    }
}

/// Register this process as serving reads for the task, once per process.
pub(crate) fn observe(origin: &Origin) {
    static OBSERVED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<Origin>>> =
        std::sync::OnceLock::new();
    let first = OBSERVED
        .get_or_init(Default::default)
        .lock()
        .map_or(true, |mut observed| observed.insert(origin.clone()));
    if first {
        let origin = origin.clone();
        std::thread::spawn(move || record(&origin, Signal::Observed));
    }
}

/// An event caused by a compressed read nobody can name: the current task's
/// signals become unknown.
pub(crate) fn record_unattributed() {
    if let Some(origin) = current_origin() {
        record(&origin, Signal::Unattributed);
    }
}

pub(crate) fn load_in(dir: &Path, origin: &Origin) -> Option<TaskSignals> {
    let path = signals_path(dir, origin);
    let unknown = TaskSignals {
        unattributed: 1,
        ..TaskSignals::default()
    };
    if incomplete_marker(&path).exists() {
        return Some(read(&path).map_or(unknown, |signals| TaskSignals {
            unattributed: signals.unattributed.max(1),
            ..signals
        }));
    }
    if !path.exists() {
        return Some(TaskSignals::default());
    }
    read(&path).or(Some(unknown))
}

/// The task's signals; a task without a file had no event.
pub(crate) fn load(origin: &Origin) -> Option<TaskSignals> {
    load_in(&gateway_dir()?, origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_count_per_task_and_scope_and_damage_means_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_a = ("[null,\"a\"]".to_owned(), "task-1".to_owned());
        let task_b = ("[null,\"b\"]".to_owned(), "task-1".to_owned());
        assert_eq!(load_in(dir.path(), &task_a), Some(TaskSignals::default()));
        record_in(dir.path(), &task_a, Signal::Bounce).expect("record");
        record_in(dir.path(), &task_a, Signal::Bounce).expect("record");
        record_in(dir.path(), &task_a, Signal::EditFailure).expect("record");
        let a = load_in(dir.path(), &task_a).expect("signals");
        assert_eq!(
            (a.bounce, a.edit_failure, a.expand, a.unattributed),
            (2, 1, 0, 0)
        );
        assert_eq!(
            load_in(dir.path(), &task_b),
            Some(TaskSignals::default()),
            "equal task ids in another project never share signals"
        );

        let path = signals_path(dir.path(), &task_b);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "{broken").expect("damage");
        assert_eq!(
            load_in(dir.path(), &task_b).map(|s| s.unattributed),
            Some(1)
        );
    }

    #[test]
    fn a_task_served_by_two_processes_or_with_a_lost_event_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task = ("[null,\"p\"]".to_owned(), "task-1".to_owned());
        record_as(dir.path(), &task, Signal::Observed, "proc-a").expect("observe");
        record_as(dir.path(), &task, Signal::Observed, "proc-a").expect("observe again");
        assert_eq!(
            load_in(dir.path(), &task).map(|s| s.unattributed),
            Some(0),
            "one process sees every bounce of its task"
        );
        record_as(dir.path(), &task, Signal::Observed, "proc-b").expect("observe");
        assert_eq!(
            load_in(dir.path(), &task).map(|s| s.unattributed),
            Some(1),
            "a second process: bounces across them are invisible"
        );

        let lost = ("[null,\"p\"]".to_owned(), "task-2".to_owned());
        std::fs::create_dir_all(dir.path().join("tasks")).expect("dir");
        std::fs::write(incomplete_marker(&signals_path(dir.path(), &lost)), b"").expect("marker");
        assert_eq!(
            load_in(dir.path(), &lost).map(|s| s.unattributed),
            Some(1),
            "an event that could not be recorded is not a clean task"
        );
    }
}
