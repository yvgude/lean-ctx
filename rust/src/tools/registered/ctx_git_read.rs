use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::core::git::{clone, repo_url, run_git};
use crate::core::protocol::append_savings;
use crate::core::tokens::count_tokens;
use crate::server::tool_trait::{get_int, get_str, McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

const DEFAULT_MAX_TOKENS: usize = 6000;
const MAX_TREE_LINES: usize = 400;
const MAX_GREP_LINES: usize = 200;

/// `ctx_git_read` — read a remote repository via a cached shallow clone instead
/// of scraping its web page. Modes: overview / tree / read / grep.
pub struct CtxGitReadTool;

impl McpTool for CtxGitReadTool {
    fn name(&self) -> &'static str {
        "ctx_git_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_git_read",
            "Read a remote git repository via a cached shallow clone (not HTML scraping).\n\
             modes: overview (tree + README) | tree (file list) | read (a file) | grep (search).\n\
             Accepts repo URLs and GitHub/GitLab blob/tree links (ref + path auto-detected). https-only, SSRF-guarded, bounded.\n\
             Use instead of ctx_url_read when you need a whole repo's files/structure.",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "https repo URL, optionally a blob/tree link carrying ref + path" },
                    "mode": {
                        "type": "string",
                        "enum": ["overview", "tree", "read", "grep"],
                        "description": "Default: read when a path is present, else overview"
                    },
                    "path": { "type": "string", "description": "File (read) or directory (tree/grep) within the repo" },
                    "ref": { "type": "string", "description": "Branch/tag/commit (overrides any ref in the URL)" },
                    "query": { "type": "string", "description": "Search term for grep mode" },
                    "max_tokens": { "type": "integer", "description": "Token budget for returned content (default: 6000)" },
                    "timeout_secs": { "type": "integer", "description": "Clone/fetch timeout (default: 90, max: 300)" }
                },
                "required": ["url"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let url = get_str(args, "url")
            .ok_or_else(|| ErrorData::invalid_params("url is required", None))?;

        let mut repo = repo_url::parse(&url).ok_or_else(|| {
            ErrorData::invalid_params(
                "not a recognized https repo URL (expected https://<host>/<owner>/<repo>[/blob|tree/<ref>/<path>])",
                None,
            )
        })?;
        if let Some(r) = get_str(args, "ref") {
            repo.git_ref = Some(r);
        }

        let path = get_str(args, "path").or_else(|| repo.subpath.clone());
        let mode = get_str(args, "mode").unwrap_or_else(|| {
            if path.is_some() {
                "read".to_string()
            } else {
                "overview".to_string()
            }
        });
        let max_tokens = get_int(args, "max_tokens")
            .map_or(DEFAULT_MAX_TOKENS, |n| n.clamp(200, 50_000) as usize);
        let timeout = Duration::from_secs(
            get_int(args, "timeout_secs").map_or(clone::DEFAULT_CLONE_TIMEOUT_SECS, |n| {
                n.clamp(5, 300) as u64
            }),
        );
        let query = get_str(args, "query");

        let result = tokio::task::block_in_place(|| {
            let dir = clone::ensure_repo(&repo, timeout)?;
            match mode.as_str() {
                "overview" => render_overview(&dir, &repo, max_tokens),
                "tree" => render_tree(&dir, path.as_deref()),
                "read" => {
                    let p = path
                        .as_deref()
                        .ok_or_else(|| "read mode requires 'path'".to_string())?;
                    render_read(&dir, &repo, p, max_tokens)
                }
                "grep" => {
                    let q = query
                        .as_deref()
                        .ok_or_else(|| "grep mode requires 'query'".to_string())?;
                    render_grep(&dir, q, path.as_deref())
                }
                other => Err(format!(
                    "invalid mode '{other}' (use: overview, tree, read, grep)"
                )),
            }
        });

        match result {
            Ok(rendered) => {
                let sent = count_tokens(&rendered.body);
                let saved = rendered.original_tokens.saturating_sub(sent);
                let text = append_savings(&rendered.body, rendered.original_tokens, sent);
                Ok(ToolOutput {
                    text,
                    original_tokens: rendered.original_tokens,
                    saved_tokens: saved,
                    mode: Some(mode),
                    path: Some(rendered.label),
                    changed: false,
                })
            }
            Err(e) => Err(ErrorData::invalid_params(
                format!("ctx_git_read failed: {e}"),
                None,
            )),
        }
    }
}

struct Rendered {
    body: String,
    original_tokens: usize,
    label: String,
}

fn render_overview(
    dir: &Path,
    repo: &repo_url::RepoRef,
    max_tokens: usize,
) -> Result<Rendered, String> {
    let files = list_files(dir, None)?;
    let total = files.len();
    let top = top_level_summary(&files);
    let readme = find_and_read_readme(dir).unwrap_or_default();

    let mut body = format!(
        "# {} ({} files)\n\nRef: {}\n\n## Top-level\n{}\n",
        repo.project_path(),
        total,
        repo.git_ref.as_deref().unwrap_or("default (HEAD)"),
        top
    );
    if !readme.is_empty() {
        body.push_str("\n## README\n");
        body.push_str(&readme);
    }
    let original_tokens = count_tokens(&body);
    Ok(Rendered {
        body: budget(&body, max_tokens),
        original_tokens,
        label: format!("{} overview", repo.project_path()),
    })
}

