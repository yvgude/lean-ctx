use crate::core::cache::SessionCache;
use crate::core::heatmap;
use crate::core::tokens::count_tokens;
use crate::tools::ctx_read;
use crate::tools::CrpMode;

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
    let n = paths.len();
    if n == 0 {
        return "Read 0 files | 0 tokens saved".to_string();
    }

    let mut sections: Vec<String> = Vec::with_capacity(n);
    let mut total_saved: usize = 0;
    let mut total_original: usize = 0;

    for path in paths {
        let chunk = ctx_read::handle_with_task(cache, path, mode, crp_mode, task);
        let original = cache.get(path).map_or(0, |e| e.original_tokens);
        let sent = count_tokens(&chunk);
        heatmap::record_file_access(path, original, original.saturating_sub(sent));
        total_original = total_original.saturating_add(original);
        total_saved = total_saved.saturating_add(original.saturating_sub(sent));
        sections.push(chunk);
    }

    let body = sections.join("\n---\n");
    let summary = format!("Read {n} files | {total_saved} tokens saved");
    format!("{body}\n---\n{summary}")
}
