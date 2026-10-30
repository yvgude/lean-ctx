//! End-to-end mutation tests for offline evidence-bundle verification.

use lean_ctx::cli::dispatch::verify::{VerifyCommand, execute_verify};
use lean_ctx::core::audit_trail::{self, AuditEntryData, AuditEventType};
use lean_ctx::core::evidence_bundle::{BundleSpec, generate};
use serial_test::serial;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

struct DataDirGuard;

impl DataDirGuard {
    fn set(path: &Path) -> Self {
        // SAFETY: tests in this binary are serialised and every test restores
        // the process environment when its guard is dropped.
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", path) };
        Self
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        // SAFETY: paired with `DataDirGuard::set`; serial execution prevents
        // another test in this binary from observing the change.
        unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn generate_test_bundle(temp: &tempfile::TempDir) -> PathBuf {
    let now = chrono::Utc::now();
    audit_trail::record(AuditEntryData {
        agent_id: "tamper-test-agent".into(),
        tool: "ctx_read".into(),
        action: None,
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 1,
        role: "developer".into(),
        event_type: AuditEventType::ToolCall,
    });

    let path = temp.path().join("evidence.zip");
    generate(&BundleSpec {
        from: (now - chrono::Duration::hours(1)).to_rfc3339(),
        to: (now + chrono::Duration::hours(1)).to_rfc3339(),
        pack: None,
        out: Some(path.clone()),
    })
    .expect("generate a real evidence bundle");
    path
}

fn verify_command(bundle_path: PathBuf) -> VerifyCommand {
    VerifyCommand {
        bundle_path,
        public_key: None,
        verbose: false,
        json_output: false,
    }
}

fn read_zip_entries(path: &Path) -> Vec<(String, Vec<u8>)> {
    let file = File::open(path).expect("open bundle");
    let mut archive = zip::ZipArchive::new(file).expect("read bundle ZIP");
    (0..archive.len())
        .map(|index| {
            let mut entry = archive.by_index(index).expect("read ZIP entry");
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .expect("read ZIP entry content");
            (entry.name().unwrap().into_owned(), bytes)
        })
        .collect()
}

fn write_zip_entries(path: &Path, entries: impl IntoIterator<Item = (String, Vec<u8>)>) {
    let file = File::create(path).expect("create modified bundle");
    let mut archive = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());

    for (name, contents) in entries {
        archive.start_file(name, options).expect("write ZIP entry");
        archive.write_all(&contents).expect("write ZIP content");
    }
    archive.finish().expect("finish modified bundle");
}

#[test]
#[serial]
fn test_valid_bundle_passes_verification() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _data_dir = DataDirGuard::set(temp.path());
    let bundle = generate_test_bundle(&temp);

    let result = execute_verify(&verify_command(bundle)).expect("verify valid bundle");

    assert!(result.bundle_valid, "{result:?}");
    assert!(result.manifest_intact, "{result:?}");
    assert!(result.signature_valid, "{result:?}");
    assert!(result.errors.is_empty(), "{result:?}");
}

/// A withheld read is in the bundle's trail with its reason class only, and
/// the bundle still verifies.
#[test]
#[serial]
fn a_withheld_read_is_in_the_bundle_trail_by_class_only() {
    use lean_ctx::core::security_events::{RefusalClass, record_refusals};
    let temp = tempfile::tempdir().expect("tempdir");
    let _data_dir = DataDirGuard::set(temp.path());
    record_refusals("ctx_read", "tamper-test-agent", RefusalClass::SecretPath, 1);
    record_refusals(
        "ctx_search",
        "tamper-test-agent",
        RefusalClass::SecretPath,
        2,
    );
    record_refusals(
        "ctx_read",
        "tamper-test-agent",
        RefusalClass::OutsideRoot,
        1,
    );
    let bundle = generate_test_bundle(&temp);

    let result = execute_verify(&verify_command(bundle.clone())).expect("verify bundle");
    assert!(result.bundle_valid, "{result:?}");

    let (_, trail) = read_zip_entries(&bundle)
        .into_iter()
        .find(|(name, _)| name == "audit/trail.jsonl")
        .expect("trail in bundle");
    let trail = String::from_utf8(trail).expect("utf-8 trail");
    let actions: Vec<(String, String)> = trail
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
        .filter_map(|entry| {
            Some((
                entry["tool"].as_str()?.to_string(),
                entry["action"].as_str()?.to_string(),
            ))
        })
        .collect();
    let has = |tool: &str, prefix: &str| {
        actions
            .iter()
            .any(|(t, action)| t == tool && action.starts_with(prefix))
    };
    assert!(
        has("ctx_read", "content_withheld:1|reason=secret_path"),
        "{actions:?}"
    );
    assert!(
        has("ctx_search", "content_withheld:2|reason=secret_path"),
        "{actions:?}"
    );
    assert!(
        has("ctx_read", "path_blocked:1|reason=outside_root"),
        "{actions:?}"
    );
    assert!(!trail.contains(".env"), "no path in the trail");
}

