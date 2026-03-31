use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ultra_compact: bool,
    pub tee_on_error: bool,
    pub checkpoint_interval: u32,
    pub excluded_commands: Vec<String>,
    pub custom_aliases: Vec<AliasEntry>,
    /// Commands taking longer than this threshold (ms) are recorded in the slow log.
    /// Set to 0 to disable slow logging.
    pub slow_command_threshold_ms: u64,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub cloud: CloudConfig,
}

fn default_theme() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CloudConfig {
    pub contribute_enabled: bool,
    pub last_contribute: Option<String>,
    pub last_sync: Option<String>,
    pub last_model_pull: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasEntry {
    pub command: String,
    pub alias: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ultra_compact: false,
            tee_on_error: false,
            checkpoint_interval: 15,
            excluded_commands: Vec::new(),
            custom_aliases: Vec::new(),
            slow_command_threshold_ms: 5000,
            theme: default_theme(),
            cloud: CloudConfig::default(),
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".lean-ctx").join("config.toml"))
    }

    pub fn load() -> Self {
        let path = match Self::path() {
            Some(p) => p,
            None => return Self::default(),
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("cannot determine home directory")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let content = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, content).map_err(|e| e.to_string())
    }

    pub fn show(&self) -> String {
        let path = Self::path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~/.lean-ctx/config.toml".to_string());
        let content = toml::to_string_pretty(self).unwrap_or_default();
        format!("Config: {path}\n\n{content}")
    }
}
