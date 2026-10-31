// SPDX-License-Identifier: Apache-2.0
//! Emspace provider — the pages, tasks and search of the space a project is
//! linked to, read through the local `emspace` CLI.
//!
//! A project is linked by a `.emspace` file at or above its root; the CLI
//! resolves that link itself, so the provider only runs it from the project
//! root. Every call is read-only: page reads go through `emspace run --query`,
//! which refuses a write, and the other actions are list/search commands.
//!
//! Pages are indexed as wiki pages whose references are the repository paths
//! their `derived` sources pin, so `ctx_read` of a documented file gets a
//! `documents` hint naming the page. Only sources under a registered emspace
//! dir that belongs to this repository (same checkout or same git remote)
//! become references; a dir of another repository would name the wrong file.
//!
//! The CLI is optional: without the binary or the link the provider reports
//! itself unavailable and never runs anything.

use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::provider_trait::{CapabilityLevel, ConnectorDescriptor};
use super::{ContextProvider, ProviderItem, ProviderParams, ProviderResult};

const DEFAULT_PAGE_LIMIT: usize = 1000;
const MAX_PAGE_LIMIT: usize = 5000;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 200;
/// Indexed body prefix per page in a listing; `id=<slug>` reads one page whole.
const LISTED_BODY_CHARS: usize = 6000;
/// Files one glob pin may contribute as references.
const MAX_GLOB_FILES: usize = 256;
const MAX_STDOUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 16_384;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Read-only program for `emspace run --query`: the pages, their derived
/// source locators and the registered dirs. `__PARAMS__` is replaced by a
/// JSON object, which is also a JavaScript literal.
const PAGES_PROGRAM: &str = r"const P = __PARAMS__;
const all = P.slug === null ? space.pages.list() : [space.pages.get(P.slug)].filter(Boolean);
const pages = all.filter(p => P.status === null || p.status === P.status
  || (P.status === 'stale' && p.stale) || (P.status === 'noted' && p.noted));
const listed = pages.slice(0, P.limit);
return {
  dirs: space.dirs.list().map(d => ({ name: d.name, localPath: d.localPath || null, remote: d.remote || null })),
  total: pages.length,
  pages: listed.map(p => ({
    slug: p.slug, title: p.title, type: p.type, status: p.status, stale: p.stale, noted: p.noted,
    description: p.description || null, createdAt: p.createdAt, updatedAt: p.updatedAt,
    body: P.bodyChars === null || p.body.length <= P.bodyChars ? p.body : p.body.slice(0, P.bodyChars),
    derived: space.pages.sources(p.slug).filter(s => s.role === 'derived').map(s => s.locator),
  })),
};
";

pub struct EmspaceProvider {
    project_root: PathBuf,
    binary: Option<PathBuf>,
}

impl EmspaceProvider {
    /// The provider for `project_root`; available only when the root is linked
    /// to a space and the `emspace` binary can be found.
    pub fn new(project_root: &Path) -> Self {
        let binary = find_link(project_root).and_then(|_| find_binary());
        Self {
            project_root: project_root.to_path_buf(),
            binary,
        }
    }

    fn binary(&self) -> Result<&Path, String> {
        self.binary.as_deref().ok_or_else(|| {
            "emspace not available (needs a .emspace link and the emspace CLI)".into()
        })
    }

    /// Run `emspace <command…> --json [-- <operands>]`: the flag goes before a
    /// `--`, where it would otherwise be read as an operand.
    fn run_cli(&self, args: &[&str], stdin: Option<&str>) -> Result<serde_json::Value, String> {
        let split = args.iter().position(|a| *a == "--").unwrap_or(args.len());
        let mut command = std::process::Command::new(self.binary()?);
        command
            .args(&args[..split])
            .arg("--json")
            .args(&args[split..])
            .current_dir(&self.project_root)
            // A bound session would stamp nothing (every call reads), but the
            // provider must not borrow the host agent's session either.
            .env_remove("EMSPACE_SESSION");
        match stdin {
            Some(program) => {
                let mut file = tempfile::tempfile()
                    .map_err(|e| format!("emspace program buffer failed: {e}"))?;
                file.write_all(program.as_bytes())
                    .and_then(|()| file.rewind())
                    .map_err(|e| format!("emspace program buffer failed: {e}"))?;
                command.stdin(std::process::Stdio::from(file));
            }
            // Never inherit the server's stdin: under MCP it is the protocol pipe.
            None => {
                command.stdin(std::process::Stdio::null());
            }
        }
        let captured = crate::core::process_capture::run_with_output_limits(
            &mut command,
            Some(CALL_TIMEOUT),
            MAX_STDOUT_BYTES,
            MAX_STDERR_BYTES,
        )
        .map_err(|_| "emspace command failed or exceeded its output bound".to_owned())?;
        if captured.timed_out {
            return Err("emspace command exceeded its deadline".into());
        }
        if captured.cancelled || !captured.output.status.success() {
            let stderr = String::from_utf8_lossy(&captured.output.stderr);
            let reason = stderr.lines().next().unwrap_or("no diagnostics").trim();
            return Err(format!(
                "emspace {} failed: {}",
                args.first().copied().unwrap_or(""),
                crate::core::redaction::redact_text_if_enabled(reason)
            ));
        }
        serde_json::from_slice(&captured.output.stdout)
            .map_err(|e| format!("emspace returned invalid JSON: {e}"))
    }

