//! Tee logging for shell output.
//!
//! Secret masking is delegated to [`crate::core::redaction`] — the single
//! source of truth shared with `ctx_read` redaction — so the regex set can
//! never drift between the two layers again (it used to be a hand-copied
//! duplicate). `save_tee` then runs the config-driven secret scanner on top
//! for defense in depth.

#[must_use]
pub fn save_tee(command: &str, output: &str) -> Option<String> {
    let tee_dir = crate::core::paths::state_dir().ok()?.join("tee");
    std::fs::create_dir_all(&tee_dir).ok()?;

    cleanup_old_tee_logs(&tee_dir);

    let cmd_slug: String = command
        .chars()
        .take(40)
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Content-addressed path (#498): the same command always maps to the same
    // file, so repeated tool outputs stay byte-identical (provider prompt
    // caches reward stable text). Re-runs overwrite — newest output wins;
    // the 24h TTL cleanup works on mtime, not the filename.
    let cmd_hash = blake3::hash(command.as_bytes()).to_hex();
    let filename = format!("{cmd_slug}_{}.log", &cmd_hash.as_str()[..8]);
    let path = tee_dir.join(&filename);

    let masked = crate::core::redaction::redact_text(output);
    let (redacted, _) = crate::core::secret_detection::scan_and_redact_from_config(&masked);
    std::fs::write(&path, redacted).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Some(path.to_string_lossy().to_string())
}

pub(crate) fn cleanup_old_tee_logs(tee_dir: &std::path::Path) {
    let cutoff = std::time::SystemTime::now().checked_sub(std::time::Duration::from_hours(24));
    let Some(cutoff) = cutoff else { return };

    if let Ok(entries) = std::fs::read_dir(tee_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
                && modified < cutoff
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Determinism contract (#498): the tee path must be content-addressed —
    /// the same command always maps to the same file so repeated tool outputs
    /// stay byte-identical for provider prompt caching.
    #[test]
    fn tee_path_is_content_addressed() {
        // Serialize against tests that repoint LEAN_CTX_DATA_DIR (isolated_data_dir);
        // without the lock the resolved tee base races and the paths diverge.
        let _lock = crate::core::data_dir::test_env_lock();
        let first = save_tee("cargo test --lib", "output run 1").expect("tee saved");
        let second = save_tee("cargo test --lib", "output run 2").expect("tee saved");
        assert_eq!(first, second, "same command must map to the same tee path");

        let other = save_tee("cargo build", "output").expect("tee saved");
        assert_ne!(first, other, "different commands get different tee paths");

        // Latest output wins on overwrite.
        let content = std::fs::read_to_string(&second).unwrap();
        assert!(content.contains("run 2"));

        for p in [first, other] {
            let _ = std::fs::remove_file(p);
        }
    }
}
