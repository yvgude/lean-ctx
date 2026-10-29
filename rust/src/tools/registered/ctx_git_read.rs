use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::git::{clone, repo_url, run_git};
use crate::core::protocol::append_savings;
use crate::core::tokens::count_tokens;
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_int, get_str};
use crate::tool_defs::tool_def;

const DEFAULT_MAX_TOKENS: usize = 6000;
const MAX_TREE_LINES: usize = 400;
const MAX_GREP_LINES: usize = 200;
/// Tool identity recorded with the admission of remote repository content.
const TOOL: &str = "ctx_git_read";

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
            "Read remote git repos via cached shallow clone (not HTML scraping).\n\
             modes: overview (tree + README) | tree (file list) | read (file content) | grep (search).\n\
             Accepts repo URLs and GitHub/GitLab blob/tree links (ref+path auto-detected).\n\
             https-only, SSRF-guarded. Prefer over ctx_url_read for whole-repo access.",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "https repo URL (blob/tree links auto-detect ref+path)" },
                    "mode": {
                        "type": "string",
                        "enum": ["overview", "tree", "read", "grep"],
                        "description": "overview|tree|read|grep"
                    },
                    "path": { "type": "string", "description": "Path within repo" },
                    "ref": { "type": "string", "description": "Branch/tag/commit (overrides URL ref)" },
                    "query": { "type": "string", "description": "Search term for grep mode" },
                    "max_tokens": { "type": "integer", "description": "Token budget (default: 6000)" },
                    "timeout_secs": { "type": "integer", "description": "Timeout seconds (default: 90, max: 300)" }
                },
                "allOf": [
                    {
                        "if": { "properties": { "mode": { "const": "grep" } }, "required": ["mode"] },
                        "then": { "required": ["mode", "query"] }
                    }
                ],
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
                    shell_outcome: None,
                    content_blocks: None,
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

    let mut body = format!(
        "# {} ({} files)\n\nRef: {}\n\n## Top-level\n{}\n",
        repo.project_path(),
        total,
        repo.git_ref.as_deref().unwrap_or("default (HEAD)"),
        top
    );
    match find_and_read_readme(dir) {
        Some((_, Ok(readme))) if !readme.is_empty() => {
            body.push_str("\n## README\n");
            body.push_str(&readme);
        }
        Some((name, Err(reason))) => {
            // A README that is withheld is named as such; the overview never
            // looks as if the repository had none.
            body.push_str("\n## README\n");
            body.push_str(&crate::tools::admitted_sources::withheld_line(
                name, &reason,
            ));
        }
        Some((_, Ok(_))) | None => {}
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
    // Admission precedes the header, the token count and the budget cut: the
    // whole file is judged, then only admitted text is rendered.
    let content = admit_clone_file(dir, rel)
        .map_err(|reason| format!("{rel}: {reason}"))?
        .text;
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
    // The scope is admitted whether or not git found anything: what the
    // result says about withheld sources must not depend on the query.
    let scope = list_files(dir, subpath)?;
    let original_tokens = count_tokens(&out.stdout);
    Ok(Rendered {
        body: admitted_grep(dir, query, &out.stdout, &scope),
        original_tokens,
        label: format!("grep '{query}'"),
    })
}

/// Files of one grep scope that are admitted before selection.
const MAX_GREP_SCOPE_FILES: usize = 2000;
/// Original bytes one grep scope may read for admission.
const MAX_GREP_SCOPE_BYTES: u64 = 64 * 1024 * 1024;

