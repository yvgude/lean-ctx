use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::ocla::cache_types::{CacheKey, CacheKeyBuilder, DirectoryWalkKey};
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str};
use crate::tool_defs::tool_def;

pub struct CtxGlobTool;

impl McpTool for CtxGlobTool {
    fn name(&self) -> &'static str {
        "ctx_glob"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_glob",
            "Find files by glob pattern (respects .gitignore; multi-root via paths).\n\
             For file CONTENT search use ctx_search.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "e.g. **/*.ts" },
                    "path": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "max_results": { "type": "integer", "description": "default 200" },
                    "ignore_gitignore": { "type": "boolean", "description": "Requires admin role" }
                },
                "required": ["pattern"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let pattern = get_str(args, "pattern")
            .ok_or_else(|| ErrorData::invalid_params("pattern is required", None))?;
        let resolved = crate::server::multi_path::resolve_tool_paths(args, ctx)
            .map_err(|e| ErrorData::invalid_params(format!("ERROR: {e}"), None))?;
        let max = (get_int(args, "max_results").unwrap_or(200) as usize).min(500);
        let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);

        if no_gitignore
            && let Err(e) = crate::core::io_boundary::ensure_ignore_gitignore_allowed("ctx_glob")
        {
            return Ok(ToolOutput::simple(e));
        }

        let respect = !no_gitignore;
        let allow_secret_paths = crate::core::roles::active_role().io.allow_secret_paths;

        if !resolved.is_multi {
            return handle_single(
                &pattern,
                &resolved.roots[0],
                respect,
                allow_secret_paths,
                max,
            );
        }

        let _mode_guard = crate::core::savings_footer::ModeGuard::new("glob");
        let per_root_max = (max / resolved.roots.len()).max(5);
        let mut combined = String::new();
        let mut total_original: usize = 0;
        let mut total_sent: usize = 0;

        for root in &resolved.roots {
            // The dispatch layer already runs `handle()` inside `block_in_place`
            // (server/dispatch/mod.rs); the per-root walk is synchronous, so we
            // call it directly and only guard against panics — nesting another
            // `block_in_place` here would needlessly consume blocking-pool
            // threads (the lesson from the ctx_multi_read crash, #271).
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cached_or_walk(&pattern, root, respect, allow_secret_paths, per_root_max)
            }));

            let Ok((result, original)) = result else {
                combined.push_str(&format!("── {root} ──\nERROR: internal panic\n\n"));
                continue;
            };

            combined.push_str(&format!("── {root} ──\n{result}\n\n"));
            if !result.starts_with("ERROR:") {
                total_original += original;
                total_sent += crate::core::tokens::count_tokens(&result);
            }
        }

        let final_out =
            crate::core::protocol::append_savings(&combined, total_original, total_sent);
        let saved = total_original.saturating_sub(total_sent);

        Ok(ToolOutput {
            text: final_out,
            original_tokens: total_original,
            saved_tokens: saved,
            mode: None,
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}

fn handle_single(
    pattern: &str,
    path: &str,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    max_results: usize,
) -> Result<ToolOutput, ErrorData> {
    let Ok((result, original)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cached_or_walk(
            pattern,
            path,
            respect_gitignore,
            allow_secret_paths,
            max_results,
        )
    })) else {
        return Err(ErrorData::internal_error(
            format!(
                "ctx_glob panicked while processing '{path}'. This is a bug — please report it."
            ),
            None,
        ));
    };

    if result.starts_with("ERROR:") {
        return Err(ErrorData::invalid_params(result, None));
    }

    let sent = crate::core::tokens::count_tokens(&result);
    let saved = original.saturating_sub(sent);
    let final_out = crate::core::protocol::append_savings(&result, original, sent);

    Ok(ToolOutput {
        text: final_out,
        original_tokens: original,
        saved_tokens: saved,
        mode: None,
        path: Some(path.to_string()),
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    })
}

/// Builds the versioned directory-walk cache key for a glob request.
fn glob_cache_key(pattern: &str, path: &str, depth: usize) -> CacheKey {
    glob_cache_builder(pattern, path, depth, true, false).cache_key()
}

fn glob_cache_builder(
    selector: &str,
    path: &str,
    depth: usize,
    respect_gitignore: bool,
    _allow_secret_paths: bool,
) -> DirectoryWalkKey {
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(path));
    let dir_mtime_ns = directory_mtime_ns(&canonical).unwrap_or_default();
    DirectoryWalkKey {
        path: canonical.to_string_lossy().into_owned(),
        depth,
        gitignore: respect_gitignore,
        dir_mtime_ns,
        selector: selector.to_owned(),
    }
}

fn directory_mtime_ns(path: &std::path::Path) -> Option<u128> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn cached_or_walk(
    pattern: &str,
    path: &str,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    max_results: usize,
) -> (String, usize) {
    // Glob has no explicit depth limit; preserve that in the key instead of
    // accidentally sharing a limited tree result.
    let selector = format!("{pattern}\\x1fmax:{max_results}");
    let builder = glob_cache_builder(
        &selector,
        path,
        usize::MAX,
        respect_gitignore,
        allow_secret_paths,
    );
    let key = if respect_gitignore && allow_secret_paths {
        glob_cache_key(&selector, path, usize::MAX)
    } else {
        builder.cache_key()
    };
    if let Some(entry) =
        crate::core::ocla::cache_delivery::check(&key, &builder.validator(), "ctx_glob")
    {
        let stub = crate::core::ocla::cache_delivery::stub(&entry, "directory walk");
        return (stub, entry.token_count as usize);
    }

    let (result, original) = crate::tools::ctx_glob::handle(
        pattern,
        path,
        respect_gitignore,
        allow_secret_paths,
        max_results,
    );
    if !result.starts_with("ERROR:") {
        crate::core::ocla::cache_delivery::record(
            key,
            crate::core::ocla::cache_types::DeliveryKind::DirectoryWalk,
            builder.validator(),
            Some(builder.path),
            &result,
            "ctx_glob",
        );
    }
    (result, original)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-agent cache serves no stub while any policy is active, and
    /// policy tests set one process-wide under the test env lock. Tests that
    /// assert a cache hit take the same lock so they never run inside one.
    fn no_policy_window() -> crate::core::data_dir::TestEnvGuard {
        crate::core::data_dir::test_env_lock()
    }

    #[test]
    fn glob_adapter_records_then_serves_a_cross_agent_reference() {
        let _window = no_policy_window();
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("cached.rs"), "fn cached() {}\n").unwrap();
        let path = directory.path().to_string_lossy();

        let first = handle_single("*.rs", &path, true, true, 20).unwrap();
        assert!(first.text.contains("cached.rs"));
        let second = handle_single("*.rs", &path, true, true, 20).unwrap();
        assert!(
            second.text.contains("[cross-agent cache"),
            "{}",
            second.text
        );
    }

    /// GH #1443: Different patterns in the same directory must each return
    /// their own correct results — never a dedup stub from a prior pattern.
    #[test]
    fn different_patterns_same_directory_return_distinct_results() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# A").unwrap();
        std::fs::write(dir.path().join("b.md"), "# B").unwrap();
        std::fs::write(dir.path().join("a.json"), "{}").unwrap();
        std::fs::write(dir.path().join("b.json"), "[]").unwrap();
        let path = dir.path().to_string_lossy();

        let md_result = handle_single("*.md", &path, true, true, 100).unwrap();
        assert!(
            md_result.text.contains("a.md") && md_result.text.contains("b.md"),
            "*.md should find both .md files: {}",
            md_result.text
        );

        let json_result = handle_single("*.json", &path, true, true, 100).unwrap();
        assert!(
            json_result.text.contains("a.json") && json_result.text.contains("b.json"),
            "*.json must return actual filenames, not a dedup stub: {}",
            json_result.text
        );
        assert!(
            !json_result.text.contains("[cross-agent cache"),
            "*.json must NOT get a cache stub from prior *.md lookup: {}",
            json_result.text
        );
    }

    /// GH #1443: Same pattern repeated does get a cache hit (regression guard
    /// for the dedup mechanism itself).
    #[test]
    fn same_pattern_repeated_uses_cache() {
        let _window = no_policy_window();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.ts"), "export {}").unwrap();
        let path = dir.path().to_string_lossy();

        let first = handle_single("*.ts", &path, true, true, 100).unwrap();
        assert!(first.text.contains("x.ts"));

        let second = handle_single("*.ts", &path, true, true, 100).unwrap();
        assert!(
            second.text.contains("[cross-agent cache"),
            "Repeated identical pattern should use cache: {}",
            second.text
        );
    }

    /// GH #1443: A pattern with zero matches returns a truthful empty result,
    /// never a stub from a different cached pattern.
    #[test]
    fn no_match_pattern_returns_zero_not_stub() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.rs"), "fn main() {}").unwrap();
        let path = dir.path().to_string_lossy();

        let _ = handle_single("*.rs", &path, true, true, 100).unwrap();

        let py_result = handle_single("*.py", &path, true, true, 100).unwrap();
        assert!(
            py_result.text.contains("0 files matched"),
            "Non-matching pattern must report zero, not cached stub: {}",
            py_result.text
        );
    }
}
