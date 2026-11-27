// SPDX-License-Identifier: Apache-2.0
//! In-process verification of `cosign sign-blob` keyless signatures.
//!
//! 3.11.0 shelled out to a `cosign` binary, so every machine without one —
//! almost all of them — refused every update and `enable-gpu`. This module
//! checks the same release evidence without any external tool:
//!
//! 1. the signing certificate was issued by the embedded Sigstore public-good
//!    Fulcio intermediate (itself checked against the embedded root), was valid
//!    at signing time, and is a code-signing end-entity certificate;
//! 2. it carries a valid SCT from the embedded Sigstore CT log — proof that the
//!    certificate was publicly logged, and the signing time the validity check
//!    uses (Fulcio certificates live for ten minutes);
//! 3. its identity is exactly the release workflow at the release tag, issued
//!    through GitHub Actions OIDC;
//! 4. its key produced the signature over the blob (ECDSA P-256 / SHA-256).
//!
//! Not checked here: the Rekor transparency-log entry (as with cosign's
//! `--insecure-ignore-tlog`). The release assets carry no bundle, so that check
//! needs an online lookup; an installed `cosign` still runs for it.
//!
//! ECDSA verification uses the rustls crypto provider already in the binary.
//! Trust material is copied from `targets/trusted_root.json` of
//! sigstore/root-signing at 79f4403c7faf92b189463c200f1f36b32462bec4
//! (2026-10-08): the current Fulcio CA chain and the `ctfe.sigstore.dev/2022`
//! log key. Both are long-lived (the root expires 2031-10-05); a rotation
//! needs a lean-ctx release.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rustls::pki_types::{AlgorithmIdentifier, SignatureVerificationAlgorithm, alg_id};
use sha2::{Digest, Sha256};

/// Fulcio root `O=sigstore.dev, CN=sigstore` (ECDSA P-384), DER.
const FULCIO_ROOT_B64: &str = "MIIB9zCCAXygAwIBAgIUALZNAPFdxHPwjeDloDwyYChAO/4wCgYIKoZIzj0EAwMwKjEVMBMGA1UEChMMc2lnc3RvcmUuZGV2MREwDwYDVQQDEwhzaWdzdG9yZTAeFw0yMTEwMDcxMzU2NTlaFw0zMTEwMDUxMzU2NThaMCoxFTATBgNVBAoTDHNpZ3N0b3JlLmRldjERMA8GA1UEAxMIc2lnc3RvcmUwdjAQBgcqhkjOPQIBBgUrgQQAIgNiAAT7XeFT4rb3PQGwS4IajtLk3/OlnpgangaBclYpsYBr5i+4ynB07ceb3LP0OIOZdxexX69c5iVuyJRQ+Hz05yi+UF3uBWAlHpiS5sh0+H2GHE7SXrk1EC5m1Tr19L9gg92jYzBhMA4GA1UdDwEB/wQEAwIBBjAPBgNVHRMBAf8EBTADAQH/MB0GA1UdDgQWBBRYwB5fkUWlZql6zJChkyLQKsXF+jAfBgNVHSMEGDAWgBRYwB5fkUWlZql6zJChkyLQKsXF+jAKBggqhkjOPQQDAwNpADBmAjEAj1nHeXZp+13NWBNa+EDsDP8G1WWg1tCMWP/WHPqpaVo0jhsweNFZgSs0eE7wYI4qAjEA2WB9ot98sIkoF3vZYdd3/VtWB5b9TNMea7Ix/stJ5TfcLLeABLE4BNJOsQ4vnBHJ";

