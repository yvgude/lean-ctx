// SPDX-License-Identifier: Apache-2.0

//! External-source compatibility fixture for the V4 policy migration.

use lean_ctx::core::context_kernel::policy::{ContextPolicy, PolicyFilter};
use lean_ctx::core::policy::{PolicyError, PolicyPack};

#[allow(deprecated)]
use lean_ctx::core::context_kernel::policy_engine::PolicyDecisionPoint;

#[test]
fn legacy_and_canonical_policy_imports_compile_during_deprecation() {
    let _candidate_filter = PolicyFilter::new(ContextPolicy::default());
    let _parse_policy_pack: fn(&str) -> Result<PolicyPack, PolicyError> =
        lean_ctx::core::policy::parse;

    #[allow(deprecated)]
    let _legacy_compatibility_surface = PolicyDecisionPoint::new(Vec::new());
}
