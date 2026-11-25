// SPDX-License-Identifier: Apache-2.0
//! Fixtures are the unmodified signature assets of the v3.11.0 GitHub release
//! (`*.pem` there, stored as `*.cert` so secret scanners do not mistake the
//! public certificates for key material).

use super::*;

const SUMS: &[u8] = include_bytes!("testdata/v3.11.0/SHA256SUMS");
const SUMS_SIG: &[u8] = include_bytes!("testdata/v3.11.0/SHA256SUMS.sig");
const SUMS_PEM: &[u8] = include_bytes!("testdata/v3.11.0/SHA256SUMS.cert");
const MANIFEST: &[u8] = include_bytes!("testdata/v3.11.0/release-manifest.json");
const MANIFEST_SIG: &[u8] = include_bytes!("testdata/v3.11.0/release-manifest.json.sig");
const MANIFEST_PEM: &[u8] = include_bytes!("testdata/v3.11.0/release-manifest.json.cert");

#[test]
fn verifies_the_real_v3_11_0_release_signatures() {
    verify_blob(SUMS, SUMS_SIG, SUMS_PEM, "v3.11.0").expect("SHA256SUMS");
    verify_blob(MANIFEST, MANIFEST_SIG, MANIFEST_PEM, "v3.11.0").expect("release manifest");
}

#[test]
fn rejects_a_modified_file() {
    let mut tampered = SUMS.to_vec();
    tampered[0] ^= 1;
    let err = verify_blob(&tampered, SUMS_SIG, SUMS_PEM, "v3.11.0").unwrap_err();
    assert!(err.contains("signature does not match"), "{err}");
}

#[test]
fn rejects_a_signature_made_for_another_file() {
    assert!(verify_blob(SUMS, MANIFEST_SIG, MANIFEST_PEM, "v3.11.0").is_err());
}

#[test]
fn rejects_a_signature_from_another_release_tag() {
    // A genuine signature of v3.11.0 must not vouch for a different release.
    let err = verify_blob(SUMS, SUMS_SIG, SUMS_PEM, "v3.11.1").unwrap_err();
    assert!(err.contains("release workflow identity"), "{err}");
    assert!(err.contains("refs/tags/v3.11.1"), "{err}");
}

#[test]
fn rejects_a_modified_certificate() {
    let leaf = decode_certificate(SUMS_PEM).unwrap();
    // Swap in another key: flip the last byte of the subject public key.
    let parsed = Certificate::parse(&leaf).unwrap();
    let spki_end = parsed.spki.as_ptr() as usize - leaf.as_ptr() as usize + parsed.spki.len();
    let mut tampered = leaf.clone();
    tampered[spki_end - 1] ^= 0x01;
    let pem = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        B64.encode(&tampered)
    );
    assert!(verify_blob(SUMS, SUMS_SIG, pem.as_bytes(), "v3.11.0").is_err());
}

#[test]
fn accepts_plain_pem_as_well_as_cosigns_base64_wrapped_pem() {
    let pem = B64.decode(SUMS_PEM.trim_ascii()).unwrap();
    assert!(pem.starts_with(b"-----BEGIN CERTIFICATE-----"));
    verify_blob(SUMS, SUMS_SIG, &pem, "v3.11.0").expect("plain PEM");
}

#[test]
fn precert_reencoding_is_byte_exact() {
    // Removing nothing must reproduce the signed TBSCertificate exactly,
    // otherwise SCT verification would rest on a wrong re-encoding.
    let leaf_der = decode_certificate(SUMS_PEM).unwrap();
    let leaf = Certificate::parse(&leaf_der).unwrap();
    let (cert, _) = expect(&leaf_der, TAG_SEQUENCE).unwrap();
    let (tbs, _) = expect(cert.content, TAG_SEQUENCE).unwrap();
    assert_eq!(leaf.tbs_without(&[]), tbs.raw);
    assert!(leaf.tbs_without(OID_SCT_LIST).len() < tbs.raw.len());
}

#[test]
fn trust_chain_and_leaf_validity_are_read_correctly() {
    let root_der = B64.decode(FULCIO_ROOT_B64).unwrap();
    let intermediate_der = B64.decode(FULCIO_INTERMEDIATE_B64).unwrap();
    let root = Certificate::parse(&root_der).unwrap();
    let intermediate = Certificate::parse(&intermediate_der).unwrap();
    // 2031-10-05T13:56:58Z, as published in trusted_root.json.
    assert_eq!(root.not_after, 1_948_975_018);
    assert_eq!(intermediate.not_after, root.not_after);
    // The intermediate verifies against the root at any time inside its window.
    verify_issued_by(&intermediate, &root, intermediate.not_before).unwrap();
    assert!(verify_issued_by(&intermediate, &root, root.not_after + 1).is_err());
    // Roles cannot be swapped.
    assert!(verify_issued_by(&root, &intermediate, intermediate.not_before).is_err());

    let leaf_der = decode_certificate(SUMS_PEM).unwrap();
    let leaf = Certificate::parse(&leaf_der).unwrap();
    assert_eq!(
        leaf.not_after - leaf.not_before,
        600,
        "Fulcio leaf lives 10 min"
    );
    verify_issued_by(&leaf, &intermediate, leaf.not_before).unwrap();
    // Outside the leaf window the signature would not count.
    assert!(verify_issued_by(&leaf, &intermediate, leaf.not_after + 1).is_err());
    verify_code_signing_leaf(&leaf).unwrap();
    // The intermediate itself is a CA and must never pass as a signing leaf.
    assert!(verify_code_signing_leaf(&intermediate).is_err());
}

