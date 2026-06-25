//! Repository URL detection & parsing.
//!
//! Turns an agent-supplied URL (a repo root, or a web `blob`/`tree` link) into a
//! structured [`RepoRef`] with a canonical https clone URL plus the optional
//! `ref` + in-repo `subpath` carried by the web form. https-only by design — the
//! clone step ([`super::clone`]) additionally SSRF-guards the host.
//!
//! Handles the three common forgejo/forge layouts:
//! * GitHub / Gitea / Bitbucket: `owner/repo/blob/<ref>/<path>` (and `/tree/`)
//! * GitLab (incl. nested groups): `group/.../repo/-/blob/<ref>/<path>`
//! * bare repo roots: `owner/repo` (optionally `.git`)

/// A parsed repository reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    /// Host, e.g. `github.com`.
    pub host: String,
    /// Namespace before the repo name (may contain `/` for GitLab subgroups).
    pub owner: String,
    /// Repository name without a `.git` suffix.
    pub repo: String,
    /// Canonical https clone URL: `https://<host>/<owner>/<repo>.git`.
    pub clone_url: String,
    /// Branch / tag / commit carried by a web `blob`/`tree` URL, if any.
    pub git_ref: Option<String>,
    /// In-repo path carried by a web `blob`/`tree` URL, if any.
    pub subpath: Option<String>,
}

impl RepoRef {
    /// `owner/repo` (namespace + name), the human project path.
    #[must_use]
    pub fn project_path(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// A stable cache slug, filesystem-safe: `host/owner/repo` with the owner's
    /// internal slashes preserved as nested dirs.
    #[must_use]
    pub fn cache_slug(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.repo)
    }
}

/// Parse a repository URL, or return `None` when it is not an https repo URL.
#[must_use]
pub fn parse(url: &str) -> Option<RepoRef> {
    let rest = url.trim().strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    if !is_valid_host(host) {
        return None;
    }
    // Drop query string / fragment, normalize slashes.
    let path = path.split(['?', '#']).next().unwrap_or("");
    let path = path.trim_matches('/');
    if path.is_empty() {
        return None;
    }

    let (project_path, git_ref, subpath) = split_project_and_location(path);
    let project_path = project_path.trim_end_matches('/').trim_end_matches(".git");

    let segments: Vec<&str> = project_path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None; // need at least owner + repo
    }
    let repo = (*segments.last()?).to_string();
    let owner = segments[..segments.len() - 1].join("/");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    let clone_url = format!("https://{host}/{owner}/{repo}.git");
    Some(RepoRef {
        host: host.to_string(),
        owner,
        repo,
        clone_url,
        git_ref,
        subpath,
    })
}

/// Split a path into `(project_path, ref, subpath)`, recognizing both the GitLab
/// `/-/blob|tree/` separator and the GitHub/Gitea `/blob|tree/` third segment.
fn split_project_and_location(path: &str) -> (String, Option<String>, Option<String>) {
    // GitLab: everything before `/-/` is the (possibly nested) project path.
    if let Some((proj, tail)) = path.split_once("/-/") {
        let (git_ref, subpath) = parse_location_tail(tail);
        return (proj.to_string(), git_ref, subpath);
    }

    // GitHub/Gitea/Bitbucket: `owner/repo/blob|tree/<ref>/<path>`. Only treat
    // `blob`/`tree` as a separator at segment index 2 so a repo can't be hidden
    // by a same-named path component.
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() >= 4 && matches!(segs[2], "blob" | "tree" | "raw" | "src" | "commits") {
        let project = segs[..2].join("/");
        let tail = segs[3..].join("/");
        let (git_ref, subpath) = parse_ref_then_path(&tail);
        return (project, git_ref, subpath);
    }

    (path.to_string(), None, None)
}

/// Parse a `blob/<ref>/<path>` style tail (the part after GitLab's `/-/`).
fn parse_location_tail(tail: &str) -> (Option<String>, Option<String>) {
    let segs: Vec<&str> = tail.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return (None, None);
    }
    // segs[0] is the kind (blob/tree/raw); the rest is `<ref>/<path>`.
    let after_kind = if matches!(segs[0], "blob" | "tree" | "raw") {
        &segs[1..]
    } else {
        &segs[..]
    };
    parse_ref_then_path(&after_kind.join("/"))
}