/// Fulcio intermediate `O=sigstore.dev, CN=sigstore-intermediate`
/// (ECDSA P-384, code-signing EKU), DER.
const FULCIO_INTERMEDIATE_B64: &str = "MIICGjCCAaGgAwIBAgIUALnViVfnU0brJasmRkHrn/UnfaQwCgYIKoZIzj0EAwMwKjEVMBMGA1UEChMMc2lnc3RvcmUuZGV2MREwDwYDVQQDEwhzaWdzdG9yZTAeFw0yMjA0MTMyMDA2MTVaFw0zMTEwMDUxMzU2NThaMDcxFTATBgNVBAoTDHNpZ3N0b3JlLmRldjEeMBwGA1UEAxMVc2lnc3RvcmUtaW50ZXJtZWRpYXRlMHYwEAYHKoZIzj0CAQYFK4EEACIDYgAE8RVS/ysH+NOvuDZyPIZtilgUF9NlarYpAd9HP1vBBH1U5CV77LSS7s0ZiH4nE7Hv7ptS6LvvR/STk798LVgMzLlJ4HeIfF3tHSaexLcYpSASr1kS0N/RgBJz/9jWCiXno3sweTAOBgNVHQ8BAf8EBAMCAQYwEwYDVR0lBAwwCgYIKwYBBQUHAwMwEgYDVR0TAQH/BAgwBgEB/wIBADAdBgNVHQ4EFgQU39Ppz1YkEZb5qNjpKFWixi4YZD8wHwYDVR0jBBgwFoAUWMAeX5FFpWapesyQoZMi0CrFxfowCgYIKoZIzj0EAwMDZwAwZAIwPCsQK4DYiZYDPIaDi5HFKnfxXx6ASSVmERfsynYBiX2X6SJRnZU84/9DZdnFvvxmAjBOt6QpBlc4J/0DxvkTCqpclvziL6BCCPnjdlIB3Pu3BxsPmygUY7Ii2zbdCdliiow=";

/// `ctfe.sigstore.dev/2022` CT log key (ECDSA P-256), SubjectPublicKeyInfo DER.
const CT_LOG_SPKI_B64: &str = "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEiPSlFi0CmFTfEjCUqF9HuCEcYXNKAaYalIJmBZ8yyezPjTqhxrKBpMnaocVtLJBI1eM3uXnQzQGAJdJ4gs9Fyw==";

const GITHUB_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";

// DER-encoded object identifier values.
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f]; // 2.5.29.15
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11]; // 2.5.29.17
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13]; // 2.5.29.19
const OID_EXTENDED_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25]; // 2.5.29.37
const OID_FULCIO_ISSUER_V1: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x83, 0xbf, 0x30, 0x01, 0x01]; // 1.3.6.1.4.1.57264.1.1
const OID_FULCIO_ISSUER_V2: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x83, 0xbf, 0x30, 0x01, 0x08]; // 1.3.6.1.4.1.57264.1.8
const OID_SCT_LIST: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0xd6, 0x79, 0x02, 0x04, 0x02]; // 1.3.6.1.4.1.11129.2.4.2
const EKU_CODE_SIGNING: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03]; // 1.3.6.1.5.5.7.3.3

/// Extensions a leaf may mark critical; any other critical one is refused.
const UNDERSTOOD_CRITICAL: [&[u8]; 4] = [
    OID_KEY_USAGE,
    OID_SUBJECT_ALT_NAME,
    OID_BASIC_CONSTRAINTS,
    OID_EXTENDED_KEY_USAGE,
];

const TAG_BOOLEAN: u8 = 0x01;
const TAG_INTEGER: u8 = 0x02;
const TAG_BIT_STRING: u8 = 0x03;
const TAG_OCTET_STRING: u8 = 0x04;
const TAG_OID: u8 = 0x06;
const TAG_UTF8_STRING: u8 = 0x0c;
const TAG_UTC_TIME: u8 = 0x17;
const TAG_GENERALIZED_TIME: u8 = 0x18;
const TAG_SEQUENCE: u8 = 0x30;
const TAG_VERSION: u8 = 0xa0; // [0] EXPLICIT in TBSCertificate
const TAG_EXTENSIONS: u8 = 0xa3; // [3] EXPLICIT in TBSCertificate
const TAG_SAN_URI: u8 = 0x86; // [6] IMPLICIT IA5String in GeneralName

/// The certificate identity the release workflow signs with for `release_tag`.
fn release_identity(release_tag: &str) -> String {
    format!(
        "https://github.com/yvgude/lean-ctx/.github/workflows/release.yml@refs/tags/{release_tag}"
    )
}

