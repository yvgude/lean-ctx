// SPDX-License-Identifier: Apache-2.0
//! Candidate collection: which files a bundle may draw from.
//!
//! The walk honours `.gitignore`/`.ignore` and the shared content-walk filter
//! (vendor dirs, agent worktree copies), so a bundle sees the same project the
//! index sees. Lockfiles, minified assets, binaries and oversized files are
//! never candidates: they spend budget without explaining anything.

use std::path::Path;

use ignore::overrides::OverrideBuilder;

/// Files larger than this are listed as skipped instead of read.
pub(crate) const MAX_FILE_BYTES: u64 = 512 * 1024;
/// Hard stop for pathological trees; the report says when it was hit.
pub(crate) const MAX_WALKED_FILES: usize = 50_000;

/// Generated or machine-owned files that never help a reader.
const DEFAULT_EXCLUDES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
    "flake.lock",
    "*.min.js",
    "*.min.css",
    "*.map",
];

/// One file the walk found.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    /// Path relative to the project root, `/`-separated.
    pub path: String,
    /// UTF-8 content; `None` when the file was skipped.
    pub content: Option<String>,
    /// Why the file was not read (`binary`, `too large`, `secret-like path`).
    pub skipped: Option<&'static str>,
}

/// Result of the walk.
#[derive(Debug, Default)]
pub(crate) struct Collection {
    pub candidates: Vec<Candidate>,
    /// `true` when [`MAX_WALKED_FILES`] cut the walk short.
    pub truncated: bool,
}

/// Walk `scope` (a directory or a single file inside `root`).
///
/// `include` globs whitelist files; `exclude` globs drop them. Both are
/// matched relative to `root`, gitignore-style.
pub(crate) fn collect(
    root: &Path,
    scope: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<Collection, String> {
    let overrides = build_overrides(root, include, exclude)?;
    let walker = ignore::WalkBuilder::new(scope)
        .hidden(crate::core::walk_filter::SKIP_HIDDEN_IN_CONTENT_WALK)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .overrides(overrides)
        .filter_entry(|entry| {
            entry.file_name() != ".git" && crate::core::walk_filter::keep_entry(entry)
        })
        .build();

    let mut collection = Collection::default();
    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if collection.candidates.len() >= MAX_WALKED_FILES {
            collection.truncated = true;
            break;
        }
        let Some(path) = relative_path(root, entry.path()) else {
            continue;
        };
        collection
            .candidates
            .push(read_candidate(entry.path(), path));
    }
    collection.candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(collection)
}

fn build_overrides(
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<ignore::overrides::Override, String> {
    let mut builder = OverrideBuilder::new(root);
    for glob in include {
        builder
            .add(glob)
            .map_err(|e| format!("invalid --include glob '{glob}': {e}"))?;
    }
    for glob in exclude
        .iter()
        .map(String::as_str)
        .chain(DEFAULT_EXCLUDES.iter().copied())
    {
        let negated = format!("!{}", glob.trim_start_matches('!'));
        builder
            .add(&negated)
            .map_err(|e| format!("invalid --ignore glob '{glob}': {e}"))?;
    }
    builder.build().map_err(|e| format!("invalid globs: {e}"))
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let joined = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    (!joined.is_empty()).then_some(joined)
}

fn read_candidate(abs: &Path, path: String) -> Candidate {
    let skip = |reason| Candidate {
        path: path.clone(),
        content: None,
        skipped: Some(reason),
    };
    if crate::core::sensitivity::classify_path(abs)
        == crate::core::sensitivity::SensitivityLevel::Secret
    {
        return skip("secret-like path");
    }
    match std::fs::metadata(abs) {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => return skip("too large"),
        Ok(_) => {}
        Err(_) => return skip("unreadable"),
    }
    let Ok(bytes) = std::fs::read(abs) else {
        return skip("unreadable");
    };
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return skip("binary");
    }
    match String::from_utf8(bytes) {
        Ok(content) => Candidate {
            path,
            content: Some(content),
            skipped: None,
        },
        Err(_) => skip("binary"),
    }
}

#[cfg(test)]
mod tests {
    use super::collect;

    fn write(dir: &std::path::Path, rel: &str, body: &[u8]) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn walk_skips_lockfiles_binaries_git_and_gitignored_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib.rs", b"pub fn a() {}\n");
        write(root, "Cargo.lock", b"# lock\n");
        write(root, "logo.png", b"\x89PNG\x00\x00");
        write(root, ".git/config", b"[core]\n");
        write(root, ".gitignore", b"target/\n");
        write(root, "target/out.rs", b"fn built() {}\n");
        write(root, ".github/workflows/ci.yml", b"on: push\n");

        let got = collect(root, root, &[], &[]).unwrap();
        let paths: Vec<&str> = got.candidates.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".github/workflows/ci.yml",
                ".gitignore",
                "logo.png",
                "src/lib.rs"
            ]
        );
        let logo = got
            .candidates
            .iter()
            .find(|c| c.path == "logo.png")
            .unwrap();
        assert_eq!(logo.skipped, Some("binary"));
        assert!(logo.content.is_none());
    }

    #[test]
    fn include_and_ignore_globs_narrow_the_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/a.rs", b"fn a() {}\n");
        write(root, "src/a_test.rs", b"fn t() {}\n");
        write(root, "docs/guide.md", b"# Guide\n");

        let got = collect(
            root,
            root,
            &["src/**/*.rs".to_string()],
            &["*_test.rs".to_string()],
        )
        .unwrap();
        let paths: Vec<&str> = got.candidates.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs"]);
    }

    #[test]
    fn scope_limits_the_walk_but_paths_stay_root_relative() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/auth/login.rs", b"fn login() {}\n");
        write(root, "src/db.rs", b"fn db() {}\n");

        let got = collect(root, &root.join("src/auth"), &[], &[]).unwrap();
        let paths: Vec<&str> = got.candidates.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["src/auth/login.rs"]);
    }

    #[test]
    fn secret_like_paths_are_never_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, ".env", b"TOKEN=abc\n");

        let got = collect(root, root, &[], &[]).unwrap();
        let env = got.candidates.iter().find(|c| c.path == ".env").unwrap();
        assert_eq!(env.skipped, Some("secret-like path"));
        assert!(env.content.is_none());
    }

    #[test]
    fn invalid_globs_are_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let err = collect(tmp.path(), tmp.path(), &["src/[".to_string()], &[]).unwrap_err();
        assert!(err.contains("--include"), "{err}");
    }
}
