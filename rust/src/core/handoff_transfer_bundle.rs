use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::core::handoff_ledger::HandoffLedgerV1;

const MAX_BUNDLE_BYTES: usize = 350_000;
const MAX_PROOF_FILES: usize = 50;
const MAX_ARTIFACT_ITEMS: usize = 80;
const MAX_LEDGER_SNAPSHOT_CHARS: usize = 80_000;
const MAX_CURATED_REF_CHARS: usize = 20_000;
const MAX_DECISION_CHARS: usize = 2_000;
const MAX_FINDING_CHARS: usize = 2_000;
const MAX_NEXT_STEP_CHARS: usize = 1_000;
const MAX_TASK_CHARS: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BundlePrivacyV1 {
    Redacted,
    Full,
}

impl BundlePrivacyV1 {
    pub(crate) fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("redacted").trim().to_lowercase().as_str() {
            "full" => Self::Full,
            _ => Self::Redacted,
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Redacted => "redacted",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffTransferBundleV1 {
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    pub privacy: String,
    pub project: ProjectIdentityV1,
    pub ledger: HandoffLedgerV1,
    pub artifacts: ArtifactsExcerptV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_public_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_agent_id: Option<String>,
    /// Signature scheme. [`BOUND_SIGNATURE_SCHEME`] covers `signer_agent_id`
    /// and `signer_public_key` (GHSA-gvg8-285q-m62g); absent means a legacy
    /// signature over the payload only, whose signer id is not bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_scheme: Option<String>,
}

/// Signature over the whole bundle except `signature` itself.
pub(crate) const BOUND_SIGNATURE_SCHEME: &str = "ed25519-bound-v2";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectIdentityV1 {
    pub project_root_hash: Option<String>,
    pub project_identity_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtifactsExcerptV1 {
    pub resolved: Vec<crate::core::artifacts::ResolvedArtifact>,
    pub proof_files: Vec<ProofFileV1>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofFileV1 {
    pub name: String,
    pub md5: String,
    pub bytes: u64,
}

pub(crate) fn build_bundle_v1(
    mut ledger: HandoffLedgerV1,
    project_root: Option<&str>,
    privacy: BundlePrivacyV1,
) -> HandoffTransferBundleV1 {
    let role_name = crate::core::roles::active_role_name();
    let effective_privacy = match privacy {
        BundlePrivacyV1::Full
            if role_name == "admin"
                && !crate::core::redaction::redaction_enabled_for_active_role() =>
        {
            BundlePrivacyV1::Full
        }
        _ => BundlePrivacyV1::Redacted,
    };

    let (project_root_hash, project_identity_hash) = project_root.map_or((None, None), |root| {
        let root_hash = crate::core::project_hash::hash_project_root(root);
        let identity = crate::core::project_hash::project_identity(root);
        let identity_hash = identity.as_deref().map(crate::core::hasher::hash_str);
        (Some(root_hash), identity_hash)
    });

    cap_ledger_in_place(&mut ledger);

    match effective_privacy {
        BundlePrivacyV1::Full => {}
        BundlePrivacyV1::Redacted => {
            redact_ledger_in_place(&mut ledger);
        }
    }

    // Keep embedded ledger internally consistent.
    ledger.content_md5 = crate::core::handoff_ledger::compute_content_md5_for_ledger(&ledger);

    let artifacts = project_root
        .map(Path::new)
        .map(build_artifacts_excerpt_v1)
        .unwrap_or_default();

    // Built unsigned by design: signing is the exporter's responsibility
    // (`ctx_handoff export` signs with the real agent identity, GL #465).
    // The old implicit role-name signing here conflated role with identity
    // and silently swallowed failures.
    HandoffTransferBundleV1 {
        schema_version: crate::core::contracts::HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION,
        exported_at: Utc::now(),
        privacy: effective_privacy.as_str().to_string(),
        project: ProjectIdentityV1 {
            project_root_hash,
            project_identity_hash,
        },
        ledger,
        artifacts,
        signature: None,
        signer_public_key: None,
        signer_agent_id: None,
        signature_scheme: None,
    }
}

pub(crate) fn sign_bundle(
    bundle: &mut HandoffTransferBundleV1,
    agent_id: &str,
) -> Result<(), String> {
    // One atomic key resolution — signature and embedded public key must come
    // from the same keypair (two separate lookups raced with concurrent
    // data-dir changes and produced unverifiable bundles).
    let key = crate::core::agent_identity::get_or_create_keypair(agent_id)?;

    // GHSA-gvg8-285q-m62g: signer id, public key and scheme are part of the
    // signed payload, so relabelling the signer breaks the signature.
    bundle.signature = None;
    bundle.signer_public_key = Some(crate::core::agent_identity::hex_encode(
        &key.verifying_key().to_bytes(),
    ));
    bundle.signer_agent_id = Some(agent_id.to_string());
    bundle.signature_scheme = Some(BOUND_SIGNATURE_SCHEME.to_string());

    let canonical =
        serde_json::to_string(bundle).map_err(|e| format!("serialize for signing: {e}"))?;
    let sig_bytes = crate::core::agent_identity::sign_bytes_with(&key, canonical.as_bytes());
    bundle.signature = Some(crate::core::agent_identity::hex_encode(&sig_bytes));
    Ok(())
}

/// A signature that verified against the bundle's embedded public key.
///
/// Integrity only: the key travels inside the bundle, so `claimed_signer` is
/// an identity *claim* unless `trusted` is set (GHSA-gvg8-285q-m62g).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedSignature {
    /// The bundle's `signer_agent_id`.
    pub claimed_signer: String,
    /// Fingerprint of the embedded public key (see `signer_trust::key_fingerprint`).
    pub key_fingerprint: String,
    /// `signer_agent_id` is covered by the signature (bound scheme). Legacy
    /// signatures leave it unbound, so anyone can relabel the signer.
    pub signer_bound: bool,
    /// The key is anchored for `claimed_signer` (pinned in the signer trust
    /// store, or held by a local agent identity). Never set when unbound.
    pub trusted: bool,
}

impl VerifiedSignature {
    /// Human-readable status that never claims identity without an anchor.
    pub(crate) fn describe(&self) -> String {
        let fp = &self.key_fingerprint;
        let who = &self.claimed_signer;
        if self.trusted {
            format!("verified (signer={who}, key {fp}, trusted)")
        } else if self.signer_bound {
            format!("integrity ok, signer key {fp} (untrusted; claimed signer={who})")
        } else {
            format!(
                "integrity ok, legacy signature (signer id not covered), signer key {fp} \
                 (untrusted; claimed signer={who})"
            )
        }
    }

    /// Stable machine status for JSON responses.
    #[cfg(any(test, feature = "http-server"))]
    pub(crate) fn status(&self) -> &'static str {
        if self.trusted {
            "verified"
        } else if self.signer_bound {
            "untrusted_signer"
        } else {
            "legacy_unbound"
        }
    }
}

/// Outcome of checking a bundle's embedded Ed25519 signature on import
/// (GL #465). Fail-closed semantics: any *present-but-broken* signature
/// material is [`Invalid`](BundleSignatureStatus::Invalid) and must block the
/// import; only the complete absence of all signature fields counts as
/// [`Unsigned`](BundleSignatureStatus::Unsigned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BundleSignatureStatus {
    /// Signature verifies against the embedded key; see [`VerifiedSignature`]
    /// for whether the signer identity is bound and trusted.
    Signed(VerifiedSignature),
    /// No signature fields at all.
    Unsigned,
    /// Signature material present but not verifiable — reject the bundle.
    Invalid(String),
}

/// Classify a bundle's signature for import-time enforcement (GL #465).
#[must_use]
pub(crate) fn check_bundle_signature(bundle: &HandoffTransferBundleV1) -> BundleSignatureStatus {
    if bundle.signature.is_none()
        && bundle.signer_public_key.is_none()
        && bundle.signer_agent_id.is_none()
        && bundle.signature_scheme.is_none()
    {
        return BundleSignatureStatus::Unsigned;
    }
    match verify_bundle_signature(bundle) {
        Ok(verified) => BundleSignatureStatus::Signed(verified),
        Err(e) => BundleSignatureStatus::Invalid(e),
    }
}

pub(crate) fn verify_bundle_signature(
    bundle: &HandoffTransferBundleV1,
) -> Result<VerifiedSignature, String> {
    let sig_hex = bundle
        .signature
        .as_deref()
        .ok_or_else(|| "bundle has no signature".to_string())?;
    let pk_hex = bundle
        .signer_public_key
        .as_deref()
        .ok_or_else(|| "bundle has no signer_public_key".to_string())?;
    let agent_id = bundle
        .signer_agent_id
        .as_deref()
        .ok_or_else(|| "bundle has no signer_agent_id".to_string())?;

    let sig_bytes = crate::core::agent_identity::hex_decode(sig_hex)?;
    let pk_bytes = crate::core::agent_identity::hex_decode(pk_hex)?;

    let mut verify_bundle = bundle.clone();
    verify_bundle.signature = None;
    let signer_bound = match bundle.signature_scheme.as_deref() {
        Some(BOUND_SIGNATURE_SCHEME) => true,
        None => {
            // Legacy scheme: signed before the signer fields were filled in.
            verify_bundle.signer_public_key = None;
            verify_bundle.signer_agent_id = None;
            false
        }
        Some(other) => return Err(format!("unsupported signature scheme `{other}`")),
    };

    let canonical =
        serde_json::to_string(&verify_bundle).map_err(|e| format!("serialize for verify: {e}"))?;

    if !crate::core::agent_identity::verify_signature(&pk_bytes, canonical.as_bytes(), &sig_bytes) {
        return Err("signature verification failed".to_string());
    }
    Ok(VerifiedSignature {
        claimed_signer: agent_id.to_string(),
        key_fingerprint: crate::core::signer_trust::key_fingerprint(pk_hex),
        signer_bound,
        trusted: signer_bound && crate::core::signer_trust::is_trusted(agent_id, pk_hex),
    })
}

pub(crate) fn serialize_bundle_v1_pretty(
    bundle: &HandoffTransferBundleV1,
) -> Result<String, String> {
    let json = serde_json::to_string_pretty(bundle).map_err(|e| e.to_string())?;
    if json.len() > MAX_BUNDLE_BYTES {
        return Err(format!(
            "ERROR: bundle too large ({} bytes > max {}). Use privacy=redacted and/or reduce curated refs.",
            json.len(),
            MAX_BUNDLE_BYTES
        ));
    }
    Ok(json)
}

pub(crate) fn parse_bundle_v1(json: &str) -> Result<HandoffTransferBundleV1, String> {
    let b: HandoffTransferBundleV1 = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if b.schema_version != crate::core::contracts::HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION {
        return Err(format!(
            "ERROR: unsupported schema_version {} (expected {})",
            b.schema_version,
            crate::core::contracts::HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION
        ));
    }
    Ok(b)
}

pub(crate) fn write_bundle_v1(path: &Path, json: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "ERROR: invalid path".to_string())?;
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("bundle")
    ));
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn read_bundle_v1(path: &Path) -> Result<HandoffTransferBundleV1, String> {
    let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if json.len() > MAX_BUNDLE_BYTES {
        return Err(format!(
            "ERROR: bundle file too large ({} bytes > max {})",
            json.len(),
            MAX_BUNDLE_BYTES
        ));
    }
    parse_bundle_v1(&json)
}

