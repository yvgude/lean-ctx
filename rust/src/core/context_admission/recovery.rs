// SPDX-License-Identifier: Apache-2.0
//! Recovery is re-authorized (G5, plan cases 29–36).
//!
//! Stored output (tee files, archives, reference results, the session
//! cache) was admitted when it was stored, under the policy of that moment.
//! Handing it back later is a new delivery: every recovery path passes the
//! stored text through the current policy again, at the store's read choke
//! point, before any selector (head, tail, search, range) sees it.
//!
//! - Masks applied at storage time stay masked; re-admission is idempotent.
//! - A policy tightened since storage applies to the recovered text.
//! - Content the current policy withholds is refused with a content-free
//!   reason, never returned partially.
//! - The decision is recorded in the current call's receipt like any read.

/// Re-admit stored text that is about to be handed back. `origin` names the
/// store (`"archive"`, `"tee"`, `"reference"`, `"session-cache"`); it never
/// carries content. The error is content-free and names the reason codes.
pub fn admit_recovered(text: &str, origin: &str) -> Result<String, String> {
    super::admit_source(text, &format!("recovery:{origin}"), false)
        .map_err(|error| format!("stored output withheld by the context gateway: {error}"))
}

/// Admit tool output before it is persisted to a recovery store. `None`:
/// it must not be stored (withheld, or restricted — owner decision E3).
#[must_use]
pub fn admit_for_storage(text: &str) -> Option<String> {
    super::stores::StoreAdmission::current().admit_text(text)
}