    fn pages(&self, params: &ProviderParams) -> Result<ProviderResult, String> {
        let slug = params.id.as_deref().filter(|s| !s.is_empty());
        let status = params.state.as_deref().filter(|s| !s.is_empty());
        if let Some(status) = status
            && !matches!(status, "draft" | "canonical" | "stale" | "noted")
        {
            return Err(format!(
                "unknown page state '{status}' (draft, canonical, stale or noted)"
            ));
        }
        let limit = params
            .limit
            .unwrap_or(DEFAULT_PAGE_LIMIT)
            .clamp(1, MAX_PAGE_LIMIT);
        let program_params = serde_json::json!({
            "slug": slug,
            "status": status,
            "limit": limit,
            "bodyChars": if slug.is_some() { None } else { Some(LISTED_BODY_CHARS) },
        });
        let program = PAGES_PROGRAM.replacen("__PARAMS__", &program_params.to_string(), 1);
        let answer = self.run_cli(&["run", "--query"], Some(&program))?;
        let listing: PagesAnswer = serde_json::from_value(
            answer
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| format!("unexpected emspace page listing: {e}"))?;
        let repo_dirs = repository_dirs(&listing.dirs, &self.project_root);
        let items: Vec<ProviderItem> = listing
            .pages
            .into_iter()
            .map(|page| page_item(page, &repo_dirs, &self.project_root))
            .collect();
        Ok(ProviderResult {
            provider: "emspace".into(),
            resource_type: "pages".into(),
            truncated: listing.total > items.len(),
            total_count: Some(listing.total),
            items,
        })
    }

    fn search(&self, params: &ProviderParams) -> Result<ProviderResult, String> {
        let query = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or("search needs a query")?;
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let limit_arg = limit.to_string();
        let answer = self.run_cli(&["search", "--limit", &limit_arg, "--", query], None)?;
        let hits: Vec<SearchHit> = serde_json::from_value(answer)
            .map_err(|e| format!("unexpected emspace search answer: {e}"))?;
        let items: Vec<ProviderItem> = hits.into_iter().map(search_item).collect();
        Ok(ProviderResult {
            provider: "emspace".into(),
            resource_type: "search".into(),
            total_count: Some(items.len()),
            truncated: items.len() >= limit,
            items,
        })
    }

    fn tasks(&self, params: &ProviderParams) -> Result<ProviderResult, String> {
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let limit_arg = limit.to_string();
        let mut args = vec!["task", "list", "--limit", limit_arg.as_str()];
        if let Some(status) = params.state.as_deref().filter(|s| !s.is_empty()) {
            if !matches!(status, "backlog" | "todo" | "doing" | "done" | "dropped") {
                return Err(format!(
                    "unknown task state '{status}' (backlog, todo, doing, done or dropped)"
                ));
            }
            args.extend(["--status", status]);
        }
        let answer = self.run_cli(&args, None)?;
        let tasks: Vec<TaskRecord> = serde_json::from_value(answer)
            .map_err(|e| format!("unexpected emspace task listing: {e}"))?;
        let items: Vec<ProviderItem> = tasks.into_iter().map(task_item).collect();
        Ok(ProviderResult {
            provider: "emspace".into(),
            resource_type: "tasks".into(),
            total_count: Some(items.len()),
            truncated: items.len() >= limit,
            items,
        })
    }
}

