use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Default)]
pub struct LinkedProjects {
    pub roots: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub source: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct WorkspaceConfigFile {
    #[serde(default, rename = "linkedProjects", alias = "linked_projects")]
    linked_projects: Vec<String>,
}

pub fn load_linked_projects(project_root: &Path) -> LinkedProjects {
    let mut out = LinkedProjects::default();

    let Some((source, content)) = read_config_file(project_root) else {
        return out;
    };
    out.source = Some(source.clone());

    let cfg: WorkspaceConfigFile = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            out.warnings.push(format!(
                "workspace config parse failed ({}): {e}",
                source.display()
            ));
            return out;
        }
    };

    let root_canon = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    for raw in cfg.linked_projects {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }

        let candidate = if Path::new(s).is_absolute() {
            PathBuf::from(s)
        } else {
            project_root.join(s)
        };

        let Ok(abs) = candidate.canonicalize() else {
            out.warnings.push(format!(
                "linked project missing/unreadable: {}",
                candidate.to_string_lossy()
            ));
            continue;
        };
        if abs == root_canon {
            continue;
        }
        if !abs.is_dir() {
            out.warnings.push(format!(
                "linked project is not a directory: {}",
                abs.display()
            ));
            continue;
        }

        match crate::core::pathjail::jail_path(&abs, project_root) {
            Ok(_) => out.roots.push(abs),
            Err(e) => out.warnings.push(format!(
                "linked project rejected by pathjail: {} ({e})",
                abs.display()
            )),
        }
    }

    out.roots.sort();
    out.roots.dedup();
    out
}

fn read_config_file(project_root: &Path) -> Option<(PathBuf, String)> {
    let lean = project_root.join(".leanctx.json");
    if let Ok(s) = std::fs::read_to_string(&lean) {
        return Some((lean, s));
    }
    let socrati = project_root.join(".socraticode.json");
    if let Ok(s) = std::fs::read_to_string(&socrati) {
        return Some((socrati, s));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn write_linked_config(root: &Path, linked: &Path) {
        let cfg = serde_json::json!({
            "linkedProjects": [linked.to_string_lossy()]
        })
        .to_string();
        std::fs::write(root.join(".leanctx.json"), cfg).expect("write cfg");
    }

    #[test]
    fn linked_projects_outside_root_are_rejected_without_allow_path() {
        let _guard = ENV_LOCK.lock().expect("lock");
        let root = tempfile::tempdir().expect("root");
        let other = tempfile::tempdir().expect("other");

        write_linked_config(root.path(), other.path());

        std::env::remove_var("LEAN_CTX_ALLOW_PATH");
        let res = load_linked_projects(root.path());
        assert!(res.roots.is_empty());
        assert!(
            res.warnings
                .iter()
                .any(|w| w.contains("rejected by pathjail")),
            "expected pathjail warning, got: {:?}",
            res.warnings
        );
    }

    #[test]
    fn linked_projects_outside_root_are_allowed_with_allow_path() {
        let _guard = ENV_LOCK.lock().expect("lock");
        let root = tempfile::tempdir().expect("root");
        let other = tempfile::tempdir().expect("other");

        write_linked_config(root.path(), other.path());

        std::env::set_var(
            "LEAN_CTX_ALLOW_PATH",
            other.path().to_string_lossy().to_string(),
        );
        let res = load_linked_projects(root.path());
        assert_eq!(res.roots.len(), 1);
        assert_eq!(res.roots[0], other.path().canonicalize().expect("canon"));

        std::env::remove_var("LEAN_CTX_ALLOW_PATH");
    }
}
