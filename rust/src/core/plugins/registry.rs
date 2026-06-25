use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::manifest::{ManifestError, PluginManifest};

#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: PluginManifest,
    pub enabled: bool,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct PluginRegistry {
    plugins: HashMap<String, Plugin>,
    plugin_dir: PathBuf,
    state_file: PathBuf,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct PluginState {
    #[serde(default)]
    disabled: Vec<String>,
}

impl PluginRegistry {
    #[must_use]
    pub fn new(plugin_dir: PathBuf) -> Self {
        let state_file = plugin_dir.join("plugin-state.json");
        Self {
            plugins: HashMap::new(),
            plugin_dir,
            state_file,
        }
    }

    #[must_use]
    pub fn from_default_dir() -> Self {
        let dir = default_plugin_dir();
        Self::new(dir)
    }

    pub fn discover(&mut self) -> Vec<DiscoveryError> {
        let mut errors = Vec::new();
        self.plugins.clear();

        let state = self.load_state();

        let Ok(entries) = std::fs::read_dir(&self.plugin_dir) else {
            return errors;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("plugin.toml");
            if !manifest_path.exists() {
                continue;
            }

            match PluginManifest::from_file(&manifest_path) {
                Ok(manifest) => {
                    let name = manifest.plugin.name.clone();
                    let enabled = !state.disabled.contains(&name);
                    self.plugins.insert(
                        name,
                        Plugin {
                            manifest,
                            enabled,
                            path,
                        },
                    );
                }
                Err(e) => {
                    errors.push(DiscoveryError {
                        path: manifest_path,
                        error: e,
                    });
                }
            }
        }

        errors
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins.get(name)
    }

    #[must_use]
    pub fn list(&self) -> Vec<&Plugin> {
        let mut plugins: Vec<_> = self.plugins.values().collect();
        plugins.sort_by(|a, b| a.manifest.plugin.name.cmp(&b.manifest.plugin.name));
        plugins
    }

    #[must_use]
    pub fn enabled_plugins(&self) -> Vec<&Plugin> {
        self.list().into_iter().filter(|p| p.enabled).collect()
    }

    pub fn enable(&mut self, name: &str) -> Result<(), RegistryError> {
        let plugin = self
            .plugins
            .get_mut(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_string()))?;
        plugin.enabled = true;
        self.save_state();
        Ok(())
    }

    pub fn disable(&mut self, name: &str) -> Result<(), RegistryError> {
        let plugin = self
            .plugins
            .get_mut(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_string()))?;
        plugin.enabled = false;
        self.save_state();
        Ok(())
    }

    #[must_use]
    pub fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }

    fn load_state(&self) -> PluginState {
        std::fs::read_to_string(&self.state_file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_state(&self) {
        let disabled: Vec<String> = self
            .plugins
            .iter()
            .filter(|(_, p)| !p.enabled)
            .map(|(name, _)| name.clone())
            .collect();
        let state = PluginState { disabled };
        let _ = std::fs::create_dir_all(&self.plugin_dir);
        let _ = std::fs::write(
            &self.state_file,
            serde_json::to_string_pretty(&state).unwrap_or_default(),
        );
    }
}

/// The directory the registry scans for plugin sub-directories.
///
/// `LEAN_CTX_PLUGINS_DIR` (the *root* containing plugin folders) overrides the
/// default so containers, CI, and tests can point at an isolated location. Note
/// this is distinct from the per-hook `LEAN_CTX_PLUGIN_DIR` the executor sets
/// for a *single* plugin's child process.
#[must_use]
pub fn default_plugin_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LEAN_CTX_PLUGINS_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Some(config_dir) = dirs::config_dir() {
        config_dir.join("lean-ctx").join("plugins")
    } else {
        PathBuf::from("~/.config/lean-ctx/plugins")
    }
}

#[derive(Debug)]
pub struct DiscoveryError {
    pub path: PathBuf,
    pub error: ManifestError,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("plugin not found: {0}")]
    NotFound(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();

        let plugin_a = dir.path().join("plugin-a");
        fs::create_dir_all(&plugin_a).unwrap();
        fs::write(
            plugin_a.join("plugin.toml"),
            r#"
[plugin]
name = "plugin-a"
version = "1.0.0"
description = "First plugin"

[hooks.on_session_start]
command = "plugin-a-bin start"
"#,
        )
        .unwrap();

        let plugin_b = dir.path().join("plugin-b");
        fs::create_dir_all(&plugin_b).unwrap();
        fs::write(
            plugin_b.join("plugin.toml"),
            r#"
[plugin]
name = "plugin-b"
version = "0.2.0"
description = "Second plugin"
author = "Test"

[hooks.pre_read]
command = "plugin-b-bin pre-read"
timeout_ms = 2000
"#,
        )
        .unwrap();

        dir
    }

    #[test]
    fn discover_finds_plugins() {
        let dir = setup_test_dir();
        let mut registry = PluginRegistry::new(dir.path().to_path_buf());
        let errors = registry.discover();
        assert!(errors.is_empty());
        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn enable_disable_persists() {
        let dir = setup_test_dir();
        let mut registry = PluginRegistry::new(dir.path().to_path_buf());
        registry.discover();

        registry.disable("plugin-a").unwrap();
        assert!(!registry.get("plugin-a").unwrap().enabled);

        let mut registry2 = PluginRegistry::new(dir.path().to_path_buf());
        registry2.discover();
        assert!(!registry2.get("plugin-a").unwrap().enabled);
        assert!(registry2.get("plugin-b").unwrap().enabled);

        registry2.enable("plugin-a").unwrap();
        assert!(registry2.get("plugin-a").unwrap().enabled);
    }

    #[test]
    fn not_found_error() {
        let dir = setup_test_dir();
        let mut registry = PluginRegistry::new(dir.path().to_path_buf());
        registry.discover();
        let err = registry.enable("nonexistent").unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn skips_dirs_without_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let empty_dir = dir.path().join("no-manifest");
        fs::create_dir_all(&empty_dir).unwrap();

        let mut registry = PluginRegistry::new(dir.path().to_path_buf());
        let errors = registry.discover();
        assert!(errors.is_empty());
        assert!(registry.list().is_empty());
    }

    #[test]
    fn reports_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        let bad_plugin = dir.path().join("bad-plugin");
        fs::create_dir_all(&bad_plugin).unwrap();
        fs::write(bad_plugin.join("plugin.toml"), "not valid toml [[[").unwrap();

        let mut registry = PluginRegistry::new(dir.path().to_path_buf());
        let errors = registry.discover();
        assert_eq!(errors.len(), 1);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn enabled_plugins_filter() {
        let dir = setup_test_dir();
        let mut registry = PluginRegistry::new(dir.path().to_path_buf());
        registry.discover();
        registry.disable("plugin-b").unwrap();
        let enabled = registry.enabled_plugins();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].manifest.plugin.name, "plugin-a");
    }
}
