// SPDX-License-Identifier: Apache-2.0
//! One organization-policy verification over captured bytes.
//!
//! The caller must admit the exact capability through an authenticated runtime
//! invocation and hand `accept` only that invocation's checked response output.
//! An announced key or a valid signature alone authenticates no runtime and
//! grants no entitlement. No caller JSON, stdout, cache or other operation may
//! supply this output.
//!
//! The caller owns the whole serialized exchange-frame budget, a fresh clock,
//! one-use consumption of PendingVerification, and marker predecessor CAS.
//! Persist the accepted marker transition before using the floor. Nothing here
//! creates, resets or enrolls a marker, or stores a key across invocations.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use lean_ctx_protocol::org_policy_verifier::{
    ORG_POLICY_VERIFY_REQUEST_SCHEMA_ID, OrgPolicyVerifyEnvelopeV1, OrgPolicyVerifyOutcomeV1,
    OrgPolicyVerifyRequestV1,
};
use lean_ctx_protocol::runtime_exchange::RuntimeExchangeProfile;
use lean_ctx_protocol::{ContentDigest, KeyId, NonceDigest, RecordDigest, TeamId, TeamTimestampV1};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub(crate) const CAPABILITY_ID: &str = "enterprise.runtime.org_policy_verifier";
pub(crate) const CAPABILITY_VERSION: &str = "1.0.0";

/// Exact captured authority bytes, never re-read by these functions.
pub(crate) struct Snapshot<'a> {
    pub organization: &'a TeamId,
    pub artifact: &'a [u8],
    pub trust: Option<&'a [u8]>,
    pub marker: Option<&'a [u8]>,
}

/// Retain for this invocation only. The caller consumes it once; no cache.
/// No Debug implementation: the input contains captured policy/trust bytes.
pub(crate) struct PendingVerification {
    pub request: OrgPolicyVerifyRequestV1,
    pub input: String,
}

