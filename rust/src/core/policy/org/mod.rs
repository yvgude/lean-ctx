//! Org-level policy distribution (ADR-023, GL #674).
//!
//! An organisation distributes a centrally signed [`OrgPolicyV1`] artifact.
//! When installed, signature-verified, and the signer's key is trust-pinned,
//! the runtime folds it in as an un-bypassable enforcement floor beneath the
//! local project pack.

pub(crate) mod marker;
pub mod model;
pub mod store;
pub mod trust;

use std::path::PathBuf;

pub use model::{OrgPolicyV1, OrgVerifyResult};
pub use trust::{TrustStore, TrustedKey};

use crate::core::policy::ResolvedPolicy;

#[must_use]
pub fn org_key_id(org: &str) -> String {
    let safe: String = org
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("org-{safe}")
}

/// Load + verify + trust-check the installed org policy artifact.
/// Returns `Some(resolved)` only when all three hold:
/// 1. An artifact is installed on disk
/// 2. Its Ed25519 signature is valid
/// 3. The signer's public key is trust-pinned
/// 4. The admin declared `enforced = true`
#[must_use]
pub fn active_resolved() -> Option<ResolvedPolicy> {
    let artifact = store::load_active()?;

    let result = artifact.verify();
    if !result.signature_valid {
        tracing::warn!(
            "org policy: signature invalid ({}), ignoring",
            result.error.as_deref().unwrap_or("unknown")
        );
        return None;
    }

    let signer = artifact.signer_public_key.as_deref()?;
    if !trust::is_trusted(signer) {
        tracing::debug!("org policy: signer not trust-pinned, ignoring (fail-open)");
        return None;
    }

    if !artifact.enforced {
        tracing::debug!("org policy: admin did not set enforced=true, skipping");
        return None;
    }

    match artifact.resolved() {
        Ok(resolved) => Some(resolved),
        Err(e) => {
            tracing::warn!("org policy: failed to resolve pack: {e}");
            None
        }
    }
}

/// Error-preserving resolution for enforcement callers. Only an absent policy
/// or a verified, trusted policy explicitly marked unenforced yields `None`.
///
/// A recorded requirement (the required marker) changes both cases: without a
/// policy it is an error, and a version-one policy does not satisfy it. A
/// version-two artifact is not a version-one artifact and fails to load, which
/// is an error as well. This is the same floor the admission snapshot applies
/// (`runtime::refresh`), for callers that resolve the policy directly.
pub fn active_resolved_checked() -> Result<Option<ResolvedPolicy>, String> {
    AuthoritySnapshot::capture()?.resolve()
}

/// The bytes organization authority is read from, captured once: the selected
/// policy source, the trust store and the required marker.
///
/// The fingerprint and the resolution both come from these captured bytes, so
/// a caller that keeps a resolved policy under the fingerprint can never pair
/// one authority's rules with another's identity: there is no second read
/// between the two. Whoever reuses a kept resolution captures again and
/// compares fingerprints first.
pub(crate) struct AuthoritySnapshot {
    fingerprint: blake3::Hash,
    /// The fingerprint's state before the marker text.
    before_marker: blake3::Hasher,
    policy: Option<String>,
    trust: Option<String>,
    marker: Option<String>,
}

impl AuthoritySnapshot {
    /// Reads the three inputs once. Paths are part of the fingerprint and
    /// absence is distinct from emptiness; any read error is an error.
    pub(crate) fn capture() -> Result<Self, String> {
        let (policy_path, policy_required) = store::selected_source()?;
        let mut hash = blake3::Hasher::new();
        // The artifact alone is read at its own, larger bound; trust store
        // and marker keep the usual one.
        let mut read =
            |path: PathBuf, required: bool, artifact: bool| -> Result<Option<String>, String> {
                let path_bytes = path.as_os_str().as_encoded_bytes();
                hash.update(&(path_bytes.len() as u64).to_le_bytes());
                hash.update(path_bytes);
                let text = if artifact {
                    crate::core::policy::files::read_org_artifact(&path, required, false)?
                } else {
                    crate::core::policy::files::read(&path, required)?
                };
                hash_text(&mut hash, text.as_deref());
                Ok(text)
            };
        let policy = read(policy_path, policy_required, true)?;
        let trust = read(trust::trust_path_read_only()?, false, false)?;
        // The marker goes last, path before text, so that the fingerprint of
        // the same authority under another marker can be derived.
        let marker_path = marker::marker_path_read_only()?;
        let path_bytes = marker_path.as_os_str().as_encoded_bytes();
        hash.update(&(path_bytes.len() as u64).to_le_bytes());
        hash.update(path_bytes);
        let marker = crate::core::policy::files::read(&marker_path, false)?;
        let before_marker = hash.clone();
        hash_text(&mut hash, marker.as_deref());
        Ok(Self {
            fingerprint: hash.finalize(),
            before_marker,
            policy,
            trust,
            marker,
        })
    }

