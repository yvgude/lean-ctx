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

#[derive(Debug, Default, Serialize)]
pub struct ResolvedArtifacts {
    pub artifacts: Vec<ResolvedArtifact>,
    pub warnings: Vec<String>,
}

pub fn load_resolved(project_root: &Path) -> ResolvedArtifacts {
    let mut out = ResolvedArtifacts::default();
    // Must go through the same canonicalizer as the jail below, otherwise the
    // two sides disagree on Windows (verbatim prefix, 8.3 short names, case)
    // and `strip_prefix` silently keeps absolute paths as index keys.
    let root_canon = crate::core::pathutil::canonicalize_secure_or_self(project_root);

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
        let candidate = resolve_candidate(project_root, raw);

        let abs = match crate::core::io_boundary::jail_and_check_path(
            "artifacts",
            &candidate,
            project_root,
        ) {
            Ok((p, _)) => p,
            Err(e) => {
                out.warnings
                    .push(format!("artifact path rejected ({name}): {raw} ({e})"));
                continue;
            }
        };

        // Secret-like paths are denied by default for artifacts unless explicitly allowed.
        // Artifacts tend to be indexed/shared; prefer safety over convenience.
        let role = crate::core::roles::active_role();
        if !role.io.allow_secret_paths
            && let Some(reason) = crate::core::io_boundary::is_secret_like(&abs)
        {
            let role_name = crate::core::roles::active_role_name();
            let msg = format!(
                "artifact rejected ({name}): {raw} (secret-like path: {reason}; role: {role_name})"
            );
            crate::core::events::emit_policy_violation(&role_name, "artifacts", &msg);
            out.warnings.push(msg);
            continue;
        }

        let (exists, is_dir) = match abs.metadata() {
            Ok(m) => (true, m.is_dir()),
            Err(_) => (false, false),
        };

        // Forward slashes for index keys / display on every platform.
        let rel_out = abs
            .strip_prefix(&root_canon)
            .unwrap_or(&abs)
            .to_string_lossy()
            .replace('\\', "/");

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
    let new = project_root.join(".lean-ctx-artifacts.json");
    if let Ok(s) = std::fs::read_to_string(&new) {
        return Some((new, s));
    }
    let legacy = project_root.join(".leanctxcontextartifacts.json");
    if let Ok(s) = std::fs::read_to_string(&legacy) {
        return Some((legacy, s));
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

/// Resolve a registry `path` to the filesystem candidate handed to the jail.
///
/// Absolute paths and `~/…` stay absolute so external doc corpora (an Obsidian
/// vault, a shared `~/notes`) can be declared directly (GL#1132) — the PathJail
/// allow-list (`read_only_roots` / `extra_roots` / `LEAN_CTX_ALLOW_PATH`)
/// remains the security gate, so an absolute entry outside the allowed roots is
/// still rejected. Legacy compatibility: a leading slash historically meant
/// "project-relative" (it was stripped), so when the stripped reading matches
/// an existing path inside the project it wins over the absolute reading.
fn resolve_candidate(project_root: &Path, raw: &str) -> PathBuf {
    let expanded = crate::core::pathjail::expand_user_path(raw.trim());
    if expanded.is_absolute() {
        let legacy = project_root.join(normalize_rel_path(raw));
        if legacy.exists() {
            return legacy;
        }
        return expanded;
    }
    project_root.join(normalize_rel_path(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "no-jail"))]
    fn write_registry(root: &Path, body: &str) {
        std::fs::write(root.join(".lean-ctx-artifacts.json"), body).unwrap();
    }

    #[test]
    fn resolve_candidate_keeps_relative_paths_project_scoped() {
        let root = Path::new("/proj");
        assert_eq!(
            resolve_candidate(root, "./docs/notes"),
            PathBuf::from("/proj/docs/notes")
        );
        assert_eq!(resolve_candidate(root, "docs"), PathBuf::from("/proj/docs"));
    }

    #[test]
    fn resolve_candidate_prefers_legacy_reading_for_existing_project_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        // "/docs" used to mean "<root>/docs"; that reading still wins while
        // the project path exists.
        assert_eq!(
            resolve_candidate(dir.path(), "/docs"),
            dir.path().join("docs")
        );
    }

    #[test]
    fn resolve_candidate_keeps_external_absolute_paths() {
        let project = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let raw = external.path().join("vault");
        std::fs::create_dir_all(&raw).unwrap();
        let got = resolve_candidate(project.path(), raw.to_str().unwrap());
        assert_eq!(got, raw);
    }

    // Verifies jail *behavior*, so it cannot run when the `no-jail` feature
    // compiles the jail out entirely (CI runs tests with --all-features).
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn external_corpus_requires_allow_list() {
        let _g = crate::core::data_dir::test_env_lock();
        let project = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let vault = external.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("note.md"), "# Vault note\nrotation policy").unwrap();

        write_registry(
            project.path(),
            &format!(
                r#"{{"artifacts":[{{"name":"vault","path":"{}","description":"notes"}}]}}"#,
                vault.display()
            ),
        );

        // Without an allow-list entry the external path is rejected by the jail.
        crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");
        let denied = load_resolved(project.path());
        assert!(denied.artifacts.is_empty(), "{:?}", denied.artifacts);
        assert!(
            denied.warnings.iter().any(|w| w.contains("rejected")),
            "{:?}",
            denied.warnings
        );

        // Allow-listing the folder (config `read_only_roots`/`extra_roots` or
        // this env var) makes it a first-class corpus.
        crate::test_env::set_var("LEAN_CTX_ALLOW_PATH", external.path());
        let allowed = load_resolved(project.path());
        crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");

        assert_eq!(allowed.artifacts.len(), 1, "{:?}", allowed.warnings);
        let a = &allowed.artifacts[0];
        assert!(a.exists);
        assert!(a.is_dir);
        assert!(Path::new(&a.path).is_absolute());
    }
}