/// The caller must persist the forward-only marker before using this result.
/// No Debug implementation: the enforced pack may contain protected content.
pub(crate) enum Accepted {
    Enforce {
        pack_toml: String,
        policy_digest: RecordDigest,
        sequence: u64,
        valid_until: TeamTimestampV1,
    },
    NotRequired {
        unpin_digest: RecordDigest,
        sequence: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum VerifierCallError {
    #[error("organization policy snapshot is invalid")]
    SnapshotInvalid,
    #[error("organization policy verification exceeds the exchange budget")]
    TooLargeForExchange,
    #[error("organization policy verification entropy is unavailable")]
    Entropy,
    #[error("organization policy verifier output is malformed")]
    OutputMalformed,
    #[error("organization policy verifier key does not match")]
    KeyMismatch,
    #[error("organization policy verifier signature is invalid")]
    SignatureInvalid,
    #[error("organization policy verifier answer is not bound")]
    NotBound,
    #[error("organization policy verifier refused")]
    Refused,
    #[error("no organization policy verifier is available")]
    Unavailable,
    #[error("organization policy verifier channel failed")]
    Channel,
    #[error("organization policy verification ran out of time")]
    Deadline,
}

/// Time the Runtime has for one answer.
const VERIFY_BUDGET_MS: i64 = 5000;
/// The one size profile of this operation. The session selects the same one
/// from the authenticated package; here it bounds what is built and accepted.
const PROFILE: RuntimeExchangeProfile = RuntimeExchangeProfile::ORG_POLICY_VERIFIER;
/// Largest captured artifact: the profile's own artifact allowance.
const MAX_ARTIFACT_BYTES: usize = PROFILE.max_output_bytes();
/// Largest captured trust store or marker.
const MAX_SNAPSHOT_BYTES: usize =
    lean_ctx_protocol::org_policy_verifier::MAX_ORG_REQUIRED_MARKER_BYTES;

/// One verification of `snapshot` through the configured Runtime.
///
/// The package is authenticated for this invocation; the request is built
/// from that package and admitted only if it declares the verifier inside a
/// complete capability set; the answer is taken from the checked response of
/// that same request and from nowhere else. Every failure is an error: the
/// caller treats it as "policy not established".
#[cfg(any(unix, windows))]
pub(crate) fn verify(
    snapshot: &Snapshot<'_>,
    not_after: std::time::Instant,
) -> Result<Accepted, VerifierCallError> {
    use super::InstallError;
    use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};

    let config = crate::core::config::Config::load_global().intelligence_runtime;
    if !config.enabled || !config.accept_proprietary || !super::bootstrap::permits_config(&config) {
        return Err(VerifierCallError::Unavailable);
    }
    let root = std::path::Path::new(&config.root);
    let trust =
        super::trust_key(&config.trust_key_hex).map_err(|_| VerifierCallError::Unavailable)?;
    let stamp = |at: chrono::DateTime<chrono::Utc>| {
        TeamTimestampV1::new(at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
            .map_err(|_| VerifierCallError::SnapshotInvalid)
    };
    // The Runtime gets what the caller has left, never more than its own
    // budget: the answer must be back before the caller's bound, not after.
    let left = not_after.saturating_duration_since(std::time::Instant::now());
    let budget_ms = i64::try_from(left.as_millis())
        .unwrap_or(VERIFY_BUDGET_MS)
        .min(VERIFY_BUDGET_MS);
    if budget_ms == 0 {
        return Err(VerifierCallError::Deadline);
    }
    let started = chrono::Utc::now();
    let deadline = started + chrono::Duration::milliseconds(budget_ms);
    let (requested_at, answer_by) = (stamp(started)?, stamp(deadline)?);
    let deadline_unix_ms = u64::try_from(deadline.timestamp_millis())
        .map_err(|_| VerifierCallError::SnapshotInvalid)?;

    let mut pending = None;
    let mut refusal = None;
    let invoked = super::session::invoke_built(root, &config.manifest_sha256, &trust, |package| {
        if !super::health::supports_org_policy_verifier(package) {
            refusal = Some(VerifierCallError::Unavailable);
            return Err(InstallError::InvocationAdmission);
        }
        let budget = PROFILE.max_input_bytes();
        let begun = begin(snapshot, &requested_at, &answer_by, budget).map_err(|error| {
            refusal = Some(error);
            InstallError::Exchange
        })?;
        let binding = sha256_hex(begun.input.as_bytes());
        let name = format!("orgverify-{binding}");
        let input_ref = format!("projection:sha256:{binding}");
        let request: RuntimeRequestV1 = serde_json::from_value(serde_json::json!({
            "protocol_version": lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION,
            "session_id": name, "request_id": name,
            "sequence": 1, "deadline_unix_ms": deadline_unix_ms, "input": begun.input,
            "invocation": {"schema_version": 1, "invocation_id": name,
                "engine": {"engine_id": "leanctx-intelligence", "engine_version": package.receipt.version},
                "operation": {"capability_id": CAPABILITY_ID, "capability_version": CAPABILITY_VERSION},
                "input_ref": input_ref, "input_digest": format!("sha256:{binding}"), "source_refs": [input_ref],
                "policy_admission": {"decision": "admitted", "policy_ref": format!("policy:{name}")}}
        }))
        .map_err(|_| InstallError::Exchange)?;
        // The whole frame, not only the input: a request that does not fit
        // is refused here, before a process is started.
        if request.to_bytes_in(PROFILE).is_err() {
            refusal = Some(VerifierCallError::TooLargeForExchange);
            return Err(InstallError::Exchange);
        }
        pending = Some(begun);
        Ok(request)
    });
    let Ok((request, executed)) = invoked else {
        return Err(refusal.unwrap_or(VerifierCallError::Channel));
    };
    let pending = pending.ok_or(VerifierCallError::Channel)?;
    let response: RuntimeResponseV1 = serde_json::from_value(executed["response"].clone())
        .map_err(|_| VerifierCallError::Channel)?;
    response
        .validate_for_in(&request, PROFILE)
        .map_err(|_| VerifierCallError::Channel)?;
    // The configuration and the active package must still be the ones this
    // invocation was authenticated under.
    if response.observation.status != lean_ctx_protocol::EngineObservationStatusV1::Succeeded
        || crate::core::config::Config::load_global().intelligence_runtime != config
        || super::install::active_manifest(root).ok().as_deref()
            != Some(config.manifest_sha256.as_str())
    {
        return Err(VerifierCallError::Channel);
    }
    let output = response.output.ok_or(VerifierCallError::Channel)?;
    let accepted = accept(&pending, &output, &stamp(chrono::Utc::now())?)?;
    if std::time::Instant::now() >= not_after {
        return Err(VerifierCallError::Deadline);
    }
    Ok(accepted)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn verify(
    _: &Snapshot<'_>,
    _: std::time::Instant,
) -> Result<Accepted, VerifierCallError> {
    Err(VerifierCallError::Unavailable)
}

/// Whether an organization policy could be verified here now: the Runtime is
/// enabled and accepted, its active package authenticates and declares the
/// verifier, and the runtime would be licensed for it — a package that needs
/// no entitlement, a commercial licence configuration, or a Hive lease for
/// a package that takes one. Reads local state only — no process, no
/// install, no network. It re-authenticates the package, so callers should
/// not ask per request.
#[cfg(any(unix, windows))]
pub(crate) fn org_policy_verifier_available() -> bool {
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    if !config.enabled || !config.accept_proprietary || !super::bootstrap::permits_config(&config) {
        return false;
    }
    let Ok(trust) = super::trust_key(&config.trust_key_hex) else {
        return false;
    };
    let root = std::path::Path::new(&config.root);
    let Ok(package) = super::install::verified_active(root, &config.manifest_sha256, &trust) else {
        return false;
    };
    super::health::supports_org_policy_verifier(&package)
        && (package.description["entitlement_required"] != true
            || super::license::configuration_path(root, &config.manifest_sha256, &trust).is_ok()
            || super::session::runtime_lease_b64(&package).is_some())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn org_policy_verifier_available() -> bool {
    false
}

#[derive(Serialize)]
struct Input<'a> {
    request: &'a OrgPolicyVerifyRequestV1,
    artifact: String,
    trust: Option<String>,
    marker: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    envelope: OrgPolicyVerifyEnvelopeV1,
    verifier_public_key: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        result.push(char::from(DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    result
}

fn snapshot_digest(bytes: &[u8]) -> Result<ContentDigest, VerifierCallError> {
    ContentDigest::new(format!("sha256:{}", sha256_hex(bytes)))
        .map_err(|_| VerifierCallError::SnapshotInvalid)
}

/// Interface key-id formula, specified in the caller contract. No private
/// crate is imported: SHA-256 of the actual key bytes, first32 hex characters.
fn derived_key_id(bytes: &[u8; 32]) -> Result<KeyId, VerifierCallError> {
    KeyId::new(format!("key:ed25519-{}", &sha256_hex(bytes)[..32]))
        .map_err(|_| VerifierCallError::KeyMismatch)
}

fn encoded_size(bytes: usize) -> Option<usize> {
    bytes.checked_add(2)?.checked_div(3)?.checked_mul(4)
}

/// Build one request with 32 bytes of OS entropy, fixed local expectations and
/// the private adapter's bounded base64 input. Each captured snapshot has its
/// own ceiling and the input as a whole has the verifier profile's; whatever
/// exceeds either is refused explicitly, before anything is hashed or encoded.
///
/// `input_budget` is only the inner input allowance. The caller must still
/// check the complete serialized RuntimeRequestV1 after adding its own fields.
pub(crate) fn begin(
    snapshot: &Snapshot<'_>,
    now: &TeamTimestampV1,
    deadline: &TeamTimestampV1,
    input_budget: usize,
) -> Result<PendingVerification, VerifierCallError> {
    let budget = input_budget.min(PROFILE.max_input_bytes());
    if snapshot.artifact.len() > MAX_ARTIFACT_BYTES
        || [snapshot.trust, snapshot.marker]
            .into_iter()
            .flatten()
            .any(|bytes| bytes.len() > MAX_SNAPSHOT_BYTES)
    {
        return Err(VerifierCallError::TooLargeForExchange);
    }
    let mut minimum = 0_usize;
    for bytes in [Some(snapshot.artifact), snapshot.trust, snapshot.marker]
        .into_iter()
        .flatten()
    {
        minimum = minimum
            .checked_add(encoded_size(bytes.len()).ok_or(VerifierCallError::TooLargeForExchange)?)
            .ok_or(VerifierCallError::TooLargeForExchange)?;
    }
    // Reject oversized captured strings before hashing/base64 allocation.
    if minimum > budget {
        return Err(VerifierCallError::TooLargeForExchange);
    }
    let mut entropy = [0_u8; 32];
    getrandom::fill(&mut entropy).map_err(|_| VerifierCallError::Entropy)?;
    let request = OrgPolicyVerifyRequestV1 {
        schema: ORG_POLICY_VERIFY_REQUEST_SCHEMA_ID.into(),
        organization_id: snapshot.organization.clone(),
        artifact_digest: snapshot_digest(snapshot.artifact)?,
        trust_digest: snapshot.trust.map(snapshot_digest).transpose()?,
        marker_digest: snapshot.marker.map(snapshot_digest).transpose()?,
        nonce: NonceDigest::new(format!("sha256:{}", sha256_hex(&entropy)))
            .map_err(|_| VerifierCallError::SnapshotInvalid)?,
        requested_at: now.clone(),
        deadline: deadline.clone(),
    };
    request
        .validate()
        .map_err(|_| VerifierCallError::SnapshotInvalid)?;
    let input = serde_json::to_string(&Input {
        request: &request,
        artifact: STANDARD.encode(snapshot.artifact),
        trust: snapshot.trust.map(|bytes| STANDARD.encode(bytes)),
        marker: snapshot.marker.map(|bytes| STANDARD.encode(bytes)),
    })
    .map_err(|_| VerifierCallError::SnapshotInvalid)?;
    if input.len() > budget {
        return Err(VerifierCallError::TooLargeForExchange);
    }
    Ok(PendingVerification { request, input })
}

/// Accept only the checked output of the same authenticated, admitted runtime
/// invocation that used `pending.input`. Authentication and full outer-frame
/// validation precede this function; this parser cannot establish either.
/// The returned key is scoped to these bytes only, never registered or cached.
pub(crate) fn accept(
    pending: &PendingVerification,
    output: &str,
    now: &TeamTimestampV1,
) -> Result<Accepted, VerifierCallError> {
    if output.len() > PROFILE.max_output_bytes() {
        return Err(VerifierCallError::TooLargeForExchange);
    }
    // Typed structs and outcomes deny unknown fields and repeated fields;
    // parsing directly preserves duplicate detection, unlike a Value roundtrip.
    let output: Output =
        serde_json::from_str(output).map_err(|_| VerifierCallError::OutputMalformed)?;
    if output.verifier_public_key.len() != 43 {
        return Err(VerifierCallError::KeyMismatch);
    }
    let key_bytes: [u8; 32] = URL_SAFE_NO_PAD
        .decode(&output.verifier_public_key)
        .map_err(|_| VerifierCallError::KeyMismatch)?
        .try_into()
        .map_err(|_| VerifierCallError::KeyMismatch)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| VerifierCallError::KeyMismatch)?;
    let envelope = output.envelope;
    if derived_key_id(&key_bytes)? != envelope.response.verifier_key_id {
        return Err(VerifierCallError::KeyMismatch);
    }
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(envelope.signature.as_str())
        .map_err(|_| VerifierCallError::SignatureInvalid)?
        .try_into()
        .map_err(|_| VerifierCallError::SignatureInvalid)?;
    let message = envelope
        .response
        .signing_bytes()
        .map_err(|_| VerifierCallError::OutputMalformed)?;
    key.verify_strict(&message, &Signature::from_bytes(&signature))
        .map_err(|_| VerifierCallError::SignatureInvalid)?;
    envelope
        .response
        .binds(&pending.request, now)
        .map_err(|_| VerifierCallError::NotBound)?;
    match envelope.response.outcome {
        OrgPolicyVerifyOutcomeV1::Enforce {
            pack_toml,
            policy_digest,
            sequence,
            valid_until,
        } => Ok(Accepted::Enforce {
            pack_toml,
            policy_digest,
            sequence,
            valid_until,
        }),
        OrgPolicyVerifyOutcomeV1::NotRequired {
            unpin_digest,
            sequence,
        } => Ok(Accepted::NotRequired {
            unpin_digest,
            sequence,
        }),
        OrgPolicyVerifyOutcomeV1::Refuse { .. } => Err(VerifierCallError::Refused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use lean_ctx_protocol::org_policy_verifier::{
        ORG_POLICY_VERIFY_RESPONSE_SCHEMA_ID, OrgPolicyVerifyResponseV1,
    };
    use lean_ctx_protocol::runtime_exchange::MAX_RUNTIME_EXCHANGE_BYTES;
    use lean_ctx_protocol::{Ed25519Signature, ReasonCode};
    use serde_json::{Value, json};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
    const NOW: &str = "2026-10-10T09:00:01Z";

    /// The probe answers from local state only: without an enabled, accepted
    /// and installed Runtime it is false, and asking it installs nothing.
    #[test]
    fn verifier_probe_is_false_without_an_enabled_installed_runtime() -> TestResult {
        let _data = crate::core::data_dir::isolated_data_dir();
        let runtime = tempfile::tempdir()?;
        let root = runtime.path().join("runtime");
        let config = crate::core::config::Config::path().ok_or("config path")?;
        std::fs::create_dir_all(config.parent().ok_or("config dir")?)?;
        for section in [
            String::new(),
            "[intelligence_runtime]\nenabled = true\n".to_string(),
            format!(
                "[intelligence_runtime]\nenabled = true\naccept_proprietary = true\nroot = {:?}\nmanifest_sha256 = \"{}\"\ntrust_key_hex = \"{}\"\n",
                root.display().to_string(),
                "a".repeat(64),
                "b".repeat(64)
            ),
        ] {
            std::fs::write(&config, section)?;
            assert!(!org_policy_verifier_available());
        }
        assert!(!root.exists(), "the probe must not create the runtime root");
        Ok(())
    }

    fn at(value: &str) -> TestResult<TeamTimestampV1> {
        Ok(TeamTimestampV1::new(value)?)
    }

    fn pending() -> TestResult<PendingVerification> {
        let organization = TeamId::new("acme")?;
        Ok(begin(
            &Snapshot {
                organization: &organization,
                artifact: b"abc",
                trust: Some(b""),
                marker: Some(b"hello"),
            },
            &at("2026-10-10T09:00:00Z")?,
            &at("2026-10-10T09:00:02Z")?,
            MAX_RUNTIME_EXCHANGE_BYTES,
        )?)
    }

    fn digest(character: char) -> TestResult<RecordDigest> {
        Ok(RecordDigest::new(format!(
            "sha256:{}",
            character.to_string().repeat(64)
        ))?)
    }

    fn enforce() -> TestResult<OrgPolicyVerifyOutcomeV1> {
        Ok(OrgPolicyVerifyOutcomeV1::Enforce {
            pack_toml: "name = \"floor\"\n".into(),
            policy_digest: digest('a')?,
            sequence: 7,
            valid_until: at("2026-11-01T00:00:00Z")?,
        })
    }

    fn response(
        pending: &PendingVerification,
        key: &SigningKey,
        outcome: OrgPolicyVerifyOutcomeV1,
    ) -> TestResult<OrgPolicyVerifyResponseV1> {
        let request = &pending.request;
        Ok(OrgPolicyVerifyResponseV1 {
            schema: ORG_POLICY_VERIFY_RESPONSE_SCHEMA_ID.into(),
            request_digest: request.digest()?,
            organization_id: request.organization_id.clone(),
            artifact_digest: request.artifact_digest.clone(),
            trust_digest: request.trust_digest.clone(),
            marker_digest: request.marker_digest.clone(),
            nonce: request.nonce.clone(),
            answered_at: at(NOW)?,
            verifier_key_id: derived_key_id(&key.verifying_key().to_bytes())?,
            outcome,
        })
    }

    fn signed(response: OrgPolicyVerifyResponseV1, key: &SigningKey) -> TestResult<String> {
        let signature = key.sign(&response.signing_bytes()?).to_bytes();
        Ok(json!({
            "envelope": OrgPolicyVerifyEnvelopeV1 {
                response,
                signature: Ed25519Signature::new(URL_SAFE_NO_PAD.encode(signature))?,
            },
            "verifier_public_key": URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
        })
        .to_string())
    }

    #[test]
    fn begin_binds_exact_bytes_and_absence_with_fresh_nonce_and_exact_budget() -> TestResult {
        let first = pending()?;
        let second = pending()?;
        assert_ne!(first.request.nonce, second.request.nonce);
        // Independent, fixed SHA-256 vectors: abc, empty bytes and hello.
        assert_eq!(
            first.request.artifact_digest.as_str(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            first
                .request
                .trust_digest
                .as_ref()
                .ok_or("missing empty trust")?
                .as_str(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            first
                .request
                .marker_digest
                .as_ref()
                .ok_or("missing marker")?
                .as_str(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let input: Value = serde_json::from_str(&first.input)?;
        for (field, expected) in [
            ("artifact", b"abc".as_slice()),
            ("trust", b"".as_slice()),
            ("marker", b"hello".as_slice()),
        ] {
            let encoded = input[field].as_str().ok_or("snapshot encoding missing")?;
            assert_eq!(STANDARD.decode(encoded)?, expected);
        }
        assert_eq!(first.request.requested_at.as_str(), "2026-10-10T09:00:00Z");
        assert_eq!(first.request.deadline.as_str(), "2026-10-10T09:00:02Z");
        let organization = TeamId::new("acme")?;
        let snapshot = Snapshot {
            organization: &organization,
            artifact: b"abc",
            trust: Some(b""),
            marker: Some(b"hello"),
        };
        let now = at("2026-10-10T09:00:00Z")?;
        let deadline = at("2026-10-10T09:00:02Z")?;
        assert_eq!(
            begin(&snapshot, &now, &deadline, first.input.len())?
                .input
                .len(),
            first.input.len()
        );
        assert_eq!(
            begin(&snapshot, &now, &deadline, first.input.len() - 1).err(),
            Some(VerifierCallError::TooLargeForExchange)
        );
        assert_eq!(
            begin(&snapshot, &now, &now, MAX_RUNTIME_EXCHANGE_BYTES).err(),
            Some(VerifierCallError::SnapshotInvalid)
        );
        let absent = begin(
            &Snapshot {
                organization: &organization,
                artifact: b"abc",
                trust: None,
                marker: None,
            },
            &now,
            &deadline,
            MAX_RUNTIME_EXCHANGE_BYTES,
        )?;
        assert!(absent.request.trust_digest.is_none());
        assert!(absent.request.marker_digest.is_none());
        let input: Value = serde_json::from_str(&absent.input)?;
        assert!(input["trust"].is_null());
        assert!(input["marker"].is_null());
        // Each captured snapshot has its own ceiling, whatever budget is
        // named: exactly at it the request is built, one byte over it is not;
        // and a megabyte artifact does not fit a megabyte budget.
        let wide = |artifact: usize, trust: usize, budget: usize| {
            begin(
                &Snapshot {
                    organization: &organization,
                    artifact: &vec![b'#'; artifact],
                    trust: Some(&vec![b'#'; trust]),
                    marker: None,
                },
                &now,
                &deadline,
                budget,
            )
            .map(|pending| pending.input.len())
        };
        let widest = wide(MAX_ARTIFACT_BYTES, MAX_SNAPSHOT_BYTES, usize::MAX)?;
        assert!(widest > MAX_RUNTIME_EXCHANGE_BYTES && widest <= PROFILE.max_input_bytes());
        for (artifact, trust, budget) in [
            (MAX_ARTIFACT_BYTES + 1, 0, usize::MAX),
            (3, MAX_SNAPSHOT_BYTES + 1, usize::MAX),
            (MAX_RUNTIME_EXCHANGE_BYTES, 0, MAX_RUNTIME_EXCHANGE_BYTES),
        ] {
            assert_eq!(
                wide(artifact, trust, budget).err(),
                Some(VerifierCallError::TooLargeForExchange)
            );
        }
        Ok(())
    }

    #[test]
    fn accepted_outcomes_keep_the_signed_floor_and_unpin() -> TestResult {
        let pending = pending()?;
        // TEST key, constructed here; never runtime or production material.
        let key = SigningKey::from_bytes(&[41; 32]);
        let output = signed(response(&pending, &key, enforce()?)?, &key)?;
        match accept(&pending, &output, &at(NOW)?)? {
            Accepted::Enforce {
                pack_toml,
                policy_digest,
                sequence,
                valid_until,
            } => {
                assert_eq!(pack_toml, "name = \"floor\"\n");
                assert_eq!(policy_digest, digest('a')?);
                assert_eq!(sequence, 7);
                assert_eq!(valid_until.as_str(), "2026-11-01T00:00:00Z");
            }
            Accepted::NotRequired { .. } => return Err("enforce became unpin".into()),
        }
        let unpin = OrgPolicyVerifyOutcomeV1::NotRequired {
            unpin_digest: digest('b')?,
            sequence: 8,
        };
        let output = signed(response(&pending, &key, unpin)?, &key)?;
        match accept(&pending, &output, &at(NOW)?)? {
            Accepted::NotRequired {
                unpin_digest,
                sequence,
            } => {
                assert_eq!(unpin_digest, digest('b')?);
                assert_eq!(sequence, 8);
            }
            Accepted::Enforce { .. } => return Err("unpin became enforce".into()),
        }
        Ok(())
    }

    #[test]
    fn mutually_signed_foreign_answers_cannot_replace_any_pending_binding() -> TestResult {
        let pending = pending()?;
        // Each forged tuple has a real signature from its announced TEST key.
        let key = SigningKey::from_bytes(&[17; 32]);
        for changed in 0..6 {
            let mut answer = response(&pending, &key, enforce()?)?;
            match changed {
                0 => answer.nonce = NonceDigest::new(format!("sha256:{}", "e".repeat(64)))?,
                1 => answer.organization_id = TeamId::new("other-org")?,
                2 => {
                    answer.artifact_digest =
                        ContentDigest::new(format!("sha256:{}", "e".repeat(64)))?;
                }
                3 => answer.trust_digest = None,
                4 => answer.marker_digest = None,
                _ => answer.request_digest = digest('e')?,
            }
            let output = signed(answer, &key)?;
            assert_eq!(
                accept(&pending, &output, &at(NOW)?).err(),
                Some(VerifierCallError::NotBound),
                "changed binding {changed}"
            );
        }
        Ok(())
    }

    #[test]
    fn mismatched_key_wrong_signature_and_signed_refusal_never_accept() -> TestResult {
        let pending = pending()?;
        let key = SigningKey::from_bytes(&[41; 32]);
        let other = SigningKey::from_bytes(&[17; 32]);
        let mut answer = response(&pending, &key, enforce()?)?;
        answer.verifier_key_id = derived_key_id(&other.verifying_key().to_bytes())?;
        assert_eq!(
            accept(&pending, &signed(answer, &key)?, &at(NOW)?).err(),
            Some(VerifierCallError::KeyMismatch)
        );
        let answer = response(&pending, &key, enforce()?)?;
        let signature = other.sign(&answer.signing_bytes()?).to_bytes();
        let output = json!({
            "envelope": OrgPolicyVerifyEnvelopeV1 {
                response: answer,
                signature: Ed25519Signature::new(URL_SAFE_NO_PAD.encode(signature))?,
            },
            "verifier_public_key": URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
        })
        .to_string();
        assert_eq!(
            accept(&pending, &output, &at(NOW)?).err(),
            Some(VerifierCallError::SignatureInvalid)
        );
        let refusal = OrgPolicyVerifyOutcomeV1::Refuse {
            reason: ReasonCode::new("high_water_mark_missing")?,
        };
        assert_eq!(
            accept(
                &pending,
                &signed(response(&pending, &key, refusal)?, &key)?,
                &at(NOW)?,
            )
            .err(),
            Some(VerifierCallError::Refused)
        );
        Ok(())
    }

    #[test]
    fn fresh_host_clock_checks_request_window_and_exclusive_policy_expiry() -> TestResult {
        let pending = pending()?;
        let key = SigningKey::from_bytes(&[41; 32]);
        for answered_at in ["2026-10-10T08:59:59Z", "2026-10-10T09:00:03Z"] {
            let mut answer = response(&pending, &key, enforce()?)?;
            answer.answered_at = at(answered_at)?;
            assert_eq!(
                accept(&pending, &signed(answer, &key)?, &at(NOW)?).err(),
                Some(VerifierCallError::NotBound)
            );
        }
        let output = signed(response(&pending, &key, enforce()?)?, &key)?;
        for now in ["2026-10-10T09:00:00Z", "2026-10-10T09:00:03Z"] {
            assert_eq!(
                accept(&pending, &output, &at(now)?).err(),
                Some(VerifierCallError::NotBound)
            );
        }
        let ending = OrgPolicyVerifyOutcomeV1::Enforce {
            pack_toml: "name = \"floor\"\n".into(),
            policy_digest: digest('a')?,
            sequence: 7,
            valid_until: at("2026-10-10T09:00:01.500Z")?,
        };
        let output = signed(response(&pending, &key, ending)?, &key)?;
        assert!(accept(&pending, &output, &at("2026-10-10T09:00:01.499Z")?).is_ok());
        for now in ["2026-10-10T09:00:01.500Z", "2026-10-10T09:00:01.900Z"] {
            assert_eq!(
                accept(&pending, &output, &at(now)?).err(),
                Some(VerifierCallError::NotBound)
            );
        }
        Ok(())
    }

    #[test]
    fn strict_wrapper_rejects_unknown_duplicate_bad_key_and_oversized_output() -> TestResult {
        let pending = pending()?;
        let key = SigningKey::from_bytes(&[41; 32]);
        let output = signed(response(&pending, &key, enforce()?)?, &key)?;
        for level in 0..4 {
            let mut value: Value = serde_json::from_str(&output)?;
            let target = match level {
                0 => &mut value,
                1 => &mut value["envelope"],
                2 => &mut value["envelope"]["response"],
                _ => &mut value["envelope"]["response"]["outcome"],
            };
            target["unexpected"] = "protected value".into();
            assert_eq!(
                accept(&pending, &value.to_string(), &at(NOW)?).err(),
                Some(VerifierCallError::OutputMalformed),
                "unknown level {level}"
            );
        }
        let public = URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes());
        let nonce = pending.request.nonce.as_str();
        // Raw text is required: Value parsing would erase duplicates.
        for field in [
            format!("\"verifier_public_key\":\"{public}\""),
            format!("\"nonce\":\"{nonce}\""),
            "\"kind\":\"enforce\"".into(),
            "\"sequence\":7".into(),
        ] {
            assert_eq!(output.matches(field.as_str()).count(), 1);
            let duplicate = output.replacen(field.as_str(), &format!("{field},{field}"), 1);
            assert_eq!(
                accept(&pending, &duplicate, &at(NOW)?).err(),
                Some(VerifierCallError::OutputMalformed),
                "duplicate field {field}"
            );
        }
        for bytes in [vec![0_u8; 31], vec![0_u8; 33]] {
            let mut value: Value = serde_json::from_str(&output)?;
            value["verifier_public_key"] = URL_SAFE_NO_PAD.encode(bytes).into();
            assert_eq!(
                accept(&pending, &value.to_string(), &at(NOW)?).err(),
                Some(VerifierCallError::KeyMismatch)
            );
        }
        assert_eq!(
            accept(
                &pending,
                &" ".repeat(PROFILE.max_output_bytes() + 1),
                &at(NOW)?
            )
            .err(),
            Some(VerifierCallError::TooLargeForExchange)
        );
        assert_eq!(
            accept(&pending, "{", &at(NOW)?).err(),
            Some(VerifierCallError::OutputMalformed)
        );
        Ok(())
    }

    #[test]
    fn key_id_matches_the_private_interface_formula_fixed_vector() -> TestResult {
        // SHA-256 of 32 zero bytes: 66687aadf862bd776c8fc18b8e9f8e200...
        // Compare the specified private standard formula without importing EE.
        assert_eq!(
            derived_key_id(&[0_u8; 32])?.as_str(),
            "key:ed25519-66687aadf862bd776c8fc18b8e9f8e20"
        );
        Ok(())
    }
}