    #[cfg(any(test, feature = "enterprise"))]
    pub(crate) fn fingerprint(&self) -> blake3::Hash {
        self.fingerprint
    }

    /// Whether the captured marker records a requirement in force. A marker
    /// that cannot be parsed is an error.
    fn required(&self) -> Result<bool, String> {
        Ok(self
            .marker
            .as_deref()
            .map(marker::parse)
            .transpose()?
            .is_some_and(|marker| marker.is_required()))
    }

    /// Resolves the organization policy from the captured bytes, without
    /// reading anything again. Same floor as the admission snapshot: under a
    /// recorded requirement a missing policy is an error and a version-one
    /// policy does not qualify; a version-two artifact is not a version-one
    /// artifact and fails to parse, which is an error too.
    pub(crate) fn resolve(&self) -> Result<Option<ResolvedPolicy>, String> {
        self.resolve_observed().map(|(floor, _)| floor)
    }

    /// [`Self::resolve`], together with what the verifier's answer stated
    /// when this very resolution asked it: handed out only after the
    /// recapture and both bounds, with the floor it belongs to. `None`
    /// whenever no verifier was asked. For a caller that records what it
    /// decided under; `resolve` is this without the observation.
    pub(crate) fn resolve_observed(
        &self,
    ) -> Result<(Option<ResolvedPolicy>, Option<Observation>), String> {
        let required = self.required()?;
        let Some(policy) = self.policy.as_deref() else {
            if required {
                return Err("required organization policy is missing".into());
            }
            return Ok((None, None));
        };
        // Version two is decided by the authenticated verifier for these
        // captured bytes, on this call; nothing here decides it locally.
        if is_v2(policy) {
            let verified = verify_v2(
                policy,
                self.trust.as_deref(),
                self.marker.as_deref(),
                std::time::Instant::now() + RESOLVE_DEADLINE,
            )?;
            // The answer took time. Before the floor is used, the authority
            // is read again and must be exactly what was captured — except
            // for the marker, which must be exactly what this decision left
            // stored. Any other change to policy, trust store or marker
            // while waiting refuses.
            let mut expected = self.before_marker.clone();
            hash_text(&mut expected, Some(&verified.marker));
            if Self::capture()?.fingerprint != expected.finalize() {
                return Err("organization authority changed during verification".into());
            }
            // That read took time too: horizon and deadline once more, last.
            verified.lifetime.check()?;
            return Ok((verified.floor, Some(verified.observation)));
        }
        // Only a version-two artifact may exceed the usual input bound.
        if policy.len() as u64 > crate::core::policy::files::MAX_BYTES {
            return Err("policy input exceeds byte limit".into());
        }
        if required {
            return Err("required organization policy is not verifiable".into());
        }
        let artifact = OrgPolicyV1::from_json(policy)?;
        let trust: TrustStore = match self.trust.as_deref() {
            Some(text) => {
                toml::from_str(text).map_err(|_| "org trust input is invalid".to_string())?
            }
            None => TrustStore::default(),
        };
        resolve_checked(&artifact, &trust).map(|floor| (floor, None))
    }
}

