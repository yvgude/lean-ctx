use std::path::Path;

use crate::core::graph_index::ProjectIndex;
use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_policy::MemoryPolicy;

const BOOTSTRAP_SESSION_ID: &str = "auto-bootstrap";
const BOOTSTRAP_CONFIDENCE: f32 = 0.95;

/// Seed a minimal set of *real*, deterministic facts so the dashboard Knowledge Graph
/// is never empty on a new project.
///
/// This does not use placeholders — it only derives values from the filesystem and/or index.
pub fn bootstrap_if_empty(
    knowledge: &mut ProjectKnowledge,
    project_root: &str,
    index: Option<&ProjectIndex>,
    policy: &MemoryPolicy,
) -> bool {
    if !knowledge.facts.is_empty() {
        return false;
    }

    let mut changed = false;

    // Always safe + real: makes the graph non-empty even for marker-less folders.
    changed |= remember_fact(knowledge, "workflow", "project_root", project_root, policy);

    if let Some(identity) = crate::core::project_hash::project_identity(project_root) {
        changed |= remember_fact(
            knowledge,
            "architecture",
            "project_identity",
            &identity,
            policy,
        );
        if let Some(url) = identity.strip_prefix("git:") {
            changed |= remember_fact(knowledge, "deployment", "git_remote", url, policy);
        }
    }

    let markers = detect_build_markers(project_root);
    if !markers.is_empty() {
        changed |= remember_fact(
            knowledge,
            "architecture",
            "build_markers",
            &markers.join(", "),
            policy,
        );
    }

    if let Some(idx) = index {
        let file_count = idx.files.len();
        let symbol_count = idx.symbols.len();
        let edge_count = idx.edges.len();
        changed |= remember_fact(
            knowledge,
            "workflow",
            "index_stats",
            &format!("files={file_count}, symbols={symbol_count}, edges={edge_count}"),
            policy,
        );

        if !idx.last_scan.trim().is_empty() {
            changed |= remember_fact(
                knowledge,
                "workflow",
                "index_last_scan",
                &idx.last_scan,
                policy,
            );
        }

        let (langs, total_tokens) = summarize_languages_and_tokens(idx);
        if !langs.is_empty() {
            changed |= remember_fact(knowledge, "architecture", "languages_top", &langs, policy);
        }
        if total_tokens > 0 {
            changed |= remember_fact(
                knowledge,
                "performance",
                "tokens_indexed",
                &total_tokens.to_string(),
                policy,
            );
        }
    }

    changed
}

fn remember_fact(
    knowledge: &mut ProjectKnowledge,
    category: &str,
    key: &str,
    value: &str,
    policy: &MemoryPolicy,
) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    knowledge.remember(
        category,
        key,
        value,
        BOOTSTRAP_SESSION_ID,
        BOOTSTRAP_CONFIDENCE,
        policy,
    );
    true
}

fn detect_build_markers(project_root: &str) -> Vec<&'static str> {
    let root = Path::new(project_root);
    let mut out: Vec<&'static str> = Vec::new();

    if root.join(".git").exists() {
        out.push("git");
    }
    if root.join("Cargo.toml").exists() {
        out.push("cargo");
    }
    if root.join("package.json").exists() {
        out.push("npm");
    }
    if root.join("pyproject.toml").exists() {
        out.push("python");
    }
    if root.join("go.mod").exists() {
        out.push("go");
    }
    if root.join("pom.xml").exists() {
        out.push("maven");
    }
    if root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
        out.push("gradle");
    }
    if root.join("CMakeLists.txt").exists() {
        out.push("cmake");
    }

    if let Ok(entries) = std::fs::read_dir(root) {
        if entries
            .flatten()
            .any(|e| e.path().extension().is_some_and(|ext| ext == "sln"))
        {
            out.push("dotnet");
        }
    }

    out
}

fn summarize_languages_and_tokens(index: &ProjectIndex) -> (String, u64) {
    let mut total_tokens: u64 = 0;
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();

    for f in index.files.values() {
        total_tokens = total_tokens.saturating_add(f.token_count as u64);
        let lang = if f.language.trim().is_empty() {
            "unknown"
        } else {
            f.language.as_str()
        };
        *counts.entry(lang).or_insert(0) += 1;
    }

    let mut entries: Vec<(&str, usize)> = counts.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let langs = entries
        .into_iter()
        .take(6)
        .map(|(lang, count)| format!("{lang}:{count}"))
        .collect::<Vec<_>>()
        .join(", ");

    (langs, total_tokens)
}