pub(crate) fn project_identity_warning(
    bundle: &HandoffTransferBundleV1,
    project_root: &str,
) -> Option<String> {
    let current_root_hash = crate::core::project_hash::hash_project_root(project_root);
    let current_identity_hash = crate::core::project_hash::project_identity(project_root)
        .as_deref()
        .map(crate::core::hasher::hash_str);

    if let Some(ref exported) = bundle.project.project_root_hash
        && exported != &current_root_hash
    {
        return Some(
            "WARNING: project_root_hash mismatch (importing into different project root)."
                .to_string(),
        );
    }

    if let (Some(exported), Some(current)) = (
        bundle.project.project_identity_hash.as_ref(),
        current_identity_hash.as_ref(),
    ) && exported != current
    {
        return Some(
            "WARNING: project_identity_hash mismatch (importing into different project identity)."
                .to_string(),
        );
    }

    None
}

fn build_artifacts_excerpt_v1(project_root: &Path) -> ArtifactsExcerptV1 {
    let mut out = ArtifactsExcerptV1::default();

    let resolved = crate::core::artifacts::load_resolved(project_root);
    out.warnings.extend(resolved.warnings);
    out.resolved = resolved
        .artifacts
        .into_iter()
        .take(MAX_ARTIFACT_ITEMS)
        .collect();

    let proofs_dir = match crate::core::pathutil::safe_project_data_dir(project_root) {
        Ok(d) => d.join("proofs"),
        Err(_) => return out,
    };
    if let Ok(rd) = std::fs::read_dir(&proofs_dir) {
        let mut files = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let bytes = p.metadata().map_or(0, |m| m.len());
            let md5 = match std::fs::read(&p) {
                Ok(b) => crate::core::hasher::hash_hex(&b),
                Err(e) => {
                    out.warnings
                        .push(format!("proof read failed: {} ({e})", p.display()));
                    continue;
                }
            };
            files.push(ProofFileV1 { name, md5, bytes });
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        out.proof_files = files.into_iter().take(MAX_PROOF_FILES).collect();
    }

    out
}

