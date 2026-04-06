use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TeeMode {
    Never,
    #[default]
    Failures,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ultra_compact: bool,
    #[serde(default, deserialize_with = "deserialize_tee_mode")]
    pub tee_mode: TeeMode,
    pub checkpoint_interval: u32,
    pub excluded_commands: Vec<String>,
    pub passthrough_urls: Vec<String>,
    pub custom_aliases: Vec<AliasEntry>,
    /// Commands taking longer than this threshold (ms) are recorded in the slow log.
    /// Set to 0 to disable slow logging.
    pub slow_command_threshold_ms: u64,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub cloud: CloudConfig,
    #[serde(default)]
    pub autonomy: AutonomyConfig,
    #[serde(default = "default_buddy_enabled")]
    pub buddy_enabled: bool,
    #[serde(default)]
    pub redirect_exclude: Vec<String>,
}

fn default_buddy_enabled() -> bool {
    true
}

fn deserialize_tee_mode<'de, D>(deserializer: D) -> Result<TeeMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(deserializer)?;
    match &v {
        serde_json::Value::Bool(true) => Ok(TeeMode::Failures),
        serde_json::Value::Bool(false) => Ok(TeeMode::Never),
        serde_json::Value::String(s) => match s.as_str() {
            "never" => Ok(TeeMode::Never),
            "failures" => Ok(TeeMode::Failures),
            "always" => Ok(TeeMode::Always),
            other => Err(D::Error::custom(format!("unknown tee_mode: {other}"))),
        },
        _ => Err(D::Error::custom("tee_mode must be string or bool")),
    }
}

fn default_theme() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomyConfig {
    pub enabled: bool,
    pub auto_preload: bool,
    pub auto_dedup: bool,
    pub auto_related: bool,
    pub silent_preload: bool,
    pub dedup_threshold: usize,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_preload: true,
            auto_dedup: true,
            auto_related: true,
            silent_preload: true,
            dedup_threshold: 8,
        }
    }
}

impl AutonomyConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("LEAN_CTX_AUTONOMY") {
            if v == "false" || v == "0" {
                cfg.enabled = false;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_PRELOAD") {
            cfg.auto_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_DEDUP") {
            cfg.auto_dedup = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_RELATED") {
            cfg.auto_related = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_SILENT_PRELOAD") {
            cfg.silent_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_DEDUP_THRESHOLD") {
            if let Ok(n) = v.parse() {
                cfg.dedup_threshold = n;
            }
        }
        cfg
    }

    pub fn load() -> Self {
        let file_cfg = Config::load().autonomy;
        let mut cfg = file_cfg;
        if let Ok(v) = std::env::var("LEAN_CTX_AUTONOMY") {
            if v == "false" || v == "0" {
                cfg.enabled = false;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_PRELOAD") {
            cfg.auto_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_DEDUP") {
            cfg.auto_dedup = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_RELATED") {
            cfg.auto_related = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_SILENT_PRELOAD") {
            cfg.silent_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_DEDUP_THRESHOLD") {
            if let Ok(n) = v.parse() {
                cfg.dedup_threshold = n;
            }
        }
        cfg
    }
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
            tee_mode: TeeMode::default(),
            checkpoint_interval: 15,
            excluded_commands: Vec::new(),
            passthrough_urls: Vec::new(),
            custom_aliases: Vec::new(),
            slow_command_threshold_ms: 5000,
            theme: default_theme(),
            cloud: CloudConfig::default(),
            autonomy: AutonomyConfig::default(),
            buddy_enabled: default_buddy_enabled(),
            redirect_exclude: Vec::new(),
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".lean-ctx").join("config.toml"))
    }

    pub fn load() -> Self {
        static CACHE: Mutex<Option<(Config, SystemTime)>> = Mutex::new(None);

        let path = match Self::path() {
            Some(p) => p,
            None => return Self::default(),
        };

        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        if let Ok(guard) = CACHE.lock() {
            if let Some((ref cfg, ref cached_mtime)) = *guard {
                if *cached_mtime == mtime {
                    return cfg.clone();
                }
            }
        }

        let cfg = match std::fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        };

        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some((cfg.clone(), mtime));
        }

        cfg
    }

    pub fn save(&self) -> std::result::Result<(), super::error::LeanCtxError> {
        let path = Self::path().ok_or_else(|| {
            super::error::LeanCtxError::Config("cannot determine home directory".into())
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| super::error::LeanCtxError::Config(e.to_string()))?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    pub fn show(&self) -> String {
        let path = Self::path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~/.lean-ctx/config.toml".to_string());
        let content = toml::to_string_pretty(self).unwrap_or_default();
        format!("Config: {path}\n\n{content}")
    }
}
