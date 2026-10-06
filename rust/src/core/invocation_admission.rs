//! Pure runtime verification for signed invocation-context admissions.
//!
//! The protocol crate owns the wire contract and canonical decoder. This
//! module owns trusted-key lookup, revocation, public-key binding, Ed25519
//! verification, expected-runtime-scope binding, and injected time gates.
//! Verification has no replay or one-time-consumption state; admission
//! consumption belongs to the caller's separate ledger gate.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use lean_ctx_protocol::{
    EngineInvocationIdV1, InvocationCapabilityBindingV1, InvocationContextBindingV1,
    InvocationSourceBindingV1, PlanId, ProtocolReference, Sha256Digest, TaskId, UtcTimestamp,
};
use sha2::{Digest, Sha256};

/// Verification failure for a signed invocation admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InvocationAdmissionVerificationError {
    /// The input was not a strict canonical protocol binding.
    CanonicalDecode(String),
    /// The binding does not equal the caller's expected runtime scope.
    ///
    /// This intentionally carries no field values or input material.
    ScopeMismatch,
    /// The binding names no trusted key.
    UnknownKey { key_id: String },
    /// The binding names a key which has been revoked.
    RevokedKey { key_id: String },
    /// The trusted key does not have the digest committed by the binding.
    PublicKeyDigestMismatch {
        key_id: String,
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    /// A trusted key could not be represented as a valid Ed25519 key.
    InvalidPublicKey { key_id: String },
    /// The signature field could not be decoded as exactly 64 bytes.
    InvalidSignatureEncoding,
    /// The signature did not verify for the binding domain and unsigned bytes.
    InvalidSignature,
    /// The binding is not active at the injected current time.
    NotYetValid {
        now: UtcTimestamp,
        not_before: UtcTimestamp,
    },
    /// The binding was issued after the injected current time.
    NotYetIssued {
        now: UtcTimestamp,
        issued_at: UtcTimestamp,
    },
    /// The binding has expired at the injected current time.
    Expired {
        now: UtcTimestamp,
        expires_at: UtcTimestamp,
    },
    /// A trust-store entry is malformed or conflicts with an existing pin.
    InvalidTrustedKey(String),
    /// An operation addressed a key which is not present in the trust store.
    TrustStoreKeyNotFound { key_id: String },
}

impl fmt::Display for InvocationAdmissionVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CanonicalDecode(error) => {
                write!(formatter, "canonical admission decode: {error}")
            }
            Self::ScopeMismatch => formatter.write_str("invocation admission scope mismatch"),
            Self::UnknownKey { key_id } => {
                write!(
                    formatter,
                    "invocation admission signer key is untrusted: {key_id}"
                )
            }
            Self::RevokedKey { key_id } => {
                write!(
                    formatter,
                    "invocation admission signer key is revoked: {key_id}"
                )
            }
            Self::PublicKeyDigestMismatch {
                key_id,
                expected,
                actual,
            } => write!(
                formatter,
                "invocation admission public-key digest mismatch for {key_id}: expected {}, got {}",
                expected.as_str(),
                actual.as_str()
            ),
            Self::InvalidPublicKey { key_id } => {
                write!(
                    formatter,
                    "trusted invocation admission key is invalid: {key_id}"
                )
            }
            Self::InvalidSignatureEncoding => {
                formatter.write_str("invocation admission signature is not 64-byte base64")
            }
            Self::InvalidSignature => {
                formatter.write_str("invocation admission signature verification failed")
            }
            Self::NotYetValid { now, not_before } => write!(
                formatter,
                "invocation admission is not valid yet: now {}, not_before {}",
                now.as_str(),
                not_before.as_str()
            ),
            Self::NotYetIssued { now, issued_at } => write!(
                formatter,
                "invocation admission is not issued yet: now {}, issued_at {}",
                now.as_str(),
                issued_at.as_str()
            ),
            Self::Expired { now, expires_at } => write!(
                formatter,
                "invocation admission is expired: now {}, expires_at {}",
                now.as_str(),
                expires_at.as_str()
            ),
            Self::InvalidTrustedKey(error) => write!(formatter, "invalid trusted key: {error}"),
            Self::TrustStoreKeyNotFound { key_id } => {
                write!(
                    formatter,
                    "trusted invocation admission key not found: {key_id}"
                )
            }
        }
    }
}

impl std::error::Error for InvocationAdmissionVerificationError {}