/// Verify a release file's keyless cosign signature. The check runs in-process
/// (no tool to install); an installed `cosign` additionally confirms the Rekor
/// transparency-log entry, which needs an online lookup.
pub(super) fn verify_release_signature(
    blob: &[u8],
    signature_file: &[u8],
    certificate_file: &[u8],
    release_tag: &str,
) -> Result<(), String> {
    verify_release_signature_with(
        blob,
        signature_file,
        certificate_file,
        release_tag,
        "cosign",
    )
}

fn verify_release_signature_with(
    blob: &[u8],
    signature_file: &[u8],
    certificate_file: &[u8],
    release_tag: &str,
    cosign_program: &str,
) -> Result<(), String> {
    verify_release_signature_in(
        blob,
        signature_file,
        certificate_file,
        release_tag,
        cosign_program,
        None,
    )
}

/// `temp_parent` = where the cosign check stages its files; `None` is the
/// system temp dir (`TMPDIR`). Only tests pass a different one.
pub(super) fn verify_release_signature_in(
    blob: &[u8],
    signature_file: &[u8],
    certificate_file: &[u8],
    release_tag: &str,
    cosign_program: &str,
    temp_parent: Option<&std::path::Path>,
) -> Result<(), String> {
    verify_blob(blob, signature_file, certificate_file, release_tag)
        .map_err(|e| format!("release signature verification failed: {e}"))?;
    let files = [blob, signature_file, certificate_file];
    match verify_with_installed_cosign(cosign_program, files, release_tag, temp_parent) {
        Ok(()) | Err(CosignCheck::NotInstalled) => Ok(()),
        Err(CosignCheck::Failed(detail)) => Err(format!(
            "cosign release signature verification failed: {detail}"
        )),
        Err(CosignCheck::NotStarted(detail)) => Err(format!(
            "could not start cosign for the release signature check: {detail}"
        )),
        Err(CosignCheck::LocalIo(detail)) => Err(format!(
            "could not run the cosign release signature check: {detail}. \
             Free disk space or set TMPDIR to a directory with room, then retry"
        )),
    }
}

enum CosignCheck {
    NotInstalled,
    /// cosign ran and rejected the signature.
    Failed(String),
    /// The check could not run here (temp files, disk quota, spawning cosign);
    /// says nothing about the signature.
    LocalIo(String),
    /// cosign is on the PATH but could not be started (e.g. permission
    /// denied); says nothing about the signature.
    NotStarted(String),
}

fn cosign_identity_regexp(release_tag: &str) -> String {
    format!("^{}$", regex::escape(&release_identity(release_tag)))
}

/// `files` = blob, signature, certificate.
fn verify_with_installed_cosign(
    program: &str,
    files: [&[u8]; 3],
    release_tag: &str,
    temp_parent: Option<&std::path::Path>,
) -> Result<(), CosignCheck> {
    let local_io = |e: std::io::Error| CosignCheck::LocalIo(e.to_string());
    let directory = match temp_parent {
        Some(parent) => tempfile::tempdir_in(parent),
        None => tempfile::tempdir(),
    }
    .map_err(local_io)?;
    let blob_path = directory.path().join("blob");
    let signature_path = directory.path().join("blob.sig");
    let certificate_path = directory.path().join("blob.pem");
    for (path, bytes) in [&blob_path, &signature_path, &certificate_path]
        .into_iter()
        .zip(files)
    {
        std::fs::write(path, bytes).map_err(local_io)?;
    }
    let output = std::process::Command::new(program)
        .arg("verify-blob")
        .arg("--signature")
        .arg(&signature_path)
        .arg("--certificate")
        .arg(&certificate_path)
        .arg("--certificate-identity-regexp")
        .arg(cosign_identity_regexp(release_tag))
        .arg("--certificate-oidc-issuer")
        .arg(GITHUB_OIDC_ISSUER)
        .arg(&blob_path)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CosignCheck::NotInstalled
            } else {
                CosignCheck::NotStarted(e.to_string())
            }
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CosignCheck::Failed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

