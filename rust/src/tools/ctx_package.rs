//! `ctx_package` business logic (#293): save/resume portable context packages.

use std::path::Path;

use crate::core::context_package;
use crate::core::session::SessionState;

pub fn handle(
    project_root: &str,
    session: Option<&SessionState>,
    action: &str,
    path: Option<&str>,
    agent_id: Option<&str>,
    description: Option<&str>,
) -> String {
    match action.trim() {
        "save" => handle_save(project_root, session, path, agent_id, description),
        "resume" => handle_resume(project_root, session, path),
        "list" => handle_list(project_root),
        "info" => handle_info(path),
        other => format!(
            "ERR: unknown package action '{other}'. Use: save | resume <path> | list | info <path>"
        ),
    }
}

fn handle_save(
    project_root: &str,
    session: Option<&SessionState>,
    path: Option<&str>,
    agent_id: Option<&str>,
    description: Option<&str>,
) -> String {
    let Some(session) = session else {
        return "ERR: no active session to save".to_string();
    };
    let output_path = path.map(Path::new);
    match context_package::save_package(session, project_root, agent_id, description, output_path) {
        Ok(p) => format!("package saved: {}", p.display()),
        Err(e) => format!("ERR: {e}"),
    }
}

fn handle_resume(project_root: &str, session: Option<&SessionState>, path: Option<&str>) -> String {
    let Some(path_str) = path else {
        return "ERR: resume requires a path to the .ctx.json package".to_string();
    };
    let pkg_path = Path::new(path_str);
    if !pkg_path.exists() {
        // Try the default packages directory.
        let hash = crate::core::project_hash::hash_project_root(project_root);
        let alt = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from(".lean-ctx"))
            .join("packages")
            .join(hash)
            .join(path_str);
        if !alt.exists() {
            return format!("ERR: package not found: {path_str}");
        }
        return do_resume(session, &alt);
    }
    do_resume(session, pkg_path)
}

fn do_resume(session: Option<&SessionState>, path: &Path) -> String {
    let mut target = match session {
        Some(base) => base.clone(),
        None => SessionState::new(),
    };
    match context_package::resume_package(&mut target, path) {
        Ok(report) => report.format(),
        Err(e) => format!("ERR: {e}"),
    }
}

/// A package's summary line carries stored text (its description, or the
/// task description of the session it was saved from). A package file has no
/// source binding, so the line is served under the originless rule (G04):
/// withheld while protection is active in any layer, re-admitted under
/// today's content rules otherwise. Counts and the file name are metadata.
fn served_summary(pkg: &context_package::ContextPackage) -> String {
    crate::tools::admitted_sources::serve_originless(&pkg.summary_line(), "package summary")
        .unwrap_or_else(|reason| format!("[withheld: {reason}]"))
}

fn handle_list(project_root: &str) -> String {
    let hash = crate::core::project_hash::hash_project_root(project_root);
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".lean-ctx"))
        .join("packages")
        .join(hash);
    if !dir.exists() {
        return "No saved packages yet.".to_string();
    }
    let mut entries: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json")
                && let Ok(json) = std::fs::read_to_string(&p)
                && let Ok(pkg) = serde_json::from_str::<context_package::ContextPackage>(&json)
            {
                entries.push(format!(
                    "  {} — {}",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    served_summary(&pkg)
                ));
            }
        }
    }
    if entries.is_empty() {
        return "No saved packages yet.".to_string();
    }
    entries.sort();
    format!("packages ({}):\n{}", entries.len(), entries.join("\n"))
}

fn handle_info(path: Option<&str>) -> String {
    let Some(path_str) = path else {
        return "ERR: info requires a path".to_string();
    };
    let p = Path::new(path_str);
    if !p.exists() {
        return format!("ERR: not found: {path_str}");
    }
    match std::fs::read_to_string(p) {
        Ok(json) => match serde_json::from_str::<context_package::ContextPackage>(&json) {
            Ok(pkg) => format!(
                "format_version: {}\ncreated: {}\nproject: {}\n{}",
                pkg.format_version,
                pkg.created_at.format("%Y-%m-%d %H:%M"),
                pkg.project_root,
                served_summary(&pkg)
            ),
            Err(e) => format!("ERR: parse: {e}"),
        },
        Err(e) => format!("ERR: read: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::policy::runtime::TestPolicyOverride;

    /// G04: `info` and `list` printed a package's stored description as it
    /// was saved. A package has no source binding, so its text follows the
    /// originless rule: served in Community developer mode, withheld once a
    /// project policy is active.
    #[test]
    fn a_package_summary_is_served_under_the_originless_rule() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let file = project.path().join("handover.ctx.json");
        let file_str = file.to_string_lossy().into_owned();
        {
            let _community = TestPolicyOverride::set(None);
            let saved = handle(
                &root,
                Some(&SessionState::new()),
                "save",
                Some(&file_str),
                None,
                Some("widget release handover"),
            );
            assert!(saved.starts_with("package saved: "), "{saved}");
            let info = handle_info(Some(&file_str));
            assert!(info.contains("widget release handover"), "{info}");
        }
        let policy = crate::core::policy::load(
            "name = \"ctx-package-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n",
        )
        .unwrap();
        let _policy = TestPolicyOverride::set(Some(policy));
        let info = handle_info(Some(&file_str));
        assert!(info.starts_with("format_version: "), "{info}");
        assert!(info.contains("[withheld: "), "{info}");
        assert!(!info.contains("widget release handover"), "{info}");
    }
}