fn hash_text(hash: &mut blake3::Hasher, text: Option<&str>) {
    match text {
        Some(text) => {
            hash.update(&[1]);
            hash.update(&(text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
        }
        None => {
            hash.update(&[0]);
        }
    }
}

/// Whether installed organization-policy text is a version-two artifact: one
/// this Engine cannot verify by itself. Read from the artifact's own schema
/// field; anything unreadable is simply not version two here and fails the
/// version-one checks instead.
pub(crate) fn is_v2(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let schema = value
        .get("schema")
        .or_else(|| value.get("statement").and_then(|s| s.get("schema")))
        .and_then(serde_json::Value::as_str);
    schema == Some(lean_ctx_protocol::org_policy_verifier::ORG_POLICY_V2_ARTIFACT_SCHEMA_ID)
}

/// A version-two organization policy as the verifier decides it for exactly
/// these captured bytes, now.
///
/// The marker must already be stored: it names the organization and is the
/// predecessor the decision is recorded on. Without one nothing is asked and
/// nothing is created — the first marker of an endpoint is a separately
/// authorized enrollment, not an outcome of a verification. The accepted
/// state is recorded on that exact marker before the policy is used; a marker
/// that changed, was damaged or was lost in between refuses. Nothing of an
/// earlier answer is kept: every call asks again.
///
/// `deadline` is the caller's one monotonic bound for the whole decision:
/// the verifier gets only the time left, the marker's replacement is not
/// admitted at or after it, and nothing is returned at or after it. The
/// bound is that admission: once admitted, the operating system's replace
/// and directory sync run to their end, and a marker that was written is
/// never taken back — a later error here does not undo it.
pub(crate) fn verify_v2(
    artifact: &str,
    trust: Option<&str>,
    marker_text: Option<&str>,
    deadline: std::time::Instant,
) -> Result<VerifiedV2, String> {
    use crate::core::intelligence_runtime::org_policy_verifier::{Accepted, Snapshot, verify};
    use lean_ctx_protocol::org_policy_verifier::{
        OrgRequiredMarkerEntryV1, OrgRequiredMarkerStateV1,
    };

    let marker_text =
        marker_text.ok_or("organization policy has no recorded requirement to verify against")?;
    let stored = marker::parse(marker_text)?;
    let accepted = verify(
        &Snapshot {
            organization: &stored.organization_id,
            artifact: artifact.as_bytes(),
            trust: trust.map(str::as_bytes),
            marker: Some(marker_text.as_bytes()),
        },
        deadline,
    )
    .map_err(|_| "organization policy could not be verified")?;
    // What this very answer states, taken before the answer is consumed: the
    // observation below is made of nothing else.
    let (state, sequence, pack, observed, statement) = match accepted {
        Accepted::Enforce {
            pack_toml,
            policy_digest,
            sequence,
            valid_until,
        } => {
            let statement = policy_digest.as_str().to_owned();
            (
                OrgRequiredMarkerStateV1::Required { policy_digest },
                sequence,
                Some((pack_toml, valid_until)),
                ObservedState::Required,
                statement,
            )
        }
        Accepted::NotRequired {
            unpin_digest,
            sequence,
        } => {
            let statement = unpin_digest.as_str().to_owned();
            (
                OrgRequiredMarkerStateV1::Unpinned { unpin_digest },
                sequence,
                None,
                ObservedState::Unpinned,
                statement,
            )
        }
    };
    let recorded_at = lean_ctx_protocol::TeamTimestampV1::new(
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
    )
    .map_err(|_| "organization policy decision could not be dated")?;
    let (_, marker) = marker::advance_within(
        &stored.organization_id,
        OrgRequiredMarkerEntryV1 {
            sequence,
            recorded_at,
            state,
        },
        marker::Predecessor::Stored(marker_text),
        deadline,
    )
    .map_err(|_| "organization policy decision could not be recorded")?;
    let (floor, horizon) = match pack {
        Some((pack_toml, valid_until)) => {
            let floor = crate::core::policy::parse(&pack_toml)
                .and_then(|pack| crate::core::policy::resolve(&pack))
                .map_err(|_| "verified organization policy is invalid")?;
            // An unreadable horizon is an expired one.
            let horizon = chrono::DateTime::parse_from_rfc3339(valid_until.as_str())
                .map_err(|_| "verified organization policy has expired")?
                .with_timezone(&chrono::Utc);
            (Some(floor), Some(horizon))
        }
        None => (None, None),
    };
    let lifetime = Lifetime { horizon, deadline };
    // Recording may have waited for the marker lock and compiling took time.
    lifetime.check()?;
    Ok(VerifiedV2 {
        floor,
        marker,
        lifetime,
        // Only here, after the decision was recorded on the marker, the pack
        // compiled and both bounds checked: an answer that got no further
        // leaves no observation behind.
        observation: Observation {
            state: observed,
            statement,
            policy_version: sequence,
            valid_until: horizon,
        },
    })
}

/// What one verification established: the floor to enforce, if any, the
/// marker text that is stored now — the captured one, or the one this very
/// decision appended — how long the decision may be acted on, and what the
/// answer stated, for the record of whatever is decided under it.
pub(crate) struct VerifiedV2 {
    pub(crate) floor: Option<ResolvedPolicy>,
    pub(crate) marker: String,
    pub(crate) lifetime: Lifetime,
    pub(crate) observation: Observation,
}

/// Which of the two authenticated answers was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObservedState {
    /// A signed policy is enforced.
    Required,
    /// The requirement was lifted by a signed unpin. This is an organization
    /// decision in force, not the absence of an organization: it is recorded
    /// although it leaves no floor.
    Unpinned,
}

/// The organization decision one verification observed, for a receipt's
/// trace: which statement the verifier accepted, the policy version it
/// enforces, and — for an enforced policy — the signed end that was checked.
///
/// Immutable and content-free: a digest, a number, a closed state and an
/// instant, all from the same accepted answer. No policy text, trust store,
/// marker, key, path or credential is in it. `policy_version` is the signed
/// sequence of the organization's statement; it is NOT the `version` a policy
/// pack carries in its own text, which stays the pack's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Observation {
    state: ObservedState,
    /// `sha256:<hex>` of the accepted statement: the policy, or the unpin.
    statement: String,
    policy_version: u64,
    /// The signed horizon of an enforced policy as this Engine parsed and
    /// checked it; `None` for an unpin, which has none.
    valid_until: Option<chrono::DateTime<chrono::Utc>>,
}