impl ContextProvider for EmspaceProvider {
    fn id(&self) -> &'static str {
        "emspace"
    }

    fn display_name(&self) -> &'static str {
        "Emspace"
    }

    fn supported_actions(&self) -> &[&str] {
        &["pages", "search", "tasks"]
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        match action {
            "pages" => self.pages(params),
            "search" => self.search(params),
            "tasks" => self.tasks(params),
            _ => Err(format!("Unsupported action: {action}")),
        }
    }

    fn cache_ttl_secs(&self) -> u64 {
        60
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        descriptor()
    }

    fn requires_auth(&self) -> bool {
        false
    }

    fn is_available(&self) -> bool {
        self.binary.is_some()
    }
}

pub(crate) fn descriptor() -> ConnectorDescriptor {
    use CapabilityLevel as L;
    ConnectorDescriptor {
        principal_binding: "none: the local user's emspace daemon, selected by the project's .emspace link",
        search: L::Supported,
        list: L::Supported,
        get: L::Supported,
        delta: L::Unsupported,
        revalidate_acl: L::Unsupported,
        revalidate_revision: L::Unsupported,
        delete_event: L::Unsupported,
        content_trust: ConnectorDescriptor::UNTRUSTED,
        write_surface: ConnectorDescriptor::NO_WRITES,
        failure_policy: ConnectorDescriptor::FAIL_CLOSED,
        notes: "no principal, so no ACL to re-check: personal-local use only, withheld from governed reuse; \
                reads run as `emspace run --query`, which refuses writes; optional CLI, unavailable without it",
    }
}

#[derive(Debug, Deserialize)]
struct PagesAnswer {
    #[serde(default)]
    dirs: Vec<DirRecord>,
    #[serde(default)]
    total: usize,
    #[serde(default)]
    pages: Vec<PageRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirRecord {
    name: String,
    local_path: Option<String>,
    remote: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageRecord {
    slug: String,
    title: String,
    #[serde(rename = "type")]
    page_type: Option<String>,
    status: Option<String>,
    #[serde(default)]
    stale: bool,
    #[serde(default)]
    noted: bool,
    description: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    derived: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchHit {
    slug: String,
    title: String,
    anchor: Option<String>,
    status: Option<String>,
    #[serde(default)]
    stale: bool,
    snippet: Option<String>,
    description: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskRecord {
    id: String,
    title: String,
    status: Option<String>,
    priority: Option<String>,
    board_slug: Option<String>,
    author: Option<String>,
    body: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    #[serde(default)]
    page_slugs: Vec<String>,
}

/// Page state as one word: freshness first, since a stale page is knowledge
/// to repair rather than to trust.
fn page_state(stale: bool, noted: bool, status: Option<&str>) -> String {
    if stale {
        "stale".into()
    } else if noted {
        "noted".into()
    } else {
        status.unwrap_or("draft").into()
    }
}

fn page_item(page: PageRecord, repo_dirs: &[&str], project_root: &Path) -> ProviderItem {
    let mut body = String::new();
    if let Some(description) = page.description.as_deref().filter(|d| !d.is_empty()) {
        body.push_str(description);
        body.push_str("\n\n");
    }
    body.push_str(&page.body);
    let state = page_state(page.stale, page.noted, page.status.as_deref());
    let mut labels: Vec<String> = page.page_type.into_iter().collect();
    labels.push(state.clone());
    ProviderItem {
        url: Some(format!("emspace:/{}", page.slug)),
        references: page
            .derived
            .iter()
            .flat_map(|locator| repository_paths(locator, repo_dirs, project_root))
            .collect(),
        id: page.slug,
        title: page.title,
        state: Some(state),
        created_at: page.created_at,
        updated_at: page.updated_at,
        labels,
        body: Some(body),
        ..ProviderItem::default()
    }
}

fn search_item(hit: SearchHit) -> ProviderItem {
    let id = match hit.anchor.as_deref().filter(|a| !a.is_empty()) {
        Some(anchor) => format!("{}#{anchor}", hit.slug),
        None => hit.slug.clone(),
    };
    let body = hit.snippet.or(hit.description);
    ProviderItem {
        url: Some(format!("emspace:/{}", hit.slug)),
        id,
        title: hit.title,
        state: Some(page_state(hit.stale, false, hit.status.as_deref())),
        updated_at: hit.updated_at,
        body,
        ..ProviderItem::default()
    }
}

fn task_item(task: TaskRecord) -> ProviderItem {
    let mut labels: Vec<String> = task.priority.into_iter().collect();
    labels.extend(task.board_slug);
    labels.extend(task.page_slugs.iter().map(|slug| format!("page:{slug}")));
    ProviderItem {
        url: Some(format!("emspace:tasks/{}", task.id)),
        id: task.id,
        title: task.title,
        state: task.status,
        author: task.author,
        created_at: task.created_at,
        updated_at: task.updated_at,
        labels,
        body: task.body,
        ..ProviderItem::default()
    }
}

/// The names of the registered dirs that are this repository: the same
/// checkout, or a checkout of the same git remote (a sibling worktree).
fn repository_dirs<'a>(dirs: &'a [DirRecord], project_root: &Path) -> Vec<&'a str> {
    let root = crate::core::pathjail::canonicalize_or_self(project_root);
    let mut remotes: Option<Vec<String>> = None;
    dirs.iter()
        .filter(|dir| {
            if dir
                .local_path
                .as_deref()
                .is_some_and(|p| crate::core::pathjail::canonicalize_or_self(Path::new(p)) == root)
            {
                return true;
            }
            let Some(remote) = dir.remote.as_deref() else {
                return false;
            };
            remotes
                .get_or_insert_with(|| git_remotes(project_root))
                .iter()
                .any(|own| normalize_remote(own) == normalize_remote(remote))
        })
        .map(|dir| dir.name.as_str())
        .collect()
}

fn git_remotes(project_root: &Path) -> Vec<String> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(project_root)
        .args(["config", "--get-regexp", r"^remote\..*\.url$"])
        .stdin(std::process::Stdio::null());
    let Ok(captured) = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_secs(5)),
        65_536,
        4096,
    ) else {
        return Vec::new();
    };
    if captured.timed_out || !captured.output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&captured.output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' ').map(|(_, url)| url.trim().to_owned()))
        .collect()
}

