use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CONFIG_FILENAME: &str = "rules.toml";
const CONFIG_DIR: &str = ".lean-ctx";
const LEGACY_CONFIG_DIR: &str = ".leanctx";

/// User-facing rules inventory persisted at `.lean-ctx/rules.toml`.
///
/// **Scope (read this before wiring it into anything):** `rules.toml` is the input
/// for `rules lint` (cross-agent consistency checks) and a user-editable export
/// produced by `rules init`. It is deliberately **not** consumed by `rules sync`
/// or `rules diff` — those regenerate from the canonical `rules_canonical` source
/// of truth and preserve user text around the markers. `core.content` here is a
/// snapshot captured at `init` time for inspection/linting, never an override that
/// `sync` would distribute (#548).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesConfig {
    pub rules: RulesSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesSection {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub core: CoreRules,
    #[serde(default)]
    pub agent: std::collections::HashMap<String, AgentRules>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoreRules {
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRules {
    #[serde(default)]
    pub extra: String,
}

fn default_version() -> String {
    "1.0".to_string()
}

impl RulesConfig {
    pub fn config_path(project_root: &Path) -> PathBuf {
        let new_path = project_root.join(CONFIG_DIR).join(CONFIG_FILENAME);
        if new_path.exists() {
            return new_path;
        }
        let legacy = project_root.join(LEGACY_CONFIG_DIR).join(CONFIG_FILENAME);
        if legacy.exists() {
            tracing::info!(
                "found legacy config at {}, consider renaming {} → {}",
                legacy.display(),
                LEGACY_CONFIG_DIR,
                CONFIG_DIR
            );
            return legacy;
        }
        new_path
    }

    pub fn load(project_root: &Path) -> Result<Self, String> {
        let path = Self::config_path(project_root);
        if !path.exists() {
            return Err(format!(
                "No rules config found at {}. Run `lean-ctx rules init` to create one.",
                path.display()
            ));
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        toml::from_str(&content).map_err(|e| format!("Failed to parse {}: {e}", path.display()))
    }

    pub fn init_from_existing(project_root: &Path, home: &Path) -> Result<Self, String> {
        let statuses = crate::rules_inject::collect_rules_status(home);

        let mut agent_rules = std::collections::HashMap::new();
        for status in &statuses {
            if status.state == "up_to_date" || status.state == "outdated" {
                let key = status.name.to_lowercase().replace(' ', "_");
                let path = Path::new(&status.path);
                if let Ok(content) = std::fs::read_to_string(path) {
                    let extra = extract_user_content(&content);
                    if !extra.is_empty() {
                        agent_rules.insert(key, AgentRules { extra });
                    }
                }
            }
        }

        let config = RulesConfig {
            rules: RulesSection {
                version: default_version(),
                core: CoreRules {
                    content: crate::rules_inject::rules_shared_content().clone(),
                },
                agent: agent_rules,
            },
        };

        let path = Self::config_path(project_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
        }
        let toml_str = toml::to_string_pretty(&config)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;
        std::fs::write(&path, &toml_str)
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;

        Ok(config)
    }
}

fn extract_user_content(content: &str) -> String {
    let start = content.find(crate::core::rules_canonical::START_MARK);
    let end = content.find(crate::core::rules_canonical::END_MARK);

    match (start, end) {
        (Some(s), Some(e)) => {
            let before = content[..s].trim();
            let after_end = e + crate::core::rules_canonical::END_MARK.len();
            let after = content[after_end..].trim();
            let mut parts = Vec::new();
            if !before.is_empty() {
                parts.push(before.to_string());
            }
            if !after.is_empty() {
                parts.push(after.to_string());
            }
            parts.join("\n\n")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_version_is_1_0() {
        assert_eq!(default_version(), "1.0");
    }

    #[test]
    fn config_path_defaults_to_new_dir() {
        let root = PathBuf::from("/tmp/project_nonexistent_ctx_test");
        let path = RulesConfig::config_path(&root);
        assert_eq!(
            path,
            PathBuf::from("/tmp/project_nonexistent_ctx_test/.lean-ctx/rules.toml")
        );
    }

    #[test]
    fn config_path_falls_back_to_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(".leanctx");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("rules.toml"),
            "[rules]\nversion = \"1.0\"\n[rules.core]\ncontent = \"\"",
        )
        .unwrap();

        let path = RulesConfig::config_path(dir.path());
        assert!(
            path.to_string_lossy().contains(".leanctx"),
            "should fall back to legacy .leanctx when .lean-ctx doesn't exist"
        );
    }

    #[test]
    fn config_path_prefers_new_over_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(".leanctx");
        let new_dir = dir.path().join(".lean-ctx");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(legacy.join("rules.toml"), "legacy").unwrap();
        std::fs::write(new_dir.join("rules.toml"), "new").unwrap();

        let path = RulesConfig::config_path(dir.path());
        assert!(
            path.components().any(|c| c.as_os_str() == ".lean-ctx"),
            "should prefer .lean-ctx over legacy .leanctx when both exist; got {}",
            path.display()
        );
    }

    #[test]
    fn load_missing_file_returns_error() {
        let root = PathBuf::from("/tmp/nonexistent_contextops_test");
        let result = RulesConfig::load(&root);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No rules config found"));
    }

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[rules]
version = "1.0"

[rules.core]
content = "test rules"
"#;
        let config: RulesConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.rules.version, "1.0");
        assert_eq!(config.rules.core.content, "test rules");
        assert!(config.rules.agent.is_empty());
    }

    #[test]
    fn parse_config_with_agents() {
        let toml_str = r#"
[rules]
version = "1.0"

[rules.core]
content = "core rules"

[rules.agent.cursor]
extra = "cursor specific"

[rules.agent.claude]
extra = "claude specific"
"#;
        let config: RulesConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.rules.agent.len(), 2);
        assert_eq!(
            config.rules.agent.get("cursor").unwrap().extra,
            "cursor specific"
        );
    }

    #[test]
    fn extract_user_content_with_markers() {
        let content = format!(
            "user preamble\n\n{}\nrules here\n{}\n\nuser postamble",
            crate::core::rules_canonical::START_MARK,
            crate::core::rules_canonical::END_MARK
        );
        let result = extract_user_content(&content);
        assert!(result.contains("user preamble"));
        assert!(result.contains("user postamble"));
        assert!(!result.contains("rules here"));
    }

    #[test]
    fn extract_user_content_no_markers() {
        let result = extract_user_content("just some text");
        assert!(result.is_empty());
    }
}
