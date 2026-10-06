// SPDX-License-Identifier: Apache-2.0
//! Local verification of the shared entitlement wire contract.
//!
//! Issuance and private keys belong to the control plane. A plan name is not
//! authority: paid access requires an independently trusted key, valid binding,
//! current signed time bounds, and the explicit canonical capability ID.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use lean_ctx_protocol::{
    EntitlementDeploymentV1, EntitlementEnvelopeV1, EntitlementPlanV1, MAX_ENTITLEMENT_TIMESTAMP,
};
use sha2::{Digest as _, Sha256};

use super::Plan;

// Registry v2 cutover: 2026-09-13T00:00:00Z. Only already-issued governance
// grants retain the legacy Team floor; renewals use Enterprise classification.
const LEGACY_TEAM_GOVERNANCE_ISSUED_BEFORE: u64 = 1_789_257_600;

/// Public trust material supplied independently of the untrusted envelope.
#[derive(Clone, Debug)]
pub struct EntitlementTrustKey {
    pub key_id: String,
    pub public_key: [u8; 32],
}

/// Explicit caller identity and deployment; verification performs no IO.
#[derive(Clone, Copy, Debug)]
pub struct EntitlementContext<'a> {
    pub account_id: Option<&'a str>,
    pub org_id: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    pub deployment_id: Option<&'a str>,
    pub deployment: EntitlementDeploymentV1,
    pub now: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntitlementValidity {
    Active,
    Grace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntitlementVerificationError {
    InvalidEnvelope,
    UntrustedKey,
    InvalidSignature,
    BindingMismatch,
    NotYetValid,
    Expired,
    InvalidClock,
}

/// Construction is private: deserializing a claim cannot manufacture authority.
#[derive(Clone, Debug)]
pub struct VerifiedEntitlement {
    claims: EntitlementEnvelopeV1,
}

impl VerifiedEntitlement {
    #[must_use]
    pub fn claims(&self) -> &EntitlementEnvelopeV1 {
        &self.claims
    }

    #[must_use]
    pub const fn plan(&self) -> Plan {
        match self.claims.plan {
            EntitlementPlanV1::Community => Plan::Community,
            EntitlementPlanV1::Pro => Plan::Pro,
            EntitlementPlanV1::Team => Plan::Team,
            EntitlementPlanV1::Enterprise => Plan::Enterprise,
        }
    }

    /// Recheck time and scope for each decision, including long-lived callers.
    pub fn validity(
        &self,
        context: EntitlementContext<'_>,
    ) -> Result<EntitlementValidity, EntitlementVerificationError> {
        check_context(&self.claims, context)
    }

    /// The registry resolves aliases, but signed claims contain canonical IDs.
    /// This never expands a signed subset into all capabilities of a plan.
    #[must_use]
    pub fn allows(&self, capability: &str, context: EntitlementContext<'_>) -> bool {
        let registry = crate::core::product_capabilities::registry();
        let Some(entry) = registry.find(capability) else {
            return false;
        };
        if context.deployment != EntitlementDeploymentV1::Hosted && !entry.self_host_available {
            return false;
        }
        if entry.minimum_plan() == Plan::Community {
            return true;
        }
        // Existing signed v1 Team grants survive the classification migration.
        // This exception grants no plan defaults and never extends their expiry.
        let legacy_team_grant = self.plan() == Plan::Team
            && self.claims.issued_at < LEGACY_TEAM_GOVERNANCE_ISSUED_BEFORE
            && matches!(entry.id.as_str(), "team.sso_oidc" | "team.audit_retention");
        let signed_key = if entry.enterprise_entitlement == "none" {
            &entry.id
        } else {
            &entry.enterprise_entitlement
        };
        self.validity(context).is_ok()
            && (registry.allows(self.plan(), &entry.id) || legacy_team_grant)
            && self.claims.capabilities.binary_search(signed_key).is_ok()
    }
}

/// Parse canonical, bounded wire bytes and verify against caller-owned trust.
pub fn verify_entitlement(
    bytes: &[u8],
    trusted_keys: &[EntitlementTrustKey],
    context: EntitlementContext<'_>,
) -> Result<VerifiedEntitlement, EntitlementVerificationError> {
    use EntitlementVerificationError as Error;
    let claims =
        EntitlementEnvelopeV1::from_canonical_bytes(bytes).map_err(|_| Error::InvalidEnvelope)?;
    let mut matching = trusted_keys
        .iter()
        .filter(|key| key.key_id == claims.signer.key_id);
    let trusted = matching.next().ok_or(Error::UntrustedKey)?;
    if matching.next().is_some() {
        return Err(Error::UntrustedKey);
    }
    let digest = format!(
        "sha256:{}",
        crate::core::agent_identity::hex_encode(&Sha256::digest(trusted.public_key))
    );
    if claims.signer.public_key_digest.as_str() != digest {
        return Err(Error::UntrustedKey);
    }
    let public_key =
        VerifyingKey::from_bytes(&trusted.public_key).map_err(|_| Error::UntrustedKey)?;
    let signature = STANDARD
        .decode(&claims.signature)
        .map_err(|_| Error::InvalidSignature)?;
    let signature = Signature::from_slice(&signature).map_err(|_| Error::InvalidSignature)?;
    let message = claims.signing_bytes().map_err(|_| Error::InvalidEnvelope)?;
    public_key
        .verify_strict(&message, &signature)
        .map_err(|_| Error::InvalidSignature)?;
    check_context(&claims, context)?;
    Ok(VerifiedEntitlement { claims })
}

fn check_context(
    claims: &EntitlementEnvelopeV1,
    context: EntitlementContext<'_>,
) -> Result<EntitlementValidity, EntitlementVerificationError> {
    use EntitlementVerificationError as Error;
    if context.now > MAX_ENTITLEMENT_TIMESTAMP {
        return Err(Error::InvalidClock);
    }
    for (required, actual) in [
        (claims.account_id.as_deref(), context.account_id),
        (claims.org_id.as_deref(), context.org_id),
        (claims.workspace_id.as_deref(), context.workspace_id),
        (claims.deployment_id.as_deref(), context.deployment_id),
    ] {
        if required.is_some() && required != actual {
            return Err(Error::BindingMismatch);
        }
    }
    if !claims.allowed_deployments.contains(&context.deployment) {
        return Err(Error::BindingMismatch);
    }
    if context.now < claims.not_before || context.now < claims.issued_at {
        return Err(Error::NotYetValid);
    }
    // Half-open intervals make both boundaries deterministic across runtimes.
    if context.now >= claims.grace_until {
        return Err(Error::Expired);
    }
    Ok(if context.now < claims.expires_at {
        EntitlementValidity::Active
    } else {
        EntitlementValidity::Grace
    })
}

#[cfg(test)]
#[path = "signed_entitlements_tests.rs"]
mod tests;
