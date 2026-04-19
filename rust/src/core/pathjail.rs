use std::path::{Path, PathBuf};

fn allow_paths_from_env() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        out.push(canonicalize_or_self(&data_dir));
    }

    let v = std::env::var("LCTX_ALLOW_PATH")
        .or_else(|_| std::env::var("LEAN_CTX_ALLOW_PATH"))
        .unwrap_or_default();
    if v.trim().is_empty() {
        return out;
    }
    for p in std::env::split_paths(&v) {
        if let Ok(canon) = std::fs::canonicalize(&p) {
            out.push(canon);
        } else {
            out.push(p);
        }
    }
    out
}

fn is_under_prefix(path: &Path, prefix: &Path) -> bool {
    path.starts_with(prefix)
}

fn canonicalize_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<(PathBuf, Vec<std::ffi::OsString>)> {
    let mut cur = path.to_path_buf();
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if cur.exists() {
            return Some((canonicalize_or_self(&cur), remainder));
        }
        let name = cur.file_name()?.to_os_string();
        remainder.push(name);
        if !cur.pop() {
            return None;
        }
    }
}

pub fn jail_path(candidate: &Path, jail_root: &Path) -> Result<PathBuf, String> {
    let root = canonicalize_or_self(jail_root);
    let allow = allow_paths_from_env();

    let (base, remainder) = canonicalize_existing_ancestor(candidate).ok_or_else(|| {
        format!(
            "path does not exist and has no existing ancestor: {}",
            candidate.display()
        )
    })?;

    let allowed = is_under_prefix(&base, &root) || allow.iter().any(|p| is_under_prefix(&base, p));

    #[cfg(windows)]
    let allowed = allowed || is_under_prefix_windows(&base, &root);

    if !allowed {
        return Err(format!(
            "path escapes project root: {} (root: {})",
            candidate.display(),
            root.display()
        ));
    }

    #[cfg(windows)]
    reject_symlink_on_windows(candidate)?;

    let mut out = base;
    for part in remainder.iter().rev() {
        out.push(part);
    }
    Ok(out)
}

#[cfg(windows)]
fn is_under_prefix_windows(path: &Path, prefix: &Path) -> bool {
    let path_str = path.to_string_lossy().to_lowercase().replace('/', "\\");
    let prefix_str = prefix.to_string_lossy().to_lowercase().replace('/', "\\");
    path_str.starts_with(&prefix_str)
}

#[cfg(windows)]
fn reject_symlink_on_windows(path: &Path) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.is_symlink() {
            return Err(format!(
                "symlink not allowed in jailed path: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_outside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(root.join("a.txt"), "ok").unwrap();
        std::fs::write(other.join("b.txt"), "no").unwrap();

        let ok = jail_path(&root.join("a.txt"), &root);
        assert!(ok.is_ok());

        let bad = jail_path(&other.join("b.txt"), &root);
        assert!(bad.is_err());
    }

    #[test]
    fn allows_nonexistent_child_under_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "ok").unwrap();

        let p = root.join("new").join("file.txt");
        let ok = jail_path(&p, &root).unwrap();
        assert!(ok.to_string_lossy().contains("file.txt"));
    }
}