/// Verify a `cosign sign-blob --output-signature/--output-certificate` pair
/// over `blob` for the release workflow at `release_tag`.
fn verify_blob(
    blob: &[u8],
    signature_file: &[u8],
    certificate_file: &[u8],
    release_tag: &str,
) -> Result<(), String> {
    let leaf_der = decode_certificate(certificate_file)?;
    let signature = decode_base64("signature", signature_file)?;
    let root_der = B64.decode(FULCIO_ROOT_B64).map_err(|e| e.to_string())?;
    let intermediate_der = B64
        .decode(FULCIO_INTERMEDIATE_B64)
        .map_err(|e| e.to_string())?;
    let root = Certificate::parse(&root_der)?;
    let intermediate = Certificate::parse(&intermediate_der)?;
    let leaf = Certificate::parse(&leaf_der)?;

    let signed_at = verify_sct(&leaf, intermediate.spki)? / 1000;
    let signed_at =
        i64::try_from(signed_at).map_err(|_| "SCT timestamp out of range".to_string())?;
    verify_issued_by(&intermediate, &root, signed_at)
        .map_err(|e| format!("embedded Fulcio intermediate: {e}"))?;
    verify_issued_by(&leaf, &intermediate, signed_at)
        .map_err(|e| format!("signing certificate is not from Sigstore Fulcio: {e}"))?;
    verify_code_signing_leaf(&leaf)?;
    verify_identity(&leaf, release_tag)?;

    algorithm(alg_id::ECDSA_P256, alg_id::ECDSA_SHA256)?
        .verify_signature(leaf.public_key, blob, &signature)
        .map_err(|_| {
            if leaf.spki_algorithm == alg_id::ECDSA_P256.as_ref() {
                "signature does not match the signed file".to_string()
            } else {
                "signing key is not ECDSA P-256".to_string()
            }
        })
}

/// The ECDSA verifier for a key/signature algorithm pair.
fn algorithm(
    public_key: AlgorithmIdentifier,
    signature: AlgorithmIdentifier,
) -> Result<&'static dyn SignatureVerificationAlgorithm, String> {
    rustls::crypto::aws_lc_rs::default_provider()
        .signature_verification_algorithms
        .all
        .iter()
        .copied()
        .find(|alg| alg.public_key_alg_id() == public_key && alg.signature_alg_id() == signature)
        .ok_or_else(|| "crypto provider lacks the required ECDSA verifier".to_string())
}

/// `child` is signed by `issuer`'s P-384 key, names it as issuer, and both are
/// valid at `at` (Unix seconds).
fn verify_issued_by(
    child: &Certificate<'_>,
    issuer: &Certificate<'_>,
    at: i64,
) -> Result<(), String> {
    if child.issuer != issuer.subject {
        return Err("issuer name does not match".to_string());
    }
    if issuer.spki_algorithm != alg_id::ECDSA_P384.as_ref()
        || child.signature_algorithm != alg_id::ECDSA_SHA384.as_ref()
    {
        return Err("unexpected certificate signature algorithm".to_string());
    }
    algorithm(alg_id::ECDSA_P384, alg_id::ECDSA_SHA384)?
        .verify_signature(issuer.public_key, child.tbs, child.signature)
        .map_err(|_| "certificate signature is invalid".to_string())?;
    for cert in [child, issuer] {
        if at < cert.not_before || at > cert.not_after {
            return Err("certificate was not valid at signing time".to_string());
        }
    }
    Ok(())
}