#[test]
#[serial]
fn test_tampered_file_fails_verification() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _data_dir = DataDirGuard::set(temp.path());
    let bundle = generate_test_bundle(&temp);
    let mut entries = read_zip_entries(&bundle);
    let (_, contents) = entries
        .iter_mut()
        .find(|(name, _)| name == "audit/trail.jsonl")
        .expect("audit file in generated bundle");
    contents.extend_from_slice(b"tampered\n");
    write_zip_entries(&bundle, entries);

    let result = execute_verify(&verify_command(bundle)).expect("verify tampered bundle");

    assert!(!result.bundle_valid, "{result:?}");
    assert!(!result.manifest_intact, "{result:?}");
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("hash mismatch for audit/trail.jsonl")),
        "{result:?}"
    );
}

#[test]
#[serial]
fn test_removed_file_fails_verification() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _data_dir = DataDirGuard::set(temp.path());
    let bundle = generate_test_bundle(&temp);
    let entries = read_zip_entries(&bundle)
        .into_iter()
        .filter(|(name, _)| name != "audit/trail.jsonl")
        .collect::<Vec<_>>();
    write_zip_entries(&bundle, entries);

    let result = execute_verify(&verify_command(bundle)).expect("verify incomplete bundle");

    assert!(!result.bundle_valid, "{result:?}");
    assert!(!result.manifest_intact, "{result:?}");
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("missing file listed in manifest: audit/trail.jsonl")),
        "{result:?}"
    );
}

#[test]
#[serial]
fn test_extra_unlisted_file_warns() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _data_dir = DataDirGuard::set(temp.path());
    let bundle = generate_test_bundle(&temp);
    let mut entries = read_zip_entries(&bundle);
    entries.push((
        "diagnostics/unlisted.txt".into(),
        b"extra evidence".to_vec(),
    ));
    write_zip_entries(&bundle, entries);

    let result = execute_verify(&verify_command(bundle)).expect("verify bundle with extra file");

    assert!(result.bundle_valid, "{result:?}");
    assert!(
        result.warnings.iter().any(|warning| {
            warning.contains("unlisted file in bundle: diagnostics/unlisted.txt")
        }),
        "{result:?}"
    );
}

#[test]
#[serial]
fn test_tampered_manifest_fails_signature() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _data_dir = DataDirGuard::set(temp.path());
    let bundle = generate_test_bundle(&temp);
    let mut entries = read_zip_entries(&bundle);
    let (_, manifest_bytes) = entries
        .iter_mut()
        .find(|(name, _)| name == "manifest.json")
        .expect("manifest in generated bundle");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(manifest_bytes).expect("parse generated manifest");
    manifest["files"][0]["sha256"] = serde_json::Value::String("0".repeat(64));
    *manifest_bytes = serde_json::to_vec(&manifest).expect("serialize tampered manifest");
    write_zip_entries(&bundle, entries);

    let result = execute_verify(&verify_command(bundle)).expect("verify tampered manifest");

    assert!(!result.bundle_valid, "{result:?}");
    assert!(!result.signature_valid, "{result:?}");
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("manifest signature verification failed")),
        "{result:?}"
    );
}

#[test]
#[serial]
fn test_wrong_public_key_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _data_dir = DataDirGuard::set(temp.path());
    let bundle = generate_test_bundle(&temp);
    let wrong_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let public_key_path = temp.path().join("wrong-public-key.hex");
    std::fs::write(
        &public_key_path,
        hex_encode(wrong_key.verifying_key().as_bytes()),
    )
    .expect("write wrong public key");
    let mut command = verify_command(bundle);
    command.public_key = Some(public_key_path);

    let result = execute_verify(&command).expect("verify with wrong public key");

    assert!(!result.bundle_valid, "{result:?}");
    assert!(!result.signature_valid, "{result:?}");
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("manifest signature verification failed")),
        "{result:?}"
    );
}

#[test]
#[serial]
fn test_empty_bundle_fails_gracefully() {
    let temp = tempfile::tempdir().expect("tempdir");
    let bundle = temp.path().join("empty.zip");
    write_zip_entries(&bundle, Vec::new());

    let result = execute_verify(&verify_command(bundle)).expect("empty ZIP is handled cleanly");

    assert!(!result.bundle_valid, "{result:?}");
    assert!(!result.manifest_intact, "{result:?}");
    assert!(!result.signature_valid, "{result:?}");
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("missing manifest.json")),
        "{result:?}"
    );
}
