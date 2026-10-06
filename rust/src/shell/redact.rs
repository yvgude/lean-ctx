//! Tee logging for shell output.
//!
//! Secret masking is delegated to [`crate::core::redaction`] — the single
//! source of truth shared with `ctx_read` redaction — so the regex set can
//! never drift between the two layers again (it used to be a hand-copied
//! duplicate). `save_tee` then runs the config-driven secret scanner on top
//! for defense in depth.

pub fn save_tee(command: &str, output: &str) -> Option<String> {
    // Shell output has no complete source authority; do not create a protected
    // recovery copy or advertise a handle that cannot be reauthorized.
    if crate::core::policy::runtime::is_active() {
        return None;
    }
    // Inspect both content and filename inputs before any recovery copy exists.
    let command = crate::core::policy::content::protect_active(command).ok()?;
    let output = crate::core::policy::content::protect_active(output).ok()?;
    let command = command.as_ref();
    let output = output.as_ref();
    let tee_dir = crate::core::paths::state_dir().ok()?.join("tee");
    std::fs::create_dir_all(&tee_dir).ok()?;

    // #950: cleanup is an O(N) read_dir + per-file metadata() scan of the
    // whole tee directory. Running it on every save_tee (i.e. every
    // compressed shell call) means its cost scales with directory size on
    // every single invocation under heavy shell activity. Entries already
    // carry a 24h TTL, so throttling the scan to once per interval is enough
    // to keep the directory bounded without paying the O(N) cost every time.
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        && TEE_CLEANUP_THROTTLE.is_due(now.as_secs())
    {
        cleanup_old_tee_logs(&tee_dir);
    }

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

    crate::proxy::ccr::write_tee(&path, output)?;
    let handle = path.to_string_lossy().to_string();
    crate::core::relevance_tracker::register_compressed(
        handle.clone(),
        output,
        "ctx_shell",
        crate::core::tokens::count_tokens(output),
        0,
    );
    Some(handle)
}

/// Lock-free gate that lets [`cleanup_old_tee_logs`]'s directory scan run at
/// most once per `interval_secs`, no matter how many `save_tee` calls land
/// concurrently. Only the caller whose compare-exchange wins gets `true`.
struct CleanupThrottle {
    last_run_unix_secs: std::sync::atomic::AtomicU64,
    interval_secs: u64,
}

impl CleanupThrottle {
    const fn new(interval_secs: u64) -> Self {
        Self {
            last_run_unix_secs: std::sync::atomic::AtomicU64::new(0),
            interval_secs,
        }
    }

    /// `0` means "never run" and is always due, regardless of `now_unix_secs`
    /// — avoids the throttle depending on `now` being a large real epoch time.
    fn is_due(&self, now_unix_secs: u64) -> bool {
        use std::sync::atomic::Ordering;
        let last = self.last_run_unix_secs.load(Ordering::Relaxed);
        let due = last == 0 || now_unix_secs.saturating_sub(last) >= self.interval_secs;
        due && self
            .last_run_unix_secs
            .compare_exchange(last, now_unix_secs, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }
}

static TEE_CLEANUP_THROTTLE: CleanupThrottle = CleanupThrottle::new(10 * 60);

/// Removes tee log entries older than 24h. Throttled by [`TEE_CLEANUP_THROTTLE`]
/// (called from `save_tee`) rather than run on every call — the read_dir +
/// per-file metadata() scan is O(N) in directory size.
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

    // --- #950: tee cleanup throttle ---

    #[test]
    fn cleanup_throttle_gates_by_interval() {
        let throttle = CleanupThrottle::new(600);
        assert!(
            throttle.is_due(1_000),
            "never run before: first call is always due"
        );
        assert!(
            !throttle.is_due(1_100),
            "only 100s elapsed of a 600s interval"
        );
        assert!(!throttle.is_due(1_599), "still short of the interval by 1s");
        assert!(
            throttle.is_due(1_600),
            "exactly interval-elapsed must be due again"
        );
    }

    #[test]
    fn cleanup_throttle_lets_only_one_racer_through_per_interval() {
        let throttle = CleanupThrottle::new(600);
        let hits = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| scope.spawn(|| throttle.is_due(1_000)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|&due| due)
                .count()
        });
        assert_eq!(hits, 1, "exactly one racer should win the cleanup slot");
    }

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