impl Observation {
    /// A signed unpin as a verification would have observed it. Tests only:
    /// outside of them an observation comes from `verify_v2` and nowhere else.
    #[cfg(test)]
    pub(crate) fn unpinned_for_test(policy_version: u64, fill: char) -> Self {
        Self {
            state: ObservedState::Unpinned,
            statement: format!("sha256:{}", fill.to_string().repeat(64)),
            policy_version,
            valid_until: None,
        }
    }

    pub(crate) fn state(&self) -> ObservedState {
        self.state
    }

    pub(crate) fn statement(&self) -> &str {
        &self.statement
    }

    pub(crate) fn policy_version(&self) -> u64 {
        self.policy_version
    }

    pub(crate) fn valid_until(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.valid_until
    }
}

/// The two bounds of one verified decision: the signed horizon of an enforced
/// policy (none when the requirement was lifted) and the request's own
/// monotonic deadline. Whoever acts on the decision checks both on fresh
/// clocks at the last moment before acting — after any further read or wait,
/// not only when the answer arrived.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Lifetime {
    horizon: Option<chrono::DateTime<chrono::Utc>>,
    deadline: std::time::Instant,
}

impl Lifetime {
    /// A decision signed up to `horizon` and asked under `deadline`. Tests
    /// only: outside of them a lifetime comes from `verify_v2` and nowhere else.
    #[cfg(test)]
    pub(crate) fn for_test(
        horizon: Option<chrono::DateTime<chrono::Utc>>,
        deadline: std::time::Instant,
    ) -> Self {
        Self { horizon, deadline }
    }

    /// The signed instant an enforced policy runs out at, for the compiled
    /// policy to carry: whoever keeps that policy keeps its end with it.
    pub(crate) fn horizon(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.horizon
    }

    /// Both bounds are exclusive, like the verifier's own check.
    pub(crate) fn check(&self) -> Result<(), String> {
        self.check_horizon()?;
        if std::time::Instant::now() >= self.deadline {
            return Err("organization policy verification deadline exceeded".into());
        }
        Ok(())
    }

    /// The signed horizon alone: for a holder that outlives the request which
    /// obtained the decision, and so has no request deadline any more.
    pub(crate) fn check_horizon(&self) -> Result<(), String> {
        if self
            .horizon
            .is_some_and(|horizon| chrono::Utc::now() >= horizon)
        {
            return Err("verified organization policy has expired".into());
        }
        Ok(())
    }
}

/// Bound for a verification asked for outside the admission worker, which
/// brings its own.
const RESOLVE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) fn resolve_checked(
    artifact: &OrgPolicyV1,
    trusted: &TrustStore,
) -> Result<Option<ResolvedPolicy>, String> {
    if !artifact.verify().signature_valid {
        return Err("org policy signature invalid".into());
    }
    let signer = artifact
        .signer_public_key
        .as_deref()
        .ok_or_else(|| "org policy signer missing".to_string())?;
    if !trusted
        .keys
        .iter()
        .any(|key| key.public_key == signer && key.org == artifact.org)
    {
        return Err("org policy signer is not pinned for this organization".into());
    }
    let resolved = artifact.resolved().map_err(|error| error.to_string())?;
    Ok(artifact.enforced.then_some(resolved))
}

/// Status and the captured-authority path against real files in an isolated
/// data directory: what the status reports must be what enforcement decides.
#[cfg(test)]
mod authority_tests {
    use super::*;
    use lean_ctx_protocol::org_policy_verifier::{
        OrgRequiredMarkerEntryV1, OrgRequiredMarkerStateV1,
    };
    use lean_ctx_protocol::{RecordDigest, TeamId, TeamTimestampV1};

    const PACK: &str = "name = \"floor\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[context]\ndeny_tools = [\"ctx_shell\"]\n";

