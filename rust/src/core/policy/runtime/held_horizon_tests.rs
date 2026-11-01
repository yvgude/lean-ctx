// SPDX-License-Identifier: Apache-2.0
use super::{PublicationAuthority, REQUEST_PROJECT, TestPolicyOverride, refresh};
use crate::core::policy::org::Lifetime;
use chrono::{DateTime, Utc};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub(crate) const MINIMAL_PACK: &str =
    "name = 'held-horizon'\nversion = '1.0.0'\ndescription = 'test'\n";

#[derive(Clone, Copy)]
pub(crate) enum Phase {
    Open,
    Closing,
    Fresh,
}

pub(crate) fn wait_until_horizon(horizon: DateTime<Utc>) {
    let cap = Instant::now() + Duration::from_secs(10);
    while Utc::now() < horizon {
        assert!(Instant::now() < cap, "the original horizon did not arrive");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The signed horizon every verified answer on this thread states, the phase
/// that decides how it answers, and how often each phase was asked. Shared by
/// the source-view test here and the proxy's admitted-send test.
pub(crate) struct HorizonClock {
    pub(crate) h: Rc<Cell<DateTime<Utc>>>,
    pub(crate) phase: Rc<Cell<Phase>>,
    pub(crate) open_calls: Rc<Cell<u32>>,
    pub(crate) closing_calls: Rc<Cell<u32>>,
    pub(crate) fresh_calls: Rc<Cell<u32>>,
}

impl HorizonClock {
    pub(crate) fn new() -> Self {
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
    pub(crate) fn arm(&self, deadline: Instant) -> impl Drop {
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

#[test]
fn the_original_horizon_passing_in_the_closing_refresh_withholds_the_result_despite_a_later_fresh_answer()
 {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(Some(crate::core::policy::load(MINIMAL_PACK).unwrap()));
    let root = tempfile::tempdir().unwrap();
    let project = crate::core::pathutil::canonicalize_secure(root.path()).unwrap();
    let clock = HorizonClock::new();
    let deadline = Instant::now() + Duration::from_mins(1);
    let _hook = clock.arm(deadline);

    REQUEST_PROJECT.sync_scope(RefCell::new(Some(project)), || {
        let mut positive_authority = None;
        let positive = super::with_source_view(|| {
            positive_authority = Some(PublicationAuthority::capture().unwrap());
            clock.phase.set(Phase::Fresh);
            42
        });
        assert_eq!(positive.unwrap(), 42);
        positive_authority.unwrap().verify().unwrap();
        assert!(clock.open_calls.get() >= 1);
        assert!(clock.fresh_calls.get() >= 1);

        clock.h.set(Utc::now() + chrono::Duration::seconds(3));
        clock.phase.set(Phase::Open);
        clock.closing_calls.set(0);
        let operation_calls = Cell::new(0_u32);
        let mut held_authority = None;
        let negative = super::with_source_view(|| {
            held_authority = Some(PublicationAuthority::capture().unwrap());
            operation_calls.set(operation_calls.get() + 1);
            clock.phase.set(Phase::Closing);
            42
        });
        assert_eq!(
            negative.unwrap_err(),
            "source policy view changed during operation"
        );
        assert_eq!(operation_calls.get(), 1);
        assert!(clock.closing_calls.get() >= 1);
        assert!(Utc::now() >= clock.h.get());
        assert!(held_authority.unwrap().verify().is_err());
    });
}
