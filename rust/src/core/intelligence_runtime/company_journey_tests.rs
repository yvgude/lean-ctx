// SPDX-License-Identifier: Apache-2.0
//! The organization policy verifier, end to end on this host: a really built
//! Runtime, packaged, signed, verified, installed and invoked over the
//! authenticated channel, deciding a signed organization policy.
//!
//! Nothing private is linked here. The Runtime executable, the signed policy,
//! the markers and the lease come from a fixture bundle that is produced
//! outside this crate and named by `LEANCTX_COMPANY_JOURNEY_BUNDLE`. Every
//! key in it is a TEST key. The tests are ignored by default and fail when
//! selected without a bundle: they never pass by doing nothing. Placing the
//! first marker is fixture setup; no test here enrolls an endpoint.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use serde_json::json;

use super::sign_document;
use crate::core::intelligence_runtime::org_policy_verifier::{
    Accepted, Snapshot, VerifierCallError, verify as ask_verifier,
};
use crate::core::intelligence_runtime::{install, manifest, sha256};
use crate::core::policy::org;

const BUNDLE_ENV: &str = "LEANCTX_COMPANY_JOURNEY_BUNDLE";
/// Generous for one Runtime start on a loaded test host; the verifier itself
/// never takes more than its own budget of it.
const AMPLE: Duration = Duration::from_secs(30);

/// One isolated endpoint with the bundle's Runtime installed and configured,
/// the organization key pinned and the signed policy in place.
pub(crate) struct Endpoint {
    pub(crate) bundle: PathBuf,
    pub(crate) facts: serde_json::Value,
    pub(crate) policy_path: PathBuf,
    trust_path: PathBuf,
    marker_path: PathBuf,
    lease_path: PathBuf,
    _license: tempfile::TempDir,
    _root: tempfile::TempDir,
    // Dropped in this order: the inherited selections come back while the
    // environment lock of `_data` is still held.
    _selections: WithoutInheritedSelections,
    _data: crate::core::data_dir::IsolatedDataDir,
}

/// The variables that select organization authority from outside the
/// isolated directories: an explicit policy source and a protected launch.
const SELECTIONS: [&str; 3] = [
    "LEANCTX_ORG_POLICY",
    crate::core::policy::runtime::REQUIRED_POLICY_ROOT_ENV,
    crate::core::policy::runtime::REQUIRED_POLICY_DIGEST_ENV,
];

/// Removes [`SELECTIONS`] for as long as it lives and puts back exactly what
/// was there, also when a test unwinds. Without it an inherited explicit
/// policy source would be where this fixture writes its policy. Only made
/// while the environment lock is held.
struct WithoutInheritedSelections([Option<std::ffi::OsString>; 3]);

impl WithoutInheritedSelections {
    fn new() -> Self {
        let inherited = SELECTIONS.map(std::env::var_os);
        for name in SELECTIONS {
            crate::test_env::remove_var(name);
        }
        Self(inherited)
    }
}

impl Drop for WithoutInheritedSelections {
    fn drop(&mut self) {
        for (name, value) in SELECTIONS.iter().zip(&self.0) {
            match value {
                Some(value) => crate::test_env::set_var(name, value),
                None => crate::test_env::remove_var(name),
            }
        }
    }
}

#[test]
fn inherited_selections_are_set_aside_and_put_back_exactly() {
    let data = crate::core::data_dir::isolated_data_dir();
    // What this process really inherited is set aside first and comes back
    // when the test ends or unwinds; the values below are synthetic.
    let _real = WithoutInheritedSelections::new();
    let outside = tempfile::tempdir().unwrap();
    let sentinel = outside.path().join("someone-elses-policy.json");
    std::fs::write(&sentinel, b"not this fixture's to write").unwrap();
    // One selection inherited with a value, one empty, one absent.
    let inherited = [Some(sentinel.as_os_str().to_owned()), Some("".into()), None];
    for (name, value) in SELECTIONS.iter().zip(&inherited) {
        match value {
            Some(value) => crate::test_env::set_var(name, value),
            None => crate::test_env::remove_var(name),
        }
    }
    assert_eq!(
        org::store::selected_source().unwrap(),
        (sentinel.clone(), true)
    );

    let guard = WithoutInheritedSelections::new();
    assert!(
        SELECTIONS
            .iter()
            .all(|name| std::env::var_os(name).is_none())
    );
    let (selected, explicit) = org::store::selected_source().unwrap();
    assert!(!explicit && selected.starts_with(data.path()));
    // Whatever happens to the variables meanwhile, the originals come back.
    for name in SELECTIONS {
        crate::test_env::set_var(name, "changed-inside");
    }
    drop(guard);

    assert_eq!(SELECTIONS.map(std::env::var_os), inherited);
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"not this fixture's to write"
    );
}

