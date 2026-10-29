// SPDX-License-Identifier: Apache-2.0
//! Keeps this process's task claims alive: one low-priority thread renews
//! each claim once half of its minutes have run out, and ends when the
//! process holds no claim any more. A claim the process stops renewing
//! (exit, crash, offline) ends at its expiry in Hive.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const PASS: Duration = Duration::from_secs(30);

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Start the renew thread unless it runs already.
pub(super) fn ensure_running() {
    if cfg!(test)
        || RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("hive-task-claims".into())
        .spawn(|| {
            loop {
                std::thread::sleep(PASS);
                if super::renew_own_due() == 0 {
                    RUNNING.store(false, Ordering::Release);
                    // A claim made between the count and the store would
                    // wait for a thread that just ended: take it up again.
                    if super::own_claims() == 0
                        || RUNNING
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_err()
                    {
                        return;
                    }
                }
            }
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Release);
    }
}
