//! Publish / import a Context Snapshot for sharing (#1027).
//!
//! Phase 4 of the Context Time Machine: take a stored snapshot out of the local
//! timeline and hand it to someone else — and take theirs in. A snapshot is
//! already a self-contained, content-addressed (`snapshot_id` = BLAKE3 of the
//! body) artifact, so sharing is just two guarded file moves:
//!
//! - **publish** — write the snapshot to a portable `*.ctxsnapshot.json` file,
//!   signing it first (reusing the ctxpkg publisher keypair) so the recipient
//!   can verify provenance. Read-only on local state.
//! - **import** — read such a file, prove its integrity (the body must still
//!   hash to its id) and — when signed — its signature, then append it to the
//!   local timeline so it can be `show`n, `verify`d and `restore`d. Idempotent:
//!   re-importing the same id is a no-op.

use std::path::{Path, PathBuf};

use super::digest::compute_id;
use super::signing::{sign_snapshot, verify_snapshot};
use super::timeline;
use super::types::ContextSnapshotV1;

/// File suffix for a shared snapshot artifact.
pub const SHARED_SUFFIX: &str = "ctxsnapshot.json";

/// Where / how to publish.
pub struct PublishOptions {
    /// Project the snapshot belongs to (selects the publish directory default).
    pub project_root: String,
    /// Explicit output path; defaults to `./<shortid>.ctxsnapshot.json`.
    pub out: Option<PathBuf>,
}

/// Result of publishing a snapshot to a shareable file.
#[derive(Debug)]
pub struct PublishOutcome {
    pub path: PathBuf,
    /// Publisher identity (ed25519 public key hex) the file is signed with.
    pub public_key: String,
    /// `true` if the snapshot was unsigned and got signed during publish.
    pub newly_signed: bool,
}

/// Result of importing a shared snapshot file into the local timeline.
#[derive(Debug)]
pub struct ImportOutcome {
    pub snapshot_id: String,
    /// The snapshot carried a signature.
    pub signed: bool,
    /// The signature validated (always `false` when unsigned).
    pub verified: bool,
    /// The id was already in the local timeline; nothing was appended.
    pub already_present: bool,
    /// Where the payload lives in the local timeline.
    pub path: PathBuf,
}

