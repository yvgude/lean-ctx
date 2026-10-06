//! Search-root resolution and result scoping (language/glob/subdir filters).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Resolve the index root for a search/index path.
///
/// The BM25 namespace is keyed on the detected *project* root (git remote /
/// build marker), not on the literal search path: `project_identity` inspects
/// only the exact directory it is handed and never walks up. A search launched
/// from — or pointed at — a subdirectory therefore hashes to a different,
/// usually empty namespace and returns zero hits, even though the real index
/// sits one directory up (#948). Promoting the search path the same way the
/// build does makes both agree. A genuinely requested subdirectory is kept as a
/// result-scope filter (second tuple field) rather than becoming its own
/// namespace.
pub(crate) fn resolve_search_root(path: &str) -> Result<(PathBuf, Option<String>), String> {
    let raw = Path::new(path);
    if !raw.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    let raw_dir = if raw.is_file() {
        raw.parent().unwrap_or(raw)
    } else {
        raw
    };
    if let Some(authority) = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
    {
        let root = authority
            .canonicalize()
            .map_err(|_| "source authority unavailable")?;
        let requested = raw_dir
            .canonicalize()
            .map_err(|_| "source path unavailable")?;
        if !requested.starts_with(&root) {
            return Err("source path is outside the request authority".into());
        }
        let subdir = search_subdir_filter(&root, &requested);
        return Ok((root, subdir));
    }
    // A local default only claims paths inside it; request authority above
    // remains binding even when the caller supplies another project (#1875).
    let raw_str = raw_dir.to_string_lossy();
    let root = match pinned_project_root() {
        Some(pinned) if !path_is_within(raw_dir, Path::new(&pinned)) => {
            crate::core::protocol::detect_project_root(&raw_str).unwrap_or_else(|| raw_str.into())
        }
        _ => crate::core::protocol::detect_project_root_or_cwd(&raw_str),
    };
    let root = PathBuf::from(root);
    let subdir = search_subdir_filter(&root, raw_dir);
    Ok((root, subdir))
}

fn pinned_project_root() -> Option<String> {
    std::env::var("LEAN_CTX_PROJECT_ROOT")
        .ok()
        .or_else(|| crate::core::config::Config::load().project_root)
        .filter(|r| !r.trim().is_empty())
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = crate::core::pathutil::safe_canonicalize_or_self(path);
    let root = crate::core::pathutil::safe_canonicalize_or_self(root);
    path.starts_with(root)
}

/// Project-relative prefix (forward slashes, no leading/trailing slash) for
/// `requested` under `root`, or `None` when `requested` is the root itself or
/// not contained in it. Lets a subdirectory search stay scoped after the path
/// was promoted to the project root for the index namespace.
pub(crate) fn search_subdir_filter(root: &Path, requested: &Path) -> Option<String> {
    let root_c = crate::core::pathutil::safe_canonicalize_or_self(root);
    let req_c = crate::core::pathutil::safe_canonicalize_or_self(requested);
    let rel = req_c.strip_prefix(&root_c).ok()?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    let rel = rel.trim_matches('/').to_string();
    if rel.is_empty() { None } else { Some(rel) }
}

pub(crate) struct SearchFilter {
    allowed_exts: Option<HashSet<String>>,
    path_glob: Option<glob::Pattern>,
    /// Relative directory prefix (forward slashes, no trailing slash) the caller
    /// scoped the search to. Set when a subdirectory was requested but promoted
    /// to the project root for the index namespace (#948), so results stay
    /// restricted to that subtree without a separate (empty) index.
    subdir: Option<String>,
}

impl SearchFilter {
    pub(crate) fn new(
        languages: Option<&[String]>,
        path_glob: Option<&str>,
    ) -> Result<Self, String> {
        let allowed_exts = languages.map(normalize_languages);
        let path_glob = match path_glob {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(glob::Pattern::new(s).map_err(|e| e.msg.to_string())?),
        };
        Ok(Self {
            allowed_exts,
            path_glob,
            subdir: None,
        })
    }

    /// Scope results to a project-relative subdirectory, in addition to any
    /// language/glob filters. `None` (or empty) clears the scope.
    pub(crate) fn with_subdir(mut self, subdir: Option<String>) -> Self {
        self.subdir = subdir.filter(|s| !s.is_empty());
        self
    }

    pub(crate) fn is_active(&self) -> bool {
        self.allowed_exts.is_some() || self.path_glob.is_some() || self.subdir.is_some()
    }

    pub(crate) fn matches(&self, rel_path: &str) -> bool {
        let rel_path = rel_path.replace('\\', "/");
        if let Some(prefix) = &self.subdir
            && rel_path != *prefix
            && !rel_path.starts_with(&format!("{prefix}/"))
        {
            return false;
        }
        if let Some(p) = &self.path_glob
            && !p.matches(&rel_path)
        {
            return false;
        }
        if let Some(exts) = &self.allowed_exts {
            let ext = Path::new(&rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ext.is_empty() || !exts.contains(&ext) {
                return false;
            }
        }
        true
    }
}

pub(crate) fn normalize_languages(langs: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for l in langs {
        let raw = l.trim().trim_start_matches('.').to_lowercase();
        match raw.as_str() {
            "rust" | "rs" => {
                out.insert("rs".to_string());
            }
            "ts" | "typescript" => {
                out.insert("ts".to_string());
                out.insert("tsx".to_string());
            }
            "js" | "javascript" => {
                out.insert("js".to_string());
                out.insert("jsx".to_string());
                out.insert("mjs".to_string());
                out.insert("cjs".to_string());
            }
            "py" | "python" => {
                out.insert("py".to_string());
            }
            "go" => {
                out.insert("go".to_string());
            }
            "java" => {
                out.insert("java".to_string());
            }
            "ruby" | "rb" => {
                out.insert("rb".to_string());
            }
            "php" => {
                out.insert("php".to_string());
            }
            "c" => {
                out.insert("c".to_string());
                out.insert("h".to_string());
            }
            "cpp" | "c++" | "cc" => {
                out.insert("cpp".to_string());
                out.insert("hpp".to_string());
                out.insert("cc".to_string());
                out.insert("hh".to_string());
            }
            "cs" | "csharp" => {
                out.insert("cs".to_string());
            }
            "swift" => {
                out.insert("swift".to_string());
            }
            "kt" | "kotlin" => {
                out.insert("kt".to_string());
                out.insert("kts".to_string());
            }
            "json" => {
                out.insert("json".to_string());
            }
            "yaml" | "yml" => {
                out.insert("yaml".to_string());
                out.insert("yml".to_string());
            }
            other if !other.is_empty() => {
                out.insert(other.to_string());
            }
            _ => {}
        }
    }
    out
}
