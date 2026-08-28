//! Shared low-level file-edit I/O primitives (epic #1008).
//!
//! Extracted from `ctx_edit` so the anchored editor (`ctx_patch`) reuses the
//! *exact same* TOCTOU-safe read→verify→atomic-write path instead of growing a
//! second, drifting copy of this security-critical code:
//!
//! * symlink rejection (`reject_symlink` + `O_NOFOLLOW`),
//! * read-size cap + UTF-8 validation,
//! * whole-file preimage fingerprint (size + mtime + BLAKE3) for the TOCTOU
//!   guard ([`ensure_preimage_still_matches`]),
//! * permission-preserving crash-atomic `rename`, with the read-only-directory
//!   in-place fallback (#459),
//! * the read-only-roots write choke point (#475).
//!
//! One implementation = one audited boundary. `ctx_edit` (`str_replace`) and
//! `ctx_patch` (anchored) both apply through these functions, so a fix here
//! protects both tools at once.
//!
//! **Known limitation (#960):** the TOCTOU guard is a point-in-time check,
//! not a held lock — see [`ensure_preimage_still_matches`] for the residual
//! window between that check and the atomic write it gates.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Size + mtime + content hash of a file, used as a TOCTOU fingerprint.
///
/// The digest is BLAKE3 (64 hex chars), not MD5 — the field and every receipt
/// that prints it say so, after a user reported chasing a `md5=` label that
/// matched neither md5 nor sha256 (#1592).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileFingerprint {
    pub(crate) size: u64,
    pub(crate) mtime_ms: u64,
    pub(crate) blake3: String,
}

/// A file read for editing: its fingerprint, permissions, raw bytes, decoded
/// text and whether it uses CRLF line endings.
#[derive(Clone, Debug)]
pub(crate) struct FilePreimage {
    pub(crate) fp: FileFingerprint,
    pub(crate) permissions: std::fs::Permissions,
    pub(crate) bytes: Vec<u8>,
    pub(crate) text: String,
    pub(crate) uses_crlf: bool,
}