/// Rebuild `git grep --heading -n` output from sources admitted BEFORE
/// selection (G01), the way `ctx_search` treats a project.
///
/// Every file of the searched scope is admitted — independently of whether it
/// matched. git's output is then used for exactly two things: which file a
/// group belongs to and which line numbers matched. A line is rendered only
/// for a file the admission delivered, with its text taken from the admitted
/// file. A file that is withheld contributes nothing, not even its name, and
/// the note that reports withheld sources is a count over the WHOLE scope:
/// the result is the same whether a withheld file matched the query or not,
/// so a query cannot be used to probe its content. For a scope no rule
/// touches the result is byte-identical to git's own output.
fn admitted_grep(dir: &Path, query: &str, stdout: &str, scope: &[String]) -> String {
    let matched: std::collections::HashSet<&str> = stdout
        .lines()
        .filter(|line| matched_line_number(line).is_none())
        .collect();

    let mut admitted: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    let mut withheld = 0usize;
    let mut covered = 0usize;
    let mut bytes_left = MAX_GREP_SCOPE_BYTES;
    for file in scope.iter().take(MAX_GREP_SCOPE_FILES) {
        let Ok(bytes) = read_clone_text_bytes(dir, file) else {
            // Not a text source (binary, too large, unreadable): git's `-I`
            // skips it as well; it is neither searched nor withheld.
            covered += 1;
            continue;
        };
        if bytes.len() as u64 > bytes_left {
            break;
        }
        bytes_left -= bytes.len() as u64;
        covered += 1;
        match crate::tools::admitted_sources::admit_remote_file(bytes, file, TOOL) {
            Ok(source) => {
                if matched.contains(file.as_str()) {
                    admitted.insert(
                        file.as_str(),
                        source.text.lines().map(str::to_string).collect(),
                    );
                }
            }
            Err(_) => withheld += 1,
        }
    }

    let mut rendered: Vec<String> = Vec::new();
    // `None`: no heading seen yet, or the current file was not admitted.
    let mut current: Option<&Vec<String>> = None;
    for line in stdout.lines() {
        if rendered.len() >= MAX_GREP_LINES {
            break;
        }
        if let Some(number) = matched_line_number(line) {
            // A matched line the admitted text does not have is dropped:
            // git's raw copy of it is never used.
            if let Some(text) = current.and_then(|lines| lines.get(number - 1)) {
                rendered.push(format!("{number}:{text}"));
            }
            continue;
        }
        current = admitted.get(line);
        if current.is_some() {
            rendered.push(line.to_string());
        }
    }

    let mut body = if rendered.is_empty() {
        format!("No matches for '{query}'.")
    } else {
        rendered.join("\n")
    };
    if withheld > 0 {
        // Same wording as `ctx_search`.
        body.push_str(&format!("\n({withheld} sources withheld under policy)"));
    }
    if covered < scope.len() {
        body.push_str(&format!(
            "\n(search covered the first {covered} of {} files — narrow with `path`)",
            scope.len()
        ));
    }
    body
}

/// The 1-based line number of a `<n>:<text>` match line; `None` for a heading.
fn matched_line_number(line: &str) -> Option<usize> {
    let (number, _) = line.split_once(':')?;
    number.parse::<usize>().ok().filter(|number| *number > 0)
}

/// The bytes of one text file of the clone. `Err` (content-free) for a path
/// outside the clone, an unreadable or oversized file and a binary body —
/// none of which is inspectable as text.
fn read_clone_text_bytes(dir: &Path, rel: &str) -> Result<Vec<u8>, String> {
    let file = safe_join(dir, rel)?;
    let cap = crate::core::limits::max_read_bytes() as u64;
    let size = std::fs::metadata(&file)
        .map_err(|e| format!("cannot read file: {e}"))?
        .len();
    if size > cap {
        return Err(format!(
            "file too large ({size} bytes, limit {cap} bytes via LCTX_MAX_READ_BYTES)"
        ));
    }
    let bytes = std::fs::read(&file).map_err(|e| format!("cannot read file: {e}"))?;
    if crate::core::binary_detect::has_binary_extension(rel)
        || crate::core::text_decode::looks_binary(&bytes[..bytes.len().min(8192)])
    {
        return Err("source format is not inspectable as text".to_owned());
    }
    Ok(bytes)
}

