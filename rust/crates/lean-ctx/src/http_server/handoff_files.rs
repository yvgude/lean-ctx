// SPDX-License-Identifier: Apache-2.0
//! Handoff files of the A2A endpoints: published atomically, and the store
//! kept at a bounded number of files.

/// Publish `bytes` at `root/rel` (`root` is the project root, `rel` lies in its
/// `.lean-ctx/handoffs/`). No component below the root — `.lean-ctx`, the
/// handoff directories or the file — may be a symlink, and the write follows
/// none. A retried delivery with identical content is a no-op.
pub(super) fn persist_handoff_file(
    root: &std::path::Path,
    rel: &std::path::Path,
    bytes: &[u8],
    delivery_id: Option<&str>,
) -> Result<(), String> {
    let path = lean_ctx::core::pathjail_write::rebuild_below(root, rel)?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("handoff destination must be a regular file".to_string());
            }
            if delivery_id.is_some() {
                return match std::fs::read(&path) {
                    Ok(existing) if existing == bytes => Ok(()),
                    Ok(_) => Err("handoff destination content mismatch".to_string()),
                    Err(error) => Err(error.to_string()),
                };
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    lean_ctx::core::pathjail_write::write_below(root, rel, bytes, 0o600).map(|_| ())
}

/// Keep `root/rel_dir` below `max_files`. A directory reached through a
/// symlinked component is left alone: eviction never deletes outside it.
pub(super) fn evict_oldest_files(
    root: &std::path::Path,
    rel_dir: &std::path::Path,
    max_files: usize,
) {
    let Ok(dir) = lean_ctx::core::pathjail_write::rebuild_below(root, rel_dir) else {
        return;
    };
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let meta = e.metadata().ok()?;
            if meta.is_file() {
                Some((meta.modified().unwrap_or(std::time::UNIX_EPOCH), e.path()))
            } else {
                None
            }
        })
        .collect();

    if files.len() < max_files {
        return;
    }
    files.sort_by_key(|(mtime, _)| *mtime);
    let to_remove = files.len().saturating_sub(max_files.saturating_sub(1));
    for (_, path) in files.into_iter().take(to_remove) {
        let _ = std::fs::remove_file(path);
    }
}
