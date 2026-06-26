use crate::core::cache::SessionCache;
use crate::core::heatmap;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;
use crate::tools::ctx_read;
use crate::tools::ctx_read::ReadMode;

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
    _fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    let n = paths.len();
    if n == 0 {
        return "Read 0 files | 0 tokens saved".to_string();
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
        let read_mode = match effective_mode {
            "signatures" => ReadMode::Signatures,
            "map" => ReadMode::Map,
            "diff" => ReadMode::Diff,
            _ => ReadMode::Full(None),
        };
        let result = ctx_read::read(path, &read_mode, crp_mode, task);
        let (chunk, original, sent) = match result {
            Ok(output) => {
                let o = output.original_tokens;
                let s = count_tokens(&output.content);
                heatmap::record_file_access(path, o, o.saturating_sub(s));
                {
                    use crate::core::savings_ledger as ledger;
                    let (lbase, lsaved) = if ledger::ledger_family()
                        == crate::core::tokens::TokenizerFamily::O200kBase
                    {
                        (o, o.saturating_sub(s))
                    } else if let Some(raw) = cache
                        .get(path)
                        .and_then(crate::core::cache::CacheEntry::content)
                    {
                        let lo = ledger::count_for_ledger(&raw);
                        (
                            lo,
                            lo.saturating_sub(ledger::count_for_ledger(&output.content)),
                        )
                    } else {
                        (o, o.saturating_sub(s))
                    };
                    ledger::record_read_event(lbase, lsaved);
                }
                (output.content, o, s)
            }
            Err(e) => {
                let err_msg = format!("ERROR: {e}");
                let s = count_tokens(&err_msg);
                (err_msg, 0, s)
            }
        };
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
    } else {
        format!("Read {n} files | {total_saved} tokens saved")
    };
    format!("{body}\n---\n{summary}")
}
