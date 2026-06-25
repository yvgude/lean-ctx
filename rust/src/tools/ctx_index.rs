use std::path::Path;

#[must_use]
pub fn handle(action: &str, project_root: &Path) -> String {
    match action {
        "status" => {
            crate::core::index_orchestrator::status_json(project_root.to_string_lossy().as_ref())
        }
        "build" => {
            // Indexes are SQLite-backed — no explicit build trigger needed.
            "started".to_string()
        }
        _ => "Unknown action. Use: status, build".to_string(),
    }
}