pub(crate) fn system_time_to_millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Rejects symlinks at `path` (TOCTOU protection, same boundary as
/// `core::io_boundary::read_file_nofollow`): a symlink planted inside the jail
/// after the jail check could otherwise read or overwrite files outside it.
pub(crate) fn reject_symlink(path: &Path) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        // Windows: also covers NTFS junctions/reparse points (GL#442).
        if crate::core::pathutil::is_symlink_or_reparse(&meta) {
            return Err(format!(
                "ERROR: {} is a symlink — refusing to edit through it (TOCTOU protection). \
                 Edit the symlink target directly via its real path.",
                path.display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn read_file_bytes_limited(
    path: &Path,
    cap: usize,
) -> Result<(Vec<u8>, std::fs::Metadata), String> {
    reject_symlink(path)?;

    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > cap as u64
    {
        return Err(format!(
            "ERROR: file too large ({} bytes, cap {} via LCTX_MAX_READ_BYTES): {}",
            meta.len(),
            cap,
            path.display()
        ));
    }

    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        // Defense in depth alongside `reject_symlink`: O_NOFOLLOW closes the
        // race between the lstat check and the open.
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts.open(path).map_err(|e| {
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::ELOOP) {
            return format!(
                "ERROR: {} is a symlink — refusing to edit through it (TOCTOU protection).",
                path.display()
            );
        }
        format!("ERROR: cannot open {}: {e}", path.display())
    })?;

    use std::io::Read;
    let mut raw: Vec<u8> = Vec::new();
    let mut limited = (&mut file).take((cap as u64).saturating_add(1));
    limited
        .read_to_end(&mut raw)
        .map_err(|e| format!("ERROR: cannot read {}: {e}", path.display()))?;
    if raw.len() > cap {
        return Err(format!(
            "ERROR: file too large (cap {} via LCTX_MAX_READ_BYTES): {}",
            cap,
            path.display()
        ));
    }

    let meta = file
        .metadata()
        .map_err(|e| format!("ERROR: cannot stat {}: {e}", path.display()))?;
    Ok((raw, meta))
}

pub(crate) fn fingerprint_from_bytes(bytes: &[u8], meta: &std::fs::Metadata) -> FileFingerprint {
    FileFingerprint {
        size: bytes.len() as u64,
        mtime_ms: meta.modified().map_or(0, system_time_to_millis),
        blake3: crate::core::hasher::hash_hex(bytes),
    }
}

pub(crate) fn read_preimage(
    path: &Path,
    cap: usize,
    allow_lossy_utf8: bool,
) -> Result<FilePreimage, String> {
    let (bytes, meta) = read_file_bytes_limited(path, cap)?;
    let permissions = meta.permissions();
    let fp = fingerprint_from_bytes(&bytes, &meta);

    let text = if allow_lossy_utf8 {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::from_utf8(bytes.clone()).map_err(|_| {
            format!(
                "ERROR: file is not valid UTF-8 (binary/encoding). Refusing to edit: {}",
                path.display()
            )
        })?
    };
    let uses_crlf = text.contains("\r\n");

    Ok(FilePreimage {
        fp,
        permissions,
        bytes,
        text,
        uses_crlf,
    })
}

/// Re-reads the file and confirms its fingerprint still equals `expected`
/// (TOCTOU guard): a concurrent writer between the preimage read and the write
/// is detected here so the edit aborts instead of clobbering newer bytes.
///
/// **Residual window (#960, accepted limitation):** this is a point-in-time
/// check, not a held lock — it only detects a write that already happened
/// *before* this call returns. Callers still compute `new_content` and hand
/// it to [`write_atomic_bytes_with_permissions`] afterward; nothing guards
/// that gap. An external editor writing to `path` between this check
/// succeeding and the temp+rename completing is not detected, and its write
/// is silently overwritten. Acceptable for lean-ctx's single-writer-daemon
/// model — the MCP server is the only intended writer of files it edits — but
/// a real gap if an external editor (or a second lean-ctx instance) writes to
/// the same file concurrently. Closing it fully would need a held file lock
/// (e.g. `flock`) spanning check-through-rename, more machinery than the
/// mono-writer case warrants today.
pub(crate) fn ensure_preimage_still_matches(
    path: &Path,
    expected: &FileFingerprint,
    cap: usize,
) -> Result<(), String> {
    let (bytes, meta) = read_file_bytes_limited(path, cap)?;
    let now = fingerprint_from_bytes(&bytes, &meta);
    if &now != expected {
        return Err(format!(
            "ERROR: file changed since read (TOCTOU guard). Re-read and retry: {}\nexpected: size={}, mtime_ms={}, blake3={}\nactual:   size={}, mtime_ms={}, blake3={}",
            path.display(),
            expected.size,
            expected.mtime_ms,
            expected.blake3,
            now.size,
            now.mtime_ms,
            now.blake3
        ));
    }
    Ok(())
}

pub(crate) fn default_backup_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let filename = path.file_name()?.to_string_lossy();
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    Some(parent.join(format!("{filename}.lean-ctx.bak.{pid}.{nanos}")))
}

pub(crate) fn write_atomic_bytes_with_permissions(
    path: &Path,
    bytes: &[u8],
    permissions: Option<&std::fs::Permissions>,
) -> Result<(), String> {
    // Read-only-roots choke point (#475). Every edit write — replace, create,
    // and the pre-edit backup — funnels here, including a backup whose raw
    // `backup_path` bypasses the dispatch jail, so this single guard makes the
    // whole tool default-deny inside a read-only root before any byte is written
    // or temp/dir created.
    crate::core::pathjail::enforce_writable(path)?;

    // The rename below would *replace* a symlink at `path` (safe), but the edit
    // pipeline read through this path moments ago — a symlink here means the
    // read/write pair straddles two different files. Reject for consistency
    // with the read-side O_NOFOLLOW boundary.
    reject_symlink(path)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    // Mechanics — temp+rename with the read-only-directory in-place fallback
    // (#459) — are shared with `config_io` via `core::atomic_fs`. The symlink /
    // TOCTOU / read-only-root policy above stays here, so the edit tools keep
    // their audited boundary while the durable-write dance lives in one place.
    crate::core::atomic_fs::write_bytes_with_fallback(path, bytes, permissions)
        .map_err(|e| format!("ERROR: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_toctou_via_preimage_guard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("toctou.txt");
        std::fs::write(&path, "aaa\n").unwrap();
        let cap = crate::core::limits::max_read_bytes();
        let pre = read_preimage(&path, cap, false).unwrap();
        std::fs::write(&path, "bbb\n").unwrap();
        let err = ensure_preimage_still_matches(&path, &pre.fp, cap).unwrap_err();
        assert!(err.contains("TOCTOU guard"), "unexpected error: {err}");
    }

    #[test]
    fn read_preimage_rejects_invalid_utf8_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
        let cap = crate::core::limits::max_read_bytes();
        let err = read_preimage(&path, cap, false).unwrap_err();
        assert!(err.contains("not valid UTF-8"), "got: {err}");
        // Lossy mode tolerates it.
        assert!(read_preimage(&path, cap, true).is_ok());
    }

    #[test]
    fn fingerprint_is_content_addressed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fp.txt");
        std::fs::write(&path, "hello\n").unwrap();
        let cap = crate::core::limits::max_read_bytes();
        let a = read_preimage(&path, cap, false).unwrap().fp;
        let b = read_preimage(&path, cap, false).unwrap().fp;
        assert_eq!(a, b, "same bytes → same fingerprint");
        assert_eq!(a.blake3, crate::core::hasher::hash_hex(b"hello\n"));
    }
}
