// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx evidence import-landing <file>` and `lean-ctx evidence landings`:
//! store and show landing receipts ([`LandingReceiptV1`]).
//!
//! A receipt is validated, its Ed25519 signature checked when it carries one,
//! and written to the project's proofs directory (`.lean-ctx/proofs/`) under
//! its content id, `landing-receipt-<first 16 hex of the digest>.json`.
//! Importing the same landing twice writes the same file once. Receipts hold
//! metadata only (ids, SHAs, verdict, check selection digest), never code.
//!
//! A stored signed receipt is never downgraded: importing a hashed copy of
//! the same landing keeps the signed one, and a copy signed by a different
//! key is refused. "Signed" means the signature verifies against the
//! embedded key; listings name that key's fingerprint so it can be compared
//! with a trusted one.

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use lean_ctx_protocol::landing_receipt::{LandingReceiptV1, MAX_LANDING_RECEIPT_BYTES};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const FILE_PREFIX: &str = "landing-receipt-";

/// How the receipt's integrity is established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Integrity {
    /// Content id only.
    Hashed,
    /// Ed25519 signature verified against the embedded public key.
    Signed,
}

impl Integrity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hashed => "hashed",
            Self::Signed => "signed",
        }
    }
}

/// Outcome of one import.
#[derive(Debug)]
pub(crate) struct Imported {
    pub receipt: LandingReceiptV1,
    pub path: PathBuf,
    pub integrity: Integrity,
    pub already_present: bool,
}

fn file_name(receipt: &LandingReceiptV1) -> String {
    format!("{FILE_PREFIX}{}.json", &receipt.digest().hex()[..16])
}

/// Lock file of the landing receipt store, next to the receipts. Its name
/// does not start with [`FILE_PREFIX`], so listings never show it.
const LOCK_FILE: &str = ".landing-receipts.lock";
/// How long an import waits for another one before giving up.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Exclusive hold on the project's landing receipt store, across the threads
/// of this process (a queued turn) and across processes (an advisory file
/// lock, bounded by [`LOCK_WAIT`]). Fields drop in order: file lock first.
struct StoreLock {
    _file: std::fs::File,
    _turn: lean_ctx::core::file_lock::Turn,
}

impl StoreLock {
    fn acquire(project_root: &Path) -> Result<Self> {
        let lock = lean_ctx::core::context_proof::proof_destination(project_root, LOCK_FILE)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let path = lock.path();
        let turn = lean_ctx::core::file_lock::take_turn(path, LOCK_WAIT)
            .context("landing receipt store is busy (another import holds it)")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        lean_ctx::core::file_lock::acquire_exclusive_timeout(&file, LOCK_WAIT)
            .map_err(|e| anyhow::anyhow!("landing receipt store is busy: {e}"))?;
        Ok(Self {
            _file: file,
            _turn: turn,
        })
    }
}

