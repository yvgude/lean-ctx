use crate::core::auto_mode_resolver::{self, AutoModeContext};
use crate::core::cache::SessionCache;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

pub fn select_mode(cache: &SessionCache, path: &str) -> String {
    select_mode_with_task(cache, path, None)
}

/// Delegates to the unified `auto_mode_resolver::resolve()`.
pub fn select_mode_with_task(cache: &SessionCache, path: &str, task: Option<&str>) -> String {
    if let Ok(meta) = std::fs::metadata(path) {
        let cap = crate::core::limits::max_read_bytes() as u64;
        if meta.len() > cap {
            return "full".to_string();
        }
    }

    // Avoid a redundant disk read: ctx_read::handle re-reads the file anyway.
    // For files already in the session cache (the common agent-loop case of
    // re-reading a file), reuse the stored token count instead of reading from
    // disk a second time just to pick a mode.
    let token_count = cache
        .get(path)
        .filter(|e| !crate::core::cache::is_cache_entry_stale(path, e.stored_mtime))
        .map(|e| e.original_tokens)
        .filter(|t| *t > 0)
        .unwrap_or_else(|| std::fs::read_to_string(path).map_or(0, |c| count_tokens(&c)));

    let ctx = AutoModeContext {
        path,
        token_count,
        task,
        cache: Some(cache),
    };
    auto_mode_resolver::resolve(&ctx).mode
}

pub fn handle(cache: &mut SessionCache, path: &str, crp_mode: CrpMode) -> String {
    crate::tools::ctx_read::handle(cache, path, "auto", crp_mode)
}

#[must_use]
pub fn is_code_ext(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cpp"
            | "cc"
            | "h"
            | "hpp"
            | "rb"
            | "cs"
            | "kt"
            | "swift"
            | "php"
            | "zig"
            | "ex"
            | "exs"
            | "scala"
            | "sc"
            | "dart"
            | "sh"
            | "bash"
            | "svelte"
            | "vue"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_detection() {
        assert!(is_code_ext("rs"));
        assert!(is_code_ext("py"));
        assert!(is_code_ext("tsx"));
        assert!(!is_code_ext("json"));
    }
}
