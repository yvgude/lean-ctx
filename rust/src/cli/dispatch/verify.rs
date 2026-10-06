#[allow(dead_code, unused_imports)]
use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use zip::ZipArchive;

/// Arguments for offline evidence-bundle verification.
#[derive(Debug, Clone, Parser)]
#[command(name = "lean-ctx verify", about = "Verify an evidence bundle offline")]
pub struct VerifyCommand {
    /// Path to the evidence-bundle ZIP archive.
    #[arg(value_name = "BUNDLE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub bundle_path: PathBuf,

    /// Optional raw or hexadecimal Ed25519 public-key file for cross-verification.
    #[arg(
        long = "public-key",
        alias = "pubkey",
        value_name = "PATH",
        value_hint = clap::ValueHint::FilePath
    )]
    pub public_key: Option<PathBuf>,

    /// Include per-file verification details in human-readable output.
    #[arg(short, long)]
    pub verbose: bool,

    /// Emit the verification result as JSON.
    #[arg(long = "json-output", alias = "json")]
    pub json_output: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyResult {
    pub bundle_valid: bool,
    pub manifest_intact: bool,
    pub signature_valid: bool,
    pub files_verified: Vec<FileVerification>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileVerification {
    pub path: String,
    pub expected_hash: String,
    pub actual_hash: String,
    pub valid: bool,
}

/// Verify an evidence bundle without contacting LeanCTX or any network service.
pub fn execute_verify(cmd: &VerifyCommand) -> Result<VerifyResult> {
    let mut result = VerifyResult {
        bundle_valid: false,
        manifest_intact: false,
        signature_valid: false,
        files_verified: Vec::new(),
        errors: Vec::new(),
        warnings: Vec::new(),
    };

    let entries = match read_zip_entries(&cmd.bundle_path) {
        Ok(entries) => entries,
        Err(error) => {
            result
                .errors
                .push(format!("cannot open evidence bundle: {error:#}"));
            return Ok(result);
        }
    };

    let Some(manifest_bytes) = entries.get("manifest.json") else {
        result.errors.push("missing manifest.json".to_string());
        return Ok(result);
    };

    let manifest: Value = match serde_json::from_slice(manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            result
                .errors
                .push(format!("manifest.json is not valid JSON: {error}"));
            return Ok(result);
        }
    };

    validate_manifest_shape(&manifest, &mut result.errors);
    let listed_paths = manifest_file_paths(&manifest);
    result.manifest_intact = verify_manifest_files(
        &manifest,
        &entries,
        &mut result.files_verified,
        &mut result.errors,
    );

    for path in entries.keys() {
        if path != "manifest.json" && !listed_paths.contains(path) {
            result
                .warnings
                .push(format!("unlisted file in bundle: {path}"));
        }
    }

    result.signature_valid = verify_manifest_signature(
        &manifest,
        cmd.public_key.as_deref(),
        &mut result.errors,
        &mut result.warnings,
    );
    result.bundle_valid =
        result.manifest_intact && result.signature_valid && result.errors.is_empty();

    Ok(result)
}

fn read_zip_entries(path: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut archive = ZipArchive::new(file).context("read ZIP archive")?;
    let mut entries = BTreeMap::new();

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("read ZIP entry {index}"))?;
        if entry.is_dir() {
            continue;
        }

        let name = entry.name().to_string();
        if entries.contains_key(&name) {
            anyhow::bail!("duplicate ZIP entry: {name}");
        }

        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("read ZIP entry {name}"))?;
        entries.insert(name, bytes);
    }

    Ok(entries)
}

fn validate_manifest_shape(manifest: &Value, errors: &mut Vec<String>) -> bool {
    let mut valid = true;
    if manifest.get("bundle").and_then(Value::as_str) != Some("evidence-bundle") {
        errors.push("manifest bundle must be \"evidence-bundle\"".to_string());
        valid = false;
    }
    if manifest.get("version").and_then(Value::as_u64) != Some(1) {
        errors.push("unsupported or missing evidence-bundle manifest version".to_string());
        valid = false;
    }
    if !manifest.get("files").is_some_and(Value::is_array) {
        errors.push("manifest files must be an array".to_string());
        valid = false;
    }
    valid
}

