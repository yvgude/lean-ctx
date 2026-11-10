// SPDX-License-Identifier: Apache-2.0
//! Evidence for a local reader (Hive Desktop, L7): `audit verify --json`
//! (the audit trail's hash chain, full pass), `savings verify --json` /
//! `ledger verify --json` (the savings ledger's chain) and
//! `evidence-export --json` (the evidence package with each file's digest
//! and both verdicts). A broken chain is a verdict, not an error: the
//! document says `valid: false` and the command exits 0.

use std::path::Path;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{Code, Failure};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AuditVerify {
    pub valid: bool,
    pub total_entries: usize,
    pub first_invalid_at: Option<usize>,
    pub source_present: bool,
    pub unknown_event_types: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct LedgerVerify {
    pub valid: bool,
    pub total: usize,
    pub first_invalid_at: Option<usize>,
}

pub(crate) fn audit_of(r: &lean_ctx::core::audit_trail::ChainVerifyResult) -> AuditVerify {
    AuditVerify {
        valid: r.valid,
        total_entries: r.total_entries,
        first_invalid_at: r.first_invalid_at,
        source_present: r.source_present,
        unknown_event_types: usize::try_from(r.unknown_event_types).unwrap_or(usize::MAX),
    }
}

pub(crate) fn ledger_of(r: &lean_ctx::core::savings_ledger::VerifyResult) -> LedgerVerify {
    LedgerVerify {
        valid: r.valid,
        total: r.total,
        first_invalid_at: r.first_invalid_at,
    }
}

/// `audit verify --json`: a full pass from genesis.
pub(crate) fn build_audit(_args: &[String]) -> Result<AuditVerify, Failure> {
    Ok(audit_of(&lean_ctx::core::audit_trail::verify_chain_full()))
}

/// `savings verify --json` / `ledger verify --json`.
pub(crate) fn build_ledger(_args: &[String]) -> Result<LedgerVerify, Failure> {
    Ok(ledger_of(&lean_ctx::core::savings_ledger::verify()))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct FileRow {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct EvidenceExport {
    pub output: String,
    pub files: Vec<FileRow>,
    pub audit: AuditVerify,
    pub ledger: LedgerVerify,
}

/// Largest file an export listing hashes; the package's files are a few
/// kilobytes, so anything larger is not one of them.
pub(crate) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The files `names` in `dir`, by name, with size and SHA-256. Only these
/// are read (never another file a directory holds), each a regular file
/// (no link) of at most [`MAX_FILE_BYTES`].
pub(crate) fn files_of(dir: &Path, names: &[&str]) -> Result<Vec<FileRow>, Failure> {
    let internal = |message: String| Failure::new(Code::Internal, message);
    let mut rows = Vec::new();
    for name in names {
        let path = dir.join(name);
        let meta =
            std::fs::symlink_metadata(&path).map_err(|e| internal(format!("{name}: {e}")))?;
        if !meta.file_type().is_file() {
            return Err(internal(format!("{name} is not a regular file")));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(internal(format!("{name} exceeds {MAX_FILE_BYTES} bytes")));
        }
        let bytes = std::fs::read(&path).map_err(|e| internal(format!("{name}: {e}")))?;
        rows.push(FileRow {
            path: (*name).to_owned(),
            bytes: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
        });
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(rows)
}

/// A new, empty package directory under `<data dir>/evidence/`; never an
/// existing one, never a path a caller names.
pub(crate) fn fresh_dir(data_dir: &Path) -> Result<std::path::PathBuf, Failure> {
    let internal = |message: String| Failure::new(Code::Internal, message);
    let parent = data_dir.join("evidence");
    std::fs::create_dir_all(&parent).map_err(|e| internal(format!("evidence dir: {e}")))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.6fZ");
    for attempt in 0..16u32 {
        let dir = parent.join(format!("export-{stamp}-{}-{attempt}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(internal(format!("evidence dir: {e}"))),
        }
    }
    Err(internal("no fresh evidence directory".into()))
}

/// `evidence-export --json`: writes the package into a fresh directory
/// under the data directory, then names each file it wrote and both chain
/// verdicts. A caller-given `--output` is refused (the directory is the
/// Instance's choice, so a request can never write elsewhere).
pub(crate) fn build_export(args: &[String]) -> Result<EvidenceExport, Failure> {
    if args
        .iter()
        .any(|a| a == "--output" || a.starts_with("--output="))
    {
        return Err(Failure::invalid(
            "evidence-export --json writes under the data directory; --output is not accepted",
        ));
    }
    let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir()
        .map_err(|e| Failure::new(Code::Internal, e))?;
    let dir = fresh_dir(&data_dir)?;
    let export_args = vec![
        "--output".to_owned(),
        dir.display().to_string(),
        "--format".to_owned(),
        "json".to_owned(),
    ];
    super::super::evidence_export::export(&export_args)
        .map_err(|e| Failure::new(Code::Internal, format!("evidence export failed: {e}")))?;
    let files = files_of(&dir, &super::super::evidence_export::EXPORTED_FILES)?;
    Ok(EvidenceExport {
        output: dir.display().to_string(),
        files,
        audit: audit_of(&lean_ctx::core::audit_trail::verify_chain_full()),
        ledger: ledger_of(&lean_ctx::core::savings_ledger::verify()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_files_the_export_wrote_are_listed_with_their_digest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.json"), b"{}").unwrap();
        std::fs::write(dir.path().join("a.md"), b"abc").unwrap();
        std::fs::write(dir.path().join("stranger.txt"), b"not ours").unwrap();
        let rows = files_of(dir.path(), &["b.json", "a.md"]).unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(names, ["a.md", "b.json"], "only the named files, sorted");
        assert_eq!(rows[0].bytes, 3);
        assert_eq!(
            rows[0].sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(
            files_of(dir.path(), &["sub"]).is_err(),
            "a directory is refused"
        );
    }

    #[test]
    fn an_export_never_writes_where_the_caller_says() {
        let data = tempfile::tempdir().unwrap();
        let first = fresh_dir(data.path()).unwrap();
        let second = fresh_dir(data.path()).unwrap();
        assert_ne!(first, second, "every export gets a new directory");
        assert!(first.starts_with(data.path().join("evidence")));
        let refused = build_export(&["--output".into(), "/tmp/elsewhere".into()]);
        assert_eq!(refused.unwrap_err().code, Code::InvalidArgument);
    }
}