    /// A signed, enforced version-one policy for `acme`, installed.
    fn install_policy() -> OrgPolicyV1 {
        let key = ed25519_dalek::SigningKey::from_bytes(&[23; 32]);
        let mut artifact = OrgPolicyV1::build("acme", "1", true, PACK).unwrap();
        artifact.sign_with_key(&key);
        store::install(&artifact).unwrap();
        artifact
    }

    fn pin(org: &str, artifact: &OrgPolicyV1) {
        trust::pin(org, artifact.signer_public_key.as_deref().unwrap()).unwrap();
    }

    fn require(sequence: u64) {
        let entry = OrgRequiredMarkerEntryV1 {
            sequence,
            recorded_at: TeamTimestampV1::new("2026-10-10T09:00:00Z").unwrap(),
            state: OrgRequiredMarkerStateV1::Required {
                policy_digest: RecordDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            },
        };
        // The fixture stands in for an accepted decision made on the marker
        // as it is stored now — or for the first one when none is stored.
        let path = marker::marker_path_read_only().unwrap();
        let stored = crate::core::policy::files::read(&path, false).unwrap();
        let predecessor = stored.as_deref().map_or(
            marker::Predecessor::NeverStored,
            marker::Predecessor::Stored,
        );
        marker::advance(&TeamId::new("acme").unwrap(), entry, predecessor).unwrap();
    }

    #[test]
    fn status_reports_a_key_pinned_for_another_organization_as_not_applied() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let artifact = install_policy();
        pin("other", &artifact);

        let status = status();
        assert!(status.present && status.signature_valid);
        assert!(!status.trusted, "the key is pinned, but not for this org");
        assert!(!status.applied);
        assert!(
            active_resolved_checked().is_err(),
            "enforcement refuses the same state"
        );

        pin("acme", &artifact);
        // `pin` keeps one entry per key: the wrong-org pin stays the only one.
        assert!(!self::status().trusted);

