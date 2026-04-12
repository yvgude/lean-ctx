use std::path::{Path, PathBuf};

fn backup_path_for(path: &Path) -> Option<PathBuf> {
    let filename = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!("{filename}.lean-ctx.bak")))
}

pub fn write_atomic_with_backup(path: &Path, content: &str) -> Result<(), String> {
    if path.exists() {
        if let Some(bak) = backup_path_for(path) {
            // Best-effort backup; if it fails we still attempt the write.
            let _ = std::fs::copy(path, &bak);
        }
    }

    write_atomic(path, content)
}

pub fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let parent = path
        .parent()
        .ok_or_else(|| "invalid path (no parent directory)".to_string())?;
    let filename = path
        .file_name()
        .ok_or_else(|| "invalid path (no filename)".to_string())?
        .to_string_lossy();

    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let tmp = parent.join(format!(".{filename}.lean-ctx.tmp.{pid}.{nanos}"));
    std::fs::write(&tmp, content).map_err(|e| e.to_string())?;

    // On Windows, rename fails if destination exists.
    #[cfg(windows)]
    {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        format!(
            "atomic write failed: {} (tmp: {})",
            e,
            tmp.to_string_lossy()
        )
    })?;

    Ok(())
}
