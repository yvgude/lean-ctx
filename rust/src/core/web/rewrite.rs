//! Known-host URL rewrites that turn an agent-friendly *page* URL into the URL
//! that actually yields clean content.
//!
//! GitHub `blob` pages are JS-rendered: fetching them as HTML returns navigation
//! chrome ("Star", "Fork", "Uh oh! There was an error while loading") instead of
//! the file, and the raw HTML is enormous. The file's real bytes live on
//! `raw.githubusercontent.com`. Rewriting `…/blob/<ref>/<path>` (and the
//! equivalent `…/raw/<ref>/<path>`) to that host gives the agent the actual file
//! in one bounded fetch (GH feedback: reading GitHub pages directly hangs/garbles).

/// Rewrite a known page URL to its clean-content equivalent, or `None` if no
/// rule applies (the original URL is then used unchanged).
pub fn rewrite_url(url: &str) -> Option<String> {
    github_blob_to_raw(url)
}

fn github_blob_to_raw(url: &str) -> Option<String> {
    let rest = strip_github_host(url)?;
    // Drop any #fragment (e.g. line anchors) — raw content has no anchors.
    let path = rest.split('#').next().unwrap_or(rest);

    // owner / repo / (blob|raw) / ref / path…
    let parts: Vec<&str> = path.splitn(5, '/').collect();
    if parts.len() != 5 {
        return None;
    }
    let [owner, repo, kind, git_ref, file_path] =
        [parts[0], parts[1], parts[2], parts[3], parts[4]];
    if kind != "blob" && kind != "raw" {
        return None;
    }
    if owner.is_empty() || repo.is_empty() || git_ref.is_empty() || file_path.is_empty() {
        return None;
    }
    Some(format!(
        "https://raw.githubusercontent.com/{owner}/{repo}/{git_ref}/{file_path}"
    ))
}

fn strip_github_host(url: &str) -> Option<&str> {
    const HOSTS: [&str; 3] = [
        "https://github.com/",
        "http://github.com/",
        "https://www.github.com/",
    ];
    HOSTS.iter().find_map(|h| url.strip_prefix(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_blob_to_raw() {
        assert_eq!(
            rewrite_url("https://github.com/yvgude/lean-ctx/blob/main/README.md").as_deref(),
            Some("https://raw.githubusercontent.com/yvgude/lean-ctx/main/README.md")
        );
    }

    #[test]
    fn rewrites_nested_path_and_strips_fragment() {
        assert_eq!(
            rewrite_url("https://github.com/o/r/blob/v1.2.3/src/core/mod.rs#L10-L20").as_deref(),
            Some("https://raw.githubusercontent.com/o/r/v1.2.3/src/core/mod.rs")
        );
    }

    #[test]
    fn rewrites_raw_page_variant() {
        assert_eq!(
            rewrite_url("https://github.com/o/r/raw/main/a/b.txt").as_deref(),
            Some("https://raw.githubusercontent.com/o/r/main/a/b.txt")
        );
    }

    #[test]
    fn leaves_repo_root_and_non_blob_untouched() {
        // No reliable raw target without knowing the default branch.
        assert_eq!(rewrite_url("https://github.com/o/r"), None);
        assert_eq!(rewrite_url("https://github.com/o/r/issues/1"), None);
        assert_eq!(rewrite_url("https://github.com/o/r/tree/main/src"), None);
    }

    #[test]
    fn leaves_other_hosts_untouched() {
        assert_eq!(rewrite_url("https://example.com/o/r/blob/main/x"), None);
        assert_eq!(rewrite_url("https://gitlab.com/o/r/blob/main/x"), None);
    }
}
