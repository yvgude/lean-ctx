// SPDX-License-Identifier: Apache-2.0
use super::held_horizon_support::{HorizonClock, MINIMAL_PACK, Phase};
use super::{PublicationAuthority, REQUEST_PROJECT, TestPolicyOverride};
use chrono::Utc;
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

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
