use super::model::{Gotcha, GotchaStore, MAX_GOTCHAS, MAX_PENDING};
use std::path::PathBuf;

impl GotchaStore {
    pub fn load(project_root: &str) -> Self {
        let capacity = crate::core::config::Config::load()
            .memory_policy_effective()
            .map_or(MAX_GOTCHAS, |policy| policy.gotcha.max_gotchas_per_project);
        let mut store = Self::load_unbounded(project_root);
        store.capacity = capacity;
        store
    }

    fn load_unbounded(project_root: &str) -> Self {
        let hash = crate::core::project_hash::hash_project_root(project_root);
        let path = gotcha_path(&hash);
        // The project's identity changed and moved its hash: the gotchas of
        // the partition whose knowledge records this path are its own.
        if !path.exists()
            && let Some(old) =
                crate::core::legacy_adoption::drifted_knowledge_partition(project_root)
        {
            let _ = crate::core::legacy_adoption::copy_knowledge_partition(&old, project_root);
        }
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(mut store) = serde_json::from_str::<GotchaStore>(&content)
        {
            store.project_hash.clone_from(&hash);
            store.apply_decay();
            store.cross_session_boost();
            // #451 shell-ding: keep persisted pending errors so a fail→fix that
            // spans two `lean-ctx -c` processes still correlates — but drop
            // expired ones (15-min TTL) and bound to the most recent MAX_PENDING
            // so a stale file can never grow unbounded or resurrect old errors.
            store.pending_errors.retain(|p| !p.is_expired());
            if store.pending_errors.len() > MAX_PENDING {
                let excess = store.pending_errors.len() - MAX_PENDING;
                store.pending_errors.drain(0..excess);
            }
            return store;
        }
        Self::new(&hash)
    }

    pub fn save(&self, project_root: &str) -> Result<(), String> {
        let hash = crate::core::project_hash::hash_project_root(project_root);
        let path = gotcha_path(&hash);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("tmp");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        if universal_gotchas_enabled() {
            let universal = self.extract_universal();
            if !universal.is_empty() {
                let _ = save_universal_gotchas(&universal);
            }
        }
        Ok(())
    }
}

fn gotcha_path(project_hash: &str) -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("knowledge")
        .join(project_hash)
        .join("gotchas.json")
}

// ---------------------------------------------------------------------------
// Universal gotchas (cross-project)
// ---------------------------------------------------------------------------

/// `boundary_policy.universal_gotchas_enabled` as configured. Off, no
/// gotcha leaves its project: none is published to or read from the
/// cross-project file.
fn universal_gotchas_enabled() -> bool {
    crate::core::config::Config::load_arc()
        .boundary_policy
        .universal_gotchas_enabled
}

pub fn load_universal_gotchas() -> Vec<Gotcha> {
    if !universal_gotchas_enabled() {
        return Vec::new();
    }
    let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return Vec::new();
    };
    let path = dir.join("universal-gotchas.json");
    if let Ok(content) = std::fs::read_to_string(&path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    }
}

pub fn save_universal_gotchas(gotchas: &[Gotcha]) -> Result<(), String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?;
    let path = dir.join("universal-gotchas.json");
    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(gotchas).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_errors_persist_across_load_for_cross_process_correlation() {
        // #451 shell-ding: in `lean-ctx -c` mode every command is its own
        // process, so the fix correlation can only work if pending errors are
        // persisted. Simulate process 1 (failing build) → process 2 (green run).
        let _iso = crate::core::data_dir::isolated_data_dir();
        let root = "/tmp/lean-ctx-gotcha-persist-test-xyzzy";

        let mut p1 = GotchaStore::load(root);
        assert!(p1.learn_from_shell(
            "cargo build",
            "error[E0382]: borrow of moved value: `x`",
            1,
            &[],
            "p1",
        ));
        assert_eq!(p1.pending_errors.len(), 1);
        p1.save(root).unwrap();

        // Fresh process must SEE the persisted pending error (was cleared before).
        let mut p2 = GotchaStore::load(root);
        assert_eq!(
            p2.pending_errors.len(),
            1,
            "pending error must survive a reload for cross-process correlation"
        );
        let changed = p2.learn_from_shell(
            "cargo build",
            "Finished `dev` profile [unoptimized + debuginfo]",
            0,
            &["src/main.rs".into()],
            "p2",
        );
        assert!(
            changed,
            "green run must correlate the persisted pending error"
        );
        assert_eq!(p2.gotchas.len(), 1, "fix correlates across processes");
        assert_eq!(p2.pending_errors.len(), 0, "pending consumed by the fix");
    }

    /// AP05-G10: the switch read the built-in default (always on), so a
    /// configured `universal_gotchas_enabled = false` never stopped gotchas
    /// from crossing projects.
    #[test]
    fn disabled_universal_gotchas_are_neither_read_nor_written() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            "[boundary_policy]\nuniversal_gotchas_enabled = false\n",
        )
        .unwrap();
        let foreign = Gotcha::new(
            super::super::model::GotchaCategory::Build,
            super::super::model::GotchaSeverity::Critical,
            "foreign-project-trigger",
            "foreign fix",
            super::super::model::GotchaSource::AgentReported {
                session_id: "s".into(),
            },
            "s",
        );
        save_universal_gotchas(std::slice::from_ref(&foreign)).unwrap();
        assert!(load_universal_gotchas().is_empty());
    }
}
