use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::ocla::cache_types::{CacheKeyBuilder, DirectoryWalkKey};
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_int};
use crate::tool_defs::tool_def;

pub struct CtxTreeTool;

impl McpTool for CtxTreeTool {
    fn name(&self) -> &'static str {
        "ctx_tree"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_tree",
            "Directory tree with file counts per directory. depth=N (default 3);\n\
             show_hidden for dotfiles; paths for multi-root.\n\
             respect_gitignore filters ignored files (default true).\n\
             WORKFLOW: lightweight orientation before ctx_repomap or ctx_compose.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Dir" },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multi-root (alternative to path)"
                    },
                    "depth": { "type": "integer", "description": "Max depth" },
                    "show_hidden": { "type": "boolean", "description": "Include dotfiles" },
                    "respect_gitignore": { "type": "boolean", "description": "Filter ignored" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let resolved = crate::server::multi_path::resolve_tool_paths(args, ctx)
            .map_err(|e| ErrorData::invalid_params(format!("ERROR: {e}"), None))?;
        let depth = (get_int(args, "depth").unwrap_or(3) as usize).min(10);
        let show_hidden = get_bool(args, "show_hidden").unwrap_or(false);
        let respect_gitignore = get_bool(args, "respect_gitignore").unwrap_or(true);

        let mut combined = String::new();
        let mut total_original = 0;

        for root in &resolved.roots {
            let root_clone = root.clone();
            let Ok((body, raw_tokens)) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cached_or_walk(&root_clone, depth, show_hidden, respect_gitignore)
                }))
            else {
                combined.push_str(&format!("── {root} ──\nERROR: internal panic\n\n"));
                continue;
            };

            if body.starts_with("ERROR:") {
                combined.push_str(&format!("── {root} ──\n{body}\n\n"));
                continue;
            }

            combined.push_str(&format!("── {root} ──\n{body}\n\n"));
            total_original += raw_tokens;
        }

        let total_sent = crate::core::tokens::count_tokens(&combined);
        let final_out = append_combined_footer(&combined, total_original, total_sent);
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

fn append_combined_footer(body: &str, raw_tokens: usize, sent_tokens: usize) -> String {
    crate::core::protocol::append_savings(body, raw_tokens, sent_tokens)
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
    path: &str,
    depth: usize,
    show_hidden: bool,
    respect_gitignore: bool,
) -> (String, usize) {
    let builder = DirectoryWalkKey {
        path: crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(path))
            .to_string_lossy()
            .into_owned(),
        depth,
        gitignore: respect_gitignore,
        dir_mtime_ns: directory_mtime_ns(&crate::core::pathutil::safe_canonicalize_or_self(
            std::path::Path::new(path),
        ))
        .unwrap_or_default(),
        selector: format!("tree:depth={depth}"),
    };
    let key = builder.cache_key();
    if let Some(entry) =
        crate::core::ocla::cache_delivery::check(&key, &builder.validator(), "ctx_tree")
    {
        let stub = crate::core::ocla::cache_delivery::stub(&entry, "directory tree");
        return (stub, entry.token_count as usize);
    }
    let (result, original) =
        crate::tools::ctx_tree::handle(path, depth, show_hidden, respect_gitignore);
    if !result.starts_with("ERROR:") {
        crate::core::ocla::cache_delivery::record(
            key,
            crate::core::ocla::cache_types::DeliveryKind::DirectoryWalk,
            builder.validator(),
            Some(builder.path),
            &result,
            "ctx_tree",
        );
    }
    (result, original)
}

#[cfg(test)]
mod tests {
    use super::{append_combined_footer, cached_or_walk};

    #[test]
    fn tree_adapter_appends_one_footer_after_combining_roots() {
        let body = "── first ──\nfirst.rs\n\n── second ──\nsecond.rs\n";
        let raw_tokens = 100;
        let sent_tokens = crate::core::tokens::count_tokens(body);
        // Footer is appended by append_combined_footer via protocol::append_savings

        let output = append_combined_footer(body, raw_tokens, sent_tokens);

        // Check exactly one savings line exists
        let savings_lines = output
            .lines()
            .filter(|l| l.contains("[lean-ctx") || l.contains("savings:") || l.contains("saved"))
            .count();
        assert!(savings_lines <= 2, "too many footer lines: {savings_lines}");
    }

    #[test]
    fn tree_adapter_records_then_serves_a_cross_agent_reference() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("cached.rs"), "fn cached() {}\n").unwrap();
        let path = directory.path().to_string_lossy();

        let (first_result, _first_orig) = cached_or_walk(&path, 3, false, true);
        assert!(first_result.contains("cached.rs"));
        let (second_result, _second_orig) = cached_or_walk(&path, 3, false, true);
        assert!(second_result.contains("[cross-agent"), "{}", second_result);
    }
}