fn normalize_remote(url: &str) -> &str {
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url)
}

/// The repository-relative paths a `dir:<name>/<path>` locator pins, when
/// `<name>` is one of this repository's dirs. A file or directory pin is its
/// path; a glob is expanded against the checkout to the files it selects
/// here (at most [`MAX_GLOB_FILES`]), since the directory in front of it —
/// `rust/src/core` for `rust/src/core/adaptive*.rs` — would claim every file
/// below. The whole dir (`dir:<name>`) pins nothing useful and is skipped.
fn repository_paths(locator: &str, repo_dirs: &[&str], project_root: &Path) -> Vec<String> {
    let Some((name, path)) = locator
        .strip_prefix("dir:")
        .and_then(|rest| rest.split_once('/'))
    else {
        return Vec::new();
    };
    let path = path.trim_matches('/');
    if !repo_dirs.contains(&name)
        || path.is_empty()
        || path.split('/').any(|part| part == ".." || part == ".")
    {
        return Vec::new();
    }
    if !path.contains(['*', '?', '[', '{']) {
        return vec![path.to_owned()];
    }
    let pattern = project_root.join(path);
    let Ok(entries) = glob::glob(&pattern.to_string_lossy()) else {
        return Vec::new();
    };
    let mut paths: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|file| file.is_file())
        .filter_map(|file| {
            let relative = file.strip_prefix(project_root).ok()?;
            Some(relative.to_string_lossy().replace('\\', "/"))
        })
        .take(MAX_GLOB_FILES)
        .collect();
    paths.sort();
    paths
}

/// The `.emspace` link at or above `start`.
fn find_link(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join(".emspace"))
        .find(|candidate| candidate.is_file())
}

