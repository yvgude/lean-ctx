//! ed25519 signing / verification for Context Snapshots.
//!
//! `ContextProofV1` is unsigned and signing today only exists for `.ctxpkg`
//! manifests, so snapshots get their own signing surface here. It reuses the
//! same publisher keypair (`context_package::keys`) so a project has one stable
//! signing identity across packages and snapshots.
//!
//! The signed message is `sha256-hex("ctxsnapshot-sign-v1:{snapshot_id}")`,
//! mirroring the `ctxpkg-sign-v1` scheme. Verification is two-fold: the body
//! must still hash to the stored `snapshot_id` (integrity), and the signature
//! must validate over that id (authenticity).

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use super::digest::{compute_id, finalize_id};
use super::types::{ContextSnapshotV1, SnapshotSignatureV1};

/// Finalize the snapshot id and attach an ed25519 signature over it.
pub fn sign_snapshot(
    snapshot: &mut ContextSnapshotV1,
    signing_key: &SigningKey,
) -> Result<(), String> {
    let id = finalize_id(snapshot)?;
    let message = signing_message(&id);
    let signature = signing_key.sign(message.as_bytes());
    let public_key = VerifyingKey::from(signing_key);
    snapshot.signature = Some(SnapshotSignatureV1 {
        algorithm: "ed25519".into(),
        public_key: to_hex(public_key.as_bytes()),
        value: to_hex(&signature.to_bytes()),
    });
    Ok(())
}

/// Verify a snapshot's signature **and** body integrity.
///
/// Returns `Ok(false)` for an unsigned snapshot, a tampered body (the
/// recomputed id no longer matches `snapshot_id`), or an invalid signature.
/// Returns `Err` only for malformed signature material (bad hex / wrong length /
/// unsupported algorithm).
pub fn verify_snapshot(snapshot: &ContextSnapshotV1) -> Result<bool, String> {
    let Some(ref sig) = snapshot.signature else {
        return Ok(false);
    };
    if sig.algorithm != "ed25519" {
        return Err(format!(
            "unsupported signature algorithm: {}",
            sig.algorithm
        ));
    }

    // Integrity: the canonical body must still hash to the recorded id.
    let recomputed = compute_id(snapshot)?;
    if recomputed != snapshot.snapshot_id {
        return Ok(false);
    }

    let pk_bytes = from_hex(&sig.public_key).map_err(|e| format!("invalid public_key hex: {e}"))?;
    let sig_bytes = from_hex(&sig.value).map_err(|e| format!("invalid signature hex: {e}"))?;
    let pk_array: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| "public_key must be 32 bytes".to_string())?;
    let sig_array: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;

    let verifying_key =
        VerifyingKey::from_bytes(&pk_array).map_err(|e| format!("invalid public key: {e}"))?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_array);
    let message = signing_message(&snapshot.snapshot_id);
    Ok(verifying_key.verify(message.as_bytes(), &signature).is_ok())
}

fn signing_message(snapshot_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("ctxsnapshot-sign-v1:{snapshot_id}"));
    to_hex(&hasher.finalize())
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("hex decode at {i}: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::types::ContextSnapshotV1;
    use super::*;

    fn sample() -> ContextSnapshotV1 {
        ContextSnapshotV1::new("2026-01-01T00:00:00Z".into(), "9.9.9".into())
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut snap = sample();
        sign_snapshot(&mut snap, &key).expect("sign");

        let sig = snap.signature.as_ref().expect("signed");
        assert_eq!(sig.algorithm, "ed25519");
        assert_eq!(sig.public_key.len(), 64);
        assert_eq!(sig.value.len(), 128);
        assert!(!snap.snapshot_id.is_empty());
        assert!(verify_snapshot(&snap).expect("verify"));
    }

    #[test]
    fn unsigned_returns_false() {
        assert!(!verify_snapshot(&sample()).expect("verify"));
    }

    #[test]
    fn tampered_body_fails_verification() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut snap = sample();
        sign_snapshot(&mut snap, &key).expect("sign");
        // Mutate the body after signing — id no longer matches the content.
        snap.git.dirty = true;
        assert!(!verify_snapshot(&snap).expect("verify"));
    }

    #[test]
    fn tampered_id_fails_verification() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut snap = sample();
        sign_snapshot(&mut snap, &key).expect("sign");
        snap.snapshot_id = "0".repeat(64);
        assert!(!verify_snapshot(&snap).expect("verify"));
    }

    #[test]
    fn wrong_key_fails_verification() {
        let mut snap = sample();
        sign_snapshot(&mut snap, &SigningKey::from_bytes(&[1u8; 32])).expect("sign");
        // Re-sign id stays the same, but swap the public key to a different one.
        let other = VerifyingKey::from(&SigningKey::from_bytes(&[2u8; 32]));
        snap.signature.as_mut().unwrap().public_key = to_hex(other.as_bytes());
        assert!(!verify_snapshot(&snap).expect("verify"));
    }

    #[test]
    fn bad_algorithm_errors() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut snap = sample();
        sign_snapshot(&mut snap, &key).expect("sign");
        snap.signature.as_mut().unwrap().algorithm = "rsa".into();
        assert!(verify_snapshot(&snap).is_err());
    }
}