/// End-entity certificate for code signing, with no critical extension this
/// module does not understand.
fn verify_code_signing_leaf(leaf: &Certificate<'_>) -> Result<(), String> {
    if let Some(ext) = leaf
        .extensions
        .iter()
        .find(|e| e.critical && !UNDERSTOOD_CRITICAL.contains(&e.oid))
    {
        return Err(format!(
            "signing certificate has an unsupported critical extension {:02x?}",
            ext.oid
        ));
    }
    if let Some(ext) = leaf.extension(OID_BASIC_CONSTRAINTS) {
        let (constraints, _) = expect(ext.value, TAG_SEQUENCE)?;
        if let Ok((ca, _)) = expect(constraints.content, TAG_BOOLEAN)
            && ca.content != [0]
        {
            return Err("signing certificate is a CA certificate".to_string());
        }
    }
    if let Some(ext) = leaf.extension(OID_KEY_USAGE) {
        let (bits, _) = expect(ext.value, TAG_BIT_STRING)?;
        if bits.content.get(1).is_none_or(|b| b & 0x80 == 0) {
            return Err("signing certificate does not allow digital signatures".to_string());
        }
    }
    let eku = leaf
        .extension(OID_EXTENDED_KEY_USAGE)
        .ok_or_else(|| "signing certificate has no extended key usage".to_string())?;
    let (purposes, _) = expect(eku.value, TAG_SEQUENCE)?;
    let mut rest = purposes.content;
    while !rest.is_empty() {
        let (purpose, after) = expect(rest, TAG_OID)?;
        if purpose.content == EKU_CODE_SIGNING {
            return Ok(());
        }
        rest = after;
    }
    Err("signing certificate is not valid for code signing".to_string())
}

/// `cosign --output-certificate` writes the PEM base64-encoded once more;
/// accept that and plain PEM.
fn decode_certificate(file: &[u8]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(file)
        .map_err(|_| "certificate is not text".to_string())?
        .trim();
    let pem = if text.starts_with("-----BEGIN") {
        text.to_string()
    } else {
        String::from_utf8(decode_base64("certificate", text.as_bytes())?)
            .map_err(|_| "certificate is not PEM".to_string())?
    };
    let body = pem
        .trim()
        .strip_prefix("-----BEGIN CERTIFICATE-----")
        .and_then(|rest| rest.strip_suffix("-----END CERTIFICATE-----"))
        .filter(|body| !body.contains("-----"))
        .ok_or_else(|| "expected exactly one PEM certificate".to_string())?;
    decode_base64("certificate", body.as_bytes())
}

fn decode_base64(what: &str, text: &[u8]) -> Result<Vec<u8>, String> {
    let compact: Vec<u8> = text
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    B64.decode(compact)
        .map_err(|e| format!("{what} is not valid base64: {e}"))
}

/// The parts of an X.509 certificate this module inspects.
struct Certificate<'a> {
    /// TBSCertificate, full TLV (the signed bytes).
    tbs: &'a [u8],
    /// TBSCertificate content before the extensions field.
    tbs_head: &'a [u8],
    issuer: &'a [u8],
    subject: &'a [u8],
    not_before: i64,
    not_after: i64,
    /// SubjectPublicKeyInfo, full TLV.
    spki: &'a [u8],
    /// Content of the key's AlgorithmIdentifier.
    spki_algorithm: &'a [u8],
    /// Raw EC point.
    public_key: &'a [u8],
    /// Content of the certificate's signature AlgorithmIdentifier.
    signature_algorithm: &'a [u8],
    signature: &'a [u8],
    extensions: Vec<Extension<'a>>,
}

struct Extension<'a> {
    oid: &'a [u8],
    critical: bool,
    /// extnValue content (the bytes inside the OCTET STRING).
    value: &'a [u8],
    /// The whole Extension TLV.
    raw: &'a [u8],
}