fn manifest_file_paths(manifest: &Value) -> BTreeSet<String> {
    manifest
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| file.get("path").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn verify_manifest_files(
    manifest: &Value,
    entries: &BTreeMap<String, Vec<u8>>,
    files_verified: &mut Vec<FileVerification>,
    errors: &mut Vec<String>,
) -> bool {
    let Some(files) = manifest.get("files").and_then(Value::as_array) else {
        return false;
    };

    let mut valid = true;
    let mut seen = BTreeSet::new();
    for file in files {
        let Some(path) = file.get("path").and_then(Value::as_str) else {
            errors.push("manifest file entry is missing a path".to_string());
            valid = false;
            continue;
        };
        let expected_hash = file
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if !seen.insert(path.to_string()) {
            errors.push(format!("duplicate manifest file entry: {path}"));
            valid = false;
        }
        if !safe_bundle_path(path) {
            errors.push(format!("unsafe manifest file path: {path}"));
            files_verified.push(FileVerification {
                path: path.to_string(),
                expected_hash,
                actual_hash: String::new(),
                valid: false,
            });
            valid = false;
            continue;
        }

        let Some(bytes) = entries.get(path) else {
            errors.push(format!("missing file listed in manifest: {path}"));
            files_verified.push(FileVerification {
                path: path.to_string(),
                expected_hash,
                actual_hash: String::new(),
                valid: false,
            });
            valid = false;
            continue;
        };

        let actual_hash = sha256_hex(bytes);
        let hash_format_valid = expected_hash.len() == 64
            && crate::core::agent_identity::hex_decode(&expected_hash).is_ok();
        let file_valid = hash_format_valid && expected_hash.eq_ignore_ascii_case(&actual_hash);
        if !hash_format_valid {
            errors.push(format!("invalid SHA-256 hash for manifest file: {path}"));
        } else if !file_valid {
            errors.push(format!(
                "hash mismatch for {path}: expected {expected_hash}, got {actual_hash}"
            ));
        }
        if !file_valid {
            valid = false;
        }
        files_verified.push(FileVerification {
            path: path.to_string(),
            expected_hash,
            actual_hash,
            valid: file_valid,
        });
    }

    valid
}

fn safe_bundle_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn verify_manifest_signature(
    manifest: &Value,
    external_public_key: Option<&Path>,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> bool {
    let Some(signing) = manifest.get("signing").and_then(Value::as_object) else {
        errors.push("manifest signing section is missing".to_string());
        return false;
    };
    if signing.get("algorithm").and_then(Value::as_str) != Some("ed25519") {
        errors.push("manifest signing algorithm is not ed25519".to_string());
    }

    let embedded_key = if let Some(value) = signing.get("public_key").and_then(Value::as_str) {
        match crate::core::agent_identity::hex_decode(value) {
            Ok(bytes) if bytes.len() == 32 => Some(bytes),
            Ok(_) => {
                if external_public_key.is_none() {
                    errors.push("manifest Ed25519 public key must contain 32 bytes".to_string());
                } else {
                    warnings.push("embedded manifest public key is invalid".to_string());
                }
                None
            }
            Err(error) => {
                if external_public_key.is_none() {
                    errors.push(format!("invalid manifest public key: {error}"));
                } else {
                    warnings.push(format!("embedded manifest public key is invalid: {error}"));
                }
                None
            }
        }
    } else {
        if external_public_key.is_none() {
            errors.push("manifest public key is missing".to_string());
        } else {
            warnings.push("manifest has no embedded public key".to_string());
        }
        None
    };

    let public_key = match external_public_key {
        Some(path) => match read_public_key(path) {
            Ok(bytes) => {
                if let Some(embedded) = &embedded_key {
                    if embedded != &bytes {
                        warnings.push(
                            "external public key differs from the embedded public key".to_string(),
                        );
                    }
                }
                warnings.push("signature checked with external public key".to_string());
                bytes
            }
            Err(error) => {
                errors.push(format!("cannot read external public key: {error:#}"));
                return false;
            }
        },
        None => match embedded_key {
            Some(bytes) => bytes,
            None => return false,
        },
    };

    let Some(expected_digest) = signing.get("signed_digest").and_then(Value::as_str) else {
        errors.push("manifest signed_digest is missing".to_string());
        return false;
    };
    let Some(signature_hex) = signing.get("signature").and_then(Value::as_str) else {
        errors.push("manifest signature is missing".to_string());
        return false;
    };

    let mut unsigned_manifest = manifest.clone();
    let Some(unsigned_signing) = unsigned_manifest
        .get_mut("signing")
        .and_then(Value::as_object_mut)
    else {
        errors.push("manifest signing section is malformed".to_string());
        return false;
    };
    // The generator initializes both fields to empty before computing the digest.
    unsigned_signing.insert("signed_digest".to_string(), Value::String(String::new()));
    unsigned_signing.insert("signature".to_string(), Value::String(String::new()));

    let canonical_manifest = match canonical_json(&unsigned_manifest) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!("cannot canonicalize manifest: {error:#}"));
            return false;
        }
    };
    let computed_digest = sha256_hex(&canonical_manifest);
    let digest_valid = expected_digest.eq_ignore_ascii_case(&computed_digest);
    if !digest_valid {
        errors.push(format!(
            "manifest digest mismatch: expected {expected_digest}, got {computed_digest}"
        ));
    }

    let signature = match decode_signature(signature_hex) {
        Ok(sig) => sig,
        Err(error) => {
            errors.push(format!("invalid manifest signature: {error}"));
            return false;
        }
    };
    // Primary path: use the canonical signer/verifier from canonical.rs
    // which strips signature fields the same way sign_manifest does.
    let cryptographically_valid =
        crate::core::canonical::verify_manifest_signature(manifest, &public_key).unwrap_or(false)
            || {
                // Fallback: try with canonical_serialize on the empty-field unsigned manifest.
                let canon_v2 = crate::core::canonical::canonical_serialize(&unsigned_manifest);
                crate::core::agent_identity::verify_signature(&public_key, &canon_v2, &signature)
            }
            || crate::core::agent_identity::verify_signature(
                &public_key,
                &canonical_manifest,
                &signature,
            )
            || crate::core::agent_identity::verify_signature(
                &public_key,
                computed_digest.as_bytes(),
                &signature,
            );
    if !cryptographically_valid {
        errors.push("manifest signature verification failed".to_string());
    }

    digest_valid && cryptographically_valid
}