/// Exact runtime scope which the caller expects the signed binding to cover.
///
/// Source and capability vectors retain protocol order. Equality therefore
/// requires the complete ordered collections, not merely a set of entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvocationAdmissionExpectedScopeV1 {
    session_identity_ref: Sha256Digest,
    task_id: TaskId,
    task_ref: Sha256Digest,
    plan_id: PlanId,
    plan_ref: Sha256Digest,
    invocation_id: EngineInvocationIdV1,
    invocation_ref: Sha256Digest,
    policy_ref: ProtocolReference,
    policy_digest: Sha256Digest,
    source_bindings: Vec<InvocationSourceBindingV1>,
    capability_bindings: Vec<InvocationCapabilityBindingV1>,
}

impl InvocationAdmissionExpectedScopeV1 {
    /// Construct the exact scope expected by the runtime admission caller.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_identity_ref: Sha256Digest,
        task_id: TaskId,
        task_ref: Sha256Digest,
        plan_id: PlanId,
        plan_ref: Sha256Digest,
        invocation_id: EngineInvocationIdV1,
        invocation_ref: Sha256Digest,
        policy_ref: ProtocolReference,
        policy_digest: Sha256Digest,
        source_bindings: Vec<InvocationSourceBindingV1>,
        capability_bindings: Vec<InvocationCapabilityBindingV1>,
    ) -> Self {
        Self {
            session_identity_ref,
            task_id,
            task_ref,
            plan_id,
            plan_ref,
            invocation_id,
            invocation_ref,
            policy_ref,
            policy_digest,
            source_bindings,
            capability_bindings,
        }
    }
}

/// A trusted Ed25519 verification key identified by its protocol key id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InvocationAdmissionTrustedKeyV1 {
    key_id: String,
    public_key: [u8; 32],
    revoked: bool,
}

impl InvocationAdmissionTrustedKeyV1 {
    /// Construct a non-revoked trusted key pin.
    pub(crate) fn new(
        key_id: impl Into<String>,
        public_key: [u8; 32],
    ) -> Result<Self, InvocationAdmissionVerificationError> {
        let key_id = key_id.into();
        validate_key_id(&key_id)?;
        VerifyingKey::from_bytes(&public_key).map_err(|_| {
            InvocationAdmissionVerificationError::InvalidTrustedKey(format!(
                "{key_id}: invalid Ed25519 public key"
            ))
        })?;
        Ok(Self {
            key_id,
            public_key,
            revoked: false,
        })
    }

    pub(crate) fn key_id(&self) -> &str {
        &self.key_id
    }

    pub(crate) fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    pub(crate) fn is_revoked(&self) -> bool {
        self.revoked
    }

    /// Return this key with its revocation bit set.
    pub(crate) fn revoked(mut self) -> Self {
        self.revoked = true;
        self
    }
}

/// In-memory trusted-key and revocation view for admission verification.
///
/// This type has no replay, consumption, or mutable admission state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct InvocationAdmissionTrustStoreV1 {
    keys: BTreeMap<String, InvocationAdmissionTrustedKeyV1>,
}

impl InvocationAdmissionTrustStoreV1 {
    /// Construct an empty trust store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add a trusted key, rejecting a conflicting replacement for its id.
    pub(crate) fn insert(
        &mut self,
        key: InvocationAdmissionTrustedKeyV1,
    ) -> Result<(), InvocationAdmissionVerificationError> {
        if let Some(existing) = self.keys.get(key.key_id()) {
            if existing != &key {
                return Err(InvocationAdmissionVerificationError::InvalidTrustedKey(
                    format!("conflicting key pin for {}", key.key_id()),
                ));
            }
            return Ok(());
        }
        self.keys.insert(key.key_id.clone(), key);
        Ok(())
    }

    /// Add a non-revoked trusted key from raw Ed25519 bytes.
    pub(crate) fn add(
        &mut self,
        key_id: impl Into<String>,
        public_key: [u8; 32],
    ) -> Result<(), InvocationAdmissionVerificationError> {
        self.insert(InvocationAdmissionTrustedKeyV1::new(key_id, public_key)?)
    }

    /// Revoke a previously pinned key.
    pub(crate) fn revoke(
        &mut self,
        key_id: &str,
    ) -> Result<(), InvocationAdmissionVerificationError> {
        let Some(key) = self.keys.get_mut(key_id) else {
            return Err(
                InvocationAdmissionVerificationError::TrustStoreKeyNotFound {
                    key_id: key_id.to_string(),
                },
            );
        };
        key.revoked = true;
        Ok(())
    }

    /// Verify one canonical binding at an injected current time and scope.
    pub(crate) fn verify(
        &self,
        canonical_bytes: &[u8],
        expected_scope: &InvocationAdmissionExpectedScopeV1,
        now: &UtcTimestamp,
    ) -> Result<VerifiedInvocationAdmissionV1, InvocationAdmissionVerificationError> {
        verify_invocation_admission(canonical_bytes, self, expected_scope, now)
    }
}