impl<'a> Certificate<'a> {
    fn parse(der: &'a [u8]) -> Result<Self, String> {
        let (cert, trailing) = expect(der, TAG_SEQUENCE)?;
        if !trailing.is_empty() {
            return Err("trailing data after certificate".to_string());
        }
        let (tbs, rest) = expect(cert.content, TAG_SEQUENCE)?;
        let (signature_algorithm, rest) = expect(rest, TAG_SEQUENCE)?;
        let (signature, rest) = expect(rest, TAG_BIT_STRING)?;
        if !rest.is_empty() {
            return Err("trailing data in certificate".to_string());
        }

        let mut rest = tbs.content;
        if let Ok((_, after)) = expect(rest, TAG_VERSION) {
            rest = after;
        }
        let rest = expect(rest, TAG_INTEGER)?.1; // serialNumber
        let rest = expect(rest, TAG_SEQUENCE)?.1; // signature
        let (issuer, rest) = expect(rest, TAG_SEQUENCE)?;
        let (validity, rest) = expect(rest, TAG_SEQUENCE)?;
        let (subject, rest) = expect(rest, TAG_SEQUENCE)?;
        let (spki, mut rest) = expect(rest, TAG_SEQUENCE)?;
        let (not_before, after) = read_tlv(validity.content)?;
        let (not_after, _) = read_tlv(after)?;
        let (spki_algorithm, after) = expect(spki.content, TAG_SEQUENCE)?;
        let (key_bits, _) = expect(after, TAG_BIT_STRING)?;

        let mut extensions = Vec::new();
        let mut tbs_head = tbs.content;
        while !rest.is_empty() {
            let (field, after) = read_tlv(rest)?;
            if field.tag == TAG_EXTENSIONS {
                if !after.is_empty() {
                    return Err("data after certificate extensions".to_string());
                }
                tbs_head = &tbs.content[..tbs.content.len() - rest.len()];
                extensions = parse_extensions(field.content)?;
            }
            rest = after;
        }
        Ok(Self {
            tbs: tbs.raw,
            tbs_head,
            issuer: issuer.raw,
            subject: subject.raw,
            not_before: parse_time(&not_before)?,
            not_after: parse_time(&not_after)?,
            spki: spki.raw,
            spki_algorithm: spki_algorithm.content,
            public_key: bit_string_bytes(&key_bits)?,
            signature_algorithm: signature_algorithm.content,
            signature: bit_string_bytes(&signature)?,
            extensions,
        })
    }

    fn extension(&self, oid: &[u8]) -> Option<&Extension<'a>> {
        self.extensions.iter().find(|e| e.oid == oid)
    }

    /// TBSCertificate re-encoded without the extension `oid` — the precert
    /// TBS a CT log signs (RFC 6962 §3.2) when `oid` is the SCT list.
    fn tbs_without(&self, oid: &[u8]) -> Vec<u8> {
        let kept: Vec<u8> = self
            .extensions
            .iter()
            .filter(|e| e.oid != oid)
            .flat_map(|e| e.raw.iter().copied())
            .collect();
        let mut body = self.tbs_head.to_vec();
        body.extend(der_wrap(TAG_EXTENSIONS, &der_wrap(TAG_SEQUENCE, &kept)));
        der_wrap(TAG_SEQUENCE, &body)
    }
}

fn parse_extensions(explicit: &[u8]) -> Result<Vec<Extension<'_>>, String> {
    let (list, trailing) = expect(explicit, TAG_SEQUENCE)?;
    if !trailing.is_empty() {
        return Err("trailing data after extension list".to_string());
    }
    let mut rest = list.content;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let (ext, after) = expect(rest, TAG_SEQUENCE)?;
        let (oid, mut inner) = expect(ext.content, TAG_OID)?;
        let mut critical = false;
        if let Ok((flag, after_flag)) = expect(inner, TAG_BOOLEAN) {
            critical = flag.content != [0];
            inner = after_flag;
        }
        let (value, trailing) = expect(inner, TAG_OCTET_STRING)?;
        if !trailing.is_empty() {
            return Err("trailing data in extension".to_string());
        }
        out.push(Extension {
            oid: oid.content,
            critical,
            value: value.content,
            raw: ext.raw,
        });
        rest = after;
    }
    Ok(out)
}

