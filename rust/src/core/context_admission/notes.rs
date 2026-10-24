// SPDX-License-Identifier: Apache-2.0
//! Bounded hand-off of HUD notes from the read choke point to the tool that
//! renders the admitted content. Keyed by the admitted bytes, so concurrent
//! reads can never attach one file's note to another and the note is a pure
//! function of the content (#498). The note keeps the content-free counts, not
//! a rendered line, so a tool that returns only part of the admitted text can
//! state what that part contains (#2135).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, PoisonError};

use lean_ctx_protocol::context_gateway::ContextDecisionV1;

use super::AdmissionCounts;

const CAPACITY: usize = 512;

type Key = [u8; 32];

/// What the gateway did to one admitted text, without any of its values.
#[derive(Debug, Clone)]
pub(super) struct Note {
    pub(super) counts: AdmissionCounts,
    pub(super) decision: ContextDecisionV1,
    pub(super) redaction_counts: bool,
}

fn store() -> &'static Mutex<VecDeque<(Key, Note)>> {
    static STORE: OnceLock<Mutex<VecDeque<(Key, Note)>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

fn key(admitted: &str) -> Key {
    *blake3::hash(admitted.as_bytes()).as_bytes()
}

pub(super) fn record(admitted: &str, note: Note) {
    let key = key(admitted);
    let mut notes = store().lock().unwrap_or_else(PoisonError::into_inner);
    notes.retain(|(existing, _)| *existing != key);
    if notes.len() == CAPACITY {
        notes.pop_front();
    }
    notes.push_back((key, note));
}

pub(super) fn lookup(admitted: &str) -> Option<Note> {
    let key = key(admitted);
    store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .rev()
        .find(|(existing, _)| *existing == key)
        .map(|(_, note)| note.clone())
}
