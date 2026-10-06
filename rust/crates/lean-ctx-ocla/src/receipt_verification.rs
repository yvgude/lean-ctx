// SPDX-License-Identifier: Apache-2.0

//! Shared production verification of canonical receipt signatures and signer trust.
//!
//! This does not validate execution lineage, artifact bytes, tenant authorization,
//! replay, or outcome quality. Consumers must complete those checks before learning.

use std::fmt::Write as _;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use lean_ctx_protocol::{ReceiptDocumentV1, Sha256Digest, UtcTimestamp};
use sha2::{Digest, Sha256};

/// Host-owned trust snapshot authorizing one receipt-signing key.
///
/// Resolve this snapshot and the verifying key from trusted host configuration,
/// never from receipt-supplied key material. This is not an admission token.
#[derive(Debug, Clone)]
pub struct ReceiptSignerAdmissionV1 {
    pub key_id: String,
    pub public_key_digest: Sha256Digest,
    pub admitted_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub revoked_at: Option<UtcTimestamp>,
}

/// A receipt or its signer failed the shared production trust checks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReceiptVerificationError {
    /// The host snapshot does not admit this key at the requested instant.
    #[error("receipt signing key is not admitted by the server trust snapshot")]
    SignerNotAdmitted,
    /// Receipt shape, signer identity, issuance time, or signature is invalid.
    #[error("{0}")]
    InvalidReceipt(String),
}

/// Check a trusted key snapshot at one issuance or verification instant.
///
/// Admission is inclusive; expiry and revocation are exclusive. Canonical
/// `UtcTimestamp` values have fixed-width UTC second precision, so their lexical
/// order is chronological. This check alone does not verify any signature.
pub fn validate_signer_admission(
    admission: &ReceiptSignerAdmissionV1,
    verifying_key: &VerifyingKey,
    at: &UtcTimestamp,
) -> Result<(), ReceiptVerificationError> {
    if admission.key_id.is_empty()
        || key_digest(verifying_key) != admission.public_key_digest.as_str()
        || at.as_str() < admission.admitted_at.as_str()
        || at.as_str() >= admission.expires_at.as_str()
        || admission
            .revoked_at
            .as_ref()
            .is_some_and(|revoked| at.as_str() >= revoked.as_str())
    {
        return Err(ReceiptVerificationError::SignerNotAdmitted);
    }
    Ok(())
}

/// Verify canonical receipt shape, trusted signer identity and strict signature.
///
/// The host-resolved admission must cover both issuance and verification time;
/// future-issued receipts are rejected. Success authenticates the receipt
/// signer's claims, not independently signed evaluator artifacts or a complete
/// execution chain. No filesystem access, caller-supplied trust discovery, or
/// learning side effect occurs here.
pub fn verify_receipt_signature(
    receipt: &ReceiptDocumentV1,
    trusted_signer: &ReceiptSignerAdmissionV1,
    verifying_key: &VerifyingKey,
    verified_at: &UtcTimestamp,
) -> Result<(), ReceiptVerificationError> {
    receipt.validate().map_err(verification_error)?;
    if receipt.signer.key_id != trusted_signer.key_id
        || receipt.issued_at.as_str() > verified_at.as_str()
    {
        return Err(ReceiptVerificationError::InvalidReceipt(
            "receipt signer or issuance time is not trusted".to_owned(),
        ));
    }
    validate_signer_admission(trusted_signer, verifying_key, &receipt.issued_at)?;
    validate_signer_admission(trusted_signer, verifying_key, verified_at)?;
    let signature_bytes = STANDARD
        .decode(&receipt.signature)
        .map_err(verification_error)?;
    let signature = Signature::from_slice(&signature_bytes).map_err(verification_error)?;
    verifying_key
        .verify_strict(
            &receipt.signing_bytes().map_err(verification_error)?,
            &signature,
        )
        .map_err(verification_error)
}

fn key_digest(verifying_key: &VerifyingKey) -> String {
    sha256_digest(verifying_key.as_bytes())
}

pub(crate) fn sha256_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

fn verification_error(error: impl std::fmt::Display) -> ReceiptVerificationError {
    ReceiptVerificationError::InvalidReceipt(error.to_string())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;

    fn time(value: &str) -> UtcTimestamp {
        UtcTimestamp::new(value).expect("canonical test timestamp")
    }

    fn fixture() -> (ReceiptSignerAdmissionV1, VerifyingKey) {
        // Deterministic test-only key, never a production trust authority.
        let key = SigningKey::from_bytes(&[17; 32]).verifying_key();
        let admission = ReceiptSignerAdmissionV1 {
            key_id: "test-receipt-key".to_owned(),
            public_key_digest: Sha256Digest::new(key_digest(&key)).expect("canonical key digest"),
            admitted_at: time("2026-08-23T12:00:00Z"),
            expires_at: time("2026-08-23T14:00:00Z"),
            revoked_at: None,
        };
        (admission, key)
    }

    #[test]
    fn admission_is_inclusive_and_expiry_is_exclusive() {
        let (admission, key) = fixture();
        for at in ["2026-08-23T12:00:00Z", "2026-08-23T13:59:59Z"] {
            validate_signer_admission(&admission, &key, &time(at)).expect("admitted instant");
        }
        for at in ["2026-08-23T11:59:59Z", "2026-08-23T14:00:00Z"] {
            assert!(validate_signer_admission(&admission, &key, &time(at)).is_err());
        }
    }

    #[test]
    fn revocation_rejects_its_boundary_and_later_instants() {
        let (mut admission, key) = fixture();
        admission.revoked_at = Some(time("2026-08-23T13:00:00Z"));
        validate_signer_admission(&admission, &key, &time("2026-08-23T12:59:59Z"))
            .expect("not revoked yet");
        for at in ["2026-08-23T13:00:00Z", "2026-08-23T13:00:01Z"] {
            assert!(validate_signer_admission(&admission, &key, &time(at)).is_err());
        }
    }

    #[test]
    fn key_digest_and_nonempty_identity_are_required() {
        let (mut admission, key) = fixture();
        let now = admission.admitted_at.clone();
        let other_key = SigningKey::from_bytes(&[99; 32]).verifying_key();
        assert!(validate_signer_admission(&admission, &other_key, &now).is_err());
        admission.key_id.clear();
        assert!(validate_signer_admission(&admission, &key, &now).is_err());
    }

    #[test]
    fn empty_or_inverted_trust_window_admits_nothing() {
        let (mut admission, key) = fixture();
        for expires_at in ["2026-08-23T12:00:00Z", "2026-08-23T11:00:00Z"] {
            admission.expires_at = time(expires_at);
            assert!(validate_signer_admission(&admission, &key, &admission.admitted_at).is_err());
        }
    }
}
// SPDX-License-Identifier: Apache-2.0