/// Parse and validate `bytes`; verify the signature when present.
pub(crate) fn verify(bytes: &[u8]) -> Result<(LandingReceiptV1, Integrity)> {
    let receipt = LandingReceiptV1::from_json(bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
    let Some(signing) = &receipt.signing else {
        return Ok((receipt, Integrity::Hashed));
    };
    let public_key = STANDARD
        .decode(&signing.public_key)
        .context("signing.public_key is not base64")?;
    let signature = STANDARD
        .decode(&signing.signature)
        .context("signing.signature is not base64")?;
    if !lean_ctx::core::agent_identity::verify_signature(
        &public_key,
        &receipt.signing_payload(),
        &signature,
    ) {
        bail!("landing receipt signature does not verify");
    }
    Ok((receipt, Integrity::Signed))
}

/// Validate `bytes` and store the receipt in `project_root`'s proofs directory.
pub(crate) fn import_bytes(bytes: &[u8], project_root: &Path) -> Result<Imported> {
    let (receipt, integrity) = verify(bytes)?;
    let destination =
        lean_ctx::core::context_proof::proof_destination(project_root, &file_name(&receipt))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut document = serde_json::to_string_pretty(&receipt)?;
    document.push('\n');
    // The read-check-write below must not interleave with another import:
    // otherwise a hashed copy read before a signed copy was written could
    // overwrite it (a downgrade) or two different signers could both pass.
    let _lock = StoreLock::acquire(project_root)?;
    let existing = std::fs::read(destination.path()).ok();
    if let Some((stored, Integrity::Signed)) = existing.as_deref().and_then(|b| verify(b).ok()) {
        if stored.digest() == receipt.digest() {
            let stored_key = stored.signer_key_id();
            if integrity == Integrity::Signed && receipt.signer_key_id() != stored_key {
                bail!(
                    "a receipt for this landing signed by key {} is already stored; \
                     refusing the copy signed by key {}",
                    stored_key.unwrap_or_default(),
                    receipt.signer_key_id().unwrap_or_default()
                );
            }
            // Same key or an unsigned copy: keep the stored signed receipt.
            return Ok(Imported {
                receipt: stored,
                path: destination.path().to_path_buf(),
                integrity: Integrity::Signed,
                already_present: true,
            });
        }
    }
    let already_present = existing.is_some_and(|bytes| bytes == document.as_bytes());
    let path = if already_present {
        destination.path().to_path_buf()
    } else {
        destination
            .write(&document)
            .map_err(|e| anyhow::anyhow!("{e}"))?
    };
    Ok(Imported {
        receipt,
        path,
        integrity,
        already_present,
    })
}

/// Read a receipt file (bounded) and import it.
pub(crate) fn import_file(file: &Path, project_root: &Path) -> Result<Imported> {
    let handle = std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?;
    let mut bytes = Vec::new();
    handle
        .take(MAX_LANDING_RECEIPT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", file.display()))?;
    import_bytes(&bytes, project_root)
}

/// Every stored landing receipt of the project, oldest decision first.
/// Files that no longer validate are reported, never silently shown.
pub(crate) fn list(
    project_root: &Path,
) -> Result<(Vec<(LandingReceiptV1, Integrity)>, Vec<String>)> {
    let probe = lean_ctx::core::context_proof::proof_destination(project_root, "probe.json")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let Some(dir) = probe.path().parent() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut receipts = Vec::new();
    let mut invalid = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok((receipts, invalid));
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            name.starts_with(FILE_PREFIX)
                && std::path::Path::new(name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .collect();
    names.sort();
    for name in names {
        let path = dir.join(&name);
        let parsed = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| verify(&bytes));
        match parsed {
            Ok((receipt, integrity)) if file_name(&receipt) == name => {
                receipts.push((receipt, integrity));
            }
            Ok(_) => invalid.push(format!("{name}: content does not match its id")),
            Err(error) => invalid.push(format!("{name}: {error}")),
        }
    }
    receipts.sort_by(|(a, _), (b, _)| {
        (a.decided_at.as_str(), a.digest()).cmp(&(b.decided_at.as_str(), b.digest()))
    });
    Ok((receipts, invalid))
}

/// `import-landing`: print the stored receipt's id and location.
pub(crate) fn run_import(file: &Path, json: bool) -> i32 {
    let root = PathBuf::from(crate::cli::common::detect_project_root(&[]));
    match import_file(file, &root) {
        Ok(imported) => {
            let digest = imported.receipt.digest();
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "id": digest.as_str(),
                        "integrity": imported.integrity.as_str(),
                        "signer_key_id": imported.receipt.signer_key_id(),
                        "path": imported.path.display().to_string(),
                        "already_present": imported.already_present,
                        "receipt": imported.receipt,
                    })
                );
            } else {
                println!(
                    "Landing receipt {} {} ({}): {}",
                    digest.as_str(),
                    if imported.already_present {
                        "already stored"
                    } else {
                        "stored"
                    },
                    imported.integrity.as_str(),
                    imported.path.display()
                );
                println!("  {}", imported.receipt.summary_line());
            }
            0
        }
        Err(error) => {
            eprintln!("evidence import-landing: {error:#}");
            1
        }
    }
}

