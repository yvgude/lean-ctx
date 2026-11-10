// SPDX-License-Identifier: Apache-2.0
//! The session taint (design pz21 §1.7): which untrusted origins the session
//! the Instance serves has read.
//!
//! - **Writer:** only the origin step of the release point, from the call's
//!   origin ledger ([`record`]). Admissions outside a tool call (egress
//!   checks, detached store writes) never taint.
//! - **Active session:** the MCP server binds the session it serves at the
//!   start of every call ([`bind`]); [`active_view`] is what
//!   `origin::session_taint()` returns. A taint recorded before the first
//!   bind is held without a session id and carried into the bound session.
//! - **Write-through:** a change is saved at once to a sidecar next to the
//!   session file (`<id>.taint`), not on the session's save cadence, so a
//!   crash between the read and the next save cannot untaint the session.
//!   Loading a session merges the sidecar into `SessionState.taint`.
//! - **Overflow is sticky:** nothing is dropped silently; a ref that cannot
//!   be recorded marks the whole session, and so does a change the bounded
//!   lock could not take in time.
//! - **Locking:** `ACTIVE` is a bounded lock (LOCK_ORDERING L148), held
//!   only to merge or copy the in-memory taint; sidecar reads and writes run
//!   after it is released.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use tokio::sync::{OwnedRwLockWriteGuard, RwLock};

use crate::core::bounded_lock;
use crate::core::context_admission::origin::{InputTaint, TaintView};

#[derive(Default)]
struct Active {
    /// Empty while no session is bound (a CLI run, a test, the first call).
    session_id: String,
    taint: InputTaint,
    /// Bumped on every change, so a sidecar write that raced a newer change
    /// writes again.
    generation: u64,
}

static ACTIVE: LazyLock<Arc<RwLock<Option<Active>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(None)));

/// A taint change was lost to a lock timeout: every view of the taint reads
/// as overflowed for the rest of the process (fails closed).
static UNRECORDED: AtomicBool = AtomicBool::new(false);

/// The lock is held for an in-memory merge only; a wait this long means
/// something is wrong, and the taint fails closed instead of queueing.
const LOCK_BUDGET: Duration = Duration::from_secs(2);

fn write_guard() -> Option<OwnedRwLockWriteGuard<Option<Active>>> {
    let guard = bounded_lock::write_for(&ACTIVE, "session taint", LOCK_BUDGET);
    if guard.is_none() {
        UNRECORDED.store(true, Ordering::SeqCst);
    }
    guard
}

fn with_unrecorded(mut taint: InputTaint) -> InputTaint {
    if UNRECORDED.load(Ordering::SeqCst) {
        taint.mark_overflow();
    }
    taint
}

/// Bind the session the Instance serves now. A different session replaces
/// the active taint with its own (loaded with the session); the same
/// session keeps the union, and a taint recorded before any session was
/// bound is carried into it and saved, so nothing recorded is ever lost.
pub fn bind(session_id: &str, taint: &InputTaint) {
    let save = {
        let Some(mut active) = write_guard() else {
            return;
        };
        match active.as_mut() {
            Some(current) if current.session_id == session_id => {
                if !current.taint.merge(taint) {
                    return;
                }
                current.generation += 1;
                Some((current.taint.clone(), current.generation))
            }
            _ => None,
        }
    };
    if let Some((taint, generation)) = save {
        persist(session_id, taint, generation);
        return;
    }
    // Another (or no) session was bound: read its sidecar without the lock.
    let mut stored = load_sidecar(session_id).unwrap_or_default();
    stored.merge(taint);
    let save = {
        let Some(mut active) = write_guard() else {
            return;
        };
        let mut carried = false;
        let mut generation = 1;
        if let Some(previous) = active.take()
            && (previous.session_id.is_empty() || previous.session_id == session_id)
        {
            // Recorded before this bind (no session id yet), or a bind of the
            // same session that won the race: keep it.
            carried = stored.merge(&previous.taint);
            generation = previous.generation + 1;
        }
        *active = Some(Active {
            session_id: session_id.to_owned(),
            taint: stored.clone(),
            generation,
        });
        carried.then_some((stored, generation))
    };
    if let Some((taint, generation)) = save {
        persist(session_id, taint, generation);
    }
}

/// Add `delta` to the active session's taint and persist a change at once.
/// Without a bound session the taint is held process-wide (a CLI run, a
/// test) and carried into the session bound next.
pub fn record(delta: &InputTaint) {
    if !delta.is_tainted() {
        return;
    }
    let save = {
        let Some(mut active) = write_guard() else {
            return;
        };
        let current = active.get_or_insert_with(Active::default);
        if !current.taint.merge(delta) {
            return;
        }
        current.generation += 1;
        (!current.session_id.is_empty()).then(|| {
            (
                current.session_id.clone(),
                current.taint.clone(),
                current.generation,
            )
        })
    };
    if let Some((session_id, taint, generation)) = save {
        persist(&session_id, taint, generation);
    }
}

