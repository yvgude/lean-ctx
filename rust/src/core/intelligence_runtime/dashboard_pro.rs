// SPDX-License-Identifier: Apache-2.0
//! The retired Cockpit Pro capability: the cockpit computes its analyses
//! itself ([`crate::core::dashboard_pro`]); the id stays in the capability
//! sets until hosted issuers stop listing it.

pub(crate) const CAPABILITY_ID: &str = "pro.dashboard.knowledge_workflows";
pub(crate) const CAPABILITY_VERSION: &str = "1.0.0";