/// `landings`: list the stored receipts.
pub(crate) fn run_list(json: bool) -> i32 {
    let root = PathBuf::from(crate::cli::common::detect_project_root(&[]));
    match list(&root) {
        Ok((receipts, invalid)) => {
            if json {
                let items: Vec<serde_json::Value> = receipts
                    .iter()
                    .map(|(receipt, integrity)| {
                        serde_json::json!({
                            "id": receipt.digest().as_str(),
                            "integrity": integrity.as_str(),
                            "signer_key_id": receipt.signer_key_id(),
                            "receipt": receipt,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({ "landings": items, "invalid": invalid })
                );
            } else if receipts.is_empty() && invalid.is_empty() {
                println!("No landing receipts stored in this project.");
            } else {
                println!("Landing receipts ({}):", receipts.len());
                for (receipt, _) in &receipts {
                    println!("  {}", receipt.summary_line());
                }
                for line in &invalid {
                    println!("  INVALID {line}");
                }
            }
            i32::from(!invalid.is_empty())
        }
        Err(error) => {
            eprintln!("evidence landings: {error:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn receipt_json(verdict: &str, landed: Option<&str>) -> serde_json::Value {
        let mut value = serde_json::json!({
            "schema_version": 1,
            "kind": "landing_receipt",
            "forge": "github",
            "project": "owner/repo",
            "request": 7,
            "request_id": "land-7-1",
            "target_branch": "main",
            "base_sha": "1".repeat(40),
            "head_sha": "2".repeat(40),
            "verdict": verdict,
            "check_selection_digest": format!("sha256:{}", "3".repeat(64)),
            "decided_at": "2026-11-18T09:30:00Z"
        });
        if let Some(sha) = landed {
            value["landed_sha"] = serde_json::Value::String(sha.to_string());
        }
        value
    }

    #[test]
    fn landing_receipt_import_round_trips_and_is_idempotent() {
        let project = tempfile::tempdir().unwrap();
        let bytes = serde_json::to_vec(&receipt_json("landed", Some(&"4".repeat(40)))).unwrap();
        let first = import_bytes(&bytes, project.path()).unwrap();
        assert_eq!(first.integrity, Integrity::Hashed);
        assert!(!first.already_present);
        assert!(
            first
                .path
                .starts_with(project.path().join(".lean-ctx").join("proofs"))
        );
        let second = import_bytes(&bytes, project.path()).unwrap();
        assert!(second.already_present);
        assert_eq!(second.path, first.path);

        let mut later = receipt_json("refused", None);
        later["decided_at"] = serde_json::Value::String("2026-11-18T10:00:00Z".to_string());
        import_bytes(&serde_json::to_vec(&later).unwrap(), project.path()).unwrap();

        let (receipts, invalid) = list(project.path()).unwrap();
        assert!(invalid.is_empty(), "{invalid:?}");
        assert_eq!(receipts.len(), 2);
        assert_eq!(receipts[0].0.digest(), first.receipt.digest());
        let lines: Vec<String> = receipts.iter().map(|(r, _)| r.summary_line()).collect();
        let again: Vec<String> = list(project.path())
            .unwrap()
            .0
            .iter()
            .map(|(r, _)| r.summary_line())
            .collect();
        assert_eq!(lines, again, "listing is deterministic");
    }

    fn signed_json(seed: u8) -> serde_json::Value {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let mut value = receipt_json("landed", Some(&"4".repeat(40)));
        value["signing"] = serde_json::json!({
            "algorithm": "ed25519",
            "public_key": STANDARD.encode(key.verifying_key().to_bytes()),
            "signature": STANDARD.encode([0u8; 64]),
        });
        let unsigned: LandingReceiptV1 = serde_json::from_value(value.clone()).unwrap();
        let signature = key.sign(&unsigned.signing_payload());
        value["signing"]["signature"] =
            serde_json::Value::String(STANDARD.encode(signature.to_bytes()));
        value
    }

    #[test]
    fn landing_receipt_signed_copy_is_never_downgraded_or_replaced() {
        let project = tempfile::tempdir().unwrap();
        let signed = serde_json::to_vec(&signed_json(7)).unwrap();
        let first = import_bytes(&signed, project.path()).unwrap();
        assert_eq!(first.integrity, Integrity::Signed);
        let stored = std::fs::read(&first.path).unwrap();

        // An unsigned copy of the same landing keeps the signed file.
        let unsigned = serde_json::to_vec(&receipt_json("landed", Some(&"4".repeat(40)))).unwrap();
        let again = import_bytes(&unsigned, project.path()).unwrap();
        assert!(again.already_present);
        assert_eq!(again.integrity, Integrity::Signed);
        assert_eq!(std::fs::read(&first.path).unwrap(), stored);

        // A copy signed by another key is refused and changes nothing.
        let other = serde_json::to_vec(&signed_json(9)).unwrap();
        let error = import_bytes(&other, project.path()).unwrap_err();
        assert!(error.to_string().contains("refusing"), "{error}");
        assert_eq!(std::fs::read(&first.path).unwrap(), stored);

        // The same signed copy is idempotent; the listing names the key.
        assert!(
            import_bytes(&signed, project.path())
                .unwrap()
                .already_present
        );
        let (listed, invalid) = list(project.path()).unwrap();
        assert!(invalid.is_empty(), "{invalid:?}");
        let key_id = listed[0].0.signer_key_id().unwrap();
        assert!(key_id.starts_with("id:sha256:"), "{key_id}");
        assert!(
            listed[0]
                .0
                .summary_line()
                .ends_with(&format!("[signed key {}]", &key_id[10..26]))
        );
    }

    #[test]
    fn landing_receipt_hashed_copy_is_upgraded_to_signed() {
        let project = tempfile::tempdir().unwrap();
        let unsigned = serde_json::to_vec(&receipt_json("landed", Some(&"4".repeat(40)))).unwrap();
        let first = import_bytes(&unsigned, project.path()).unwrap();
        assert_eq!(first.integrity, Integrity::Hashed);
        let signed = import_bytes(
            &serde_json::to_vec(&signed_json(7)).unwrap(),
            project.path(),
        )
        .unwrap();
        assert!(!signed.already_present);
        assert_eq!(signed.path, first.path);
        assert_eq!(list(project.path()).unwrap().0[0].1, Integrity::Signed);
    }

    #[test]
    fn landing_receipt_non_canonical_key_is_refused() {
        let mut value = signed_json(7);
        let key = value["signing"]["public_key"].as_str().unwrap().to_string();
        value["signing"]["public_key"] =
            serde_json::Value::String(key.trim_end_matches('=').to_string());
        assert!(verify(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn landing_receipt_signature_is_verified() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut value = receipt_json("landed", Some(&"4".repeat(40)));
        value["signing"] = serde_json::json!({
            "algorithm": "ed25519",
            "public_key": STANDARD.encode(key.verifying_key().to_bytes()),
            "signature": STANDARD.encode([0u8; 64]),
        });
        let unsigned: LandingReceiptV1 = serde_json::from_value(value.clone()).unwrap();
        let signature = key.sign(&unsigned.signing_payload());
        value["signing"]["signature"] =
            serde_json::Value::String(STANDARD.encode(signature.to_bytes()));
        let (receipt, integrity) = verify(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(integrity, Integrity::Signed);

        let mut tampered = value.clone();
        tampered["request"] = serde_json::json!(8);
        assert!(verify(&serde_json::to_vec(&tampered).unwrap()).is_err());

        let project = tempfile::tempdir().unwrap();
        let imported = import_bytes(&serde_json::to_vec(&value).unwrap(), project.path()).unwrap();
        assert_eq!(imported.receipt, receipt);
        let (listed, invalid) = list(project.path()).unwrap();
        assert!(invalid.is_empty());
        assert_eq!(listed[0].1, Integrity::Signed);
    }

    #[test]
    fn landing_receipt_concurrent_imports_never_downgrade_the_signed_copy() {
        let project = tempfile::tempdir().unwrap();
        let signed = serde_json::to_vec(&signed_json(7)).unwrap();
        let unsigned = serde_json::to_vec(&receipt_json("landed", Some(&"4".repeat(40)))).unwrap();
        for _ in 0..8 {
            let barrier = std::sync::Barrier::new(8);
            std::thread::scope(|scope| {
                for n in 0..8 {
                    let bytes = if n % 2 == 0 { &signed } else { &unsigned };
                    let (barrier, root) = (&barrier, project.path());
                    scope.spawn(move || {
                        barrier.wait();
                        import_bytes(bytes, root).unwrap();
                    });
                }
            });
            let (listed, invalid) = list(project.path()).unwrap();
            assert!(invalid.is_empty(), "{invalid:?}");
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].1, Integrity::Signed, "signed copy was downgraded");
        }
        let names: Vec<String> = std::fs::read_dir(project.path().join(".lean-ctx/proofs"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name != LOCK_FILE)
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn landing_receipt_with_code_content_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let mut value = receipt_json("landed", Some(&"4".repeat(40)));
        value["patch"] = serde_json::Value::String("diff --git a/x b/x".into());
        assert!(import_bytes(&serde_json::to_vec(&value).unwrap(), project.path()).is_err());
        assert!(list(project.path()).unwrap().0.is_empty());
    }
}