/// The only value which may cross the runtime admission boundary.
///
/// Fields are private and the type is deliberately not `Clone`; callers can
/// obtain it only after every verifier gate, including expected-scope equality.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct VerifiedInvocationAdmissionV1 {
    binding: InvocationContextBindingV1,
    canonical_bytes: Vec<u8>,
    binding_digest: Sha256Digest,
}

/// Exact ordered source bytes for one freshly verified admission.
/// Deliberately has no Debug/Serialize implementation: payloads are not logs.
pub(crate) struct MaterializedInvocationSourcesV1 {
    admission: VerifiedInvocationAdmissionV1,
    sources: Vec<(InvocationSourceBindingV1, Vec<u8>)>,
}

impl MaterializedInvocationSourcesV1 {
    pub(crate) fn admission(&self) -> &VerifiedInvocationAdmissionV1 {
        &self.admission
    }

    pub(crate) fn sources(&self) -> &[(InvocationSourceBindingV1, Vec<u8>)] {
        &self.sources
    }
}

/// Resolve only the signed source digests, never paths supplied by source_ref.
///
/// The root and expected scope must come from trusted runtime state. The caller
/// must authorize workspace/classification and consume the admission separately;
/// integrity verification does not supply either of those policy decisions.
/// No partially materialized result escapes if any source fails.
pub(crate) fn materialize_invocation_sources(
    canonical_bytes: &[u8],
    trust_store: &InvocationAdmissionTrustStoreV1,
    expected_scope: &InvocationAdmissionExpectedScopeV1,
    now: &UtcTimestamp,
    artifact_root: &Path,
    max_source_bytes: usize,
    max_total_bytes: usize,
) -> Result<MaterializedInvocationSourcesV1, String> {
    if max_source_bytes == 0
        || max_source_bytes > 1024 * 1024
        || max_total_bytes == 0
        || max_total_bytes > 4 * 1024 * 1024
    {
        return Err("invocation source byte budget invalid".into());
    }
    // Recheck signature, revocation, exact runtime scope and time before I/O.
    let admission = verify_invocation_admission(canonical_bytes, trust_store, expected_scope, now)
        .map_err(|_| "invocation source admission rejected".to_string())?;
    let mut remaining = max_total_bytes;
    let mut sources = Vec::with_capacity(admission.binding.source_bindings.len());
    for source in &admission.binding.source_bindings {
        if remaining == 0 {
            return Err("invocation source total byte budget exhausted".into());
        }
        let digest = source
            .digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| "invocation source digest invalid".to_string())?;
        let bytes = super::engine_artifact::read_bounded_content(
            artifact_root,
            "invocation-sources",
            digest,
            "txt",
            remaining.min(max_source_bytes),
        )
        .map_err(|_| "invocation source artifact rejected".to_string())?;
        if digest_bytes(&bytes).map_err(|_| "invocation source digest failed")? != source.digest {
            return Err("invocation source digest mismatch".into());
        }
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| "invocation source total byte budget exceeded".to_string())?;
        sources.push((source.clone(), bytes));
    }
    Ok(MaterializedInvocationSourcesV1 { admission, sources })
}

impl VerifiedInvocationAdmissionV1 {
    /// Return the validated protocol binding.
    pub(crate) fn binding(&self) -> &InvocationContextBindingV1 {
        &self.binding
    }

    /// Return the exact canonical bytes which were verified.
    pub(crate) fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Return the SHA-256 identity of the complete signed binding bytes.
    pub(crate) fn binding_digest(&self) -> &Sha256Digest {
        &self.binding_digest
    }

    /// Alias used by evidence adapters when naming the admission artifact.
    pub(crate) fn admission_digest(&self) -> &Sha256Digest {
        self.binding_digest()
    }

    /// Return the signer key id committed by the binding.
    pub(crate) fn signer_key_id(&self) -> &str {
        &self.binding.signer.key_id
    }

    /// Return the signer public-key digest committed by the binding.
    pub(crate) fn signer_public_key_digest(&self) -> &Sha256Digest {
        &self.binding.signer.public_key_digest
    }
}