        // The exact assignment the commands admit on is the one enforcement
        // uses: this key is pinned, but not for this organization — and
        // pinning again says so instead of moving the pin.
        let key = artifact.signer_public_key.as_deref().unwrap();
        assert!(trust::is_trusted(key), "the key alone is pinned");
        assert!(!trust::is_trusted_for("acme", key));
        assert!(trust::is_trusted_for("other", key));
        assert_eq!(trust::pinned_organization(key).as_deref(), Some("other"));
        assert_eq!(
            trust::pin_for("acme", key).unwrap(),
            trust::PinOutcome::PinnedForAnother("other".into())
        );
        assert_eq!(
            trust::pin_for("other", key).unwrap(),
            trust::PinOutcome::AlreadyPinned
        );
        assert!(!trust::is_trusted_for("acme", key), "nothing was moved");
        assert_eq!(trust::trusted_keys().len(), 1);
    }

    /// The status observes: it reads what is stored and leaves the endpoint as
    /// it found it — no directory is created for it, with nothing stored and
    /// with an explicit source that does not exist (an error, not "absent").
    #[test]
    fn status_creates_nothing_and_reports_a_missing_explicit_source() {
        let data = crate::core::data_dir::isolated_data_dir();
        let observed = || {
            [
                store::selected_source().unwrap().0,
                trust::trust_path_read_only().unwrap(),
                marker::marker_path_read_only().unwrap(),
            ]
            .map(|path| {
                (
                    path.exists(),
                    path.parent().is_some_and(std::path::Path::exists),
                )
            })
        };
        let before = observed();
        let status = status();
        assert!(!status.present && status.resolve_error.is_none());
        assert_eq!(status.recorded, RecordedRequirement::Absent);
        assert_eq!(observed(), before, "the status left a trace on disk");

        let missing = data.path().join("does-not-exist.json");
        crate::test_env::set_var("LEANCTX_ORG_POLICY", &missing);
        let status = self::status();
        crate::test_env::remove_var("LEANCTX_ORG_POLICY");
        assert!(
            status.resolve_error.is_some(),
            "an explicit source that is missing is not 'nothing installed'"
        );
        assert!(!missing.exists());
    }

    #[test]
    fn status_and_enforcement_agree_for_a_correctly_pinned_policy() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let artifact = install_policy();
        pin("acme", &artifact);

        let status = status();
        assert!(status.trusted && status.applied && status.resolve_error.is_none());
        assert!(active_resolved_checked().unwrap().is_some());
    }

    #[test]
    fn a_recorded_requirement_or_a_damaged_marker_is_never_reported_as_applied() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let artifact = install_policy();
        pin("acme", &artifact);
        require(1);

        let status = status();
        assert!(status.present && status.trusted);
        assert!(!status.applied, "a version-one policy does not satisfy it");
        assert!(status.resolve_error.is_some());
        assert!(active_resolved_checked().is_err());

        // Artifact removed: still required, still an error, not "absent".
        store::uninstall().unwrap();
        let status = self::status();
        assert!(!status.present && !status.applied);
        assert!(status.resolve_error.is_some());
        assert!(active_resolved_checked().is_err());

        // A damaged marker is an error for both, with or without a policy.
        std::fs::write(marker::marker_path_read_only().unwrap(), b"{}").unwrap();
        assert!(self::status().resolve_error.is_some());
        assert!(active_resolved_checked().is_err());
        store::install(&artifact).unwrap();
        let status = self::status();
        assert!(!status.applied && status.resolve_error.is_some());
        assert!(AuthoritySnapshot::capture().unwrap().resolve().is_err());
    }

    /// The stored record is read as what it is. A signed unpin without any
    /// artifact is a decision on record — neither an error nor "no central
    /// policy" — and a record that cannot be read is unavailable, not absent.
    #[test]
    fn the_stored_record_is_read_as_recorded_absent_or_unavailable() {
        let _data = crate::core::data_dir::isolated_data_dir();
        assert_eq!(recorded_requirement(), RecordedRequirement::Absent);
        assert_eq!(status().recorded, RecordedRequirement::Absent);

        require(3);
        match recorded_requirement() {
            RecordedRequirement::Recorded {
                organization,
                required: true,
                sequence: 3,
                entries: 1,
                ..
            } => assert_eq!(organization, "acme"),
            other => panic!("not the recorded requirement: {other:?}"),
        }

        let path = marker::marker_path_read_only().unwrap();
        let stored = crate::core::policy::files::read(&path, false)
            .unwrap()
            .unwrap();
        marker::advance(
            &TeamId::new("acme").unwrap(),
            OrgRequiredMarkerEntryV1 {
                sequence: 4,
                recorded_at: TeamTimestampV1::new("2026-10-11T09:00:00Z").unwrap(),
                state: OrgRequiredMarkerStateV1::Unpinned {
                    unpin_digest: RecordDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
                },
            },
            marker::Predecessor::Stored(&stored),
        )
        .unwrap();
        let status = status();
        assert!(!status.present && status.source.is_none());
        assert!(
            status.resolve_error.is_none(),
            "a lifted requirement refuses nothing"
        );
        match status.recorded {
            RecordedRequirement::Recorded {
                required: false,
                sequence: 4,
                entries: 2,
                statement,
                ..
            } => assert_eq!(statement, format!("sha256:{}", "b".repeat(64))),
            other => panic!("the unpin is not on record: {other:?}"),
        }

        std::fs::write(&path, b"{}").unwrap();
        assert!(matches!(
            recorded_requirement(),
            RecordedRequirement::Unavailable(_)
        ));
    }

    #[test]
    fn a_capture_resolves_its_own_bytes_whatever_changes_afterwards() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let artifact = install_policy();
        pin("acme", &artifact);

        let enforced = AuthoritySnapshot::capture().unwrap();
        // Between capture and resolve: the policy is removed, the trust store
        // emptied and a requirement recorded.
        store::uninstall().unwrap();
        trust::remove(artifact.signer_public_key.as_deref().unwrap()).unwrap();
        require(1);
        assert!(
            enforced.resolve().unwrap().is_some(),
            "the capture answers for the bytes it holds"
        );

        let now = AuthoritySnapshot::capture().unwrap();
        assert_ne!(now.fingerprint(), enforced.fingerprint());
        assert!(now.resolve().is_err(), "required, and nothing installed");

        // Each of the three inputs is part of the identity.
        let before = AuthoritySnapshot::capture().unwrap().fingerprint();
        store::install(&artifact).unwrap();
        let with_policy = AuthoritySnapshot::capture().unwrap().fingerprint();
        pin("acme", &artifact);
        let with_trust = AuthoritySnapshot::capture().unwrap().fingerprint();
        require(2);
        let with_marker = AuthoritySnapshot::capture().unwrap().fingerprint();
        assert!(before != with_policy && with_policy != with_trust && with_trust != with_marker);
    }

    #[test]
    fn an_unreadable_explicit_source_refuses_instead_of_resolving_to_no_policy() {
        let data = crate::core::data_dir::isolated_data_dir();
        let missing = data.path().join("does-not-exist.json");
        crate::test_env::set_var("LEANCTX_ORG_POLICY", &missing);
        let captured = AuthoritySnapshot::capture();
        crate::test_env::remove_var("LEANCTX_ORG_POLICY");
        assert!(captured.is_err(), "an explicit source that is absent");
    }
}

