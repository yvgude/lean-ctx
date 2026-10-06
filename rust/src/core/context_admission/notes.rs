// SPDX-License-Identifier: Apache-2.0
//! Bounded hand-off of HUD lines from the read choke point to the tool that
//! renders the admitted content. Keyed by the admitted bytes, so concurrent
//! reads can never attach one file's note to another and the note is a pure
//! function of the content (#498).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, PoisonError};

const CAPACITY: usize = 512;

type Key = [u8; 32];

fn store() -> &'static Mutex<VecDeque<(Key, String)>> {
    static STORE: OnceLock<Mutex<VecDeque<(Key, String)>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

fn key(admitted: &str) -> Key {
    *blake3::hash(admitted.as_bytes()).as_bytes()
}

pub(super) fn record(admitted: &str, line: String) {
    let key = key(admitted);
    let mut notes = store().lock().unwrap_or_else(PoisonError::into_inner);
    notes.retain(|(existing, _)| *existing != key);
    if notes.len() == CAPACITY {
        notes.pop_front();
    }
    notes.push_back((key, line));
}

pub(super) fn lookup(admitted: &str) -> Option<String> {
    let key = key(admitted);
    store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .rev()
        .find(|(existing, _)| *existing == key)
        .map(|(_, line)| line.clone())
}
