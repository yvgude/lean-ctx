// SPDX-License-Identifier: Apache-2.0
//! Recovery references of a delivery, for its trace sidecar (G08).
//!
//! A recovery reference is the id a host passes back to expand a block that
//! was compressed or cut down — an address, never content. This process
//! notes every id at the moment a recovery copy is stored under it
//! (`core::archive::recovery::store_recovery_with_lineage`). A delivery's
//! sidecar then lists the noted ids that the delivered bytes actually carry:
//! a reference the host was not handed is not one it can pass back.
//!
//! Bounded on both sides: the newest [`MAX_ISSUED`] ids of the process are
//! remembered, a sidecar lists at most [`MAX_RECOVERY_REFERENCES`] and counts
//! the rest.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, PoisonError};

const MAX_ISSUED: usize = 1024;
pub(crate) const MAX_RECOVERY_REFERENCES: usize = 32;
const ID_LEN: usize = 64;

fn issued() -> &'static Mutex<VecDeque<String>> {
    static ISSUED: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    ISSUED.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn is_id_byte(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

/// A recovery copy was stored under `id` by this process. Anything that is
/// not a canonical id (64 lowercase hex characters) is ignored.
pub(crate) fn note_recovery(id: &str) {
    if id.len() != ID_LEN || !id.bytes().all(is_id_byte) {
        return;
    }
    let mut issued = issued().lock().unwrap_or_else(PoisonError::into_inner);
    if issued.iter().any(|known| known == id) {
        return;
    }
    if issued.len() == MAX_ISSUED {
        issued.pop_front();
    }
    issued.push_back(id.to_owned());
}

/// The recovery ids this process issued that `bytes` carry, in the order of
/// their first occurrence, and how many more than the bound there are. One
/// pass over the bytes: only a run of exactly 64 id characters is looked up.
pub(super) fn recovery_in(bytes: &[u8]) -> (Vec<String>, u32) {
    let issued = issued().lock().unwrap_or_else(PoisonError::into_inner);
    let mut found: Vec<String> = Vec::new();
    let mut omitted = 0_u32;
    if issued.is_empty() {
        return (found, omitted);
    }
    let mut start = 0;
    while start < bytes.len() {
        if !is_id_byte(bytes[start]) {
            start += 1;
            continue;
        }
        let run = bytes[start..]
            .iter()
            .take_while(|byte| is_id_byte(**byte))
            .count();
        if run == ID_LEN
            && let Ok(candidate) = std::str::from_utf8(&bytes[start..start + run])
            && issued.iter().any(|known| known == candidate)
            && !found.iter().any(|known| known == candidate)
        {
            if found.len() < MAX_RECOVERY_REFERENCES {
                found.push(candidate.to_owned());
            } else {
                omitted = omitted.saturating_add(1);
            }
        }
        start += run;
    }
    (found, omitted)
}
