use std::path::{Path, PathBuf};

const IDE_CONFIG_DIRS: &[&str] = &[
    ".lean-ctx",
    ".cursor",
    ".claude",
    ".codex",
    ".codeium",
    ".gemini",
    ".qwen",
    ".trae",
    ".kiro",
    ".verdent",
    ".pi",
    ".amp",
    ".aider",
    ".continue",
];

fn allow_paths_from_env_and_config() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        out.push(canonicalize_or_self(&data_dir));
    }

    if let Some(home) = dirs::home_dir() {
        for dir in IDE_CONFIG_DIRS {
            let p = home.join(dir);
            if p.exists() {
                out.push(canonicalize_or_self(&p));
            }
        }
    }

    let cfg = crate::core::config::Config::load();
    for p in &cfg.allow_paths {
        let pb = PathBuf::from(p);
        out.push(canonicalize_or_self(&pb));
    }

    let v = std::env::var("LCTX_ALLOW_PATH")
        .or_else(|_| std::env::var("LEAN_CTX_ALLOW_PATH"))
        .unwrap_or_default();
    if v.trim().is_empty() {
        return out;
    }
    for p in std::env::split_paths(&v) {
        out.push(canonicalize_or_self(&p));
    }
    out
}

fn is_under_prefix(path: &Path, prefix: &Path) -> bool {
    path.starts_with(prefix)
}

fn canonicalize_or_self(path: &Path) -> PathBuf {
    super::pathutil::safe_canonicalize_or_self(path)
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
    let allow = allow_paths_from_env_and_config();

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
            "path escapes project root: {} (root: {}). \
             Hint: set LEAN_CTX_ALLOW_PATH={} or add it to allow_paths in ~/.lean-ctx/config.toml",
            candidate.display(),
            root.display(),
            candidate.parent().unwrap_or(candidate).display()
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
    let path_str = normalize_windows_path(&path.to_string_lossy());
    let prefix_str = normalize_windows_path(&prefix.to_string_lossy());
    path_str.starts_with(&prefix_str)
}

#[cfg(windows)]
fn normalize_windows_path(s: &str) -> String {
    let stripped = super::pathutil::strip_verbatim_str(s).unwrap_or_else(|| s.to_string());
    stripped.to_lowercase().replace('/', "\\")
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

    #[test]
    fn ide_config_dirs_list_is_not_empty() {
        assert!(IDE_CONFIG_DIRS.len() >= 10);
        assert!(IDE_CONFIG_DIRS.contains(&".codex"));
        assert!(IDE_CONFIG_DIRS.contains(&".cursor"));
        assert!(IDE_CONFIG_DIRS.contains(&".claude"));
        assert!(IDE_CONFIG_DIRS.contains(&".gemini"));
    }

    #[test]
    fn canonicalize_or_self_strips_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();

        let result = canonicalize_or_self(&dir);
        let s = result.to_string_lossy();
        assert!(
            !s.starts_with(r"\\?\"),
            "canonicalize_or_self should strip verbatim prefix, got: {s}"
        );
    }

    #[test]
    fn jail_path_accepts_same_dir_different_format() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.rs"), "ok").unwrap();

        let result = jail_path(&root.join("file.rs"), &root);
        assert!(result.is_ok(), "same dir should be accepted: {result:?}");
    }

    #[test]
    fn error_message_contains_allow_path_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("b.txt"), "no").unwrap();

        let err = jail_path(&other.join("b.txt"), &root).unwrap_err();
        assert!(
            err.contains("LEAN_CTX_ALLOW_PATH"),
            "error should hint at LEAN_CTX_ALLOW_PATH: {err}"
        );
        assert!(
            err.contains("allow_paths"),
            "error should hint at config allow_paths: {err}"
        );
    }

    #[test]
    fn allow_path_env_permits_outside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("b.txt"), "allowed").unwrap();

        let canon = canonicalize_or_self(&other);
        std::env::set_var("LEAN_CTX_ALLOW_PATH", canon.to_string_lossy().as_ref());
        let result = jail_path(&other.join("b.txt"), &root);
        std::env::remove_var("LEAN_CTX_ALLOW_PATH");

        assert!(
            result.is_ok(),
            "LEAN_CTX_ALLOW_PATH should permit access: {result:?}"
        );
    }
}