fn render_tree(dir: &Path, subpath: Option<&str>) -> Result<Rendered, String> {
    let files = list_files(dir, subpath)?;
    let original_tokens = count_tokens(&files.join("\n"));
    let shown: Vec<&String> = files.iter().take(MAX_TREE_LINES).collect();
    let mut body = shown
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if files.len() > shown.len() {
        body.push_str(&format!(
            "\n… {} more file(s) (narrow with `path`)",
            files.len() - shown.len()
        ));
    }
    Ok(Rendered {
        body,
        original_tokens,
        label: format!("tree {}", subpath.unwrap_or(".")),
    })
}

fn render_read(
    dir: &Path,
    repo: &repo_url::RepoRef,
    rel: &str,
    max_tokens: usize,
) -> Result<Rendered, String> {
    let file = safe_join(dir, rel)?;
    if file.is_dir() {
        // A directory was requested in read mode — show its listing instead.
        return render_tree(dir, Some(rel));
    }
    let content = std::fs::read_to_string(&file)
        .map_err(|e| format!("cannot read {rel}: {e} (is it a text file?)"))?;
    let header = format!(
        "// {} @ {}\n",
        rel,
        repo.git_ref.as_deref().unwrap_or("HEAD")
    );
    let body = format!("{header}{content}");
    let original_tokens = count_tokens(&body);
    Ok(Rendered {
        body: budget(&body, max_tokens),
        original_tokens,
        label: format!("{}:{}", repo.project_path(), rel),
    })
}

fn render_grep(dir: &Path, query: &str, subpath: Option<&str>) -> Result<Rendered, String> {
    let mut args: Vec<&str> = vec![
        "grep",
        "--no-color",
        "-n",
        "-I",
        "-i",
        "--heading",
        "-e",
        query,
    ];
    if let Some(p) = subpath {
        args.push("--");
        args.push(p);
    }
    let out = run_git(&args, dir, Duration::from_secs(30), &[])?;
    // git grep exits 1 with no matches — treat that as an empty result, not error.
    if !out.success && !out.stdout.is_empty() {
        return Err(out.stderr.trim().to_string());
    }
    if out.stdout.trim().is_empty() {
        return Ok(Rendered {
            body: format!("No matches for '{query}'."),
            original_tokens: 0,
            label: format!("grep '{query}'"),
        });
    }
    let original_tokens = count_tokens(&out.stdout);
    let body: String = out
        .stdout
        .lines()
        .take(MAX_GREP_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Rendered {
        body,
        original_tokens,
        label: format!("grep '{query}'"),
    })
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Tracked files (gitignore-aware), optionally scoped to a subpath.
fn list_files(dir: &Path, subpath: Option<&str>) -> Result<Vec<String>, String> {
    let mut args = vec!["ls-files"];
    if let Some(p) = subpath {
        args.push("--");
        args.push(p);
    }
    let out = run_git(&args, dir, Duration::from_secs(20), &[])?.ok_stdout()?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

fn top_level_summary(files: &[String]) -> String {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for f in files {
        let top = f.split('/').next().unwrap_or(f);
        let key = if top == f.as_str() {
            top.to_string() // a top-level file
        } else {
            format!("{top}/")
        };
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .take(40)
        .map(|(k, n)| {
            if k.ends_with('/') {
                format!("- {k} ({n})")
            } else {
                format!("- {k}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_and_read_readme(dir: &Path) -> Option<String> {
    for name in [
        "README.md",
        "README.MD",
        "Readme.md",
        "README",
        "README.txt",
    ] {
        let p = dir.join(name);
        if p.is_file() {
            if let Ok(s) = std::fs::read_to_string(&p) {
                return Some(s);
            }
        }
    }
    None
}

/// Join `rel` under `base`, rejecting any path that escapes `base`.
fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim_start_matches('/');
    if rel.split('/').any(|seg| seg == "..") {
        return Err("path may not contain '..'".to_string());
    }
    let joined = base.join(rel);
    let canon_base = std::fs::canonicalize(base).map_err(|e| e.to_string())?;
    match std::fs::canonicalize(&joined) {
        Ok(canon) if canon.starts_with(&canon_base) => Ok(canon),
        Ok(_) => Err("path escapes the repository".to_string()),
        Err(e) => Err(format!("path not found: {rel} ({e})")),
    }
}

fn budget(content: &str, max_tokens: usize) -> String {
    let tokens = count_tokens(content);
    if tokens <= max_tokens {
        return content.to_string();
    }
    let ratio = max_tokens as f64 / tokens as f64;
    let keep = ((content.chars().count() as f64 * ratio) as usize).max(1);
    let truncated: String = content.chars().take(keep).collect();
    format!("{truncated}\n\n…[truncated to ~{max_tokens} tokens]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_blocks_parent_escape() {
        let tmp = std::env::temp_dir();
        assert!(safe_join(&tmp, "../etc/passwd").is_err());
        assert!(safe_join(&tmp, "a/../../b").is_err());
    }

    #[test]
    fn top_level_summary_groups_dirs_and_files() {
        let files = vec![
            "README.md".to_string(),
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "tests/t.rs".to_string(),
        ];
        let s = top_level_summary(&files);
        assert!(s.contains("- src/ (2)"));
        assert!(s.contains("- tests/ (1)"));
        assert!(s.contains("- README.md"));
    }

    #[test]
    fn budget_truncates_oversized_content() {
        let big = "word ".repeat(4000);
        let out = budget(&big, 50);
        assert!(out.contains("[truncated"));
        assert!(count_tokens(&out) < count_tokens(&big));
    }
}
