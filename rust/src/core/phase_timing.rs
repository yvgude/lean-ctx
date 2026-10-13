// SPDX-License-Identifier: Apache-2.0
//! Named phases of a long tool call (#2063).
//!
//! `LEAN_CTX_PHASE_TIMING=1` prints each phase's duration to stderr, so a slow
//! call can be attributed without a debugger. Independently, the phase that is
//! still running is kept, so `lean-ctx call` can say where it is stuck instead
//! of going silent.

use std::sync::Mutex;
use std::time::{Duration, Instant};

static CURRENT: Mutex<Option<(&'static str, Instant)>> = Mutex::new(None);

/// Marks `name` as running until the guard drops.
#[must_use = "the phase ends when the guard drops"]
pub(crate) fn enter(name: &'static str) -> PhaseGuard {
    let started = Instant::now();
    let outer = CURRENT
        .lock()
        .map(|mut slot| slot.replace((name, started)))
        .unwrap_or(None);
    PhaseGuard {
        name,
        started,
        outer,
    }
}

/// The innermost phase still running and how long it has run.
pub(crate) fn current() -> Option<(&'static str, Duration)> {
    CURRENT
        .lock()
        .ok()
        .and_then(|slot| slot.map(|(name, started)| (name, started.elapsed())))
}

pub(crate) struct PhaseGuard {
    name: &'static str,
    started: Instant,
    outer: Option<(&'static str, Instant)>,
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = CURRENT.lock() {
            *slot = self.outer;
        }
        if std::env::var_os("LEAN_CTX_PHASE_TIMING").is_some_and(|v| v == "1") {
            eprintln!("[phase] {} {:?}", self.name, self.started.elapsed());
        }
    }
}
