//! Verification for recovery handles used by lossy context views.
//!
//! Results contain only status, byte length, and digest so a quality receipt can
//! establish retrievability without copying recovered content into telemetry.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryOutcome {
    Verified,
    Missing,
    Expired,
    Malformed,
    DigestMismatch,
    Refused(String),
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RecoveryCounters {
    pub(crate) handles_emitted: u64,
    pub(crate) handles_verified: u64,
    pub(crate) failures: u64,
    pub(crate) critical_failures: u64,
}

impl RecoveryCounters {
    /// Record a verification result; policy refusals are unavailable to the
    /// model, but are not mechanism failures.
    pub(crate) fn observe(&mut self, outcome: &RecoveryOutcome, critical: bool) {
        match outcome {
            RecoveryOutcome::Verified => {
                self.handles_verified = self.handles_verified.saturating_add(1);
            }
            RecoveryOutcome::Refused(_) => {}
            _ => {
                self.failures = self.failures.saturating_add(1);
                if critical {
                    self.critical_failures = self.critical_failures.saturating_add(1);
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryVerification {
    pub(crate) outcome: RecoveryOutcome,
    pub(crate) byte_len: Option<usize>,
    /// Lowercase, full BLAKE3 digest of the bytes returned by the resolver.
    pub(crate) digest: Option<String>,
}

impl RecoveryVerification {
    /// Only a successful resolver result with an optional matching digest is
    /// recoverable for the model.
    pub(crate) fn is_recoverable_for_model(&self) -> bool {
        matches!(self.outcome, RecoveryOutcome::Verified)
    }

    fn unavailable(outcome: RecoveryOutcome) -> Self {
        Self {
            outcome,
            byte_len: None,
            digest: None,
        }
    }

    fn resolved(bytes: &[u8], outcome: RecoveryOutcome) -> Self {
        Self {
            outcome,
            byte_len: Some(bytes.len()),
            digest: Some(blake3::hash(bytes).to_hex().to_string()),
        }
    }
}

const MAX_HANDLE_BYTES: usize = 4096;

/// Resolve through the same tee, reference, and archive paths used by ctx_expand.
/// expected_digest may be a 16-hex short BLAKE3 digest or a 64-hex full BLAKE3
/// digest; the returned digest is always full length.
pub(crate) fn verify_handle(handle: &str, expected_digest: Option<&str>) -> RecoveryVerification {
    if handle.is_empty()
        || handle.len() > MAX_HANDLE_BYTES
        || expected_digest.is_some_and(|digest| !valid_digest(digest))
    {
        return RecoveryVerification::unavailable(RecoveryOutcome::Malformed);
    }

    // Context-ledger references resolve to source paths. This API has no source
    // authority with which to read those paths, so never report them as verified.
    if crate::tools::ctx_expand::is_handle_ref(handle) {
        return if crate::tools::ctx_expand::resolve_handle_ref(handle).is_some() {
            RecoveryVerification::unavailable(RecoveryOutcome::Refused(
                "no source authority to read the resolved path".to_string(),
            ))
        } else {
            RecoveryVerification::unavailable(RecoveryOutcome::Missing)
        };
    }

    match crate::proxy::ccr::resolve_tee_checked(handle) {
        Ok(path) => return verify_tee_file(&path, expected_digest),
        Err(crate::proxy::ccr::TeeResolveError::Refused(reason)) => {
            return RecoveryVerification::unavailable(RecoveryOutcome::Refused(reason.to_string()));
        }
        Err(crate::proxy::ccr::TeeResolveError::Malformed)
            if crate::proxy::ccr::looks_like_tee_handle(handle) =>
        {
            return RecoveryVerification::unavailable(RecoveryOutcome::Malformed);
        }
        Err(crate::proxy::ccr::TeeResolveError::Missing)
            if crate::proxy::ccr::is_tee_handle(handle)
                && !crate::proxy::ccr::is_bare_tee_handle(handle) =>
        {
            return RecoveryVerification::unavailable(RecoveryOutcome::Missing);
        }
        _ => {}
    }

    if handle.starts_with("ref_") {
        return match crate::server::reference_store::resolve_checked(handle) {
            Ok(content) => verify_content(&content, expected_digest, true),
            Err(crate::server::reference_store::ReferenceResolveError::Malformed) => {
                RecoveryVerification::unavailable(RecoveryOutcome::Malformed)
            }
            Err(crate::server::reference_store::ReferenceResolveError::Missing) => {
                RecoveryVerification::unavailable(RecoveryOutcome::Missing)
            }
            Err(crate::server::reference_store::ReferenceResolveError::Expired) => {
                RecoveryVerification::unavailable(RecoveryOutcome::Expired)
            }
        };
    }

    if handle.starts_with("shell_") {
        let Some(archive_id) = crate::core::archive::resolve_alias(handle) else {
            return RecoveryVerification::unavailable(RecoveryOutcome::Missing);
        };
        return verify_archive(&archive_id, expected_digest);
    }

    if has_uri_scheme(handle) {
        return RecoveryVerification::unavailable(RecoveryOutcome::Unsupported);
    }

    verify_archive(handle, expected_digest)
}

fn verify_tee_file(path: &std::path::Path, expected_digest: Option<&str>) -> RecoveryVerification {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return RecoveryVerification::unavailable(RecoveryOutcome::Refused(
                "tee read refused by filesystem policy".to_string(),
            ));
        }
        Err(_) => return RecoveryVerification::unavailable(RecoveryOutcome::Missing),
    };
    let Ok(content) = std::str::from_utf8(&bytes) else {
        return RecoveryVerification::resolved(&bytes, RecoveryOutcome::Malformed);
    };
    // Tee IDs hash pre-redaction input, so byte identity uses an explicit digest.
    verify_content(content, expected_digest, true)
}

fn verify_archive(id: &str, expected_digest: Option<&str>) -> RecoveryVerification {
    match crate::core::archive::retrieve_checked(id) {
        Ok(content) => {
            let address_matches = crate::core::archive::content_matches_id(id, &content);
            verify_content(&content, expected_digest, address_matches)
        }
        Err(crate::core::archive::ArchiveResolveError::Malformed) => {
            RecoveryVerification::unavailable(RecoveryOutcome::Malformed)
        }
        Err(crate::core::archive::ArchiveResolveError::Missing) => {
            RecoveryVerification::unavailable(RecoveryOutcome::Missing)
        }
        Err(crate::core::archive::ArchiveResolveError::Refused(reason)) => {
            RecoveryVerification::unavailable(RecoveryOutcome::Refused(reason.to_string()))
        }
    }
}

fn verify_content(
    content: &str,
    expected_digest: Option<&str>,
    address_matches: bool,
) -> RecoveryVerification {
    let actual = blake3::hash(content.as_bytes()).to_hex().to_string();
    let expected_matches = expected_digest.is_none_or(|expected| digest_matches(&actual, expected));
    let outcome = if expected_matches && address_matches {
        RecoveryOutcome::Verified
    } else {
        RecoveryOutcome::DigestMismatch
    };
    RecoveryVerification {
        outcome,
        byte_len: Some(content.len()),
        digest: Some(actual),
    }
}

fn valid_digest(digest: &str) -> bool {
    matches!(digest.len(), 16 | 64) && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn digest_matches(actual_full: &str, expected: &str) -> bool {
    match expected.len() {
        16 => actual_full
            .get(..16)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(expected)),
        64 => actual_full.eq_ignore_ascii_case(expected),
        _ => false,
    }
}

fn has_uri_scheme(handle: &str) -> bool {
    let Some((scheme, target)) = handle.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && !target.is_empty()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_exclude_policy_refusals_from_mechanism_failures() {
        let mut counters = RecoveryCounters {
            handles_emitted: 3,
            ..RecoveryCounters::default()
        };
        counters.observe(&RecoveryOutcome::Verified, false);
        counters.observe(
            &RecoveryOutcome::Refused("no source authority".to_string()),
            true,
        );
        counters.observe(&RecoveryOutcome::Missing, true);

        assert_eq!(counters.handles_emitted, 3);
        assert_eq!(counters.handles_verified, 1);
        assert_eq!(counters.failures, 1);
        assert_eq!(counters.critical_failures, 1);
    }

    #[test]
    fn source_path_references_are_refused_without_source_authority() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().expect("ledger test directory");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());

        let mut ledger = crate::core::context_ledger::ContextLedger::new();
        ledger.record("/private/source-without-authority.txt", "full", 80, 20);
        ledger.save();

        let verification = verify_handle("@F1", None);
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");

        assert!(matches!(
            verification.outcome,
            RecoveryOutcome::Refused(ref reason) if reason.contains("no source authority")
        ));
        assert!(!verification.is_recoverable_for_model());
    }
}
