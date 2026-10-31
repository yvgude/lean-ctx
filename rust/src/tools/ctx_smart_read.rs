use crate::core::auto_mode_resolver::{self, AutoModeContext};
use crate::core::cache::SessionCache;
use crate::core::tokens::count_tokens;

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
        // Counted on what the read path itself returns for this request (G01):
        // inside a request scope that is the admitted text, and a source that
        // is withheld counts as 0 — the mode is never picked from raw bytes
        // the read that follows would not deliver.
        .unwrap_or_else(|| {
            crate::tools::ctx_read::read_file_lossy(path).map_or(0, |c| count_tokens(&c))
        });

    let ctx = AutoModeContext {
        path,
        token_count,
        line_count: None,
        task,
        cache: Some(cache),
    };
    auto_mode_resolver::resolve(&ctx).mode
}

/// The view for a `ctx_read` that names no range: the resolver's choice under
/// [`auto_mode_resolver::unranged_view`] — an outline above
/// [`auto_mode_resolver::OUTLINE_MIN_LINES`] lines, the whole file below.
/// Counts come from the fresh cache entry or, like the resolver's own count,
/// from what the read path would deliver (G01).
pub fn select_unranged_mode(cache: &SessionCache, path: &str, task: Option<&str>) -> String {
    // Over the read cap the handler refuses the read; do not load it here.
    if std::fs::metadata(path)
        .is_ok_and(|meta| meta.len() > crate::core::limits::max_read_bytes() as u64)
    {
        return "full".to_string();
    }
    let (token_count, line_count) = cache
        .get(path)
        .filter(|e| !crate::core::cache::is_cache_entry_stale(path, e.stored_mtime))
        .filter(|e| e.original_tokens > 0)
        .map(|e| (e.original_tokens, e.line_count))
        .unwrap_or_else(|| {
            crate::tools::ctx_read::read_file_lossy(path)
                .map_or((0, 0), |c| (count_tokens(&c), c.lines().count()))
        });
    let ctx = AutoModeContext {
        path,
        token_count,
        line_count: Some(line_count),
        task,
        cache: Some(cache),
    };
    auto_mode_resolver::unranged_view(auto_mode_resolver::resolve(&ctx), path, line_count).mode
}

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

    /// G01: the mode was picked from the token count of the raw file
    /// (`fs::read_to_string`), also for a source the request may not read.
    /// Inside a request the count now comes from the read path: a withheld
    /// source counts as nothing, exactly like a file that does not exist.
    #[test]
    fn a_withheld_source_does_not_steer_the_mode() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = crate::core::policy::load(
            "name = \"ctx-smart-read-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let body = "pub fn generated_handler(input: &str) -> usize { input.len() }\n".repeat(4000);
        let blocked = project.path().join("blocked.rs");
        let open = project.path().join("open.rs");
        std::fs::write(&blocked, format!("CONFIDENTIAL\n{body}")).unwrap();
        std::fs::write(&open, &body).unwrap();
        let missing = project.path().join("missing.rs");
        let cache = SessionCache::new();
        let mode = |path: &std::path::Path| {
            crate::tools::admitted_sources::as_agent_acquisition(&root, || {
                select_mode(&cache, &path.to_string_lossy())
            })
        };
        assert_eq!(mode(&blocked), mode(&missing));
        // Control: a readable source of the same size is still measured.
        let counted = crate::tools::admitted_sources::as_agent_acquisition(&root, || {
            crate::tools::ctx_read::read_file_lossy(&open.to_string_lossy())
                .map_or(0, |text| count_tokens(&text))
        });
        assert!(counted > 10_000, "{counted}");
    }
}
