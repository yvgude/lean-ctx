use super::model::{Gotcha, GotchaStore};
use std::path::PathBuf;

impl GotchaStore {
    pub fn load(project_root: &str) -> Self {
        let hash = crate::core::project_hash::hash_project_root(project_root);
        let path = gotcha_path(&hash);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(mut store) = serde_json::from_str::<GotchaStore>(&content) {
                store.apply_decay();
                store.pending_errors = Vec::new();
                return store;
            }
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

pub fn load_universal_gotchas() -> Vec<Gotcha> {
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