/// UTCTime `YYMMDDHHMMSSZ` or GeneralizedTime `YYYYMMDDHHMMSSZ` → Unix seconds.
fn parse_time(time: &Tlv<'_>) -> Result<i64, String> {
    let invalid = || "invalid certificate time".to_string();
    let digits = match (time.tag, time.content.len()) {
        (TAG_UTC_TIME, 13) | (TAG_GENERALIZED_TIME, 15) => &time.content[..time.content.len() - 1],
        _ => return Err(invalid()),
    };
    if time.content.last() != Some(&b'Z') || !digits.iter().all(u8::is_ascii_digit) {
        return Err(invalid());
    }
    let num = |range: std::ops::Range<usize>| {
        digits[range]
            .iter()
            .fold(0u32, |acc, d| acc * 10 + u32::from(d - b'0'))
    };
    let (year, rest) = if time.tag == TAG_UTC_TIME {
        let yy = num(0..2) as i32;
        (if yy < 50 { 2000 + yy } else { 1900 + yy }, 2)
    } else {
        (num(0..4) as i32, 4)
    };
    lean_ctx_time::UtcDateTime::with_ymd_and_hms(
        year,
        num(rest..rest + 2),
        num(rest + 2..rest + 4),
        num(rest + 4..rest + 6),
        num(rest + 6..rest + 8),
        num(rest + 8..rest + 10),
    )
    .map(lean_ctx_time::UtcDateTime::timestamp)
    .ok_or_else(invalid)
}

fn bit_string_bytes<'a>(bits: &Tlv<'a>) -> Result<&'a [u8], String> {
    match bits.content.split_first() {
        Some((0, bytes)) => Ok(bytes),
        _ => Err("malformed BIT STRING".to_string()),
    }
}

fn verify_identity(leaf: &Certificate<'_>, release_tag: &str) -> Result<(), String> {
    let expected = release_identity(release_tag);
    let san = leaf
        .extension(OID_SUBJECT_ALT_NAME)
        .ok_or_else(|| "signing certificate has no identity (SAN)".to_string())?;
    let (names, _) = expect(san.value, TAG_SEQUENCE)?;
    let mut rest = names.content;
    let mut found = Vec::new();
    while !rest.is_empty() {
        let (name, after) = read_tlv(rest)?;
        if name.tag == TAG_SAN_URI {
            found.push(String::from_utf8_lossy(name.content).into_owned());
        }
        rest = after;
    }
    if !found.contains(&expected) {
        return Err(format!(
            "signed by {found:?}, expected the release workflow identity {expected}"
        ));
    }

    let mut issuers = Vec::new();
    if let Some(v1) = leaf.extension(OID_FULCIO_ISSUER_V1) {
        issuers.push(v1.value);
    }
    if let Some(v2) = leaf.extension(OID_FULCIO_ISSUER_V2) {
        issuers.push(expect(v2.value, TAG_UTF8_STRING)?.0.content);
    }
    if issuers.is_empty() {
        return Err("signing certificate names no OIDC issuer".to_string());
    }
    if let Some(other) = issuers
        .iter()
        .find(|issuer| **issuer != GITHUB_OIDC_ISSUER.as_bytes())
    {
        return Err(format!(
            "signing certificate was issued for {}, expected {GITHUB_OIDC_ISSUER}",
            String::from_utf8_lossy(other)
        ));
    }
    Ok(())
}

