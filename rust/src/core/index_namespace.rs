use std::path::{Path, PathBuf};

use md5::{Digest, Md5};

pub(crate) fn vectors_dir(project_root: &Path) -> PathBuf {
    let hash = namespace_hash(project_root);
    let legacy = legacy_vectors_hash(project_root);

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let old_dir = data_dir.join("vectors").join(&legacy);
        let new_dir = data_dir.join("vectors").join(&hash);
        migrate_dir_if_needed(&old_dir, &new_dir);
        return new_dir;
    }

    PathBuf::from(".").join("vectors").join(hash)
}

#[allow(dead_code)]
pub(crate) fn graphs_dir(project_root: &str) -> Option<PathBuf> {
    let root_path = Path::new(project_root);
    let hash = namespace_hash(root_path);
    let legacy = legacy_graphs_hash(project_root);

    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let old_dir = data_dir.join("graphs").join(&legacy);
    let new_dir = data_dir.join("graphs").join(&hash);
    migrate_dir_if_needed(&old_dir, &new_dir);
    Some(new_dir)
}

pub(crate) fn namespace_hash(project_root: &Path) -> String {
    let seed = namespace_seed(project_root);
    let mut hasher = Md5::new();
    hasher.update(seed.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn namespace_seed(project_root: &Path) -> String {
    let root_s = project_root.to_string_lossy().to_string();
    let base = crate::core::project_hash::project_identity(&root_s)
        .unwrap_or_else(|| crate::core::graph_index::normalize_project_root(&root_s));

    if !branch_aware_enabled() {
        return base;
    }

    let branch = git_branch(project_root).unwrap_or_else(|| "HEAD".to_string());
    format!("{base}|branch:{branch}")
}

fn branch_aware_enabled() -> bool {
    let Ok(v) = std::env::var("LEANCTX_INDEX_BRANCH_AWARE") else {
        return false;
    };
    matches!(
        v.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn git_branch(project_root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn legacy_vectors_hash(project_root: &Path) -> String {
    let mut hasher = Md5::new();
    hasher.update(project_root.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[allow(dead_code)]
fn legacy_graphs_hash(project_root: &str) -> String {
    let input = crate::core::graph_index::normalize_project_root(project_root);
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:08x}", hasher.finish() & 0xFFFF_FFFF)
}

fn migrate_dir_if_needed(old_dir: &Path, new_dir: &Path) {
    if old_dir == new_dir {
        return;
    }
    if !old_dir.exists() || new_dir.exists() {
        return;
    }
    if !verify_index_ownership(old_dir) {
        tracing::warn!(
            "lean-ctx: skipping index migration — ownership check failed for {old_dir:?}"
        );
        return;
    }
    if let Err(e) = copy_dir_contents(old_dir, new_dir) {
        tracing::error!("lean-ctx: index migration failed: {e}");
    }
}

fn verify_index_ownership(dir: &Path) -> bool {
    let marker = dir.join("bm25_index.json");
    if !marker.exists() {
        return true;
    }
    let Ok(meta) = std::fs::metadata(&marker) else {
        return true;
    };
    if meta.len() == 0 || meta.len() > 500_000_000 {
        return false;
    }
    true
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_hash_is_stable_across_clones_with_same_git_remote() {
        let _env = crate::core::data_dir::test_env_lock();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();

        std::fs::create_dir_all(a.path().join(".git")).unwrap();
        std::fs::write(
            a.path().join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/my-repo.git\n",
        )
        .unwrap();

        std::fs::create_dir_all(b.path().join(".git")).unwrap();
        std::fs::write(
            b.path().join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/my-repo.git\n",
        )
        .unwrap();

        let ha = namespace_hash(a.path());
        let hb = namespace_hash(b.path());
        assert_eq!(ha, hb);
    }

    #[test]
    fn vectors_dir_migrates_legacy_path_hash_directory() {
        let _env = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());

        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".git")).unwrap();
        std::fs::write(
            project.path().join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/my-repo.git\n",
        )
        .unwrap();

        let legacy = legacy_vectors_hash(project.path());
        let old_dir = data_dir.path().join("vectors").join(&legacy);
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("bm25_index.json"), "{\"doc_count\":0}").unwrap();

        let new_dir = vectors_dir(project.path());
        assert!(new_dir.exists());
        assert!(new_dir.join("bm25_index.json").exists());
    }
}