/// Verify a signed invocation-context binding without consuming it.
///
/// `expected_scope` is mandatory: no authorizing API exists without it.
pub(crate) fn verify_invocation_admission(
    canonical_bytes: &[u8],
    trust_store: &InvocationAdmissionTrustStoreV1,
    expected_scope: &InvocationAdmissionExpectedScopeV1,
    now: &UtcTimestamp,
) -> Result<VerifiedInvocationAdmissionV1, InvocationAdmissionVerificationError> {
    let binding =
        InvocationContextBindingV1::from_canonical_bytes(canonical_bytes).map_err(|error| {
            InvocationAdmissionVerificationError::CanonicalDecode(error.to_string())
        })?;

    if expected_scope != &expected_scope_for_binding(&binding) {
        return Err(InvocationAdmissionVerificationError::ScopeMismatch);
    }

    let trusted_key = trust_store.resolve(&binding.signer.key_id).ok_or_else(|| {
        InvocationAdmissionVerificationError::UnknownKey {
            key_id: binding.signer.key_id.clone(),
        }
    })?;
    if trusted_key.is_revoked() {
        return Err(InvocationAdmissionVerificationError::RevokedKey {
            key_id: trusted_key.key_id().to_string(),
        });
    }

    // Digest the exact 32 bytes pinned in the trust store, before constructing
    // an Ed25519 key object or verifying any signature.
    let actual_digest = digest_public_key(trusted_key.public_key())?;
    if actual_digest != binding.signer.public_key_digest {
        return Err(
            InvocationAdmissionVerificationError::PublicKeyDigestMismatch {
                key_id: trusted_key.key_id().to_string(),
                expected: binding.signer.public_key_digest.clone(),
                actual: actual_digest,
            },
        );
    }

    let verifying_key = VerifyingKey::from_bytes(trusted_key.public_key()).map_err(|_| {
        InvocationAdmissionVerificationError::InvalidPublicKey {
            key_id: trusted_key.key_id().to_string(),
        }
    })?;
    let signature_bytes = STANDARD
        .decode(binding.signature.as_bytes())
        .map_err(|_| InvocationAdmissionVerificationError::InvalidSignatureEncoding)?;
    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| InvocationAdmissionVerificationError::InvalidSignatureEncoding)?;
    let signature = Signature::from_bytes(&signature_array);
    let signing_bytes = binding.signing_bytes().map_err(|error| {
        InvocationAdmissionVerificationError::CanonicalDecode(error.to_string())
    })?;
    verifying_key
        .verify(&signing_bytes, &signature)
        .map_err(|_| InvocationAdmissionVerificationError::InvalidSignature)?;

    if now < &binding.not_before {
        return Err(InvocationAdmissionVerificationError::NotYetValid {
            now: now.clone(),
            not_before: binding.not_before.clone(),
        });
    }
    if now < &binding.issued_at {
        return Err(InvocationAdmissionVerificationError::NotYetIssued {
            now: now.clone(),
            issued_at: binding.issued_at.clone(),
        });
    }
    if now >= &binding.expires_at {
        return Err(InvocationAdmissionVerificationError::Expired {
            now: now.clone(),
            expires_at: binding.expires_at.clone(),
        });
    }

    let binding_digest = digest_bytes(canonical_bytes)?;
    Ok(VerifiedInvocationAdmissionV1 {
        binding,
        canonical_bytes: canonical_bytes.to_vec(),
        binding_digest,
    })
}

fn expected_scope_for_binding(
    binding: &InvocationContextBindingV1,
) -> InvocationAdmissionExpectedScopeV1 {
    InvocationAdmissionExpectedScopeV1::new(
        binding.session_identity_ref.clone(),
        binding.task_id.clone(),
        binding.task_ref.clone(),
        binding.plan_id.clone(),
        binding.plan_ref.clone(),
        binding.invocation_id.clone(),
        binding.invocation_ref.clone(),
        binding.policy_ref.clone(),
        binding.policy_digest.clone(),
        binding.source_bindings.clone(),
        binding.capability_bindings.clone(),
    )
}

fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, InvocationAdmissionVerificationError> {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use fmt::Write as _;
        write!(hex, "{byte:02x}").map_err(|_| {
            InvocationAdmissionVerificationError::InvalidTrustedKey(
                "failed to encode SHA-256 digest".to_string(),
            )
        })?;
    }
    Sha256Digest::new(format!("sha256:{hex}"))
        .map_err(|error| InvocationAdmissionVerificationError::InvalidTrustedKey(error.to_string()))
}

fn digest_public_key(
    public_key: &[u8; 32],
) -> Result<Sha256Digest, InvocationAdmissionVerificationError> {
    digest_bytes(public_key)
}

fn validate_key_id(key_id: &str) -> Result<(), InvocationAdmissionVerificationError> {
    if key_id.is_empty()
        || key_id.len() > 128
        || !key_id.is_ascii()
        || !key_id.as_bytes()[0].is_ascii_alphanumeric()
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
        || key_id.starts_with("base64:")
        || key_id.starts_with("hex:")
    {
        return Err(InvocationAdmissionVerificationError::InvalidTrustedKey(
            "key id must be a bounded non-material identifier".to_string(),
        ));
    }
    Ok(())
}

