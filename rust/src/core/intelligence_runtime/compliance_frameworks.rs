// SPDX-License-Identifier: Apache-2.0
//! Enterprise framework coverage over the one engine: the engine sends the
//! facts of [`crate::core::compliance_evidence::facts`], the licensed runtime
//! answers with its framework assessment, and the engine checks, signs and
//! renders it. The mappings never reach this tree.
use super::InstallError;
use crate::core::compliance_evidence::{facts::ComplianceFactsV1, frameworks::FrameworksAnswer};

pub(crate) const CAPABILITY_ID: &str = "enterprise.runtime.compliance_frameworks";
pub(crate) const CAPABILITY_VERSION: &str = "1.0.0";

const CAPABILITY: super::exchange::Capability = super::exchange::Capability {
    id: CAPABILITY_ID,
    version: CAPABILITY_VERSION,
    declared: super::health::supports_compliance_frameworks,
    deadline_ms: 10_000,
    tag: "compliance",
};

/// What a Community install tells the operator instead of a framework report.
pub(crate) const ENTERPRISE_HINT: &str = "framework coverage (EU AI Act, ISO 42001, SOC 2) is \
     part of LeanCTX Enterprise and needs the licensed runtime; `lean-ctx compliance facts` \
     shows what it is assessed on, `lean-ctx compliance verify` works without a license";

/// Ask the licensed runtime for the framework reports of `facts`. The caller
/// checks the answer against the request ([`FrameworksAnswer::check`]).
pub(crate) fn assess(facts: &ComplianceFactsV1) -> Result<FrameworksAnswer, String> {
    let input = serde_json::to_string(facts).map_err(|e| format!("compliance facts: {e}"))?;
    match super::exchange::invoke(&CAPABILITY, &input) {
        Ok(output) => serde_json::from_str(&output)
            .map_err(|_| "compliance runtime answer refused: not a frameworks answer".to_string()),
        Err(InstallError::Policy | InstallError::Manifest) => Err(ENTERPRISE_HINT.to_string()),
        Err(error) => Err(format!(
            "compliance runtime failed ({error}); an active LeanCTX Enterprise lease is required"
        )),
    }
}