pub(crate) fn bundle_file(bundle: &Path, name: &str) -> Vec<u8> {
    std::fs::read(bundle.join(name))
        .unwrap_or_else(|error| panic!("fixture bundle member {name}: {error}"))
}

fn text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

pub(crate) fn instant(timestamp: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// The seven members of a package, around the bundle's real executable and
/// its real self-description.
fn archive(bundle: &Path, commit: &str) -> (Vec<u8>, Vec<u8>) {
    let lock = bundle_file(bundle, "Cargo.lock");
    let provenance = serde_json::to_vec(&json!({
        "source_revision": commit, "release_approved": false, "entitlement_required": true
    }))
    .unwrap();
    let files = [
        (
            "leanctx-intelligence",
            bundle_file(bundle, "leanctx-intelligence"),
        ),
        (
            "runtime-description.json",
            bundle_file(bundle, "runtime-description.json"),
        ),
        ("SOURCE-PROVENANCE.json", provenance),
        ("Cargo.lock", lock.clone()),
        // No bill of materials exists for a local TEST build.
        ("SBOM.cdx.json", b"{}".to_vec()),
        ("LICENSE.md", bundle_file(bundle, "LICENSE.md")),
        (
            "LICENSE_MATRIX.toml",
            bundle_file(bundle, "LICENSE_MATRIX.toml"),
        ),
    ];
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(encoder);
    for (name, bytes) in files {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    (tar.into_inner().unwrap().finish().unwrap(), lock)
}

impl Endpoint {
    /// `marker` names the bundle's marker this endpoint starts with, if any.
    pub(crate) fn new(marker: Option<&str>) -> Self {
        let bundle = PathBuf::from(std::env::var_os(BUNDLE_ENV).unwrap_or_else(|| {
            panic!("{BUNDLE_ENV} must name the TEST fixture bundle for this journey")
        }));
        let facts: serde_json::Value =
            serde_json::from_slice(&bundle_file(&bundle, "bundle.json")).unwrap();
        let data = crate::core::data_dir::isolated_data_dir();
        let selections = WithoutInheritedSelections::new();

        let commit = String::from_utf8(bundle_file(&bundle, "source-revision")).unwrap();
        let (archive, lock) = archive(&bundle, commit.trim());
        let document = json!({
            "schema": "leanctx.private-runtime-artifact/v3", "commit": commit.trim(),
            "lockfile_sha256": sha256(&lock), "handshake_schema": "leanctx.runtime-handshake/v1",
            "wire_protocol": "leanctx.protocol/v4", "license": "LicenseRef-LeanCTX-Staging-Only",
            "artifact_sha256": sha256(&archive), "signature": "ed25519-detached-v1",
            "rollback_artifact": null
        });
        // TEST manifest anchor: its own key, neither the vendor's nor the
        // organization's.
        let anchor = SigningKey::from_bytes(&[83; 32]);
        let (manifest, signature, selected) =
            sign_document(&document, &anchor, b"leanctx-release-manifest-v3\0");
        let anchor_key = anchor.verifying_key().to_bytes();
        let verified =
            manifest::verify(&archive, &manifest, &signature, &selected, &anchor_key).unwrap();
        assert!(verified.receipt.staging_only);
        let root_directory = super::super::private_root();
        let root = root_directory.path().canonicalize().unwrap();
        install::install(&root, "none", &verified, &archive, &manifest, &signature).unwrap();

        let license = super::super::private_root();
        let license_directory = license.path().canonicalize().unwrap();
        let lease_path = license_directory.join("lease.json");
        std::fs::write(&lease_path, bundle_file(&bundle, "lease.json")).unwrap();
        let license_configuration = license_directory.join("license-config.json");
        std::fs::write(
            &license_configuration,
            serde_json::to_vec(&json!({
                "artifact_path": lease_path,
                "organization_id": facts["organization_id"],
                "installation_id": facts["installation_id"],
                "state_directory": license_directory.join("license-state"),
                "trusted_vendor_keys": [facts["vendor_key_hex"]],
            }))
            .unwrap(),
        )
        .unwrap();

        let expected = crate::core::config::IntelligenceRuntimeConfig {
            enabled: true,
            accept_proprietary: true,
            staging: true,
            root: root.to_str().unwrap().to_owned(),
            manifest_sha256: selected,
            trust_key_hex: hex::encode(anchor_key),
            license_configuration: license_configuration.to_str().unwrap().to_owned(),
            ..Default::default()
        };
        crate::core::config::Config::update_global(|config| {
            config.intelligence_runtime = expected.clone();
        })
        .unwrap();
        assert_eq!(
            crate::core::config::Config::load_global().intelligence_runtime,
            expected
        );

        let organization = facts["organization_id"].as_str().unwrap();
        let key = String::from_utf8(bundle_file(&bundle, "org-public-key.hex")).unwrap();
        assert!(org::trust::pin(organization, key.trim()).unwrap());
        // Nothing is written unless the selected source is the installed
        // file of the isolated configuration directory, not an explicit one.
        let (policy_path, explicit) = org::store::selected_source().unwrap();
        assert!(!explicit);
        assert!(policy_path.starts_with(data.path()));
        assert_eq!(
            policy_path.parent(),
            crate::core::paths::config_dir_read_only().ok().as_deref()
        );
        std::fs::write(&policy_path, bundle_file(&bundle, "policy-enforce.json")).unwrap();
        let marker_path = org::marker::marker_path_read_only().unwrap();
        if let Some(name) = marker {
            std::fs::write(&marker_path, bundle_file(&bundle, name)).unwrap();
        }
        Self {
            trust_path: org::trust::trust_path_read_only().unwrap(),
            bundle,
            facts,
            policy_path,
            marker_path,
            lease_path,
            _license: license,
            _root: root_directory,
            _selections: selections,
            _data: data,
        }
    }

    /// One real verification of what is stored now.
    fn decide(&self) -> Result<org::VerifiedV2, String> {
        let marker = std::fs::read_to_string(&self.marker_path).ok();
        org::verify_v2(
            &text(&self.policy_path),
            Some(&text(&self.trust_path)),
            marker.as_deref(),
            Instant::now() + AMPLE,
        )
    }

    /// The newest entry of the stored marker and how many entries it has.
    fn marker_head(&self) -> (serde_json::Value, usize) {
        let marker: serde_json::Value = serde_json::from_str(&text(&self.marker_path)).unwrap();
        let entries = marker["entries"].as_array().unwrap();
        (entries.last().unwrap().clone(), entries.len())
    }
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn installed_runtime_decides_the_signed_policy_and_the_decision_is_recorded_first() {
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    let earlier = text(&endpoint.marker_path);
    let organization =
        lean_ctx_protocol::TeamId::new(endpoint.facts["organization_id"].as_str().unwrap())
            .unwrap();

    // The call alone: the authenticated package answers, nothing is stored.
    let answer = ask_verifier(
        &Snapshot {
            organization: &organization,
            artifact: text(&endpoint.policy_path).as_bytes(),
            trust: Some(text(&endpoint.trust_path).as_bytes()),
            marker: Some(earlier.as_bytes()),
        },
        Instant::now() + AMPLE,
    );
    let (pack_toml, policy_digest, sequence, valid_until) = match answer {
        Ok(Accepted::Enforce {
            pack_toml,
            policy_digest,
            sequence,
            valid_until,
        }) => (pack_toml, policy_digest, sequence, valid_until),
        Ok(Accepted::NotRequired { .. }) => panic!("the installed Runtime lifted the requirement"),
        Err(error) => panic!("the installed Runtime did not answer: {error:?}"),
    };
    assert_eq!(pack_toml, endpoint.facts["pack_toml"].as_str().unwrap());
    assert_eq!(json!(policy_digest), endpoint.facts["policy_digest"]);
    assert_eq!(json!(sequence), endpoint.facts["policy_sequence"]);
    // The same instant; the Runtime renders it in its own precision.
    assert_eq!(
        instant(valid_until.as_str()),
        instant(endpoint.facts["not_after"].as_str().unwrap())
    );
    assert_eq!(text(&endpoint.marker_path), earlier);

    // The decision as the Engine uses it: recorded on the captured marker,
    // then the signed floor.
    let verified = endpoint.decide().unwrap();
    let floor = verified.floor.expect("an enforced policy has a floor");
    assert!(floor.deny_tools.iter().any(|tool| tool == "ctx_shell"));
    assert_eq!(verified.marker, text(&endpoint.marker_path));
    assert_ne!(verified.marker, earlier);
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, 2);
    assert_eq!(head["sequence"], endpoint.facts["policy_sequence"]);
    assert_eq!(head["state"]["kind"], "required");
    assert_eq!(
        head["state"]["policy_digest"],
        endpoint.facts["policy_digest"]
    );

    // The same decision again appends nothing.
    let again = endpoint.decide().unwrap();
    assert_eq!(again.marker, verified.marker);
    assert_eq!(text(&endpoint.marker_path), verified.marker);
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_full_width_signed_policy_travels_from_its_file_to_the_floor_and_back() {
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    let pack = String::from_utf8(bundle_file(&endpoint.bundle, "pack-wide.toml")).unwrap();
    assert_eq!(
        pack.len(),
        lean_ctx_protocol::org_policy_verifier::MAX_ORG_POLICY_VERIFIED_PACK_BYTES
    );
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-wide.json"),
    )
    .unwrap();
    // Wider than every ordinary policy input and than an ordinary exchange.
    let artifact_bytes = std::fs::metadata(&endpoint.policy_path).unwrap().len();
    assert!(artifact_bytes > 2 * crate::core::policy::files::MAX_BYTES);

    // From the file itself: captured, asked over the installed Runtime,
    // recorded, read again, and only then the floor.
    let captured = org::AuthoritySnapshot::capture().unwrap();
    let floor = captured
        .resolve()
        .unwrap()
        .expect("an enforced policy has a floor");
    assert!(floor.deny_tools.iter().any(|tool| tool == "ctx_shell"));
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, 2);
    assert_eq!(
        head["state"]["policy_digest"],
        endpoint.facts["wide_policy_digest"]
    );

    // The answer carries the signed pack byte for byte.
    let organization =
        lean_ctx_protocol::TeamId::new(endpoint.facts["organization_id"].as_str().unwrap())
            .unwrap();
    let answer = ask_verifier(
        &Snapshot {
            organization: &organization,
            artifact: text(&endpoint.policy_path).as_bytes(),
            trust: Some(text(&endpoint.trust_path).as_bytes()),
            marker: Some(text(&endpoint.marker_path).as_bytes()),
        },
        Instant::now() + AMPLE,
    );
    match answer {
        // Compared by length and digest: a megabyte is no failure message.
        Ok(Accepted::Enforce { pack_toml, .. }) => assert_eq!(
            (pack_toml.len(), sha256(pack_toml.as_bytes())),
            (pack.len(), sha256(pack.as_bytes()))
        ),
        Ok(Accepted::NotRequired { .. }) => panic!("the installed Runtime lifted the requirement"),
        Err(error) => panic!("the installed Runtime did not answer: {error:?}"),
    }

    // The same bytes without the version-two shape are an ordinary policy
    // input and stay at its bound: the larger read admits nothing by itself.
    std::fs::write(&endpoint.policy_path, "x".repeat(artifact_bytes as usize)).unwrap();
    assert_eq!(
        org::AuthoritySnapshot::capture()
            .unwrap()
            .resolve()
            .err()
            .as_deref(),
        Some("policy input exceeds byte limit")
    );
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_source_view_runs_on_a_full_width_policy_within_the_refresh_bound() {
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-wide.json"),
    )
    .unwrap();
    let project = tempfile::tempdir().unwrap();
    // Through the refresh worker and its own request bound, twice: opening
    // the view and closing it. The operation really runs, on the signed floor.
    let ran = std::cell::Cell::new(0);
    let shell_allowed =
        crate::core::policy::runtime::with_explicit_project_source_view(project.path(), |_| {
            ran.set(ran.get() + 1);
            crate::core::policy::runtime::active().map(|policy| policy.tool_allowed("ctx_shell"))
        });
    assert_eq!(shell_allowed, Ok(Some(false)));
    assert_eq!(ran.get(), 1);
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, 2);
    assert_eq!(
        head["state"]["policy_digest"],
        endpoint.facts["wide_policy_digest"]
    );
}