/// Publish a stored snapshot as a portable, signed file others can import.
///
/// Always ships signed — provenance is the entire point of sharing — but never
/// mutates the locally stored snapshot: it signs a clone and writes that out.
pub fn publish(
    snapshot: &ContextSnapshotV1,
    opts: &PublishOptions,
) -> Result<PublishOutcome, String> {
    let mut snap = snapshot.clone();
    let newly_signed = snap.signature.is_none();
    if newly_signed {
        let (key, _newly_created) = crate::core::context_package::keys::load_or_create()?;
        // Signing re-finalizes the id; the body is unchanged, so the id is
        // identical — the recipient imports it under the same id.
        sign_snapshot(&mut snap, &key)?;
    }
    let public_key = snap
        .signature
        .as_ref()
        .map(|s| s.public_key.clone())
        .unwrap_or_default();

    let path = match &opts.out {
        Some(p) => p.clone(),
        None => default_publish_path(&snap.snapshot_id),
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("create publish dir: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(&snap).map_err(|e| format!("serialize snapshot: {e}"))?;
    crate::config_io::write_atomic(&path, &json)?;

    Ok(PublishOutcome {
        path,
        public_key,
        newly_signed,
    })
}

/// Import a shared snapshot file into the local project timeline.
///
/// Integrity is mandatory (the body must hash to its id); a present-but-invalid
/// signature is fatal. An unsigned-but-intact snapshot imports with a warning
/// (`signed == false`). Re-importing an id already in the timeline is a no-op.
pub fn import(file: &Path, project_root: &str) -> Result<ImportOutcome, String> {
    let content =
        std::fs::read_to_string(file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let snap: ContextSnapshotV1 =
        serde_json::from_str(&content).map_err(|e| format!("parse snapshot: {e}"))?;

    if compute_id(&snap)? != snap.snapshot_id {
        return Err("integrity check failed: snapshot body does not match its id".into());
    }
    let signed = snap.signature.is_some();
    let verified = signed && verify_snapshot(&snap)?;
    if signed && !verified {
        return Err("signature verification failed — refusing to import".into());
    }

    let already_present = timeline::load_entries(project_root)
        .iter()
        .any(|e| e.snapshot_id == snap.snapshot_id);
    let path = if already_present {
        timeline::snapshots_dir(project_root)?.join(format!("{}.json", snap.snapshot_id))
    } else {
        timeline::write_snapshot(project_root, &snap)?
    };

    Ok(ImportOutcome {
        snapshot_id: snap.snapshot_id,
        signed,
        verified,
        already_present,
        path,
    })
}

/// `./<shortid>.ctxsnapshot.json` — share-ready in the current directory.
fn default_publish_path(snapshot_id: &str) -> PathBuf {
    let short: String = snapshot_id.chars().take(12).collect();
    PathBuf::from(format!("{short}.{SHARED_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_snapshot::digest::finalize_id;
    use ed25519_dalek::SigningKey;

    fn signed_snapshot() -> ContextSnapshotV1 {
        let mut s = ContextSnapshotV1::new("2026-06-28T00:00:00Z".into(), "9.9.9".into());
        s.git.commit = Some("abc1234".into());
        sign_snapshot(&mut s, &SigningKey::from_bytes(&[5u8; 32])).expect("sign");
        s
    }

    #[test]
    fn publish_then_import_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("share.ctxsnapshot.json");
        let snap = signed_snapshot();

        // Publish an already-signed snapshot: file is written, nothing re-signed.
        let outcome = publish(
            &snap,
            &PublishOptions {
                project_root: "/unused-for-explicit-out".into(),
                out: Some(out.clone()),
            },
        )
        .expect("publish");
        assert!(!outcome.newly_signed);
        assert!(out.exists());

        // Import into a fresh project timeline rooted at the tempdir.
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let root = project.to_string_lossy().to_string();
        let imported = import(&out, &root).expect("import");
        assert_eq!(imported.snapshot_id, snap.snapshot_id);
        assert!(imported.signed && imported.verified);
        assert!(!imported.already_present);
        assert_eq!(timeline::load_entries(&root).len(), 1);

        // Re-import is idempotent — no duplicate timeline entry.
        let again = import(&out, &root).expect("re-import");
        assert!(again.already_present);
        assert_eq!(timeline::load_entries(&root).len(), 1);
    }

    #[test]
    fn import_rejects_a_tampered_body() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("tampered.ctxsnapshot.json");
        let mut snap = signed_snapshot();
        // Mutate the body after signing without re-hashing: id no longer matches.
        snap.git.dirty = true;
        std::fs::write(&out, serde_json::to_string_pretty(&snap).unwrap()).unwrap();

        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let err = import(&out, &project.to_string_lossy()).unwrap_err();
        assert!(err.contains("integrity"), "got: {err}");
    }

    #[test]
    fn publish_signs_an_unsigned_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("fresh.ctxsnapshot.json");
        let mut snap = ContextSnapshotV1::new("2026-06-28T00:00:00Z".into(), "9.9.9".into());
        finalize_id(&mut snap).expect("finalize");
        assert!(snap.signature.is_none());

        let outcome = publish(
            &snap,
            &PublishOptions {
                project_root: "/unused".into(),
                out: Some(out.clone()),
            },
        )
        .expect("publish");
        assert!(outcome.newly_signed);
        assert_eq!(outcome.public_key.len(), 64);

        // The published file verifies and keeps the same id (body unchanged).
        let written: ContextSnapshotV1 =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(written.snapshot_id, snap.snapshot_id);
        assert!(verify_snapshot(&written).expect("verify"));
    }

    #[test]
    fn default_path_is_short_and_suffixed() {
        let p = default_publish_path(&"a".repeat(64));
        assert_eq!(p.to_string_lossy(), format!("aaaaaaaaaaaa.{SHARED_SUFFIX}"));
    }
}