/// The `emspace` binary on `PATH`, else in `~/.local/bin` (its installer's
/// default, often missing from a GUI-started agent's `PATH`).
fn find_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "emspace.exe"
    } else {
        "emspace"
    };
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    });
    on_path.or_else(|| {
        dirs::home_dir()
            .map(|home| home.join(".local/bin").join(name))
            .filter(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locators_of_this_repository_become_paths() {
        let repo = ["repo"];
        let root = tempfile::tempdir().unwrap();
        let core = root.path().join("src/core");
        std::fs::create_dir_all(&core).unwrap();
        for file in ["adaptive.rs", "adaptive_mode.rs", "budget.rs"] {
            std::fs::write(core.join(file), "").unwrap();
        }
        let paths = |locator: &str| repository_paths(locator, &repo, root.path());
        assert_eq!(
            paths("dir:repo/src/core/budget.rs"),
            vec!["src/core/budget.rs"]
        );
        assert_eq!(paths("dir:repo/src/dashboard/"), vec!["src/dashboard"]);
        // A glob names the files it selects in this checkout, never its parent dir.
        assert_eq!(
            paths("dir:repo/src/core/adaptive*.rs"),
            vec!["src/core/adaptive.rs", "src/core/adaptive_mode.rs"]
        );
        assert_eq!(paths("dir:repo/src/**/*.rs").len(), 3);
        // Another repository's dir, a whole dir, other kinds and escapes name nothing.
        for locator in [
            "dir:site/src/main.rs",
            "dir:repo",
            "url:https://example.com/a.md",
            "dir:repo/../etc/passwd",
            "dir:repo/src/../../*.rs",
        ] {
            assert!(paths(locator).is_empty(), "{locator}");
        }
    }

    #[test]
    fn a_dir_belongs_to_the_repository_by_checkout_or_remote() {
        let root = tempfile::tempdir().unwrap();
        let dirs = vec![
            DirRecord {
                name: "here".into(),
                local_path: Some(root.path().display().to_string()),
                remote: None,
            },
            DirRecord {
                name: "elsewhere".into(),
                local_path: Some("/nonexistent/other".into()),
                remote: None,
            },
        ];
        assert_eq!(repository_dirs(&dirs, root.path()), vec!["here"]);
        assert_eq!(
            normalize_remote("git@host:o/r.git/"),
            normalize_remote("git@host:o/r")
        );
    }

    #[test]
    fn a_page_becomes_a_wiki_item_with_its_pinned_paths() {
        let page: PageRecord = serde_json::from_value(serde_json::json!({
            "slug": "core/patterns",
            "title": "Core: Shell Output Patterns",
            "type": "component",
            "status": "draft",
            "stale": true,
            "noted": false,
            "description": "Shell output pattern engine",
            "body": "# Core: Shell Output Patterns\n\nCompresses CLI output.",
            "derived": ["dir:repo/rust/src/core/patterns", "dir:site/x/y.ts"],
        }))
        .unwrap();
        let item = page_item(page, &["repo"], Path::new("/nonexistent"));
        assert_eq!(item.id, "core/patterns");
        assert_eq!(item.state.as_deref(), Some("stale"));
        assert_eq!(item.labels, vec!["component", "stale"]);
        assert_eq!(item.references, vec!["rust/src/core/patterns"]);
        assert!(
            item.body
                .as_deref()
                .unwrap()
                .starts_with("Shell output pattern engine\n\n# Core")
        );

        let result = ProviderResult {
            provider: "emspace".into(),
            resource_type: "pages".into(),
            items: vec![item],
            total_count: Some(1),
            truncated: false,
        };
        let chunks = super::super::registry::result_to_chunks(&result);
        assert_eq!(chunks[0].kind, crate::core::bm25_index::ChunkKind::WikiPage);
        assert!(
            chunks[0]
                .references
                .contains(&"rust/src/core/patterns".to_owned())
        );
    }

    #[test]
    fn without_a_link_the_provider_is_unavailable_and_runs_nothing() {
        let root = tempfile::tempdir().unwrap();
        // tempdir has no .emspace at or above it on any CI host.
        if find_link(root.path()).is_some() {
            return;
        }
        let provider = EmspaceProvider::new(root.path());
        assert!(!provider.is_available());
        let err = provider
            .execute("pages", &ProviderParams::default())
            .unwrap_err();
        assert!(err.contains("not available"), "{err}");
    }

    #[test]
    fn invalid_states_fail_before_any_process_runs() {
        let provider = EmspaceProvider {
            project_root: PathBuf::from("/nonexistent"),
            binary: Some(PathBuf::from("/nonexistent/emspace")),
        };
        let params = ProviderParams {
            state: Some("everything".into()),
            ..ProviderParams::default()
        };
        assert!(
            provider
                .pages(&params)
                .unwrap_err()
                .contains("unknown page state")
        );
        assert!(
            provider
                .tasks(&params)
                .unwrap_err()
                .contains("unknown task state")
        );
        let empty = ProviderParams::default();
        assert!(
            provider
                .search(&empty)
                .unwrap_err()
                .contains("needs a query")
        );
    }
}