impl InvocationAdmissionTrustStoreV1 {
    pub(crate) fn resolve(&self, key_id: &str) -> Option<&InvocationAdmissionTrustedKeyV1> {
        self.keys.get(key_id)
    }
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signer, SigningKey};
    use lean_ctx_protocol::{
        CapabilityId, EngineInvocationIdV1, InvocationContextBindingV1, ProtocolReference,
        SemanticVersion, UtcTimestamp,
    };
    use serde_json::{Value, json};

    use super::*;

    const NOW: &str = "2026-08-24T12:00:00Z";
    const NOT_BEFORE: &str = "2026-08-24T11:59:00Z";
    const EXPIRES_AT: &str = "2026-08-24T12:05:00Z";

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn public_key_digest(signing_key: &SigningKey) -> String {
        let digest = Sha256::digest(VerifyingKey::from(signing_key).as_bytes());
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use fmt::Write as _;
            write!(hex, "{byte:02x}").expect("hex write");
        }
        format!("sha256:{hex}")
    }

    fn digest(value: char) -> String {
        format!("sha256:{}", value.to_string().repeat(64))
    }

    fn unsigned_value(signing_key: &SigningKey) -> Value {
        json!({
            "schema_version": 1,
            "admission_id": "admission-1",
            "session_identity_ref": digest('a'),
            "task_id": "task-1",
            "task_ref": digest('b'),
            "plan_id": "plan-1",
            "plan_ref": digest('c'),
            "invocation_id": "invocation-1",
            "invocation_ref": digest('d'),
            "policy_ref": "policy:invocation-admission",
            "policy_digest": digest('e'),
            "decision": "admitted",
            "source_bindings": [{
                "source_ref": "source:input",
                "digest": digest('f'),
                "role": "input"
            }],
            "capability_bindings": [{
                "capability_id": "capability:engine",
                "capability_version": "1.0.0",
                "manifest_digest": digest('1')
            }],
            "issued_at": NOW,
            "not_before": NOT_BEFORE,
            "expires_at": EXPIRES_AT,
            "signer": {
                "algorithm": "ed25519",
                "key_id": "test-key",
                "public_key_digest": public_key_digest(signing_key)
            },
            "signature": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="
        })
    }

    fn signed_binding(signing_key: &SigningKey) -> Vec<u8> {
        let value = unsigned_value(signing_key);
        let mut binding: InvocationContextBindingV1 =
            serde_json::from_value(value).expect("binding fixture");
        let signature = signing_key.sign(&binding.signing_bytes().expect("signing bytes"));
        binding.signature = STANDARD.encode(signature.to_bytes());
        binding.canonical_bytes().expect("canonical binding")
    }

    fn binding(signing_key: &SigningKey) -> InvocationContextBindingV1 {
        InvocationContextBindingV1::from_canonical_bytes(&signed_binding(signing_key))
            .expect("canonical binding")
    }

    fn scope(signing_key: &SigningKey) -> InvocationAdmissionExpectedScopeV1 {
        expected_scope_for_binding(&binding(signing_key))
    }

    fn store(signing_key: &SigningKey) -> InvocationAdmissionTrustStoreV1 {
        let mut store = InvocationAdmissionTrustStoreV1::new();
        store
            .add("test-key", VerifyingKey::from(signing_key).to_bytes())
            .expect("trusted key");
        store
    }

    fn now(value: &str) -> UtcTimestamp {
        UtcTimestamp::new(value).expect("timestamp")
    }

    fn source_fixture() -> (
        tempfile::TempDir,
        Vec<u8>,
        InvocationAdmissionExpectedScopeV1,
    ) {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("invocation-sources");
        std::fs::create_dir(&directory).unwrap();
        let signing_key = key();
        let mut value = unsigned_value(&signing_key);
        let payloads: [&[u8]; 2] = [b"task input", b"selected context"];
        value["source_bindings"] = json!(
            payloads
                .iter()
                .enumerate()
                .map(|(index, bytes)| {
                    let digest = digest_bytes(bytes).unwrap();
                    std::fs::write(
                        directory.join(format!(
                            "{}.txt",
                            digest.as_str().strip_prefix("sha256:").unwrap()
                        )),
                        bytes,
                    )
                    .unwrap();
                    json!({
                        "source_ref": format!("source:item-{index}"),
                        "digest": digest,
                        "role": if index == 0 { "input" } else { "context" },
                    })
                })
                .collect::<Vec<_>>()
        );
        let mut binding: InvocationContextBindingV1 = serde_json::from_value(value).unwrap();
        binding.signature = STANDARD.encode(
            signing_key
                .sign(&binding.signing_bytes().unwrap())
                .to_bytes(),
        );
        let scope = expected_scope_for_binding(&binding);
        (root, binding.canonical_bytes().unwrap(), scope)
    }

    #[test]
    fn materialization_preserves_exact_signed_order_and_bytes() {
        let (root, bytes, scope) = source_fixture();
        let result = materialize_invocation_sources(
            &bytes,
            &store(&key()),
            &scope,
            &now(NOW),
            root.path(),
            16,
            26,
        )
        .unwrap();
        assert_eq!(result.admission.canonical_bytes(), bytes);
        assert_eq!(result.sources.len(), 2);
        assert_eq!(result.sources[0].0.source_ref.as_str(), "source:item-0");
        assert_eq!(result.sources[0].1, b"task input");
        assert_eq!(result.sources[1].1, b"selected context");
    }

    #[test]
    fn materialization_rejects_source_and_aggregate_budget_overflow() {
        let (root, bytes, scope) = source_fixture();
        for (per_source, total) in [
            (15, 26),
            (16, 25),
            (16, 10),
            (0, 26),
            (16, 0),
            (1024 * 1024 + 1, 26),
            (16, 4 * 1024 * 1024 + 1),
        ] {
            assert!(
                materialize_invocation_sources(
                    &bytes,
                    &store(&key()),
                    &scope,
                    &now(NOW),
                    root.path(),
                    per_source,
                    total,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn materialization_rejects_changed_or_missing_artifact() {
        let (root, bytes, scope) = source_fixture();
        let digest = digest_bytes(b"selected context").unwrap();
        let path = root.path().join("invocation-sources").join(format!(
            "{}.txt",
            digest.as_str().strip_prefix("sha256:").unwrap()
        ));
        std::fs::write(&path, b"different bytes").unwrap();
        assert!(
            materialize_invocation_sources(
                &bytes,
                &store(&key()),
                &scope,
                &now(NOW),
                root.path(),
                32,
                64,
            )
            .is_err()
        );
        std::fs::remove_file(path).unwrap();
        assert!(
            materialize_invocation_sources(
                &bytes,
                &store(&key()),
                &scope,
                &now(NOW),
                root.path(),
                32,
                64,
            )
            .is_err()
        );
    }

    #[test]
    fn materialization_rechecks_admission_before_reading_sources() {
        let (root, bytes, mut scope) = source_fixture();
        // A missing store must not mask an expired or mismatched admission.
        let missing = root.path().join("missing");
        let error = materialize_invocation_sources(
            &bytes,
            &store(&key()),
            &scope,
            &now(EXPIRES_AT),
            &missing,
            32,
            64,
        )
        .err()
        .unwrap();
        assert_eq!(error, "invocation source admission rejected");
        scope.task_id = TaskId::new("different-task").unwrap();
        let error = materialize_invocation_sources(
            &bytes,
            &store(&key()),
            &scope,
            &now(NOW),
            &missing,
            32,
            64,
        )
        .err()
        .unwrap();
        assert_eq!(error, "invocation source admission rejected");
    }

    #[cfg(unix)]
    #[test]
    fn materialization_rejects_symlink_even_with_matching_content() {
        let (root, bytes, scope) = source_fixture();
        let digest = digest_bytes(b"task input").unwrap();
        let path = root.path().join("invocation-sources").join(format!(
            "{}.txt",
            digest.as_str().strip_prefix("sha256:").unwrap()
        ));
        let outside = root.path().join("outside.bin");
        std::fs::rename(&path, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(
            materialize_invocation_sources(
                &bytes,
                &store(&key()),
                &scope,
                &now(NOW),
                root.path(),
                32,
                64,
            )
            .is_err()
        );
    }

    #[test]
    fn verifies_canonical_signature_scope_and_identity() {
        let signing_key = key();
        let bytes = signed_binding(&signing_key);
        let verified = store(&signing_key)
            .verify(&bytes, &scope(&signing_key), &now(NOW))
            .expect("verified admission");

        assert_eq!(verified.canonical_bytes(), bytes.as_slice());
        assert_eq!(verified.binding().admission_id.as_str(), "admission-1");
        assert_eq!(verified.binding().invocation_id.as_str(), "invocation-1");
        assert_eq!(verified.signer_key_id(), "test-key");
        assert_eq!(
            verified.signer_public_key_digest().as_str(),
            public_key_digest(&signing_key)
        );
        assert_eq!(verified.binding_digest(), verified.admission_digest());
        assert_eq!(
            verified.binding_digest(),
            &digest_bytes(&bytes).expect("digest")
        );
    }

    #[test]
    fn unknown_key_is_rejected() {
        let signing_key = key();
        let error = InvocationAdmissionTrustStoreV1::new()
            .verify(
                &signed_binding(&signing_key),
                &scope(&signing_key),
                &now(NOW),
            )
            .expect_err("unknown key");
        assert!(matches!(
            error,
            InvocationAdmissionVerificationError::UnknownKey { .. }
        ));
    }

    #[test]
    fn revoked_key_is_rejected() {
        let signing_key = key();
        let trusted = InvocationAdmissionTrustedKeyV1::new(
            "test-key",
            VerifyingKey::from(&signing_key).to_bytes(),
        )
        .expect("trusted key")
        .revoked();
        let mut store = InvocationAdmissionTrustStoreV1::new();
        store.insert(trusted).expect("insert");
        let error = store
            .verify(
                &signed_binding(&signing_key),
                &scope(&signing_key),
                &now(NOW),
            )
            .expect_err("revoked key");
        assert!(matches!(
            error,
            InvocationAdmissionVerificationError::RevokedKey { .. }
        ));
    }

    #[test]
    fn public_key_digest_mismatch_is_rejected() {
        let signing_key = key();
        let other = SigningKey::from_bytes(&[8u8; 32]);
        let mut value = unsigned_value(&signing_key);
        value["signer"]["public_key_digest"] = json!(public_key_digest(&other));
        let mut binding: InvocationContextBindingV1 =
            serde_json::from_value(value).expect("binding fixture");
        binding.signature = STANDARD.encode(
            signing_key
                .sign(&binding.signing_bytes().expect("signing bytes"))
                .to_bytes(),
        );
        let bytes = binding.canonical_bytes().expect("canonical");
        let error = store(&signing_key)
            .verify(&bytes, &scope(&signing_key), &now(NOW))
            .expect_err("digest mismatch");
        assert!(matches!(
            error,
            InvocationAdmissionVerificationError::PublicKeyDigestMismatch { .. }
        ));
    }

    #[test]
    fn field_isolated_scope_mismatches_are_rejected_before_authorization() {
        let signing_key = key();
        let bytes = signed_binding(&signing_key);
        let expected = scope(&signing_key);
        let mut cases = Vec::new();

        let mut mismatch = expected.clone();
        mismatch.session_identity_ref = Sha256Digest::new(digest('9')).unwrap();
        cases.push(mismatch);
        let mut mismatch = expected.clone();
        mismatch.task_id = TaskId::new("task-other").unwrap();
        cases.push(mismatch);
        let mut mismatch = expected.clone();
        mismatch.task_ref = Sha256Digest::new(digest('8')).unwrap();
        cases.push(mismatch);
        let mut mismatch = expected.clone();
        mismatch.plan_id = PlanId::new("plan-other").unwrap();
        cases.push(mismatch);
        let mut mismatch = expected.clone();
        mismatch.plan_ref = Sha256Digest::new(digest('7')).unwrap();
        cases.push(mismatch);
        let mut mismatch = expected.clone();
        mismatch.invocation_id = EngineInvocationIdV1::new("invocation-other").unwrap();
        cases.push(mismatch);
        let mut mismatch = expected.clone();
        mismatch.invocation_ref = Sha256Digest::new(digest('6')).unwrap();
        cases.push(mismatch);
        let mut mismatch = expected.clone();
        mismatch.policy_ref = ProtocolReference::new("policy:other").unwrap();
        cases.push(mismatch);
        let mut mismatch = expected.clone();
        mismatch.policy_digest = Sha256Digest::new(digest('5')).unwrap();
        cases.push(mismatch);

        for mismatch in cases {
            assert!(matches!(
                store(&signing_key).verify(&bytes, &mismatch, &now(NOW)),
                Err(InvocationAdmissionVerificationError::ScopeMismatch)
            ));
        }
    }

    #[test]
    fn whole_source_and_capability_collections_are_scope_bound() {
        let signing_key = key();
        let bytes = signed_binding(&signing_key);
        let expected = scope(&signing_key);

        let mut source_cross_scope = expected.clone();
        let mut source = source_cross_scope.source_bindings[0].clone();
        source.source_ref = ProtocolReference::new("source:other").unwrap();
        source.digest = Sha256Digest::new(digest('4')).unwrap();
        source_cross_scope.source_bindings = vec![source];
        assert!(matches!(
            store(&signing_key).verify(&bytes, &source_cross_scope, &now(NOW)),
            Err(InvocationAdmissionVerificationError::ScopeMismatch)
        ));

        let mut capability_cross_scope = expected.clone();
        let mut capability = capability_cross_scope.capability_bindings[0].clone();
        capability.capability_id = CapabilityId::new("capability:other").unwrap();
        capability.capability_version = SemanticVersion::new("2.0.0").unwrap();
        capability.manifest_digest = Sha256Digest::new(digest('3')).unwrap();
        capability_cross_scope.capability_bindings = vec![capability];
        assert!(matches!(
            store(&signing_key).verify(&bytes, &capability_cross_scope, &now(NOW)),
            Err(InvocationAdmissionVerificationError::ScopeMismatch)
        ));
    }

    #[test]
    fn signature_and_domain_mutations_are_rejected() {
        let signing_key = key();
        let mut value = unsigned_value(&signing_key);
        let mut binding: InvocationContextBindingV1 =
            serde_json::from_value(value.clone()).expect("binding fixture");
        let mut wrong_domain = b"wrong-domain\0".to_vec();
        wrong_domain.extend_from_slice(&binding.unsigned_canonical_bytes().expect("unsigned"));
        binding.signature = STANDARD.encode(signing_key.sign(&wrong_domain).to_bytes());
        let wrong_domain_bytes = binding.canonical_bytes().expect("canonical");
        assert!(matches!(
            store(&signing_key).verify(&wrong_domain_bytes, &scope(&signing_key), &now(NOW)),
            Err(InvocationAdmissionVerificationError::InvalidSignature)
        ));

        value["task_id"] = json!("task-mutated");
        let mut mutated: InvocationContextBindingV1 =
            serde_json::from_value(value).expect("binding fixture");
        let original_signature = STANDARD
            .decode(
                InvocationContextBindingV1::from_canonical_bytes(&signed_binding(&signing_key))
                    .expect("original")
                    .signature,
            )
            .expect("signature");
        mutated.signature = STANDARD.encode(original_signature);
        let mutated_bytes = mutated.canonical_bytes().expect("canonical");
        assert!(matches!(
            store(&signing_key).verify(&mutated_bytes, &scope(&signing_key), &now(NOW)),
            Err(InvocationAdmissionVerificationError::ScopeMismatch)
        ));
    }

    #[test]
    fn validity_window_rejects_not_before_issued_and_expiry_boundaries() {
        let signing_key = key();
        let bytes = signed_binding(&signing_key);
        let scope = scope(&signing_key);
        assert!(matches!(
            store(&signing_key).verify(&bytes, &scope, &now("2026-08-24T11:58:59Z")),
            Err(InvocationAdmissionVerificationError::NotYetValid { .. })
        ));
        assert!(matches!(
            store(&signing_key).verify(&bytes, &scope, &now(NOT_BEFORE)),
            Err(InvocationAdmissionVerificationError::NotYetIssued { .. })
        ));
        assert!(
            store(&signing_key)
                .verify(&bytes, &scope, &now(NOW))
                .is_ok()
        );
        assert!(matches!(
            store(&signing_key).verify(&bytes, &scope, &now(EXPIRES_AT)),
            Err(InvocationAdmissionVerificationError::Expired { .. })
        ));
    }

    #[test]
    fn canonical_tamper_is_rejected_before_runtime_gates() {
        let signing_key = key();
        let mut bytes = signed_binding(&signing_key);
        let closing = bytes.pop().expect("json closing brace");
        bytes.extend_from_slice(b" ");
        bytes.push(closing);
        assert!(matches!(
            store(&signing_key).verify(&bytes, &scope(&signing_key), &now(NOW)),
            Err(InvocationAdmissionVerificationError::CanonicalDecode(_))
        ));
    }

    #[test]
    fn trust_store_rejects_conflicting_key_replacement() {
        let mut store = InvocationAdmissionTrustStoreV1::new();
        store
            .add("test-key", VerifyingKey::from(&key()).to_bytes())
            .expect("first pin");
        let error = store
            .add(
                "test-key",
                VerifyingKey::from(&SigningKey::from_bytes(&[8u8; 32])).to_bytes(),
            )
            .expect_err("conflicting pin");
        assert!(matches!(
            error,
            InvocationAdmissionVerificationError::InvalidTrustedKey(_)
        ));
    }

    #[test]
    fn invalid_key_ids_are_rejected() {
        assert!(InvocationAdmissionTrustedKeyV1::new("base64:secret", [1u8; 32]).is_err());
        assert!(InvocationAdmissionTrustedKeyV1::new("", [1u8; 32]).is_err());
    }
}