fn decode_signature(encoded: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    if let Ok(bytes) = crate::core::agent_identity::hex_decode(encoded) {
        if bytes.len() == 64 {
            return Ok(bytes);
        }
    }
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
        if bytes.len() == 64 {
            return Ok(bytes);
        }
    }
    Err(format!(
        "signature must be 64 bytes (hex or base64), got {} chars",
        encoded.len()
    ))
}

fn read_public_key(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() == 32 {
        return Ok(bytes);
    }
    let text = std::str::from_utf8(&bytes)
        .context("public key is neither 32 raw bytes nor UTF-8 hexadecimal")?;
    let decoded = crate::core::agent_identity::hex_decode(text.trim())
        .map_err(|error| anyhow::anyhow!("decode hexadecimal public key: {error}"))?;
    if decoded.len() != 32 {
        anyhow::bail!("public key must contain 32 bytes");
    }
    Ok(decoded)
}

fn canonical_json(value: &Value) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    write_canonical_json(value, &mut output)?;
    Ok(output)
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Object(map) => {
            output.push(b'{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key)?;
                output.push(b':');
                write_canonical_json(map.get(*key).expect("key came from object"), output)?;
            }
            output.push(b'}');
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        _ => serde_json::to_writer(&mut *output, value)?,
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

/// Return whether `verify` arguments contain a bundle path rather than the
/// legacy package-stat flags handled by `cli::verify_cmd`.
pub(crate) fn looks_like_bundle_command(args: &[String]) -> bool {
    let mut consumes_next = false;
    for arg in args {
        if consumes_next {
            consumes_next = false;
            continue;
        }
        match arg.as_str() {
            "--public-key" | "--pubkey" | "--format" => {
                consumes_next = true;
            }
            "--" => return true,
            "--json" | "--json-output" | "--verbose" | "-v" => {}
            _ if arg.starts_with("--public-key=")
                || arg.starts_with("--pubkey=")
                || arg.starts_with("--format=") => {}
            _ if arg.starts_with('-') => {}
            _ => return true,
        }
    }
    false
}

/// Parse, execute, print, and return the process status for the bundle mode.
pub(crate) fn run(args: &[String]) -> i32 {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push("lean-ctx".to_string());
    argv.extend(args.iter().cloned());

    let command = match VerifyCommand::try_parse_from(argv) {
        Ok(command) => command,
        Err(error) => {
            let code = error.exit_code();
            eprint!("{error}");
            return code;
        }
    };

    match execute_verify(&command) {
        Ok(result) => {
            let valid = result.bundle_valid;
            if command.json_output {
                match serde_json::to_string_pretty(&result) {
                    Ok(json) => println!("{json}"),
                    Err(error) => {
                        eprintln!("verify: cannot serialize result: {error}");
                        return 1;
                    }
                }
            } else {
                print_human_result(&result, command.verbose);
            }
            i32::from(!valid)
        }
        Err(error) => {
            eprintln!("verify: {error:#}");
            1
        }
    }
}

fn print_human_result(result: &VerifyResult, verbose: bool) {
    println!(
        "Evidence bundle: {}",
        if result.bundle_valid {
            "VALID"
        } else {
            "INVALID"
        }
    );
    println!("Manifest intact: {}", result.manifest_intact);
    println!("Signature valid: {}", result.signature_valid);
    println!("Files verified: {}", result.files_verified.len());
    if verbose {
        for file in &result.files_verified {
            println!(
                "  {}: {} (expected {}, actual {})",
                file.path, file.valid, file.expected_hash, file.actual_hash
            );
        }
    }
    for error in &result.errors {
        eprintln!("ERROR: {error}");
    }
    for warning in &result.warnings {
        eprintln!("WARNING: {warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};

    #[allow(clippy::fn_params_excessive_bools)]
    fn fixture(
        name: &str,
        tamper_file: bool,
        tamper_manifest: bool,
        extra_file: bool,
        missing_file: bool,
    ) -> PathBuf {
        let payload = b"audit-entry\n";
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let mut manifest = json!({
            "bundle": "evidence-bundle",
            "version": 1,
            "files": [{
                "path": "audit/trail.jsonl",
                "sha256": sha256_hex(payload)
            }],
            "signing": {
                "algorithm": "ed25519",
                "public_key": crate::core::agent_identity::hex_encode(
                    key.verifying_key().as_bytes()
                ),
                "signed_digest": "",
                "signature": ""
            }
        });
        let digest = sha256_hex(&canonical_json(&manifest).unwrap());
        let signature = crate::core::agent_identity::sign_bytes_with(&key, digest.as_bytes());
        manifest["signing"]["signed_digest"] = Value::String(digest);
        manifest["signing"]["signature"] =
            Value::String(crate::core::agent_identity::hex_encode(&signature));
        if tamper_manifest {
            manifest["subject"] = json!({"project": "tampered"});
        }

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(&canonical_json(&manifest).unwrap()).unwrap();
        if !missing_file {
            zip.start_file("audit/trail.jsonl", options).unwrap();
            zip.write_all(if tamper_file { b"tampered\n" } else { payload })
                .unwrap();
        }
        if extra_file {
            zip.start_file("extra.txt", options).unwrap();
            zip.write_all(b"extra").unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let path =
            std::env::temp_dir().join(format!("lean-ctx-verify-{name}-{}.zip", std::process::id()));
        fs::write(&path, bytes).unwrap();
        path
    }

    fn command(path: PathBuf) -> VerifyCommand {
        VerifyCommand {
            bundle_path: path,
            public_key: None,
            verbose: false,
            json_output: false,
        }
    }

    #[test]
    fn valid_bundle_passes() {
        let path = fixture("valid", false, false, false, false);
        let result = execute_verify(&command(path.clone())).unwrap();
        let _ = fs::remove_file(path);
        assert!(result.bundle_valid, "{result:?}");
        assert!(result.manifest_intact);
        assert!(result.signature_valid);
    }

    #[test]
    fn unicode_signature_material_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unicode-bundle.zip");
        for field in ["public_key", "signature"] {
            for malformed in ["\u{1f512}", "0\u{20ac}", "\u{e9}\u{e9}"] {
                let mut manifest = json!({
                    "bundle": "evidence-bundle",
                    "version": 1,
                    "files": [],
                    "signing": {
                        "algorithm": "ed25519",
                        "public_key": "00".repeat(32),
                        "signed_digest": "",
                        "signature": ""
                    }
                });
                if field == "public_key" {
                    manifest["signing"][field] = json!(malformed);
                }
                let digest = sha256_hex(&canonical_json(&manifest).unwrap());
                manifest["signing"]["signed_digest"] = json!(digest);
                manifest["signing"]["signature"] = json!("00".repeat(64));
                manifest["signing"][field] = json!(malformed);
                let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
                zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(&canonical_json(&manifest).unwrap()).unwrap();
                zip.finish().unwrap();

                let result = execute_verify(&command(path.clone())).unwrap();
                assert!(result.manifest_intact, "{result:?}");
                assert!(!result.bundle_valid);
                assert!(!result.signature_valid);
                let expected = if field == "public_key" {
                    "invalid manifest public key: non-ASCII hex string".to_string()
                } else {
                    format!(
                        "invalid manifest signature: signature must be 64 bytes (hex or base64), got {} chars",
                        malformed.len()
                    )
                };
                assert_eq!(result.errors, vec![expected]);
                assert!(result.warnings.is_empty());
            }
        }
    }

    #[test]
    fn tampered_file_fails_hash_check() {
        let path = fixture("tampered-file", true, false, false, false);
        let result = execute_verify(&command(path.clone())).unwrap();
        let _ = fs::remove_file(path);
        assert!(!result.bundle_valid);
        assert!(!result.manifest_intact);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("hash mismatch"))
        );
    }

    #[test]
    fn tampered_manifest_fails_signature_check() {
        let path = fixture("tampered-manifest", false, true, false, false);
        let result = execute_verify(&command(path.clone())).unwrap();
        let _ = fs::remove_file(path);
        assert!(!result.bundle_valid);
        assert!(!result.signature_valid);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("signature"))
        );
    }

    #[test]
    fn extra_file_is_a_warning() {
        let path = fixture("extra", false, false, true, false);
        let result = execute_verify(&command(path.clone())).unwrap();
        let _ = fs::remove_file(path);
        assert!(result.bundle_valid, "{result:?}");
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("extra.txt"))
        );
    }

    #[test]
    fn missing_file_is_an_error() {
        let path = fixture("missing", false, false, false, true);
        let result = execute_verify(&command(path.clone())).unwrap();
        let _ = fs::remove_file(path);
        assert!(!result.bundle_valid);
        assert!(!result.manifest_intact);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("missing file"))
        );
    }
}