/// One more valid `[[keys]]` entry of another organization, as the trust
/// store writes them.
fn neighbour(index: u32) -> String {
    let mut seed = [0x5a_u8; 32];
    seed[..4].copy_from_slice(&index.to_be_bytes());
    format!(
        "\n[[keys]]\norg = \"neighbour-{index}\"\npublic_key = \"{}\"\n\
         added_at = \"2026-10-10T00:00:00+00:00\"\n",
        hex::encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
    )
}

/// Fill the endpoint's real trust store to its ceiling: the organization's
/// pinned key and as many other organizations' valid keys as fit. Returns the
/// stored text and how many were added.
fn widest_trust_store(endpoint: &Endpoint) -> (String, u32) {
    let ceiling = lean_ctx_protocol::org_policy_verifier::MAX_ORG_REQUIRED_MARKER_BYTES;
    let mut trust = text(&endpoint.trust_path);
    let mut neighbours = 0_u32;
    while trust.len() + neighbour(neighbours).len() <= ceiling {
        trust.push_str(&neighbour(neighbours));
        neighbours += 1;
    }
    assert!(trust.len() > ceiling - 256 && trust.len() <= ceiling);
    std::fs::write(&endpoint.trust_path, &trust).unwrap();
    let pinned: org::TrustStore = toml::from_str(&trust).unwrap();
    assert_eq!(pinned.keys.len(), neighbours as usize + 1);
    (trust, neighbours)
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_source_view_runs_with_every_input_at_its_widest_within_the_refresh_bound() {
    let endpoint = Endpoint::new(Some("marker-long.json"));
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-wide.json"),
    )
    .unwrap();
    widest_trust_store(&endpoint);
    let history = endpoint.facts["long_marker_entries"].as_u64().unwrap() as usize;
    assert_eq!(endpoint.marker_head().1, history);

    // The widest trust store, the longest marker and the full-width pack
    // together, asked twice through the refresh worker's own request bound:
    // when the view opens — which also records the decision — and when it
    // closes. The operation runs once, on the signed floor.
    let project = tempfile::tempdir().unwrap();
    let ran = std::cell::Cell::new(0);
    let shell_allowed =
        crate::core::policy::runtime::with_explicit_project_source_view(project.path(), |_| {
            ran.set(ran.get() + 1);
            crate::core::policy::runtime::active().map(|policy| policy.tool_allowed("ctx_shell"))
        });
    assert_eq!(shell_allowed, Ok(Some(false)));
    assert_eq!(ran.get(), 1);
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, history + 1);
    assert_eq!(head["sequence"], endpoint.facts["wide_policy_sequence"]);
    assert_eq!(
        head["state"]["policy_digest"],
        endpoint.facts["wide_policy_digest"]
    );
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_full_width_artifact_with_a_signed_unpin_lifts_the_requirement() {
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    // The full-width policy is enforced and recorded first.
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-wide.json"),
    )
    .unwrap();
    assert!(endpoint.decide().unwrap().floor.is_some());
    assert_eq!(
        endpoint.marker_head().0["sequence"],
        endpoint.facts["wide_policy_sequence"]
    );

    // The same artifact, as wide, now carrying a signed unpin above it: read
    // from its file, decided by the installed Runtime, recorded exactly.
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-wide-unpin.json"),
    )
    .unwrap();
    assert!(
        std::fs::metadata(&endpoint.policy_path).unwrap().len()
            > 2 * crate::core::policy::files::MAX_BYTES
    );
    let lifted = org::AuthoritySnapshot::capture().unwrap().resolve();
    assert!(matches!(lifted, Ok(None)));
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, 3);
    assert_eq!(head["sequence"], endpoint.facts["wide_unpin_sequence"]);
    assert_eq!(head["state"]["kind"], "unpinned");
    assert_eq!(
        head["state"]["unpin_digest"],
        endpoint.facts["wide_unpin_digest"]
    );
    let tombstone = text(&endpoint.marker_path);

    // The full-width policy without the unpin cannot bring it back.
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-wide.json"),
    )
    .unwrap();
    assert!(endpoint.decide().is_err());
    assert_eq!(text(&endpoint.marker_path), tombstone);
}

