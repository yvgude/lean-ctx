// SPDX-License-Identifier: Apache-2.0
//! Cache-lock contention handling for `ctx_read`: the lock deadline, the
//! prepare-phase outcome, and the uncached fallback a read degrades to when
//! the global cache write lock stays contended past its deadline.

use crate::core::cache::ReuseOutcome;
use crate::tools::ctx_read::ReadTuning;

/// How long a read waits for the global cache write lock before it degrades
/// to an uncached read. Unit tests cap it so the degradation path runs
/// without a multi-second stall.
pub(super) fn cache_lock_deadline(secs: u64) -> std::time::Duration {
    let deadline = std::time::Duration::from_secs(secs);
    if cfg!(test) {
        deadline.min(std::time::Duration::from_secs(2))
    } else {
        deadline
    }
}

/// Result of the brief prepare phase under the cache write lock.
#[allow(clippy::large_enum_variant)]
pub(super) enum PrepareOutcome {
    Hit(
        String,
        String,
        usize,
        bool,
        Option<String>,
        (u64, u64),
        ReuseOutcome,
    ),
    Compute {
        file_ref: String,
        resolved_mode: String,
        content: String,
        original_tokens: usize,
        reuse_outcome: ReuseOutcome,
        /// False when the prepare lock was never acquired: the result
        /// carries the `F?` placeholder ref and must not be written into the
        /// render cache.
        cacheable: bool,
    },
}

/// The answer for a file without content. An empty file is a valid read, not
/// an error: agents read freshly created or truncated files and must not be
/// told to retry them.
pub(super) fn empty_file_notice(path: &str) -> String {
    format!("{path}: empty file (0 bytes)")
}

/// The ready result for an empty file, delivered like a cache hit.
fn empty_file_outcome(path: &str) -> PrepareOutcome {
    PrepareOutcome::Hit(
        empty_file_notice(path),
        "full".to_string(),
        0,
        false,
        None,
        (0, 0),
        ReuseOutcome::Cold,
    )
}

/// Builds the compute input without touching the cache. `Err` carries the
/// user-facing message for an unreadable file.
pub(super) fn prepare_uncached(
    preread: Option<String>,
    preread_tokens: Option<usize>,
    path: &str,
    mode_eff: &str,
    tuning: &ReadTuning,
    task_ref: Option<&str>,
) -> Result<PrepareOutcome, String> {
    let (raw, counted) = match preread {
        Some(c) if !c.is_empty() => (c, preread_tokens),
        _ => match crate::tools::ctx_read::read_file_lossy(path) {
            Ok(c) if !c.is_empty() => (c, None),
            Ok(_) => return Ok(empty_file_outcome(path)),
            Err(e) => return Err(format!("Cannot read file: {path}: {e}")),
        },
    };
    let original_tokens = counted.unwrap_or_else(|| crate::core::tokens::count_tokens(&raw));
    // `diff` needs the cached baseline, which is exactly what is out of reach
    // here.
    let resolved_mode = match mode_eff {
        "auto" => tuning.auto_density_mode().unwrap_or_else(|| {
            crate::tools::ctx_read::resolve_auto_mode(
                None,
                path,
                original_tokens,
                Some(raw.lines().count()),
                task_ref,
            )
        }),
        "diff" => "full".to_string(),
        _ => mode_eff.to_string(),
    };
    Ok(PrepareOutcome::Compute {
        file_ref: "F?".to_string(),
        resolved_mode,
        content: raw,
        original_tokens,
        reuse_outcome: ReuseOutcome::Cold,
        cacheable: false,
    })
}