fn cap_ledger_in_place(ledger: &mut HandoffLedgerV1) {
    if ledger.session_snapshot.len() > MAX_LEDGER_SNAPSHOT_CHARS {
        ledger.session_snapshot =
            truncate_chars(&ledger.session_snapshot, MAX_LEDGER_SNAPSHOT_CHARS);
    }

    if let Some(ref mut task) = ledger.session.task {
        *task = truncate_chars(task, MAX_TASK_CHARS);
    }

    for d in &mut ledger.session.decisions {
        *d = truncate_chars(d, MAX_DECISION_CHARS);
    }
    for f in &mut ledger.session.findings {
        *f = truncate_chars(f, MAX_FINDING_CHARS);
    }
    for s in &mut ledger.session.next_steps {
        *s = truncate_chars(s, MAX_NEXT_STEP_CHARS);
    }

    for r in &mut ledger.curated_refs {
        if r.content.len() > MAX_CURATED_REF_CHARS {
            r.content = truncate_chars(&r.content, MAX_CURATED_REF_CHARS);
        }
    }
}

fn redact_ledger_in_place(ledger: &mut HandoffLedgerV1) {
    ledger.project_root = None;
    ledger.session_snapshot.clear();

    if let Some(ref mut task) = ledger.session.task {
        *task = crate::core::redaction::redact_text(task);
    }
    for d in &mut ledger.session.decisions {
        *d = crate::core::redaction::redact_text(d);
    }
    for f in &mut ledger.session.findings {
        *f = crate::core::redaction::redact_text(f);
    }
    for s in &mut ledger.session.next_steps {
        *s = crate::core::redaction::redact_text(s);
    }

    for fact in &mut ledger.knowledge.facts {
        fact.value = crate::core::redaction::redact_text(&fact.value);
    }

    for r in &mut ledger.curated_refs {
        r.content = crate::core::redaction::redact_text(&r.content);
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ledger() -> HandoffLedgerV1 {
        HandoffLedgerV1 {
            schema_version: crate::core::contracts::HANDOFF_LEDGER_V1_SCHEMA_VERSION,
            created_at: "20260503T000000Z".to_string(),
            content_md5: "old".to_string(),
            manifest_md5: "m".to_string(),
            project_root: Some("/abs/project".to_string()),
            agent_id: Some("a".to_string()),
            client_name: Some("cursor".to_string()),
            workflow: None,
            session_snapshot: "snapshot".to_string(),
            session: crate::core::handoff_ledger::SessionExcerpt {
                id: "s".to_string(),
                task: Some("task".to_string()),
                decisions: vec!["d1".to_string()],
                findings: vec!["f1".to_string()],
                next_steps: vec!["n1".to_string()],
            },
            tool_calls: crate::core::handoff_ledger::ToolCallsSummary::default(),
            evidence_keys: vec!["tool:ctx_read".to_string()],
            knowledge: crate::core::handoff_ledger::KnowledgeExcerpt {
                project_hash: None,
                facts: vec![crate::core::handoff_ledger::KnowledgeFactMini {
                    category: "c".to_string(),
                    key: "k".to_string(),
                    value: "secret=abcdef0123456789abcdef0123456789".to_string(),
                    confidence: 0.9,
                }],
            },
            curated_refs: vec![crate::core::handoff_ledger::CuratedRef {
                path: "src/lib.rs".to_string(),
                mode: "signatures".to_string(),
                content_md5: "x".to_string(),
                content: "fn a() {}".to_string(),
            }],
            active_overlays: Vec::new(),
            merged_from: Vec::new(),
            request_id: None,
            request_digest: None,
        }
    }

    #[test]
    fn redacted_bundle_removes_sensitive_fields() {
        let ledger = sample_ledger();
        let b = build_bundle_v1(ledger, None, BundlePrivacyV1::Redacted);
        assert_eq!(b.privacy, "redacted");
        assert!(b.ledger.project_root.is_none());
        assert!(
            b.ledger.session_snapshot.is_empty(),
            "{:?}",
            b.ledger.session_snapshot
        );
    }

    #[test]
    fn serialize_parse_roundtrip() {
        let ledger = sample_ledger();
        let b = build_bundle_v1(ledger, None, BundlePrivacyV1::Redacted);
        let json = serialize_bundle_v1_pretty(&b).expect("json");
        assert!(json.len() < MAX_BUNDLE_BYTES);
        let parsed = parse_bundle_v1(&json).expect("parse");
        assert_eq!(parsed.schema_version, b.schema_version);
        assert_eq!(parsed.privacy, "redacted");
    }

    #[test]
    fn unicode_signature_material_is_invalid() {
        for malformed in ["\u{1f512}", "0\u{20ac}", "\u{e9}\u{e9}"] {
            for signature_is_malformed in [true, false] {
                let mut bundle = build_bundle_v1(sample_ledger(), None, BundlePrivacyV1::Redacted);
                bundle.signer_agent_id = Some("unicode-regression".to_string());
                bundle.signature = Some(if signature_is_malformed {
                    malformed.to_string()
                } else {
                    "00".repeat(64)
                });
                bundle.signer_public_key = Some(if signature_is_malformed {
                    "00".repeat(32)
                } else {
                    malformed.to_string()
                });
                let serialized = serialize_bundle_v1_pretty(&bundle).unwrap();
                let parsed = parse_bundle_v1(&serialized).unwrap();
                assert_eq!(
                    check_bundle_signature(&parsed),
                    BundleSignatureStatus::Invalid("non-ASCII hex string".to_string())
                );
            }
        }
    }

    /// GL #465: signed bundles verify; any tampering after signing flips the
    /// status to `Invalid` (fail-closed); bundles without signature fields are
    /// `Unsigned` (legacy, allowed with warning).
    #[test]
    fn import_signature_check_verified_unsigned_invalid() {
        // Signing writes the agent key under the data dir. Own it for the
        // test: a concurrent test that points LEAN_CTX_DATA_DIR at a temp dir
        // and deletes it made `sign` fail with "path not found" (Windows CI).
        let _lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());

        let unsigned = build_bundle_v1(sample_ledger(), None, BundlePrivacyV1::Redacted);
        assert_eq!(
            check_bundle_signature(&unsigned),
            BundleSignatureStatus::Unsigned
        );

        let mut signed = build_bundle_v1(sample_ledger(), None, BundlePrivacyV1::Redacted);
        sign_bundle(&mut signed, "handoff-sig-test-agent").expect("sign");
        // Signed by a key held by a local agent identity → trusted anchor.
        match check_bundle_signature(&signed) {
            BundleSignatureStatus::Signed(v) => {
                assert_eq!(v.claimed_signer, "handoff-sig-test-agent");
                assert!(v.signer_bound && v.trusted, "{v:?}");
                assert_eq!(v.status(), "verified");
            }
            other => panic!("expected Signed, got {other:?}"),
        }

        // GHSA-gvg8-285q-m62g: relabelling the signer breaks the signature.
        let mut relabelled = signed.clone();
        relabelled.signer_agent_id = Some("trusted-release-bot".to_string());
        assert!(matches!(
            check_bundle_signature(&relabelled),
            BundleSignatureStatus::Invalid(_)
        ));
        // Stripping the scheme (downgrade to legacy) does not verify either.
        let mut downgraded = signed.clone();
        downgraded.signature_scheme = None;
        assert!(matches!(
            check_bundle_signature(&downgraded),
            BundleSignatureStatus::Invalid(_)
        ));

        // Tamper with the payload after signing → must be Invalid, not Unsigned.
        let mut tampered = signed.clone();
        tampered.ledger.session.task = Some("tampered task".to_string());
        assert!(matches!(
            check_bundle_signature(&tampered),
            BundleSignatureStatus::Invalid(_)
        ));

        // Partial signature material (e.g. stripped pubkey) is Invalid too.
        let mut partial = signed;
        partial.signer_public_key = None;
        assert!(matches!(
            check_bundle_signature(&partial),
            BundleSignatureStatus::Invalid(_)
        ));

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    /// GHSA-gvg8-285q-m62g: a self-embedded key is never reported as a
    /// verified identity, and legacy signatures surface as unbound.
    #[test]
    fn foreign_and_legacy_signatures_are_not_verified_identities() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pk_hex = crate::core::agent_identity::hex_encode(&key.verifying_key().to_bytes());

        // Bound signature from a key no anchor knows.
        let mut bound = build_bundle_v1(sample_ledger(), None, BundlePrivacyV1::Redacted);
        bound.signer_public_key = Some(pk_hex.clone());
        bound.signer_agent_id = Some("trusted-release-bot".to_string());
        bound.signature_scheme = Some(BOUND_SIGNATURE_SCHEME.to_string());
        let canonical = serde_json::to_string(&bound).unwrap();
        bound.signature = Some(crate::core::agent_identity::hex_encode(
            &crate::core::agent_identity::sign_bytes_with(&key, canonical.as_bytes()),
        ));
        let BundleSignatureStatus::Signed(v) = check_bundle_signature(&bound) else {
            panic!("bound signature must verify");
        };
        assert!(v.signer_bound && !v.trusted);
        assert_eq!(v.status(), "untrusted_signer");
        assert!(v.describe().contains("(untrusted"), "{}", v.describe());
        assert!(!v.describe().starts_with("verified"));

        // Pinning (subject, key) in the trust store anchors it.
        crate::core::signer_trust::pin_if_absent("trusted-release-bot", &pk_hex, "test").unwrap();
        let BundleSignatureStatus::Signed(v) = check_bundle_signature(&bound) else {
            panic!("bound signature must verify");
        };
        assert!(v.trusted);

        // Legacy signature (signer fields outside the payload) still verifies,
        // but is reported unbound and never trusted — even for a pinned key.
        let mut legacy = build_bundle_v1(sample_ledger(), None, BundlePrivacyV1::Redacted);
        let canonical = serde_json::to_string(&legacy).unwrap();
        legacy.signature = Some(crate::core::agent_identity::hex_encode(
            &crate::core::agent_identity::sign_bytes_with(&key, canonical.as_bytes()),
        ));
        legacy.signer_public_key = Some(pk_hex);
        legacy.signer_agent_id = Some("trusted-release-bot".to_string());
        let BundleSignatureStatus::Signed(v) = check_bundle_signature(&legacy) else {
            panic!("legacy signature must still verify");
        };
        assert!(!v.signer_bound && !v.trusted);
        assert_eq!(v.status(), "legacy_unbound");
        assert!(v.describe().contains("legacy signature"));

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
