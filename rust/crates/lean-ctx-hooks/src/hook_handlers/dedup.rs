// SPDX-License-Identifier: Apache-2.0
//! PID-independent dedup for Cursor's double-fired hooks (#1032).
//!
//! Cursor spawns `preToolUse` twice — two separate processes, 2–128 ms apart,
//! with byte-identical payloads (confirmed in `debug.log`). The redirect path
//! then runs the lean-ctx subprocess twice and logs twice. A naive "temp file
//! exists" guard misses, because [`super::redirect_temp_path`] mixes the PID into
//! its hash, so the two processes target different paths.
//!
//! This module coordinates the two processes through a shared, PID-independent
//! claim/response pair keyed on the *semantic* call (event + tool + args), so the
//! second fire replays the first's response instead of repeating the work.
//!
//! Correctness first: dedup must never break a redirect. Every failure path falls
//! back to running the work, so the worst case degrades to today's duplicate
//! behaviour rather than a dropped or corrupted response. The claim/resp files
//! are pure side channels — the stdout body stays byte-identical (#498).

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::Duration;

/// How long a fresh claim suppresses a duplicate. The double-fire lands within
/// ~128 ms; a small window dedups it while letting a legitimate later re-read of
/// the same target start a new round.
const DEDUP_WINDOW: Duration = Duration::from_secs(3);

/// Upper bound the loser waits for the winner's response. Sized to the subprocess
/// timeout so a slow winner is still awaited — both processes run in parallel, so
/// waiting adds no latency over the winner doing the work alone.
const RESP_WAIT: Duration = Duration::from_secs(11);

/// Poll interval while the loser waits for the response file.
const POLL: Duration = Duration::from_millis(5);

/// Remove claim/resp files older than this on each winning round, so the dir
/// stays bounded without a background sweeper.
const CLEANUP_AGE: Duration = Duration::from_mins(1);

/// Run `work` with PID-independent dedup. The first (winning) process runs `work`
/// and caches its stdout; a concurrent second (losing) process replays the cached
/// stdout without re-running `work`. `event` + `key_material` identify the call —
/// they MUST be identical across the double-fire and MUST exclude the PID.
pub(super) fn deduped<F: FnOnce() -> String>(event: &str, key_material: &str, work: F) -> String {
    match hook_dir() {
        Some(dir) => deduped_in(&dir, event, key_material, work),
        // No usable temp dir → no caching, just do the work (never break the hook).
        None => work(),
    }
}

fn deduped_in<F: FnOnce() -> String>(
    dir: &Path,
    event: &str,
    key_material: &str,
    work: F,
) -> String {
    let key = key(event, key_material);
    let claim = dir.join(format!("{key}.claim"));
    let resp = dir.join(format!("{key}.resp"));

    match claim_round(&claim) {
        Round::Winner => {
            sweep_stale(dir);
            let out = work();
            write_atomic(&resp, &out);
            out
        }
        // Winner vanished/timed out: do the work ourselves rather than returning
        // nothing. Don't write `resp` — avoid racing the still-running winner.
        Round::Loser => await_resp(&resp, RESP_WAIT).unwrap_or_else(work),
        Round::NoCache => work(),
    }
}

enum Round {
    Winner,
    Loser,
    NoCache,
}

fn claim_round(claim: &Path) -> Round {
    match create_exclusive(claim) {
        Ok(()) => Round::Winner,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if claim_is_fresh(claim) {
                Round::Loser
            } else {
                // Stale claim (previous round or crashed winner): reclaim it so a
                // legitimate later call is never blocked by a dead marker.
                let _ = fs::remove_file(claim);
                match create_exclusive(claim) {
                    Ok(()) => Round::Winner,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Round::Loser,
                    Err(_) => Round::NoCache,
                }
            }
        }
        Err(_) => Round::NoCache,
    }
}

/// Atomic create-if-absent (`O_CREAT|O_EXCL`), the portable race-free claim.
fn create_exclusive(path: &Path) -> std::io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
}

fn claim_is_fresh(claim: &Path) -> bool {
    claim
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < DEDUP_WINDOW)
}

fn await_resp(resp: &Path, timeout: Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(s) = fs::read_to_string(resp) {
            return Some(s);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL);
    }
}

