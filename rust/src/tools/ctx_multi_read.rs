use crate::core::cache::{ReuseOutcome, SessionCache};
use crate::core::heatmap;
use crate::core::ocla::cache_types::{CacheKeyBuilder, FileReadKey};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;
use crate::tools::ctx_read;

pub fn handle(cache: &mut SessionCache, paths: &[String], mode: &str, crp_mode: CrpMode) -> String {
    handle_with_task(cache, paths, mode, crp_mode, None)
}

pub fn handle_with_task(
    cache: &mut SessionCache,
    paths: &[String],
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_task_fresh(cache, paths, mode, false, crp_mode, task)
}

const DEFAULT_MAX_MULTI_READ_BYTES: usize = 512 * 1024;

fn max_multi_read_bytes() -> usize {
    std::env::var("LCTX_MAX_MULTI_READ_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_MULTI_READ_BYTES)
}

pub fn handle_with_task_fresh(
    cache: &mut SessionCache,
    paths: &[String],
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_task_fresh_result(cache, paths, mode, fresh, crp_mode, task).text
}

/// Batch-read result with the aggregate baseline across local and cross-agent hits.
pub struct MultiReadResult {
    pub text: String,
    pub original_tokens: usize,
}

pub fn handle_with_task_fresh_result(
    cache: &mut SessionCache,
    paths: &[String],
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> MultiReadResult {
    let n = paths.len();
    if n == 0 {
        return MultiReadResult {
            text: "Read 0 files | 0 tokens saved".to_string(),
            original_tokens: 0,
        };
    }

    let max_bytes = max_multi_read_bytes();
    let mut sections: Vec<String> = Vec::with_capacity(n);
    let mut total_saved: usize = 0;
    let mut total_original: usize = 0;
    let mut accumulated_bytes: usize = 0;
    let mut files_read = 0usize;
    let mut truncated = false;

    for path in paths {
        let effective_mode = if ctx_read::is_instruction_file(path) {
            "full"
        } else {
            mode
        };
        let cache_key = file_read_cache_key(path, effective_mode, crp_mode, task);
        let cross_agent = (!fresh)
            .then(|| {
                crate::core::ocla::cache_delivery::check(
                    &cache_key.cache_key(),
                    &cache_key.validator(),
                    "ctx_multi_read",
                )
            })
            .flatten();
        let (chunk, cross_agent_original, reuse_outcome) = if let Some(entry) = cross_agent {
            (
                crate::core::ocla::cache_delivery::stub(&entry, "file read"),
                Some(entry.token_count as usize),
                ReuseOutcome::CrossFileRef,
            )
        } else {
            let read = if fresh {
                ctx_read::handle_fresh_with_task_result(cache, path, effective_mode, crp_mode, task)
            } else {
                ctx_read::handle_with_task_result(cache, path, effective_mode, crp_mode, task)
            };
            let reuse_outcome = if fresh {
                ReuseOutcome::FreshBypass
            } else if read.is_cache_hit {
                if matches!(read.resolved_mode.as_str(), "full" | "full-compact") {
                    ReuseOutcome::UnchangedStub
                } else {
                    ReuseOutcome::RenderCacheHit
                }
            } else {
                ReuseOutcome::Cold
            };
            let chunk = read.content;
            if !chunk.contains("[cross-agent") {
                crate::core::ocla::cache_delivery::record(
                    cache_key.cache_key(),
                    crate::core::ocla::cache_types::DeliveryKind::FileRead,
                    cache_key.validator(),
                    Some(cache_key.path.clone()),
                    &chunk,
                    "ctx_multi_read",
                );
            }
            (chunk, None, reuse_outcome)
        };
        crate::core::cache::record_ctx_read_outcome(reuse_outcome);
        let original = cross_agent_original
            .or_else(|| cache.get(path).map(|entry| entry.original_tokens))
            .unwrap_or(0);
        let sent = count_tokens(&chunk);
        heatmap::record_file_access(path, original, original.saturating_sub(sent));
        // Verified ledger (#685): model-correct counts. The default O200kBase model
        // reuses the o200k counts above (same BPE + cache key → zero extra work); a
        // resolved Claude/Gemini/Llama model re-tokenizes the raw source (from the
        // cache) and the sent chunk so savings match the provider's billing units.
        {
            use crate::core::savings_ledger as ledger;
            let (lbase, lsaved) =
                if ledger::ledger_family() == crate::core::tokens::TokenizerFamily::O200kBase {
                    (original, original.saturating_sub(sent))
                } else if let Some(raw) = cache
                    .get(path)
                    .and_then(crate::core::cache::CacheEntry::content)
                {
                    let lo = ledger::count_for_ledger(&raw);
                    (lo, lo.saturating_sub(ledger::count_for_ledger(&chunk)))
                } else {
                    (original, original.saturating_sub(sent))
                };
            ledger::record_read_event(lbase, lsaved, None, None);
        }
        total_original = total_original.saturating_add(original);
        total_saved = total_saved.saturating_add(original.saturating_sub(sent));

        let chunk_bytes = chunk.len();
        if accumulated_bytes > 0 && accumulated_bytes + chunk_bytes > max_bytes {
            truncated = true;
            break;
        }
        accumulated_bytes += chunk_bytes;
        sections.push(chunk);
        files_read += 1;
    }

    let body = sections.join("\n---\n");
    let summary = if truncated {
        let skipped = n - files_read;
        format!(
            "Read {files_read}/{n} files | {total_saved} tokens saved\n\
             ⚠ Output capped at {max_bytes} bytes (LCTX_MAX_MULTI_READ_BYTES). \
             {skipped} file(s) skipped. Use individual ctx_read calls for remaining files."
        )
    } else if total_saved > 0 {
        format!("Read {n} files | {total_saved} tokens saved")
    } else {
        format!("Read {n} files")
    };
    MultiReadResult {
        text: format!("{body}\n---\n{summary}"),
        original_tokens: total_original,
    }
}

fn file_read_cache_key(
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> FileReadKey {
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(path));
    let mtime_ns = std::fs::metadata(&canonical)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    FileReadKey {
        path: canonical.to_string_lossy().into_owned(),
        mtime_ns,
        mode: mode.into(),
        crp_mode: format!("{crp_mode:?}").to_ascii_lowercase(),
        task_digest: blake3::hash(task.unwrap_or_default().as_bytes())
            .to_hex()
            .to_string(),
        policy_rev: env!("CARGO_PKG_VERSION").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_read_deduplicates_each_file_with_cross_agent_references() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.rs");
        let second = directory.path().join("second.rs");
        std::fs::write(&first, "fn first_probe() {}\n").unwrap();
        std::fs::write(&second, "fn second_probe() {}\n").unwrap();
        let paths = vec![
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ];

        let mut local = SessionCache::new();
        let initial =
            handle_with_task_fresh_result(&mut local, &paths, "full", false, CrpMode::Off, None);
        assert!(initial.text.contains("first_probe"));

        let mut another_agent = SessionCache::new();
        let repeated = handle_with_task_fresh_result(
            &mut another_agent,
            &paths,
            "full",
            false,
            CrpMode::Off,
            None,
        );
        assert_eq!(repeated.text.matches("[cross-agent cache").count(), 2);
        assert!(repeated.original_tokens > 0);
    }
}