#[cfg(test)]
mod checked_tests {
    use super::*;

    fn fixture(enforced: bool) -> (OrgPolicyV1, TrustStore) {
        let key = ed25519_dalek::SigningKey::from_bytes(&[19; 32]);
        let mut artifact = OrgPolicyV1::build(
            "acme", "1", enforced,
            "name = \"floor\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[context]\ndeny_tools = [\"ctx_shell\"]\n",
        ).unwrap();
        artifact.sign_with_key(&key);
        let trusted = TrustStore {
            keys: vec![TrustedKey {
                org: "acme".into(),
                public_key: artifact.signer_public_key.clone().unwrap(),
                added_at: "2026-09-09T00:00:00Z".into(),
            }],
        };
        (artifact, trusted)
    }

    #[test]
    fn checked_resolution_requires_valid_signature_and_org_pin() {
        let (mut artifact, mut trusted) = fixture(true);
        assert!(resolve_checked(&artifact, &trusted).unwrap().is_some());
        assert!(resolve_checked(&artifact, &TrustStore::default()).is_err());
        trusted.keys[0].org = "other".into();
        assert!(resolve_checked(&artifact, &trusted).is_err());
        trusted.keys[0].org = "acme".into();
        artifact.enforced = false;
        assert!(resolve_checked(&artifact, &trusted).is_err());
    }

    #[test]
    fn checked_resolution_allows_only_trusted_signed_disable() {
        let (artifact, trusted) = fixture(false);
        assert!(resolve_checked(&artifact, &trusted).unwrap().is_none());
        assert!(resolve_checked(&artifact, &TrustStore::default()).is_err());
    }
}

/// The endpoint's stored requirement record, as it reads now.
///
/// A record of the last decision that was accepted and stored — read from the
/// marker file and nothing else. It is not a verification: no verifier is
/// asked, nothing is appended, and what it shows may be older than what the
/// enforcement pipeline would decide at the next use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RecordedRequirement {
    /// No record is stored on this endpoint.
    #[default]
    Absent,
    /// A record is stored and cannot be read or parsed. Never "absent":
    /// governed operations refuse while it is in this state.
    Unavailable(String),
    /// The newest stored entry.
    Recorded {
        organization: String,
        /// `true`: a policy is required. `false`: the requirement was lifted
        /// by a signed unpin — an organization decision that stays on record.
        required: bool,
        /// The signed sequence of the recorded statement.
        sequence: u64,
        /// `sha256:<hex>` of the recorded statement (the policy, or the unpin).
        statement: String,
        recorded_at: String,
        /// Entries on record; this is the last of them.
        entries: usize,
    },
}