/// Split `<ref>/<path>` taking the first segment as the ref. Branch names with
/// slashes can't be disambiguated from a URL alone — callers may pass `ref`
/// explicitly to override.
fn parse_ref_then_path(s: &str) -> (Option<String>, Option<String>) {
    let s = s.trim_matches('/');
    if s.is_empty() {
        return (None, None);
    }
    match s.split_once('/') {
        Some((r, p)) if !p.is_empty() => (Some(r.to_string()), Some(p.to_string())),
        _ => (Some(s.to_string()), None),
    }
}

/// A plausible DNS host: has a dot, no spaces/auth markers, not just a port.
fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.contains('.')
        && !host.contains(' ')
        && !host.contains('@')
        && !host.starts_with('.')
        && !host.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_repo_root() {
        let r = parse("https://github.com/yvgude/lean-ctx").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "yvgude");
        assert_eq!(r.repo, "lean-ctx");
        assert_eq!(r.clone_url, "https://github.com/yvgude/lean-ctx.git");
        assert_eq!(r.git_ref, None);
        assert_eq!(r.subpath, None);
    }

    #[test]
    fn strips_dot_git_and_trailing_slash() {
        let r = parse("https://github.com/o/r.git/").unwrap();
        assert_eq!(r.repo, "r");
        assert_eq!(r.clone_url, "https://github.com/o/r.git");
    }

    #[test]
    fn parses_github_blob_ref_and_subpath() {
        let r = parse("https://github.com/yvgude/lean-ctx/blob/main/src/core/mod.rs").unwrap();
        assert_eq!(r.project_path(), "yvgude/lean-ctx");
        assert_eq!(r.git_ref.as_deref(), Some("main"));
        assert_eq!(r.subpath.as_deref(), Some("src/core/mod.rs"));
        assert_eq!(r.clone_url, "https://github.com/yvgude/lean-ctx.git");
    }

    #[test]
    fn parses_github_tree_ref_only() {
        let r = parse("https://github.com/o/r/tree/v1.2.3").unwrap();
        assert_eq!(r.git_ref.as_deref(), Some("v1.2.3"));
        assert_eq!(r.subpath, None);
    }

    #[test]
    fn parses_gitlab_dash_blob_with_nested_group() {
        let r = parse("https://gitlab.com/group/sub/proj/-/blob/main/a/b.rs").unwrap();
        assert_eq!(r.host, "gitlab.com");
        assert_eq!(r.owner, "group/sub");
        assert_eq!(r.repo, "proj");
        assert_eq!(r.git_ref.as_deref(), Some("main"));
        assert_eq!(r.subpath.as_deref(), Some("a/b.rs"));
        assert_eq!(r.clone_url, "https://gitlab.com/group/sub/proj.git");
    }

    #[test]
    fn parses_gitlab_tree_dir() {
        let r = parse("https://gitlab.com/o/r/-/tree/dev/src").unwrap();
        assert_eq!(r.git_ref.as_deref(), Some("dev"));
        assert_eq!(r.subpath.as_deref(), Some("src"));
    }

    #[test]
    fn drops_query_and_fragment() {
        let r = parse("https://github.com/o/r/blob/main/x.rs?plain=1#L10").unwrap();
        assert_eq!(r.subpath.as_deref(), Some("x.rs"));
    }

    #[test]
    fn rejects_non_https_and_garbage() {
        assert!(parse("http://github.com/o/r").is_none());
        assert!(parse("git@github.com:o/r.git").is_none());
        assert!(parse("https://github.com/justowner").is_none());
        assert!(parse("https://localhost/o/r").is_none()); // no dot in host
        assert!(parse("not a url").is_none());
    }

    #[test]
    fn cache_slug_is_filesystem_nested() {
        let r = parse("https://gitlab.com/group/sub/proj").unwrap();
        assert_eq!(r.cache_slug(), "gitlab.com/group/sub/proj");
    }
}
