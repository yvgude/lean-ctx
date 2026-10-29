// SPDX-License-Identifier: Apache-2.0
//! The live session of one MCP server and the instance a request is bound to.
//!
//! A host may change its workspace roots while a slow call is still running.
//! Replacing the session *contents* in place let that call's late writes —
//! receipts, intents, touched files, the batch save — land in the session of
//! the project the host switched to. A root change therefore installs a new
//! session instance, and a request keeps the instance it was bound to: late
//! results belong to the original request and its project (AP01 §3).

use std::cell::RefCell;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use super::SessionState;

pub type SharedSession = Arc<RwLock<SessionState>>;
type LiveCell = Arc<std::sync::RwLock<SharedSession>>;

tokio::task_local! {
    /// The session instance the current request is bound to, with the slot it
    /// was taken from. Empty until the request pins it.
    static BOUND: RefCell<Option<(LiveCell, SharedSession)>>;
}

/// The session instance the running request is bound to, whichever server's
/// slot it came from. `None` outside a bound request.
pub(super) fn bound_session() -> Option<SharedSession> {
    BOUND
        .try_with(|bound| {
            bound
                .borrow()
                .as_ref()
                .map(|(_, session)| Arc::clone(session))
        })
        .ok()
        .flatten()
}

/// Shared between all clones of one server; the live instance is replaced only
/// on a project change.
#[derive(Clone)]
pub struct SessionSlot {
    live: LiveCell,
    /// Per project root, the session this server worked in there before it
    /// switched away: the one saved session it may take up again. Any other
    /// saved session of a project is another agent's private state (AP01 §1).
    own: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl SessionSlot {
    pub fn new(session: SharedSession) -> Self {
        Self {
            live: Arc::new(std::sync::RwLock::new(session)),
            own: Arc::default(),
        }
    }

    /// Record that this server's session for `project_root` is `session_id`.
    pub(crate) fn remember_own(&self, project_root: &str, session_id: &str) {
        self.own
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_root.to_owned(), session_id.to_owned());
    }

    /// The session this server left `project_root` with, if any.
    pub(crate) fn own_session_id(&self, project_root: &str) -> Option<String> {
        self.own
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned()
    }

    /// The instance new requests bind to, ignoring any request binding.
    pub fn live(&self) -> SharedSession {
        Arc::clone(
            &self
                .live
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// The instance this request is bound to, or the live one outside a
    /// bound request.
    pub fn instance(&self) -> SharedSession {
        BOUND
            .try_with(|bound| {
                bound
                    .borrow()
                    .as_ref()
                    .filter(|(slot, _)| Arc::ptr_eq(slot, &self.live))
                    .map(|(_, session)| Arc::clone(session))
            })
            .ok()
            .flatten()
            .unwrap_or_else(|| self.live())
    }

    pub async fn read(&self) -> OwnedRwLockReadGuard<SessionState> {
        self.instance().read_owned().await
    }

    pub async fn write(&self) -> OwnedRwLockWriteGuard<SessionState> {
        self.instance().write_owned().await
    }

    /// Install `next` as the live instance if `current` still is it. Requests
    /// already bound keep the previous one. `false`: another switch won.
    pub fn replace_if_live(&self, current: &SharedSession, next: SharedSession) -> bool {
        let mut live = self
            .live
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !Arc::ptr_eq(&live, current) {
            return false;
        }
        *live = next;
        true
    }

    /// Move the current request's binding to `next`: a request that switched
    /// the session itself continues in the new one. No-op outside a bound
    /// request.
    pub fn rebind(&self, next: SharedSession) {
        let _ = BOUND.try_with(|bound| {
            if let Some((slot, session)) = bound.borrow_mut().as_mut()
                && Arc::ptr_eq(slot, &self.live)
            {
                *session = next;
            }
        });
    }

    /// Run one request with an empty binding; [`Self::pin`] fills it.
    ///
    /// A request that ends bound to an instance no longer live saves its late
    /// changes there, under that session's own project: the batch cadence
    /// that would otherwise pick them up only runs on the live instance.
    pub async fn scope<F: Future>(&self, request: F) -> F::Output {
        let (output, bound) = BOUND
            .scope(RefCell::new(None), async {
                let output = request.await;
                let bound = BOUND.with(|bound| bound.borrow_mut().take());
                (output, bound)
            })
            .await;
        if let Some((_, session)) = bound
            && !Arc::ptr_eq(&session, &self.live())
        {
            let pending = {
                let state = session.read().await;
                (state.stats.unsaved_changes > 0).then(|| state.project_root.clone())
            };
            if let Some(project) = pending {
                crate::core::policy::runtime::REQUEST_PROJECT
                    .scope(
                        RefCell::new(project.map(std::path::PathBuf::from)),
                        SessionState::save_shared_logged(session),
                    )
                    .await;
            }
        }
        output
    }

    /// Bind the current request to the live instance once its roots are
    /// resolved. A request keeps its first binding; outside [`Self::scope`]
    /// this is a no-op.
    pub fn pin(&self) {
        let _ = BOUND.try_with(|bound| {
            let mut bound = bound.borrow_mut();
            if bound.is_none() {
                *bound = Some((Arc::clone(&self.live), self.live()));
            }
        });
    }
}