/// Reads the stored requirement record. Read-only: see [`RecordedRequirement`].
#[must_use]
pub fn recorded_requirement() -> RecordedRequirement {
    use lean_ctx_protocol::org_policy_verifier::OrgRequiredMarkerStateV1;

    let text = match marker::marker_path_read_only()
        .and_then(|path| crate::core::policy::files::read(&path, false))
    {
        Ok(Some(text)) => text,
        Ok(None) => return RecordedRequirement::Absent,
        Err(error) => return RecordedRequirement::Unavailable(error),
    };
    let stored = match marker::parse(&text) {
        Ok(stored) => stored,
        Err(error) => return RecordedRequirement::Unavailable(error),
    };
    let current = stored.current();
    let (required, statement) = match &current.state {
        OrgRequiredMarkerStateV1::Required { policy_digest } => (true, policy_digest.as_str()),
        OrgRequiredMarkerStateV1::Unpinned { unpin_digest } => (false, unpin_digest.as_str()),
    };
    RecordedRequirement::Recorded {
        organization: stored.organization_id.as_str().to_owned(),
        required,
        sequence: current.sequence,
        statement: statement.to_owned(),
        recorded_at: current.recorded_at.as_str().to_owned(),
        entries: stored.entries.len(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct OrgStatus {
    /// The stored requirement record, read as it is — not a verification.
    pub recorded: RecordedRequirement,
    pub present: bool,
    pub source: Option<PathBuf>,
    pub org: Option<String>,
    pub policy_version: Option<String>,
    pub enforced: bool,
    pub issued_at: Option<String>,
    pub signer_public_key: Option<String>,
    pub signature_valid: bool,
    pub trusted: bool,
    pub applied: bool,
    pub resolve_error: Option<String>,
    pub pinned_anchors: usize,
    /// The resolved pack of the version-one artifact this status read — what
    /// a floor made of it would be. A preview of stored bytes, independent of
    /// trust and of `applied`; `None` without such an artifact.
    pub floor_preview: Option<ResolvedPolicy>,
}

#[must_use]
pub fn status() -> OrgStatus {
    // An observation and nothing else. Every input is read through the
    // read-only resolutions the admission snapshot uses — the selected
    // source, the trust store, the stored record — each once: this function
    // creates and repairs no directory, asks no verifier and writes nothing.
    // What it reports is a diagnosis of what is stored, not an authority.
    let recorded = recorded_requirement();
    // The requirement, from that one read of the record. A record that
    // cannot be read is an error, never "no requirement".
    let required = match &recorded {
        RecordedRequirement::Absent => Ok(false),
        RecordedRequirement::Recorded { required, .. } => Ok(*required),
        RecordedRequirement::Unavailable(error) => Err(error.clone()),
    };
    // An unreadable trust store pins nothing here and is reported: it is
    // not an empty store, and enforcement refuses on it.
    let (anchors, trust_error) = match trust::load_read_only() {
        Ok(store) => (store.keys, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let pinned_anchors = anchors.len();
    // The selected source, read once. An explicit source that is missing or
    // unreadable is an error — it never falls back to another artifact and
    // is never "nothing installed".
    let (source, text) = match store::selected_source() {
        Ok((path, explicit)) => {
            match crate::core::policy::files::read_org_artifact(&path, explicit, false) {
                Ok(text) => (text.as_ref().map(|_| path), text),
                Err(error) => {
                    return OrgStatus {
                        recorded,
                        present: true,
                        source: Some(path),
                        resolve_error: Some(error),
                        pinned_anchors,
                        ..Default::default()
                    };
                }
            }
        }
        Err(error) => {
            return OrgStatus {
                recorded,
                resolve_error: Some(error),
                pinned_anchors,
                ..Default::default()
            };
        }
    };
    let artifact = text
        .as_deref()
        .filter(|text| text.len() as u64 <= crate::core::policy::files::MAX_BYTES)
        .and_then(|text| OrgPolicyV1::from_json(text).ok());
    let Some(artifact) = artifact else {
        // "No version-one policy" is not always "no organization policy":
        // say so when a requirement is recorded, when the record is damaged,
        // or when something is installed that this status cannot read.
        let resolve_error = match required {
            Ok(true) => Some(
                "an organization policy is required on this endpoint, but none that can be \
                 verified is installed"
                    .to_string(),
            ),
            Ok(false) => source
                .as_ref()
                .map(|_| {
                    "the installed organization policy is not a version-one artifact; it needs \
                     a verifier"
                        .to_string()
                })
                .or(trust_error),
            Err(error) => Some(error),
        };
        return OrgStatus {
            recorded,
            // Something is installed even when this status cannot read it.
            present: source.is_some(),
            source,
            resolve_error,
            pinned_anchors,
            ..Default::default()
        };
    };
    let verify = artifact.verify();
    let signer_key = artifact.signer_public_key.clone();
    // Trusted means what enforcement means by it (`resolve_checked`): this key
    // pinned for this artifact's organization. The same key pinned under
    // another organization does not count.
    let trusted = signer_key.as_deref().is_some_and(|signer| {
        anchors
            .iter()
            .any(|key| key.public_key == signer && key.org == artifact.org)
    });
    let resolve_result = artifact.resolved();
    // A version-one policy is not applied under a recorded requirement, nor
    // while the record or the trust store cannot be read: enforcement refuses
    // in each case, and the status must not say otherwise.
    let authority_error = match required {
        Ok(true) => Some(
            "an organization policy is required on this endpoint; the installed version-one \
             policy does not satisfy it"
                .to_string(),
        ),
        Ok(false) => trust_error,
        Err(error) => Some(error),
    };
    let applied = authority_error.is_none()
        && trusted
        && verify.signature_valid
        && artifact.enforced
        && resolve_result.is_ok();

    OrgStatus {
        recorded,
        present: true,
        source,
        org: Some(artifact.org.clone()),
        policy_version: Some(artifact.policy_version.clone()),
        enforced: artifact.enforced,
        issued_at: Some(artifact.issued_at.clone()),
        signer_public_key: signer_key,
        signature_valid: verify.signature_valid,
        trusted,
        applied,
        // The floor of the artifact read above, for a preview: the same
        // bytes this status describes, not a second load.
        floor_preview: resolve_result.as_ref().ok().cloned(),
        resolve_error: authority_error.or_else(|| resolve_result.err().map(|e| e.to_string())),
        pinned_anchors,
    }
}