#[test]
fn time_parsing_covers_both_x509_forms() {
    let utc = Tlv {
        tag: TAG_UTC_TIME,
        content: b"491231235959Z",
        raw: &[],
    };
    assert_eq!(parse_time(&utc).unwrap(), 2_524_607_999); // 2049-12-31T23:59:59Z
    let utc_1950 = Tlv {
        tag: TAG_UTC_TIME,
        content: b"500101000000Z",
        raw: &[],
    };
    assert_eq!(parse_time(&utc_1950).unwrap(), -631_152_000);
    let generalized = Tlv {
        tag: TAG_GENERALIZED_TIME,
        content: b"20311005135658Z",
        raw: &[],
    };
    assert_eq!(parse_time(&generalized).unwrap(), 1_948_975_018);
    for bad in [
        &b"2031100513565Z"[..],
        b"20311305135658Z",
        b"2031100513565+Z",
        b"20311005135658",
    ] {
        let tlv = Tlv {
            tag: TAG_GENERALIZED_TIME,
            content: bad,
            raw: &[],
        };
        assert!(
            parse_time(&tlv).is_err(),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn embedded_ct_log_key_matches_the_trusted_root_log_id() {
    let spki = B64.decode(CT_LOG_SPKI_B64).unwrap();
    assert_eq!(
        B64.encode(Sha256::digest(&spki)),
        "3T0wasbHETJjGR4cmWc3AqJKXrjePK3/h4pygC8p7o4="
    );
}

#[test]
fn identity_is_pinned_to_the_release_workflow_and_tag() {
    assert_eq!(
        release_identity("v3.11.0"),
        "https://github.com/yvgude/lean-ctx/.github/workflows/release.yml@refs/tags/v3.11.0"
    );
    // The external cosign check gets the same identity as an anchored regexp.
    let identity = regex::Regex::new(&cosign_identity_regexp("v3.9.20")).unwrap();
    assert!(identity.is_match(&release_identity("v3.9.20")));
    assert!(!identity.is_match(&release_identity("v3x9x20")));
    assert!(!identity.is_match(&format!("{}-evil", release_identity("v3.9.20"))));
}

#[test]
fn release_check_needs_no_cosign_binary() {
    // 3.11.0 refused every update without a `cosign` on PATH. The in-process
    // check alone must accept a genuine release when none is installed…
    let absent = "lean-ctx-test-no-such-cosign-binary";
    verify_release_signature_with(SUMS, SUMS_SIG, SUMS_PEM, "v3.11.0", absent)
        .expect("genuine release verifies without cosign");
    // …and still refuse a forged one.
    let mut forged = SUMS.to_vec();
    forged[0] ^= 1;
    assert!(verify_release_signature_with(&forged, SUMS_SIG, SUMS_PEM, "v3.11.0", absent).is_err());
}

#[cfg(unix)]
#[test]
fn an_installed_cosign_that_rejects_blocks_the_update() {
    // `false` stands in for a cosign whose Rekor lookup fails.
    let err =
        verify_release_signature_with(SUMS, SUMS_SIG, SUMS_PEM, "v3.11.0", "false").unwrap_err();
    assert!(
        err.contains("cosign release signature verification failed"),
        "{err}"
    );
}

#[test]
fn a_local_io_error_in_the_cosign_check_is_not_reported_as_a_bad_signature() {
    // A full disk or quota made `lean-ctx update` say "cosign release signature
    // verification failed: Disk quota exceeded". The update must still refuse,
    // but name the local problem, not the signature.
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("no-such-dir");
    let err =
        verify_release_signature_in(SUMS, SUMS_SIG, SUMS_PEM, "v3.11.0", "false", Some(&missing))
            .unwrap_err();
    assert!(
        err.contains("could not run the cosign release signature check"),
        "{err}"
    );
    assert!(err.contains("TMPDIR"), "{err}");
    assert!(!err.contains("signature verification failed"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_cosign_that_cannot_start_is_not_reported_as_a_full_disk_or_a_bad_signature() {
    // bd !656 NIT: a spawn error other than NotFound (here: not executable)
    // must not get the disk/TMPDIR hint, and must still refuse.
    let dir = tempfile::tempdir().unwrap();
    let cosign = dir.path().join("cosign");
    std::fs::write(&cosign, b"not a program").unwrap();
    let err = verify_release_signature_with(
        SUMS,
        SUMS_SIG,
        SUMS_PEM,
        "v3.11.0",
        cosign.to_str().unwrap(),
    )
    .unwrap_err();
    assert!(
        err.contains("could not start cosign for the release signature check"),
        "{err}"
    );
    assert!(!err.contains("TMPDIR"), "{err}");
    assert!(!err.contains("signature verification failed"), "{err}");
}

#[test]
fn malformed_inputs_are_errors_not_panics() {
    for cert in [&b""[..], b"not base64 !!", b"LS0tLS1CRUdJTg=="] {
        assert!(verify_blob(SUMS, SUMS_SIG, cert, "v3.11.0").is_err());
    }
    assert!(verify_blob(SUMS, b"%%%", SUMS_PEM, "v3.11.0").is_err());
    for der in [
        &[][..],
        &[0x30],
        &[0x30, 0x85, 1, 2, 3, 4, 5],
        &[0x30, 0x05, 0x00],
    ] {
        assert!(Certificate::parse(der).is_err());
    }
}