/// Install the bundle's artifact `name` and check what it is: the same
/// artifact as `ordinary` to a strict reader, with only its ASCII pack
/// spelled as one `\u00XX` per byte — about six megabytes as a file, inside
/// the artifact read bound. This is one legal spelling near that bound, not
/// a claim about the largest JSON any valid artifact could be written as.
fn install_escaped(endpoint: &Endpoint, name: &str, ordinary: &str) {
    let escaped = bundle_file(&endpoint.bundle, name);
    let pack = lean_ctx_protocol::org_policy_verifier::MAX_ORG_POLICY_VERIFIED_PACK_BYTES;
    assert!(escaped.len() > 6 * pack);
    assert!(escaped.len() as u64 <= crate::core::policy::files::MAX_ORG_ARTIFACT_BYTES);
    let read = |bytes: &[u8]| serde_json::from_slice::<serde_json::Value>(bytes).unwrap();
    assert_eq!(
        read(&escaped),
        read(&bundle_file(&endpoint.bundle, ordinary))
    );
    std::fs::write(&endpoint.policy_path, escaped).unwrap();
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_source_view_runs_on_a_near_six_megabyte_ascii_spelling_of_a_full_width_artifact() {
    // With the endpoint's ordinary trust store and a short marker; this
    // spelling together with the widest trust store and the longest marker
    // is not driven here.
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    install_escaped(&endpoint, "policy-wide-escaped.json", "policy-wide.json");
    let pack = String::from_utf8(bundle_file(&endpoint.bundle, "pack-wide.toml")).unwrap();

    // File → refresh worker (2 s, twice) → installed Runtime → marker → floor.
    let project = tempfile::tempdir().unwrap();
    let ran = std::cell::Cell::new(0);
    let shell_allowed =
        crate::core::policy::runtime::with_explicit_project_source_view(project.path(), |_| {
            ran.set(ran.get() + 1);
            crate::core::policy::runtime::active().map(|policy| policy.tool_allowed("ctx_shell"))
        });
    assert_eq!(shell_allowed, Ok(Some(false)));
    assert_eq!(ran.get(), 1);
    // The statement is the one of the ordinary spelling: same signed digest.
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, 2);
    assert_eq!(head["sequence"], endpoint.facts["wide_policy_sequence"]);
    assert_eq!(
        head["state"]["policy_digest"],
        endpoint.facts["wide_policy_digest"]
    );

    // The answer carries the decoded pack, byte for byte the megabyte that
    // was signed — not the six megabytes of its spelling.
    let organization =
        lean_ctx_protocol::TeamId::new(endpoint.facts["organization_id"].as_str().unwrap())
            .unwrap();
    let answer = ask_verifier(
        &Snapshot {
            organization: &organization,
            artifact: text(&endpoint.policy_path).as_bytes(),
            trust: Some(text(&endpoint.trust_path).as_bytes()),
            marker: Some(text(&endpoint.marker_path).as_bytes()),
        },
        Instant::now() + AMPLE,
    );
    match answer {
        Ok(Accepted::Enforce { pack_toml, .. }) => assert_eq!(
            (pack_toml.len(), sha256(pack_toml.as_bytes())),
            (pack.len(), sha256(pack.as_bytes()))
        ),
        Ok(Accepted::NotRequired { .. }) => panic!("the installed Runtime lifted the requirement"),
        Err(error) => panic!("the installed Runtime did not answer: {error:?}"),
    }
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_source_view_runs_on_the_near_six_megabyte_spelling_with_the_widest_trust_and_marker() {
    // The three implemented widths driven together: the near-six-megabyte
    // ASCII spelling of the full-width artifact, the trust store at its
    // ceiling and the longest marker that can still take one more entry.
    let endpoint = Endpoint::new(Some("marker-long.json"));
    install_escaped(&endpoint, "policy-wide-escaped.json", "policy-wide.json");
    widest_trust_store(&endpoint);
    let history = endpoint.facts["long_marker_entries"].as_u64().unwrap() as usize;
    assert_eq!(endpoint.marker_head().1, history);

    // Two refreshes under the refresh worker's own unchanged request bound;
    // the operation runs once, on the signed floor.
    let project = tempfile::tempdir().unwrap();
    let ran = std::cell::Cell::new(0);
    let shell_allowed =
        crate::core::policy::runtime::with_explicit_project_source_view(project.path(), |_| {
            ran.set(ran.get() + 1);
            crate::core::policy::runtime::active().map(|policy| policy.tool_allowed("ctx_shell"))
        });
    assert_eq!(shell_allowed, Ok(Some(false)));
    assert_eq!(ran.get(), 1);
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, history + 1);
    assert_eq!(head["sequence"], endpoint.facts["wide_policy_sequence"]);
    assert_eq!(
        head["state"]["policy_digest"],
        endpoint.facts["wide_policy_digest"]
    );
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_near_six_megabyte_ascii_spelling_with_a_signed_unpin_lifts_the_requirement() {
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    install_escaped(&endpoint, "policy-wide-escaped.json", "policy-wide.json");
    assert!(endpoint.decide().unwrap().floor.is_some());
    assert_eq!(
        endpoint.marker_head().0["sequence"],
        endpoint.facts["wide_policy_sequence"]
    );

    install_escaped(
        &endpoint,
        "policy-wide-unpin-escaped.json",
        "policy-wide-unpin.json",
    );
    let lifted = org::AuthoritySnapshot::capture().unwrap().resolve();
    assert!(matches!(lifted, Ok(None)));
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, 3);
    assert_eq!(head["sequence"], endpoint.facts["wide_unpin_sequence"]);
    assert_eq!(head["state"]["kind"], "unpinned");
    assert_eq!(
        head["state"]["unpin_digest"],
        endpoint.facts["wide_unpin_digest"]
    );
    let tombstone = text(&endpoint.marker_path);

    // Neither spelling of the policy without the unpin brings it back.
    for (name, ordinary) in [
        ("policy-wide-escaped.json", "policy-wide.json"),
        ("policy-wide.json", "policy-wide.json"),
    ] {
        if name == ordinary {
            std::fs::write(&endpoint.policy_path, bundle_file(&endpoint.bundle, name)).unwrap();
        } else {
            install_escaped(&endpoint, name, ordinary);
        }
        assert!(endpoint.decide().is_err());
        assert_eq!(text(&endpoint.marker_path), tombstone);
    }
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn the_widest_trust_store_and_the_longest_marker_reach_the_installed_runtime() {
    let endpoint = Endpoint::new(Some("marker-long.json"));
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-wide.json"),
    )
    .unwrap();
    let ceiling = lean_ctx_protocol::org_policy_verifier::MAX_ORG_REQUIRED_MARKER_BYTES;
    let (mut trust, neighbours) = widest_trust_store(&endpoint);
    // The longest history a marker may have before one more decision.
    let history = endpoint.facts["long_marker_entries"].as_u64().unwrap() as usize;
    assert_eq!(endpoint.marker_head().1, history);
    assert_eq!(
        history + 1,
        lean_ctx_protocol::org_policy_verifier::MAX_ORG_REQUIRED_MARKER_ENTRIES
    );

    // All three inputs at their widest, from their files to the floor.
    let floor = org::AuthoritySnapshot::capture()
        .unwrap()
        .resolve()
        .unwrap()
        .expect("an enforced policy has a floor");
    assert!(floor.deny_tools.iter().any(|tool| tool == "ctx_shell"));
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, history + 1);
    assert_eq!(head["sequence"], endpoint.facts["wide_policy_sequence"]);
    assert_eq!(
        head["state"]["policy_digest"],
        endpoint.facts["wide_policy_digest"]
    );
    let full = text(&endpoint.marker_path);

    // One more organization does not fit the trust store's ceiling: refused
    // before anything is asked, and nothing recorded.
    trust.push_str(&neighbour(neighbours));
    assert!(trust.len() > ceiling);
    std::fs::write(&endpoint.trust_path, &trust).unwrap();
    assert_eq!(
        endpoint.decide().err().as_deref(),
        Some("organization policy could not be verified")
    );
    assert_eq!(text(&endpoint.marker_path), full);
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_source_view_runs_on_the_floor_the_installed_runtime_signed() {
    let endpoint = Endpoint::new(Some("marker-current.json"));
    let project = tempfile::tempdir().unwrap();
    let shell_allowed =
        crate::core::policy::runtime::with_explicit_project_source_view(project.path(), |_| {
            crate::core::policy::runtime::active().map(|policy| policy.tool_allowed("ctx_shell"))
        });
    assert_eq!(shell_allowed, Ok(Some(false)));
    drop(endpoint);
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_signed_unpin_lifts_the_requirement_for_good() {
    let endpoint = Endpoint::new(Some("marker-current.json"));
    let enforce = text(&endpoint.policy_path);
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-unpin.json"),
    )
    .unwrap();

    let lifted = endpoint.decide().unwrap();
    assert!(lifted.floor.is_none());
    let (head, entries) = endpoint.marker_head();
    assert_eq!(entries, 2);
    assert_eq!(head["sequence"], endpoint.facts["unpin_sequence"]);
    assert_eq!(head["state"]["kind"], "unpinned");
    assert_eq!(
        head["state"]["unpin_digest"],
        endpoint.facts["unpin_digest"]
    );
    let tombstone = text(&endpoint.marker_path);
    assert_eq!(lifted.marker, tombstone);

    // Asked again, the same answer and the same record.
    assert!(endpoint.decide().unwrap().floor.is_none());
    assert_eq!(text(&endpoint.marker_path), tombstone);

    // The earlier policy cannot bring the requirement back.
    std::fs::write(&endpoint.policy_path, enforce).unwrap();
    assert!(endpoint.decide().is_err());
    assert_eq!(text(&endpoint.marker_path), tombstone);
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn no_marker_or_a_damaged_one_is_refused_before_anything_is_asked() {
    let endpoint = Endpoint::new(None);
    assert_eq!(
        endpoint.decide().err().as_deref(),
        Some("organization policy has no recorded requirement to verify against")
    );
    assert!(!endpoint.marker_path.exists());

    std::fs::write(&endpoint.marker_path, "{}").unwrap();
    assert_eq!(
        endpoint.decide().err().as_deref(),
        Some("org required marker is invalid")
    );
    assert_eq!(text(&endpoint.marker_path), "{}");
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_decision_for_another_marker_than_the_stored_one_is_not_recorded() {
    let endpoint = Endpoint::new(Some("marker-current.json"));
    let stored = text(&endpoint.marker_path);
    let captured = String::from_utf8(bundle_file(&endpoint.bundle, "marker-earlier.json")).unwrap();
    assert_ne!(captured, stored);

    // The Runtime answers for the captured marker; the record refuses it.
    let refused = org::verify_v2(
        &text(&endpoint.policy_path),
        Some(&text(&endpoint.trust_path)),
        Some(&captured),
        Instant::now() + AMPLE,
    );
    assert_eq!(
        refused.err().as_deref(),
        Some("organization policy decision could not be recorded")
    );
    assert_eq!(text(&endpoint.marker_path), stored);
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn authority_that_changed_while_the_runtime_answered_is_refused() {
    let endpoint = Endpoint::new(Some("marker-current.json"));
    let captured = org::AuthoritySnapshot::capture().unwrap();
    let mut trust = text(&endpoint.trust_path);
    trust.push_str("\n# changed after the capture\n");
    std::fs::write(&endpoint.trust_path, trust).unwrap();

    // The captured bytes verify; what is stored now is no longer them.
    assert_eq!(
        captured.resolve().err().as_deref(),
        Some("organization authority changed during verification")
    );
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_removed_lease_ends_enforcement_although_no_policy_byte_changed() {
    let endpoint = Endpoint::new(Some("marker-current.json"));
    let project = tempfile::tempdir().unwrap();
    let authority = (
        text(&endpoint.policy_path),
        text(&endpoint.trust_path),
        text(&endpoint.marker_path),
    );
    let ran = std::cell::Cell::new(0);
    let view = || {
        crate::core::policy::runtime::with_explicit_project_source_view(project.path(), |_| {
            ran.set(ran.get() + 1);
        })
    };
    assert_eq!(view(), Ok(()));
    assert_eq!(ran.get(), 1);

    // Identical bytes are no reason to enforce again: the refresh asks anew,
    // and without the lease nothing answers. The operation never runs.
    std::fs::remove_file(&endpoint.lease_path).unwrap();
    assert!(view().is_err());
    assert_eq!(ran.get(), 1);
    assert_eq!(
        endpoint.decide().err().as_deref(),
        Some("organization policy could not be verified")
    );
    assert_eq!(
        (
            text(&endpoint.policy_path),
            text(&endpoint.trust_path),
            text(&endpoint.marker_path),
        ),
        authority
    );
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_real_decision_that_cannot_take_the_marker_lock_changes_nothing() {
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    let stored = text(&endpoint.marker_path);
    // Another writer holds the marker lock for the whole call. The Runtime
    // answers; recording gives up at the lock's own bound, which here is
    // shorter than the request's. That a shorter request deadline ends the
    // wait instead is shown at the marker itself, without a Runtime.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(endpoint.marker_path.with_extension("lock"))
        .unwrap();
    crate::core::file_lock::acquire_exclusive_timeout(&lock, Duration::from_secs(1)).unwrap();

    let started = Instant::now();
    let decided = org::verify_v2(
        &text(&endpoint.policy_path),
        Some(&text(&endpoint.trust_path)),
        Some(&stored),
        started + AMPLE,
    );
    assert_eq!(
        decided.err().as_deref(),
        Some("organization policy decision could not be recorded")
    );
    assert_eq!(text(&endpoint.marker_path), stored);
    drop(lock);

    // With the lock free again the same decision is recorded.
    assert!(endpoint.decide().unwrap().floor.is_some());
    assert_ne!(text(&endpoint.marker_path), stored);
}

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_request_that_has_run_out_asks_nothing_and_records_nothing() {
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    let stored = text(&endpoint.marker_path);
    let organization =
        lean_ctx_protocol::TeamId::new(endpoint.facts["organization_id"].as_str().unwrap())
            .unwrap();
    let spent = Instant::now();

    let answer = ask_verifier(
        &Snapshot {
            organization: &organization,
            artifact: text(&endpoint.policy_path).as_bytes(),
            trust: Some(text(&endpoint.trust_path).as_bytes()),
            marker: Some(stored.as_bytes()),
        },
        spent,
    );
    assert_eq!(answer.err(), Some(VerifierCallError::Deadline));
    let decided = org::verify_v2(
        &text(&endpoint.policy_path),
        Some(&text(&endpoint.trust_path)),
        Some(&stored),
        spent,
    );
    assert_eq!(
        decided.err().as_deref(),
        Some("organization policy could not be verified")
    );
    assert_eq!(text(&endpoint.marker_path), stored);
}