/// Write to a unique sibling then rename, so a reader never observes a half file.
fn write_atomic(resp: &Path, body: &str) {
    let tmp = resp.with_extension(format!("resp.tmp.{}", std::process::id()));
    if fs::write(&tmp, body).is_ok() && fs::rename(&tmp, resp).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

fn key(event: &str, key_material: &str) -> String {
    let hash = blake3::hash(format!("{event}\u{0}{key_material}").as_bytes());
    hash.to_hex()[..16].to_string()
}

fn hook_dir() -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("lean-ctx-hook");
    fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

/// Extensions [`sweep_stale`] may delete: our own dedup side channels
/// (`.claim`/`.resp`) and the redirect temp files (`.lctx`) the host reads back
/// right after the hook returns. The `.lctx` files previously had no sweeper and
/// leaked thousands of files (#1035).
fn is_sweepable_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "claim" || e == "resp" || e == "lctx")
}

fn sweep_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !is_sweepable_ext(&p) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > CLEANUP_AGE);
        if stale {
            let _ = fs::remove_file(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn unique_material(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{tag}-{nanos}-{:?}", std::thread::current().id())
    }

    #[test]
    fn winner_runs_once_and_loser_replays() {
        let dir = tempfile::tempdir().unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let material = unique_material("read");

        let r1 = runs.clone();
        let first = deduped_in(dir.path(), "redirect", &material, move || {
            r1.fetch_add(1, Ordering::SeqCst);
            "RESPONSE-A".to_string()
        });

        let r2 = runs.clone();
        let second = deduped_in(dir.path(), "redirect", &material, move || {
            r2.fetch_add(1, Ordering::SeqCst);
            "SHOULD-NOT-RUN".to_string()
        });

        assert_eq!(first, "RESPONSE-A");
        assert_eq!(
            second, "RESPONSE-A",
            "loser must replay the winner's stdout"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "work must run exactly once across the double-fire"
        );
    }

    #[test]
    fn distinct_keys_both_run() {
        let dir = tempfile::tempdir().unwrap();
        let runs = Arc::new(AtomicUsize::new(0));

        for tag in ["a", "b"] {
            let r = runs.clone();
            let material = unique_material(tag);
            let out = deduped_in(dir.path(), "redirect", &material, move || {
                r.fetch_add(1, Ordering::SeqCst);
                format!("out-{tag}")
            });
            assert_eq!(out, format!("out-{tag}"));
        }

        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "different calls must not dedup each other"
        );
    }

    #[test]
    fn winner_persists_response_for_loser() {
        let dir = tempfile::tempdir().unwrap();
        let material = unique_material("resp");
        let out = deduped_in(dir.path(), "redirect", &material, || "CACHED".to_string());

        let resp = dir
            .path()
            .join(format!("{}.resp", key("redirect", &material)));
        assert_eq!(out, "CACHED");
        assert_eq!(
            fs::read_to_string(&resp).unwrap(),
            "CACHED",
            "winner must cache its stdout for the loser to replay"
        );
    }

    #[test]
    fn missing_dir_falls_back_to_work() {
        // A non-existent, non-creatable dir must never break the hook.
        let bogus = Path::new("/proc/nonexistent-lean-ctx/does/not/exist");
        let out = deduped_in(bogus, "redirect", "x", || "FALLBACK".to_string());
        assert_eq!(out, "FALLBACK");
    }

    #[test]
    fn sweeps_redirect_and_dedup_extensions_only() {
        // #1035: the redirect temp files (`.lctx`) must be sweepable so they stop
        // leaking, alongside our own `.claim`/`.resp` side channels — but unrelated
        // files must never be touched.
        assert!(is_sweepable_ext(Path::new("abc.lctx")));
        assert!(is_sweepable_ext(Path::new("abc.claim")));
        assert!(is_sweepable_ext(Path::new("abc.resp")));
        assert!(!is_sweepable_ext(Path::new("abc.txt")));
        assert!(!is_sweepable_ext(Path::new("noext")));
    }

    #[test]
    fn sweep_keeps_fresh_lctx() {
        // A just-written redirect temp file is younger than CLEANUP_AGE, so the
        // sweep must keep it (the host still needs to read it back).
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh.lctx");
        fs::write(&fresh, "x").unwrap();
        sweep_stale(dir.path());
        assert!(fresh.exists(), "a fresh .lctx must survive the sweep");
    }
}
