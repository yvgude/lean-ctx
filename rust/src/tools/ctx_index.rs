use std::path::Path;

#[must_use]
pub fn handle(action: &str, project_root: &Path) -> String {
    match action {
        "status" => {
            crate::core::index_orchestrator::status_json(project_root.to_string_lossy().as_ref())
        }
        "build" => {
            crate::core::index_orchestrator::ensure_all_background(
                project_root.to_string_lossy().as_ref(),
            );
            "started".to_string()
        }
        _ => "Unknown action. Use: status, build".to_string(),
    }
}
