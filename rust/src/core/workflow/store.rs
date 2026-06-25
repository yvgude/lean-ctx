use crate::core::workflow::types::WorkflowRun;
use std::path::PathBuf;

/// Stale threshold: workflows inactive for over 30 minutes are auto-cleared on load.
const STALE_MINUTES: i64 = 30;

/// TTL for expired workflow files (24 hours).
pub const WORKFLOW_TTL_SECS: u64 = 24 * 60 * 60;

fn workflows_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("workflows"))
}

fn workflow_path_for_agent(agent_id: Option<&str>) -> Option<PathBuf> {
    let dir = workflows_dir()?;
    let filename = match agent_id {
        Some(id) if !id.trim().is_empty() => {
            let safe_id: String = id
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("workflow-{safe_id}.json")
        }
        _ => "active.json".to_string(),
    };
    Some(dir.join(filename))
}

pub fn load_active() -> Result<Option<WorkflowRun>, String> {
    load_active_for_agent(None)
}

pub fn load_active_for_agent(agent_id: Option<&str>) -> Result<Option<WorkflowRun>, String> {
    let Some(path) = workflow_path_for_agent(agent_id) else {
        return Ok(None);
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Backward compat: if agent-scoped file missing, try legacy active.json (read-only migration)
            if agent_id.is_some()
                && let Some(legacy) = workflow_path_for_agent(None)
                && let Ok(lc) = std::fs::read_to_string(&legacy)
            {
                let run: WorkflowRun = serde_json::from_str(&lc)
                    .map_err(|e| format!("Invalid legacy workflow JSON: {e}"))?;
                let elapsed = chrono::Utc::now()
                    .signed_duration_since(run.updated_at)
                    .num_minutes();
                if elapsed <= STALE_MINUTES && run.current != "done" {
                    return Ok(Some(run));
                }
            }
            return Ok(None);
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let run: WorkflowRun =
        serde_json::from_str(&content).map_err(|e| format!("Invalid workflow JSON: {e}"))?;

    let elapsed = chrono::Utc::now()
        .signed_duration_since(run.updated_at)
        .num_minutes();
    if elapsed > STALE_MINUTES || run.current == "done" {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(run))
}

pub fn save_active(run: &WorkflowRun) -> Result<(), String> {
    save_active_for_agent(run, None)
}

pub fn save_active_for_agent(run: &WorkflowRun, agent_id: Option<&str>) -> Result<(), String> {
    let Some(path) = workflow_path_for_agent(agent_id) else {
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
    clear_active_for_agent(None)
}

pub fn clear_active_for_agent(agent_id: Option<&str>) -> Result<(), String> {
    let Some(path) = workflow_path_for_agent(agent_id) else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}

/// Remove workflow files older than `WORKFLOW_TTL_SECS`.
#[must_use]
pub fn cleanup_expired() -> (u32, u64) {
    let Some(dir) = workflows_dir() else {
        return (0, 0);
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (0, 0);
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0u32;
    let mut freed = 0u64;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("json") {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map_or(0, |d| d.as_secs());
        if age > WORKFLOW_TTL_SECS {
            freed += meta.len();
            let _ = std::fs::remove_file(&path);
            removed += 1;
        }
    }
    (removed, freed)
}
