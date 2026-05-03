use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRegistry {
    pub artifacts: Vec<ArtifactSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSpec {
    pub name: String,
    pub path: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedArtifact {
    pub name: String,
    pub path: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub exists: bool,
    pub is_dir: bool,
}

#[derive(Debug, Default)]
pub struct ResolvedArtifacts {
    pub artifacts: Vec<ResolvedArtifact>,
    pub warnings: Vec<String>,
}

pub fn load_resolved(project_root: &Path) -> ResolvedArtifacts {
    let mut out = ResolvedArtifacts::default();
    let root_canon = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let Some((registry_path, content)) = read_registry_file(project_root) else {
        return out;
    };

    let parsed = parse_registry_json(&content).unwrap_or_else(|e| {
        out.warnings.push(format!(
            "artifact registry parse failed ({}): {e}",
            registry_path.display()
        ));
        ArtifactRegistry { artifacts: vec![] }
    });

    let mut seen = std::collections::HashSet::<String>::new();
    for spec in parsed.artifacts {
        let name = spec.name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if !seen.insert(name.clone()) {
            continue;
        }

        let raw = spec.path.trim();
        if raw.is_empty() {
            continue;
        }
        let rel = normalize_rel_path(raw);
        let candidate = if Path::new(&rel).is_absolute() {
            PathBuf::from(&rel)
        } else {
            project_root.join(&rel)
        };

        let abs = match crate::core::pathjail::jail_path(&candidate, project_root) {
            Ok(p) => p,
            Err(e) => {
                out.warnings
                    .push(format!("artifact path rejected ({name}): {rel} ({e})"));
                continue;
            }
        };

        let (exists, is_dir) = match abs.metadata() {
            Ok(m) => (true, m.is_dir()),
            Err(_) => (false, false),
        };

        let rel_out = abs
            .strip_prefix(&root_canon)
            .unwrap_or(&abs)
            .to_string_lossy()
            .to_string();

        out.artifacts.push(ResolvedArtifact {
            name,
            path: rel_out,
            description: spec.description.trim().to_string(),
            tags: spec.tags,
            exists,
            is_dir,
        });
    }

    out
}

fn read_registry_file(project_root: &Path) -> Option<(PathBuf, String)> {
    let lean = project_root.join(".leanctxcontextartifacts.json");
    if let Ok(s) = std::fs::read_to_string(&lean) {
        return Some((lean, s));
    }
    let socrati = project_root.join(".socraticodecontextartifacts.json");
    if let Ok(s) = std::fs::read_to_string(&socrati) {
        return Some((socrati, s));
    }
    None
}

fn parse_registry_json(content: &str) -> Result<ArtifactRegistry, String> {
    if let Ok(reg) = serde_json::from_str::<ArtifactRegistry>(content) {
        return Ok(reg);
    }
    if let Ok(list) = serde_json::from_str::<Vec<ArtifactSpec>>(content) {
        return Ok(ArtifactRegistry { artifacts: list });
    }
    Err("invalid JSON schema (expected { artifacts: [...] } or [...])".to_string())
}

fn normalize_rel_path(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s.trim_start_matches(['/', '\\']).to_string()
}
