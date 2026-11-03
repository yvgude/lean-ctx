// SPDX-License-Identifier: Apache-2.0
//! The held-horizon clock shared by the source-view tests and the proxy's
//! admitted-send test.
use super::refresh;
use crate::core::policy::org::Lifetime;
use chrono::{DateTime, Utc};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const MINIMAL_PACK: &str = "name = 'held-horizon'\nversion = '1.0.0'\ndescription = 'test'\n";

#[derive(Clone, Copy)]
pub enum Phase {
    Open,
    Closing,
    Fresh,
}

pub fn wait_until_horizon(horizon: DateTime<Utc>) {
    let cap = Instant::now() + Duration::from_secs(10);
    while Utc::now() < horizon {
        assert!(Instant::now() < cap, "the original horizon did not arrive");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The signed horizon every verified answer on this thread states, the phase
/// that decides how it answers, and how often each phase was asked. Shared by
/// the source-view test here and the proxy's admitted-send test.
pub struct HorizonClock {
    pub h: Rc<Cell<DateTime<Utc>>>,
    pub phase: Rc<Cell<Phase>>,
    pub open_calls: Rc<Cell<u32>>,
    pub closing_calls: Rc<Cell<u32>>,
    pub fresh_calls: Rc<Cell<u32>>,
}

impl Default for HorizonClock {
    fn default() -> Self {
        Self::new()
    }
}

impl HorizonClock {
    pub fn new() -> Self {
        Self {
            h: Rc::new(Cell::new(Utc::now() + chrono::Duration::seconds(60))),
            phase: Rc::new(Cell::new(Phase::Open)),
            open_calls: Rc::new(Cell::new(0_u32)),
            closing_calls: Rc::new(Cell::new(0_u32)),
            fresh_calls: Rc::new(Cell::new(0_u32)),
        }
    }

    /// Shapes every verified answer on this thread until the returned guard
    /// drops: `Open` states the original horizon, `Closing` waits for it to
    /// pass and then states a later one, `Fresh` states the later one.
    #[must_use]
    pub fn arm(&self, deadline: Instant) -> impl Drop {
        let h = Rc::clone(&self.h);
        let phase = Rc::clone(&self.phase);
        let open_calls = Rc::clone(&self.open_calls);
        let closing_calls = Rc::clone(&self.closing_calls);
        let fresh_calls = Rc::clone(&self.fresh_calls);
        refresh::answer_hook_for_test(move |lifetime| {
            let original = h.get();
            let later = original + chrono::Duration::hours(1);
            let horizon = match phase.get() {
                Phase::Open => {
                    open_calls.set(open_calls.get() + 1);
                    original
                }
                Phase::Closing => {
                    closing_calls.set(closing_calls.get() + 1);
                    wait_until_horizon(original);
                    later
                }
                Phase::Fresh => {
                    fresh_calls.set(fresh_calls.get() + 1);
                    later
                }
            };
            *lifetime = Some(Lifetime::for_test(Some(horizon), deadline));
        })
    }
}
