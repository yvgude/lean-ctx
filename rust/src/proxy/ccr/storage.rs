// SPDX-License-Identifier: Apache-2.0
//! Legacy tee files have no source authority. They are Community recovery only;
//! protected file recovery uses the existing source-bound archive store.

use crate::core::policy::{content, runtime};
use std::path::Path;

fn limit() -> usize {
    crate::core::limits::max_read_bytes().min(content::MAX_PROTECTED_CONTENT_BYTES)
}

fn community<T>(operation: impl FnOnce() -> Option<T>) -> Option<T> {
    runtime::with_source_view(|| {
        if runtime::is_active() {
            return None;
        }
        operation()
    })
    .ok()
    .flatten()
}

fn store_path(path: &Path) -> Option<()> {
    let directory = crate::core::paths::state_dir_read_only().ok()?.join("tee");
    if path.parent()? != directory
        || crate::core::pathutil::is_symlink_or_reparse(
            &std::fs::symlink_metadata(&directory).ok()?,
        )
    {
        return None;
    }
    Some(())
}

/// One bounded, no-follow read shared by selectors, HTTP, CLI and in-band
/// recovery. Policy is freshly resolved before I/O and rechecked at publication.
pub(crate) fn read(path: &Path) -> Option<String> {
    read_detailed(path).ok()
}

pub(crate) fn read_detailed(path: &Path) -> Result<String, &'static str> {
    runtime::with_source_view(|| {
        if runtime::is_active() {
            return Err(
                "stored output has no source authority; repeat the original authorized operation",
            );
        }
        const UNAVAILABLE: &str =
            "stored output is unavailable or is not a regular UTF-8 text file";
        const TOO_LARGE: &str = "stored output exceeds the configured recovery byte limit";
        store_path(path).ok_or(UNAVAILABLE)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let file = options.open(path).map_err(|_| UNAVAILABLE)?;
        let metadata = file.metadata().map_err(|_| UNAVAILABLE)?;
        if !metadata.is_file() || crate::core::pathutil::is_symlink_or_reparse(&metadata) {
            return Err(UNAVAILABLE);
        }
        if metadata.len() > limit() as u64 {
            return Err(TOO_LARGE);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err(UNAVAILABLE);
            }
        }
        use std::io::Read;
        let mut text = String::new();
        file.take(limit() as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|_| UNAVAILABLE)?;
        if text.len() > limit() {
            return Err(TOO_LARGE);
        }
        if text.contains('\0') {
            return Err(UNAVAILABLE);
        }
        let checked = content::protect_active(&text)
            .map_err(|_| "stored output is withheld by current content rules")?;
        // Recovery is a new delivery: re-admitted under the current policy
        // before any selector sees it (G5).
        crate::core::context_admission::recovery::admit_recovered(&checked, "tee")
            .map_err(|_| "stored output is withheld by the context gateway")
    })
    .map_err(|_| "current policy cannot be verified; retry the authorized operation")?
}

/// Never publish an incomplete, oversized, unfiltered or protected originless
/// copy. Atomic replacement preserves shell's latest-output handle contract.
pub(crate) fn write(path: &Path, text: &str) -> Option<()> {
    community(|| {
        if text.len() > limit() || text.contains('\0') {
            return None;
        }
        let checked = content::protect_active(text).ok()?;
        // Derived store: only admitted text, never withheld or restricted
        // content (G5, E3).
        let admitted = crate::core::context_admission::recovery::admit_for_storage(&checked)?;
        let masked = crate::core::redaction::redact_text(&admitted);
        let (redacted, _) = crate::core::secret_detection::scan_and_redact_from_config(&masked);
        if redacted.len() > limit() {
            return None;
        }
        let directory = crate::core::paths::state_dir_read_only().ok()?.join("tee");
        if path.parent()? != directory {
            return None;
        }
        std::fs::create_dir_all(&directory).ok()?;
        store_path(path)?;
        // Content-addressed: the proxy re-persists every pruned message on
        // every turn, and an atomic replace costs two fsyncs each time. When
        // the published bytes already match, only refresh the mtime so the
        // age-based tee cleanup still sees the entry as live.
        // A symlink never takes this path: the atomic replace below swaps it
        // for a regular file instead of following it.
        if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
            && std::fs::read(path).is_ok_and(|existing| existing == redacted.as_bytes())
        {
            return std::fs::File::options()
                .write(true)
                .open(path)
                .and_then(|file| file.set_modified(std::time::SystemTime::now()))
                .ok();
        }
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(std::fs::Permissions::from_mode(0o600))
        };
        #[cfg(not(unix))]
        let permissions = None;
        crate::core::atomic_fs::try_atomic_write(path, redacted.as_bytes(), permissions.as_ref())
            .ok()
    })
}
