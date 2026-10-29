// SPDX-License-Identifier: Apache-2.0
//! Handoff files of the A2A endpoints: published atomically, and the store
//! kept at a bounded number of files.

pub(super) fn persist_handoff_file(
    path: &std::path::Path,
    bytes: &[u8],
    delivery_id: Option<&str>,
) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("handoff destination must be a regular file".to_string());
            }
            if delivery_id.is_some() {
                return match std::fs::read(path) {
                    Ok(existing) if existing == bytes => Ok(()),
                    Ok(_) => Err("handoff destination content mismatch".to_string()),
                    Err(error) => Err(error.to_string()),
                };
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }

    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("handoff")
    ));
    if std::fs::symlink_metadata(&temporary).is_ok() {
        return Err("handoff temporary path already exists".to_string());
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    let mut file = options
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    use std::io::Write as _;
    if let Err(error) = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&temporary, path))
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or("handoff path has no parent")?)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn evict_oldest_files(dir: &std::path::Path, max_files: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
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
