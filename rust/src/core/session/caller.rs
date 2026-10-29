// SPDX-License-Identifier: Apache-2.0
//! The calling agent's own session, for code that runs inside one request.
//!
//! Session continuity — the wake-up briefing, the last task, recent
//! decisions, findings, intents, the last search query — comes only from the
//! caller's own session or from what was explicitly handed off into it. The
//! project's latest saved session is whichever agent saved last: reading it
//! handed one agent's private task to another, and writing it changed another
//! agent's session (AP01 §1/§2, ABNAHME I07/S03).
//!
//! Outside a request (CLI, hooks, background threads) there is no caller
//! session: readers get `None` and writers write nothing.

use std::cell::RefCell;

use super::slot::bound_session;
use super::{SessionState, SharedSession};

tokio::task_local! {
    /// The caller's session for a synchronous handler on the blocking pool,
    /// where the request's task locals do not reach.
    static CALLER: RefCell<Option<SharedSession>>;
}

/// Run `operation` with `session` as the caller's own session.
pub fn scope<R>(session: Option<SharedSession>, operation: impl FnOnce() -> R) -> R {
    CALLER.sync_scope(RefCell::new(session), operation)
}

/// The caller's session instance: the explicit scope, else the instance the
/// running request is bound to.
pub fn current() -> Option<SharedSession> {
    CALLER
        .try_with(|caller| caller.borrow().clone())
        .ok()
        .flatten()
        .or_else(bound_session)
}

/// Read the caller's own session. `None` without a caller, or while the
/// session is being written (never waits: callers hold no ordering on it).
pub fn with<R>(read: impl FnOnce(&SessionState) -> R) -> Option<R> {
    let session = current()?;
    let guard = session.try_read().ok()?;
    Some(read(&guard))
}

/// [`with`] restricted to a caller whose session belongs to `project_root`.
pub fn with_for_project<R>(project_root: &str, read: impl FnOnce(&SessionState) -> R) -> Option<R> {
    with(|session| belongs_to(session, project_root).then(|| read(session))).flatten()
}

/// Change the caller's own session in memory; the server's save cadence
/// persists it. `None` without a caller or while the session is locked.
pub fn with_mut<R>(write: impl FnOnce(&mut SessionState) -> R) -> Option<R> {
    let session = current()?;
    let mut guard = session.try_write().ok()?;
    Some(write(&mut guard))
}

fn belongs_to(session: &SessionState, project_root: &str) -> bool {
    let Some(own) = session.project_root.as_deref() else {
        return false;
    };
    own == project_root
        || crate::core::project_hash::hash_project_root(own)
            == crate::core::project_hash::hash_project_root(project_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(root: &str) -> SharedSession {
        let mut session = SessionState::new();
        session.project_root = Some(root.to_string());
        std::sync::Arc::new(tokio::sync::RwLock::new(session))
    }

    /// Outside a request nothing is "own": a CLI or hook process must never
    /// fall back to a saved session. Inside, only the caller's session of the
    /// asked project is.
    #[test]
    fn only_the_scoped_caller_session_of_the_project_is_own() {
        assert!(with(|session| session.id.clone()).is_none());
        assert!(with_mut(|session| session.id.clone()).is_none());

        let session = shared("/work/p1");
        let expected = session.try_read().unwrap().id.clone();
        let own = scope(Some(session.clone()), || {
            with_for_project("/work/p1", |s| s.id.clone())
        });
        assert_eq!(own, Some(expected));
        assert!(
            scope(Some(session), || with_for_project("/work/p2", |_| ())).is_none(),
            "a caller session of another project is not this project's own"
        );
    }
}
