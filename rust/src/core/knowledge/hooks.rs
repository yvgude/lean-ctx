// SPDX-License-Identifier: Apache-2.0
//! Knowledge bookkeeping the Cloud connector keeps: how often its facts
//! reached an agent (M1, M5) and the session learnings it queues for review
//! at a session's end. The connector registers [`KnowledgeHooks`] at
//! start-up ([`register_knowledge_hooks`]); without them (library use,
//! tests) nothing is counted or queued. Neither hook decides what an agent
//! is shown.

use std::sync::OnceLock;

use lean_ctx_time::UtcDateTime;

/// `(project_root, session (id, started_at), cloud_facts, asked)`.
pub type DeliveryHook = fn(&str, Option<(&str, UtcDateTime)>, usize, bool);
/// `(project_root, session_id)`.
pub type SessionEndHook = fn(&str, &str);

/// The registered knowledge bookkeeping.
#[derive(Clone, Copy)]
pub struct KnowledgeHooks {
    pub record_delivery: DeliveryHook,
    pub session_ended: SessionEndHook,
}

static HOOKS: OnceLock<KnowledgeHooks> = OnceLock::new();

/// Register the knowledge hooks. The first registration wins.
pub fn register_knowledge_hooks(hooks: KnowledgeHooks) {
    let _ = HOOKS.set(hooks);
}

/// A delivery to an agent in `project_root` handed it `cloud_facts` facts
/// pulled from a Cloud vault. `asked`: the agent asked for knowledge (a
/// recall).
pub fn record_delivery(
    project_root: &str,
    session: Option<(&str, UtcDateTime)>,
    cloud_facts: usize,
    asked: bool,
) {
    if let Some(hooks) = HOOKS.get() {
        (hooks.record_delivery)(project_root, session, cloud_facts, asked);
    }
}

/// A host session in `project_root` ended. Enqueues only, never network,
/// never an error for the caller.
pub fn session_ended(project_root: &str, session_id: &str) {
    if let Some(hooks) = HOOKS.get() {
        (hooks.session_ended)(project_root, session_id);
    }
}
