use crate::core::workflow::types::WorkflowRun;
use std::path::PathBuf;

fn active_workflow_path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("workflows").join("active.json"))
}

pub fn load_active() -> Result<Option<WorkflowRun>, String> {
    let Some(path) = active_workflow_path() else {
        return Ok(None);
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let run: WorkflowRun =
        serde_json::from_str(&content).map_err(|e| format!("Invalid workflow JSON: {e}"))?;
    Ok(Some(run))
}

pub fn save_active(run: &WorkflowRun) -> Result<(), String> {
    let Some(path) = active_workflow_path() else {
        return Err("No home directory available".to_string());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_string_pretty(run).map_err(|e| format!("serialize failed: {e}"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write failed: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename failed: {e}"))?;
    Ok(())
}

pub fn clear_active() -> Result<(), String> {
    let Some(path) = active_workflow_path() else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}