/// Read one file of the clone and admit it as a source that is not a project
/// file, under its repository-relative path. `Err` is a content-free reason.
fn admit_clone_file(
    dir: &Path,
    rel: &str,
) -> Result<crate::core::context_admission::SourceAdmission, String> {
    let bytes = read_clone_text_bytes(dir, rel)?;
    crate::tools::admitted_sources::admit_remote_file(bytes, rel, TOOL)
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

/// The repository's README, admitted: its name and either the delivered text
/// or the content-free reason it is withheld for. `None`: there is none.
fn find_and_read_readme(dir: &Path) -> Option<(&'static str, Result<String, String>)> {
    for name in [
        "README.md",
        "README.MD",
        "Readme.md",
        "README",
        "README.txt",
    ] {
        if dir.join(name).is_file() {
            return Some((
                name,
                admit_clone_file(dir, name).map(|admitted| admitted.text),
            ));
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
    crate::core::recovery::with_original_pointer(
        "ctx_git_read",
        "budget",
        content,
        format!("{truncated}\n\n…[truncated to ~{max_tokens} tokens]"),
    )
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

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn clone_file(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn repo() -> repo_url::RepoRef {
        repo_url::parse("https://github.com/acme/widgets").expect("repo url")
    }

    fn blocking_policy() -> crate::core::policy::ResolvedPolicy {
        crate::core::policy::load(
            "name = \"ctx-git-read-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap()
    }

    /// G01: a file of a remote repository is a source. The old `read` mode
    /// returned `fs::read_to_string` of the clone verbatim — a key in the
    /// remote file reached the model, and a file the project policy blocks
    /// was delivered. A clean file keeps its bytes.
    #[test]
    fn a_remote_file_is_admitted_before_it_is_rendered() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let clone = tempfile::tempdir().unwrap();
        let clean = "pub fn widget() -> u8 {\n    7\n}\n";
        clone_file(clone.path(), "src/lib.rs", clean);
        clone_file(
            clone.path(),
            "deploy/settings.py",
            &format!("AWS_ACCESS_KEY_ID = \"{AWS_KEY}\"\nREGION = \"eu-central-2\"\n"),
        );
        clone_file(
            clone.path(),
            "docs/plan.md",
            "CONFIDENTIAL\nroadmap-marker for the launch\n",
        );
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let rendered = render_read(clone.path(), &repo(), "src/lib.rs", 6000).unwrap();
            assert_eq!(rendered.body, format!("// src/lib.rs @ HEAD\n{clean}"));

            let rendered = render_read(clone.path(), &repo(), "deploy/settings.py", 6000).unwrap();
            assert!(!rendered.body.contains(AWS_KEY), "{}", rendered.body);
            assert!(rendered.body.contains("eu-central-2"), "{}", rendered.body);
        }
        let _policy =
            crate::core::policy::runtime::TestPolicyOverride::set(Some(blocking_policy()));
        let Err(reason) = render_read(clone.path(), &repo(), "docs/plan.md", 6000) else {
            panic!("a blocked remote file must not be rendered");
        };
        assert!(reason.contains("withheld"), "{reason}");
        assert!(!reason.contains("roadmap-marker"), "{reason}");
    }

    /// The README of the overview and every line of a grep result come from
    /// admitted sources; a withheld file is named, never rendered and never
    /// silently missing. git's own raw match text is not used.
    #[test]
    fn overview_readme_and_grep_lines_come_from_admitted_sources() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let clone = tempfile::tempdir().unwrap();
        clone_file(
            clone.path(),
            "notes.txt",
            "alpha widget\nbeta\ngamma widget\n",
        );
        clone_file(
            clone.path(),
            "deploy/settings.py",
            &format!("# widget deploy\nAWS_ACCESS_KEY_ID = \"{AWS_KEY}\" # widget\n"),
        );
        clone_file(
            clone.path(),
            "docs/plan.md",
            "CONFIDENTIAL\nwidget roadmap-marker\n",
        );
        clone_file(clone.path(), "README.md", "CONFIDENTIAL\nreadme-marker\n");
        // What `git grep --heading -n -i -e widget` prints for this clone.
        let git_output = format!(
            "deploy/settings.py\n1:# widget deploy\n2:AWS_ACCESS_KEY_ID = \"{AWS_KEY}\" # widget\n\
             docs/plan.md\n2:widget roadmap-marker\n\
             notes.txt\n1:alpha widget\n3:gamma widget\n"
        );
        // What `git ls-files` lists for this clone (the grep scope).
        let scope: Vec<String> = [
            "README.md",
            "deploy/settings.py",
            "docs/plan.md",
            "notes.txt",
        ]
        .map(String::from)
        .to_vec();
        {
            // Control: for a scope no rule touches the result is git's output.
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let clean_output = "notes.txt\n1:alpha widget\n3:gamma widget\n";
            assert_eq!(
                admitted_grep(clone.path(), "widget", clean_output, &scope),
                clean_output.trim_end()
            );
            let body = admitted_grep(clone.path(), "widget", &git_output, &scope);
            assert!(!body.contains(AWS_KEY), "{body}");
            assert!(body.contains("1:# widget deploy"), "{body}");
        }
        let _policy =
            crate::core::policy::runtime::TestPolicyOverride::set(Some(blocking_policy()));
        let body = admitted_grep(clone.path(), "widget", &git_output, &scope);
        assert!(!body.contains("roadmap-marker"), "{body}");
        assert!(
            !body.contains("docs/plan.md"),
            "a withheld file is not named: {body}"
        );
        assert!(
            body.contains("notes.txt\n1:alpha widget\n3:gamma widget"),
            "{body}"
        );
        // README.md and docs/plan.md are blocked by the pack. deploy/settings.py
        // is NOT withheld: a key inside otherwise admissible content is masked
        // and the rest delivered, with a pack exactly as without one. The
        // pack's built-in pass and the gateway spell the mask the same way, so
        // their two orders agree (`compose_active_gateway_output`).
        let keyed = admit_clone_file(clone.path(), "deploy/settings.py")
            .expect("a source with a masked key is delivered under a pack");
        assert_eq!(
            keyed.text,
            "# widget deploy\nAWS_ACCESS_KEY_ID = \"[REDACTED:AWS key]\" # widget\n"
        );
        assert!(
            body.contains(
                "deploy/settings.py\n1:# widget deploy\n\
                 2:AWS_ACCESS_KEY_ID = \"[REDACTED:AWS key]\" # widget"
            ),
            "{body}"
        );
        assert!(!body.contains(AWS_KEY), "{body}");
        // Counted over the scope.
        assert!(
            body.ends_with("\n(2 sources withheld under policy)"),
            "{body}"
        );

        // No oracle: a query that matches ONLY a withheld file and a query
        // that matches nothing produce the same result.
        let only_withheld = admitted_grep(
            clone.path(),
            "q",
            "docs/plan.md\n2:widget roadmap-marker\n",
            &scope,
        );
        let nothing = admitted_grep(clone.path(), "q", "", &scope);
        assert_eq!(only_withheld, nothing);
        assert_eq!(
            nothing,
            "No matches for 'q'.\n(2 sources withheld under policy)"
        );

        let (name, readme) = find_and_read_readme(clone.path()).expect("README present");
        assert_eq!(name, "README.md");
        let reason = readme.expect_err("a blocked README is withheld");
        assert!(!reason.contains("readme-marker"), "{reason}");
    }

    #[test]
    fn budget_truncates_oversized_content() {
        let big = "word ".repeat(4000);
        let out = budget(&big, 50);
        assert!(out.contains("[truncated"));
        assert!(count_tokens(&out) < count_tokens(&big));
    }
}