/// Write `taint` (of change `generation`) to the sidecar, outside the lock;
/// when a newer change of the same session landed meanwhile, write that
/// too, so the last write on disk is never older than the last change.
fn persist(session_id: &str, mut taint: InputTaint, mut generation: u64) {
    loop {
        save_sidecar(session_id, &taint);
        let Some(active) = bounded_lock::read_for(&ACTIVE, "session taint", LOCK_BUDGET) else {
            UNRECORDED.store(true, Ordering::SeqCst);
            return;
        };
        match active.as_ref() {
            Some(current)
                if current.session_id == session_id && current.generation > generation =>
            {
                taint = current.taint.clone();
                generation = current.generation;
            }
            _ => return,
        }
    }
}

/// The active session's taint, read-only.
pub fn active_view() -> TaintView {
    snapshot().view()
}

/// The active session's taint. A lock timeout reads as overflowed.
pub fn snapshot() -> InputTaint {
    let Some(active) = bounded_lock::read_for(&ACTIVE, "session taint", LOCK_BUDGET) else {
        UNRECORDED.store(true, Ordering::SeqCst);
        return with_unrecorded(InputTaint::default());
    };
    with_unrecorded(
        active
            .as_ref()
            .map(|active| active.taint.clone())
            .unwrap_or_default(),
    )
}

/// The id of the session bound last, if any.
pub fn active_session_id() -> Option<String> {
    bounded_lock::read_for(&ACTIVE, "session taint", LOCK_BUDGET)?
        .as_ref()
        .map(|active| active.session_id.clone())
        .filter(|id| !id.is_empty())
}

/// The taint of `session_id` when it is the active session.
pub fn snapshot_for(session_id: &str) -> Option<InputTaint> {
    let taint = bounded_lock::read_for(&ACTIVE, "session taint", LOCK_BUDGET)?
        .as_ref()
        .filter(|active| active.session_id == session_id)
        .map(|active| active.taint.clone())?;
    Some(with_unrecorded(taint))
}

/// Forget the active session (tests; a server shutting down). Persisted
/// taints stay on disk.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_active() {
    if let Some(mut active) = bounded_lock::write_for(&ACTIVE, "session taint", LOCK_BUDGET) {
        *active = None;
    }
    UNRECORDED.store(false, Ordering::SeqCst);
}

fn sidecar_path(session_id: &str) -> Option<std::path::PathBuf> {
    let valid = !session_id.is_empty()
        && !session_id.starts_with('.')
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if !valid {
        return None;
    }
    super::paths::sessions_dir().map(|dir| dir.join(format!("{session_id}.taint")))
}

/// The persisted taint of `session_id`, merged into the session on load.
pub(crate) fn load_sidecar(session_id: &str) -> Option<InputTaint> {
    let path = sidecar_path(session_id)?;
    let json = std::fs::read_to_string(path).ok()?;
    // A sidecar that does not parse fails closed: the session is tainted.
    Some(serde_json::from_str(&json).unwrap_or_else(|_| InputTaint {
        overflow: true,
        ..InputTaint::default()
    }))
}

fn save_sidecar(session_id: &str, taint: &InputTaint) {
    let Some(path) = sidecar_path(session_id) else {
        return;
    };
    let Ok(json) = serde_json::to_string(taint) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = crate::core::atomic_fs::try_atomic_write(&path, json.as_bytes(), None) {
        tracing::warn!("session taint not persisted: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_admission::origin::{InputKind, InputRef};

    #[test]
    fn taint_is_written_through_and_survives_a_rebind() {
        let _data = crate::core::data_dir::isolated_data_dir();
        reset_active();
        bind("20261106-000000u000000p1s0", &InputTaint::default());
        let mut delta = InputTaint::default();
        delta.add(InputRef::new(InputKind::Issue, "github:o/r#1"));
        delta.mark_overflow();
        record(&delta);
        assert!(active_view().overflow());
        // A process restart: the active binding is gone, the sidecar is not.
        reset_active();
        assert!(!active_view().is_tainted());
        bind("20261106-000000u000000p1s0", &InputTaint::default());
        let view = active_view();
        assert!(view.overflow());
        assert!(view.contains(&InputRef::new(InputKind::Issue, "github:o/r#1").unwrap()));
        // Another session starts untainted.
        bind("20261106-000000u000000p1s1", &InputTaint::default());
        assert!(!active_view().is_tainted());
        reset_active();
    }

    /// Review 22480 finding 3: a taint recorded before the first `bind` was
    /// held under an empty session id, and `bind` replaced it with the
    /// sidecar, so the session forgot what it had read.
    #[test]
    fn taint_recorded_before_the_first_bind_is_kept_and_saved() {
        let _data = crate::core::data_dir::isolated_data_dir();
        reset_active();
        let issue = InputRef::new(InputKind::Issue, "github:o/r#7").unwrap();
        let mut delta = InputTaint::default();
        delta.add(Some(issue.clone()));
        record(&delta);
        bind("20261106-000000u000000p1s7", &InputTaint::default());
        assert!(active_view().contains(&issue), "lost on bind");
        // Saved: a restart reads it back from the sidecar.
        reset_active();
        bind("20261106-000000u000000p1s7", &InputTaint::default());
        assert!(active_view().contains(&issue), "not written through");
        reset_active();
    }
}
