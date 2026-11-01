// SPDX-License-Identifier: Apache-2.0
//! Named phases of a long tool call (#2063).
//!
//! `LEAN_CTX_PHASE_TIMING=1` prints each phase's start and duration to stderr,
//! so a slow call can be attributed without a debugger — also a call that
//! never finishes, whose phases would otherwise print nothing. Independently,
//! the phases that are still running are kept, so `lean-ctx call` can say
//! where it is stuck instead of going silent.
//!
//! Each guard owns its own entry, so concurrent calls (the MCP server runs many
//! at once) cannot overwrite or resurrect each other's phase when their guards
//! drop out of order.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static RUNNING: Mutex<BTreeMap<u64, (&'static str, Instant)>> = Mutex::new(BTreeMap::new());

/// Whether `LEAN_CTX_PHASE_TIMING=1` asks for phase lines on stderr.
pub(crate) fn enabled() -> bool {
    std::env::var_os("LEAN_CTX_PHASE_TIMING").is_some_and(|v| v == "1")
}

/// Marks `name` as running until the guard drops.
#[must_use = "the phase ends when the guard drops"]
pub(crate) fn enter(name: &'static str) -> PhaseGuard {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    if let Ok(mut running) = RUNNING.lock() {
        running.insert(id, (name, started));
    }
    if enabled() {
        eprintln!("[phase] {name} started");
    }
    PhaseGuard { id, name, started }
}

/// Reports one slow step inside `phase` (one source of a scan, say) when
/// phase timing is on. `detail` names the step; it goes to stderr only.
pub(crate) fn note_slow(phase: &'static str, detail: &str, spent: Duration) {
    if enabled() {
        eprintln!("[phase] {phase} slow step {spent:?}: {detail}");
    }
}

/// The most recently entered phase still running and how long it has run.
pub(crate) fn current() -> Option<(&'static str, Duration)> {
    RUNNING.lock().ok().and_then(|running| {
        running
            .last_key_value()
            .map(|(_, (name, started))| (*name, started.elapsed()))
    })
}

pub(crate) struct PhaseGuard {
    id: u64,
    name: &'static str,
    started: Instant,
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        if let Ok(mut running) = RUNNING.lock() {
            running.remove(&self.id);
        }
        if enabled() {
            eprintln!("[phase] {} {:?}", self.name, self.started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_names() -> Vec<&'static str> {
        RUNNING
            .lock()
            .map(|running| running.values().map(|(name, _)| *name).collect())
            .unwrap_or_default()
    }

    #[test]
    fn out_of_order_drops_never_resurrect_a_finished_phase() {
        let first = enter("phase-test: first call");
        let second = enter("phase-test: second call");
        drop(first);
        assert!(running_names().contains(&"phase-test: second call"));
        assert!(!running_names().contains(&"phase-test: first call"));
        drop(second);
        let names = running_names();
        assert!(!names.contains(&"phase-test: first call"));
        assert!(!names.contains(&"phase-test: second call"));
    }
}
