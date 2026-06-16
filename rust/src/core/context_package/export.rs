//! Export a context package from the current session (#293).

use std::path::{Path, PathBuf};

use chrono::Utc;

use super::bundle::{ContextPackage, FORMAT_VERSION, KnowledgeFact, PackageMetadata, SessionSlice};
use crate::core::session::SessionState;
use crate::core::session_summary;

/// Export the current session as a context package (JSON file).
///
/// Returns the path where the package was written.
pub fn save_package(
    session: &SessionState,
    project_root: &str,
    agent_id: Option<&str>,
    description: Option<&str>,
    output_path: Option<&Path>,
) -> Result<PathBuf, String> {
    let pkg = build_package(session, project_root, agent_id, description);
    let json = serde_json::to_string_pretty(&pkg).map_err(|e| e.to_string())?;

    let path = output_path.map_or_else(|| default_path(project_root, &session.id), PathBuf::from);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

fn build_package(
    session: &SessionState,
    project_root: &str,
    agent_id: Option<&str>,
    description: Option<&str>,
) -> ContextPackage {
    let summaries = session_summary::list(project_root);
    let knowledge = load_knowledge_facts(project_root);

    ContextPackage {
        format_version: FORMAT_VERSION,
        created_at: Utc::now(),
        project_root: project_root.to_string(),
        session_id: session.id.clone(),
        metadata: PackageMetadata {
            agent_id: agent_id.map(str::to_string),
            description: description.map(str::to_string),
            tool_calls: session.stats.total_tool_calls,
            tokens_saved: session.stats.total_tokens_saved,
        },
        session: SessionSlice {
            task: session.task.clone(),
            findings: session.findings.clone(),
            decisions: session.decisions.clone(),
            files: session.files_touched.clone(),
            next_steps: session.next_steps.clone(),
            test_results: session.test_results.clone(),
        },
        summaries,
        knowledge,
    }
}

fn load_knowledge_facts(project_root: &str) -> Vec<KnowledgeFact> {
    let Some(pk) = crate::core::knowledge::ProjectKnowledge::load(project_root) else {
        return Vec::new();
    };
    pk.facts
        .iter()
        .filter(|f| f.is_current() && f.confidence >= 0.5)
        .take(100)
        .map(|f| KnowledgeFact {
            category: f.category.clone(),
            key: f.key.clone(),
            value: f.value.clone(),
            confidence: f.confidence,
            created_at: f.created_at,
        })
        .collect()
}

fn default_path(project_root: &str, session_id: &str) -> PathBuf {
    let hash = crate::core::project_hash::hash_project_root(project_root);
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from(".lean-ctx"))
        .join("packages")
        .join(hash);
    let short = session_id.split('-').next().unwrap_or(session_id);
    dir.join(format!("{short}.ctx.json"))
}