/// Verify the embedded SCT from the trusted CT log; returns its timestamp (ms).
fn verify_sct(leaf: &Certificate<'_>, issuer_spki: &[u8]) -> Result<u64, String> {
    let ext = leaf
        .extension(OID_SCT_LIST)
        .ok_or_else(|| "signing certificate carries no CT log proof (SCT)".to_string())?;
    let (list, _) = expect(ext.value, TAG_OCTET_STRING)?;
    let mut data = list.content;
    let mut scts = read_vec16(&mut data)?;
    if !data.is_empty() {
        return Err("trailing data after SCT list".to_string());
    }

    let log_spki = B64.decode(CT_LOG_SPKI_B64).map_err(|e| e.to_string())?;
    let log_id = Sha256::digest(&log_spki);
    let (log_spki_seq, _) = expect(&log_spki, TAG_SEQUENCE)?;
    let (_, log_key_bits) = expect(log_spki_seq.content, TAG_SEQUENCE)?;
    let log_key = bit_string_bytes(&expect(log_key_bits, TAG_BIT_STRING)?.0)?;
    let verifier = algorithm(alg_id::ECDSA_P256, alg_id::ECDSA_SHA256)?;
    let precert_tbs = leaf.tbs_without(OID_SCT_LIST);
    let tbs_len = u32::try_from(precert_tbs.len())
        .ok()
        .filter(|len| *len < 1 << 24)
        .ok_or_else(|| "certificate too large for an SCT".to_string())?;
    let issuer_key_hash = Sha256::digest(issuer_spki);

    while !scts.is_empty() {
        let mut sct = read_vec16(&mut scts)?;
        let version = read_bytes(&mut sct, 1)?[0];
        let id = read_bytes(&mut sct, 32)?;
        let timestamp = u64::from_be_bytes(
            read_bytes(&mut sct, 8)?
                .try_into()
                .map_err(|_| "truncated SCT".to_string())?,
        );
        let extensions = read_vec16(&mut sct)?;
        let algorithm_ids = read_bytes(&mut sct, 2)?;
        let signature = read_vec16(&mut sct)?;
        if version != 0 || id != log_id.as_slice() {
            continue; // another log's SCT: not trusted, not needed
        }
        // hash = sha256 (4), signature = ecdsa (3)
        if algorithm_ids != [4, 3] || !sct.is_empty() {
            return Err("SCT from the Sigstore CT log has an unexpected format".to_string());
        }
        let mut signed = Vec::with_capacity(precert_tbs.len() + 64);
        signed.extend([0u8, 0u8]); // v1, certificate_timestamp
        signed.extend(timestamp.to_be_bytes());
        signed.extend([0u8, 1u8]); // precert_entry
        signed.extend(issuer_key_hash);
        signed.extend(&tbs_len.to_be_bytes()[1..]);
        signed.extend(&precert_tbs);
        signed.extend(
            u16::try_from(extensions.len())
                .map_err(|_| "SCT extensions too large".to_string())?
                .to_be_bytes(),
        );
        signed.extend(extensions);
        return verifier
            .verify_signature(log_key, &signed, signature)
            .map(|()| timestamp)
            .map_err(|_| "SCT signature from the Sigstore CT log is invalid".to_string());
    }
    Err("signing certificate has no SCT from the Sigstore CT log".to_string())
}

fn read_bytes<'a>(data: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
    if data.len() < n {
        return Err("truncated SCT data".to_string());
    }
    let (head, tail) = data.split_at(n);
    *data = tail;
    Ok(head)
}

/// TLS `opaque<0..2^16-1>`.
fn read_vec16<'a>(data: &mut &'a [u8]) -> Result<&'a [u8], String> {
    let len = read_bytes(data, 2)?;
    read_bytes(data, usize::from(u16::from_be_bytes([len[0], len[1]])))
}

struct Tlv<'a> {
    tag: u8,
    content: &'a [u8],
    raw: &'a [u8],
}

/// One DER TLV (low tag numbers, definite lengths up to 4 bytes).
fn read_tlv(input: &[u8]) -> Result<(Tlv<'_>, &[u8]), String> {
    let (&tag, rest) = input
        .split_first()
        .ok_or_else(|| "truncated DER".to_string())?;
    if tag & 0x1f == 0x1f {
        return Err("unsupported DER tag".to_string());
    }
    let (&first, rest) = rest
        .split_first()
        .ok_or_else(|| "truncated DER".to_string())?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return Err("invalid DER length".to_string());
        }
        let len = rest[..n]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        (len, &rest[n..])
    };
    if rest.len() < len {
        return Err("truncated DER".to_string());
    }
    let header = input.len() - rest.len();
    Ok((
        Tlv {
            tag,
            content: &rest[..len],
            raw: &input[..header + len],
        },
        &rest[len..],
    ))
}

fn expect(input: &[u8], tag: u8) -> Result<(Tlv<'_>, &[u8]), String> {
    let (tlv, rest) = read_tlv(input)?;
    if tlv.tag != tag {
        return Err(format!(
            "unexpected DER tag {:#04x} (expected {tag:#04x})",
            tlv.tag
        ));
    }
    Ok((tlv, rest))
}

fn der_wrap(tag: u8, content: &[u8]) -> Vec<u8> {
    let len = content.len();
    let mut out = vec![tag];
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend(&bytes[skip..]);
    }
    out.extend(content);
    out
}

#[cfg(test)]
#[path = "sigstore_tests.rs"]
mod tests;
