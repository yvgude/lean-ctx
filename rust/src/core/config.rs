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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TerseAgent {
    #[default]
    Off,
    Lite,
    Full,
    Ultra,
}

impl TerseAgent {
    pub fn from_env() -> Self {
        match std::env::var("LEAN_CTX_TERSE_AGENT")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "lite" => Self::Lite,
            "full" => Self::Full,
            "ultra" => Self::Ultra,
            "off" | "0" | "false" => Self::Off,
            _ => Self::Off,
        }
    }

    pub fn effective(config_val: &TerseAgent) -> Self {
        match std::env::var("LEAN_CTX_TERSE_AGENT") {
            Ok(val) if !val.is_empty() => match val.to_lowercase().as_str() {
                "lite" => Self::Lite,
                "full" => Self::Full,
                "ultra" => Self::Ultra,
                _ => Self::Off,
            },
            _ => config_val.clone(),
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OutputDensity {
    #[default]
    Normal,
    Terse,
    Ultra,
}

impl OutputDensity {
    pub fn from_env() -> Self {
        match std::env::var("LEAN_CTX_OUTPUT_DENSITY")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "terse" => Self::Terse,
            "ultra" => Self::Ultra,
            _ => Self::Normal,
        }
    }

    pub fn effective(config_val: &OutputDensity) -> Self {
        let env_val = Self::from_env();
        if env_val != Self::Normal {
            return env_val;
        }
        config_val.clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ultra_compact: bool,
    #[serde(default, deserialize_with = "deserialize_tee_mode")]
    pub tee_mode: TeeMode,
    #[serde(default)]
    pub output_density: OutputDensity,
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
    /// Tools to exclude from the MCP tool list returned by list_tools.
    /// Accepts exact tool names (e.g. ["ctx_graph", "ctx_agent"]).
    /// Empty by default — all tools listed, no behaviour change.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub loop_detection: LoopDetectionConfig,
    /// Controls where lean-ctx installs agent rule files.
    /// Values: "both" (default), "global" (home-dir only), "project" (repo-local only).
    /// Override via LEAN_CTX_RULES_SCOPE env var.
    #[serde(default)]
    pub rules_scope: Option<String>,
    /// Extra glob patterns to ignore in graph/overview/preload (repo-local).
    /// Example: ["externals/**", "target/**", "temp/**"]
    #[serde(default)]
    pub extra_ignore_patterns: Vec<String>,
    /// Controls agent output verbosity via instructions injection.
    /// Values: "off" (default), "lite", "full", "ultra".
    /// Override via LEAN_CTX_TERSE_AGENT env var.
    #[serde(default)]
    pub terse_agent: TerseAgent,
    /// Archive configuration for zero-loss compression.
    #[serde(default)]
    pub archive: ArchiveConfig,
    /// Additional paths allowed by PathJail (absolute).
    /// Useful for multi-project workspaces where the jail root is a parent directory.
    /// Override via LEAN_CTX_ALLOW_PATH env var (path-list separator).
    #[serde(default)]
    pub allow_paths: Vec<String>,
    /// Enable content-defined chunking (Rabin-Karp) for cache-optimal output ordering.
    /// Stable chunks are emitted first to maximize prompt cache hits.
    #[serde(default)]
    pub content_defined_chunking: bool,
    /// Skip session/knowledge/gotcha blocks in MCP instructions to minimize token overhead.
    /// Override via LEAN_CTX_MINIMAL env var.
    #[serde(default)]
    pub minimal_overhead: bool,
    /// Disable shell hook injection (the _lc() function that wraps CLI commands).
    /// Override via LEAN_CTX_NO_HOOK env var.
    #[serde(default)]
    pub shell_hook_disabled: bool,
    /// Disable the daily version check against leanctx.com/version.txt.
    /// Override via LEAN_CTX_NO_UPDATE_CHECK env var.
    #[serde(default)]
    pub update_check_disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveConfig {
    pub enabled: bool,
    pub threshold_chars: usize,
    pub max_age_hours: u64,
    pub max_disk_mb: u64,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_chars: 4096,
            max_age_hours: 48,
            max_disk_mb: 500,
        }
    }
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
    pub auto_consolidate: bool,
    pub silent_preload: bool,
    pub dedup_threshold: usize,
    pub consolidate_every_calls: u32,
    pub consolidate_cooldown_secs: u64,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_preload: true,
            auto_dedup: true,
            auto_related: true,
            auto_consolidate: true,
            silent_preload: true,
            dedup_threshold: 8,
            consolidate_every_calls: 25,
            consolidate_cooldown_secs: 120,
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
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_CONSOLIDATE") {
            cfg.auto_consolidate = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_SILENT_PRELOAD") {
            cfg.silent_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_DEDUP_THRESHOLD") {
            if let Ok(n) = v.parse() {
                cfg.dedup_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_CONSOLIDATE_EVERY_CALLS") {
            if let Ok(n) = v.parse() {
                cfg.consolidate_every_calls = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_CONSOLIDATE_COOLDOWN_SECS") {
            if let Ok(n) = v.parse() {
                cfg.consolidate_cooldown_secs = n;
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
    pub last_gain_sync: Option<String>,
    pub last_model_pull: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasEntry {
    pub command: String,
    pub alias: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoopDetectionConfig {
    pub normal_threshold: u32,
    pub reduced_threshold: u32,
    pub blocked_threshold: u32,
    pub window_secs: u64,
    pub search_group_limit: u32,
}

impl Default for LoopDetectionConfig {
    fn default() -> Self {
        Self {
            normal_threshold: 2,
            reduced_threshold: 4,
            blocked_threshold: 6,
            window_secs: 300,
            search_group_limit: 10,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ultra_compact: false,
            tee_mode: TeeMode::default(),
            output_density: OutputDensity::default(),
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
            disabled_tools: Vec::new(),
            loop_detection: LoopDetectionConfig::default(),
            rules_scope: None,
            extra_ignore_patterns: Vec::new(),
            terse_agent: TerseAgent::default(),
            archive: ArchiveConfig::default(),
            allow_paths: Vec::new(),
            content_defined_chunking: false,
            minimal_overhead: false,
            shell_hook_disabled: false,
            update_check_disabled: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesScope {
    Both,
    Global,
    Project,
}

impl Config {
    pub fn rules_scope_effective(&self) -> RulesScope {
        let raw = std::env::var("LEAN_CTX_RULES_SCOPE")
            .ok()
            .or_else(|| self.rules_scope.clone())
            .unwrap_or_default();
        match raw.trim().to_lowercase().as_str() {
            "global" => RulesScope::Global,
            "project" => RulesScope::Project,
            _ => RulesScope::Both,
        }
    }

    fn parse_disabled_tools_env(val: &str) -> Vec<String> {
        val.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn disabled_tools_effective(&self) -> Vec<String> {
        if let Ok(val) = std::env::var("LEAN_CTX_DISABLED_TOOLS") {
            Self::parse_disabled_tools_env(&val)
        } else {
            self.disabled_tools.clone()
        }
    }

    pub fn minimal_overhead_effective(&self) -> bool {
        std::env::var("LEAN_CTX_MINIMAL").is_ok() || self.minimal_overhead
    }

    pub fn shell_hook_disabled_effective(&self) -> bool {
        std::env::var("LEAN_CTX_NO_HOOK").is_ok() || self.shell_hook_disabled
    }

    pub fn update_check_disabled_effective(&self) -> bool {
        std::env::var("LEAN_CTX_NO_UPDATE_CHECK").is_ok() || self.update_check_disabled
    }
}

#[cfg(test)]
mod disabled_tools_tests {
    use super::*;

    #[test]
    fn config_field_default_is_empty() {
        let cfg = Config::default();
        assert!(cfg.disabled_tools.is_empty());
    }

    #[test]
    fn effective_returns_config_field_when_no_env_var() {
        // Only meaningful when LEAN_CTX_DISABLED_TOOLS is unset; skip otherwise.
        if std::env::var("LEAN_CTX_DISABLED_TOOLS").is_ok() {
            return;
        }
        let cfg = Config {
            disabled_tools: vec!["ctx_graph".to_string(), "ctx_agent".to_string()],
            ..Default::default()
        };
        assert_eq!(
            cfg.disabled_tools_effective(),
            vec!["ctx_graph", "ctx_agent"]
        );
    }

    #[test]
    fn parse_env_basic() {
        let result = Config::parse_disabled_tools_env("ctx_graph,ctx_agent");
        assert_eq!(result, vec!["ctx_graph", "ctx_agent"]);
    }

    #[test]
    fn parse_env_trims_whitespace_and_skips_empty() {
        let result = Config::parse_disabled_tools_env(" ctx_graph , , ctx_agent ");
        assert_eq!(result, vec!["ctx_graph", "ctx_agent"]);
    }

    #[test]
    fn parse_env_single_entry() {
        let result = Config::parse_disabled_tools_env("ctx_graph");
        assert_eq!(result, vec!["ctx_graph"]);
    }

    #[test]
    fn parse_env_empty_string_returns_empty() {
        let result = Config::parse_disabled_tools_env("");
        assert!(result.is_empty());
    }

    #[test]
    fn disabled_tools_deserialization_defaults_to_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.disabled_tools.is_empty());
    }

    #[test]
    fn disabled_tools_deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"disabled_tools = ["ctx_graph", "ctx_agent"]"#).unwrap();
        assert_eq!(cfg.disabled_tools, vec!["ctx_graph", "ctx_agent"]);
    }
}

#[cfg(test)]
mod rules_scope_tests {
    use super::*;

    #[test]
    fn default_is_both() {
        let cfg = Config::default();
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn config_global() {
        let cfg = Config {
            rules_scope: Some("global".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Global);
    }

    #[test]
    fn config_project() {
        let cfg = Config {
            rules_scope: Some("project".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Project);
    }

    #[test]
    fn unknown_value_falls_back_to_both() {
        let cfg = Config {
            rules_scope: Some("nonsense".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn deserialization_none_by_default() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.rules_scope.is_none());
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"rules_scope = "project""#).unwrap();
        assert_eq!(cfg.rules_scope.as_deref(), Some("project"));
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Project);
    }
}

#[cfg(test)]
mod loop_detection_config_tests {
    use super::*;

    #[test]
    fn defaults_are_reasonable() {
        let cfg = LoopDetectionConfig::default();
        assert_eq!(cfg.normal_threshold, 2);
        assert_eq!(cfg.reduced_threshold, 4);
        assert_eq!(cfg.blocked_threshold, 6);
        assert_eq!(cfg.window_secs, 300);
        assert_eq!(cfg.search_group_limit, 10);
    }

    #[test]
    fn deserialization_defaults_when_missing() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.loop_detection.blocked_threshold, 6);
        assert_eq!(cfg.loop_detection.search_group_limit, 10);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(
            r#"
            [loop_detection]
            normal_threshold = 1
            reduced_threshold = 3
            blocked_threshold = 5
            window_secs = 120
            search_group_limit = 8
            "#,
        )
        .unwrap();
        assert_eq!(cfg.loop_detection.normal_threshold, 1);
        assert_eq!(cfg.loop_detection.reduced_threshold, 3);
        assert_eq!(cfg.loop_detection.blocked_threshold, 5);
        assert_eq!(cfg.loop_detection.window_secs, 120);
        assert_eq!(cfg.loop_detection.search_group_limit, 8);
    }

    #[test]
    fn partial_override_keeps_defaults() {
        let cfg: Config = toml::from_str(
            r#"
            [loop_detection]
            blocked_threshold = 10
            "#,
        )
        .unwrap();
        assert_eq!(cfg.loop_detection.blocked_threshold, 10);
        assert_eq!(cfg.loop_detection.normal_threshold, 2);
        assert_eq!(cfg.loop_detection.search_group_limit, 10);
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|d| d.join("config.toml"))
    }

    pub fn local_path(project_root: &str) -> PathBuf {
        PathBuf::from(project_root).join(".lean-ctx.toml")
    }

    fn find_project_root() -> Option<String> {
        let cwd = std::env::current_dir().ok();

        if let Some(root) =
            crate::core::session::SessionState::load_latest().and_then(|s| s.project_root)
        {
            let root_path = std::path::Path::new(&root);
            let cwd_is_under_root = cwd
                .as_ref()
                .map(|c| c.starts_with(root_path))
                .unwrap_or(false);
            let has_marker = root_path.join(".git").exists()
                || root_path.join("Cargo.toml").exists()
                || root_path.join("package.json").exists()
                || root_path.join("go.mod").exists()
                || root_path.join("pyproject.toml").exists()
                || root_path.join(".lean-ctx.toml").exists();

            if cwd_is_under_root || has_marker {
                return Some(root);
            }
        }

        if let Some(ref cwd) = cwd {
            let git_root = std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .current_dir(cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout)
                            .ok()
                            .map(|s| s.trim().to_string())
                    } else {
                        None
                    }
                });
            if let Some(root) = git_root {
                return Some(root);
            }
            return Some(cwd.to_string_lossy().to_string());
        }
        None
    }

    pub fn load() -> Self {
        static CACHE: Mutex<Option<(Config, SystemTime, Option<SystemTime>)>> = Mutex::new(None);

        let path = match Self::path() {
            Some(p) => p,
            None => return Self::default(),
        };

        let local_path = Self::find_project_root().map(|r| Self::local_path(&r));

        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let local_mtime = local_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());

        if let Ok(guard) = CACHE.lock() {
            if let Some((ref cfg, ref cached_mtime, ref cached_local_mtime)) = *guard {
                if *cached_mtime == mtime && *cached_local_mtime == local_mtime {
                    return cfg.clone();
                }
            }
        }

        let mut cfg: Config = match std::fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        };

        if let Some(ref lp) = local_path {
            if let Ok(local_content) = std::fs::read_to_string(lp) {
                cfg.merge_local(&local_content);
            }
        }

        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some((cfg.clone(), mtime, local_mtime));
        }

        cfg
    }

    fn merge_local(&mut self, local_toml: &str) {
        let local: Config = match toml::from_str(local_toml) {
            Ok(c) => c,
            Err(_) => return,
        };
        if local.ultra_compact {
            self.ultra_compact = true;
        }
        if local.tee_mode != TeeMode::default() {
            self.tee_mode = local.tee_mode;
        }
        if local.output_density != OutputDensity::default() {
            self.output_density = local.output_density;
        }
        if local.checkpoint_interval != 15 {
            self.checkpoint_interval = local.checkpoint_interval;
        }
        if !local.excluded_commands.is_empty() {
            self.excluded_commands.extend(local.excluded_commands);
        }
        if !local.passthrough_urls.is_empty() {
            self.passthrough_urls.extend(local.passthrough_urls);
        }
        if !local.custom_aliases.is_empty() {
            self.custom_aliases.extend(local.custom_aliases);
        }
        if local.slow_command_threshold_ms != 5000 {
            self.slow_command_threshold_ms = local.slow_command_threshold_ms;
        }
        if local.theme != "default" {
            self.theme = local.theme;
        }
        if !local.buddy_enabled {
            self.buddy_enabled = false;
        }
        if !local.redirect_exclude.is_empty() {
            self.redirect_exclude.extend(local.redirect_exclude);
        }
        if !local.disabled_tools.is_empty() {
            self.disabled_tools.extend(local.disabled_tools);
        }
        if !local.extra_ignore_patterns.is_empty() {
            self.extra_ignore_patterns
                .extend(local.extra_ignore_patterns);
        }
        if local.rules_scope.is_some() {
            self.rules_scope = local.rules_scope;
        }
        if !local.autonomy.enabled {
            self.autonomy.enabled = false;
        }
        if !local.autonomy.auto_preload {
            self.autonomy.auto_preload = false;
        }
        if !local.autonomy.auto_dedup {
            self.autonomy.auto_dedup = false;
        }
        if !local.autonomy.auto_related {
            self.autonomy.auto_related = false;
        }
        if !local.autonomy.auto_consolidate {
            self.autonomy.auto_consolidate = false;
        }
        if local.autonomy.silent_preload {
            self.autonomy.silent_preload = true;
        }
        if !local.autonomy.silent_preload && self.autonomy.silent_preload {
            self.autonomy.silent_preload = false;
        }
        if local.autonomy.dedup_threshold != AutonomyConfig::default().dedup_threshold {
            self.autonomy.dedup_threshold = local.autonomy.dedup_threshold;
        }
        if local.autonomy.consolidate_every_calls
            != AutonomyConfig::default().consolidate_every_calls
        {
            self.autonomy.consolidate_every_calls = local.autonomy.consolidate_every_calls;
        }
        if local.autonomy.consolidate_cooldown_secs
            != AutonomyConfig::default().consolidate_cooldown_secs
        {
            self.autonomy.consolidate_cooldown_secs = local.autonomy.consolidate_cooldown_secs;
        }
        if local.terse_agent != TerseAgent::default() {
            self.terse_agent = local.terse_agent;
        }
        if !local.archive.enabled {
            self.archive.enabled = false;
        }
        if local.archive.threshold_chars != ArchiveConfig::default().threshold_chars {
            self.archive.threshold_chars = local.archive.threshold_chars;
        }
        if local.archive.max_age_hours != ArchiveConfig::default().max_age_hours {
            self.archive.max_age_hours = local.archive.max_age_hours;
        }
        if local.archive.max_disk_mb != ArchiveConfig::default().max_disk_mb {
            self.archive.max_disk_mb = local.archive.max_disk_mb;
        }
        if !local.allow_paths.is_empty() {
            self.allow_paths.extend(local.allow_paths);
        }
        if local.minimal_overhead {
            self.minimal_overhead = true;
        }
        if local.shell_hook_disabled {
            self.shell_hook_disabled = true;
        }
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
        let global_path = Self::path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~/.lean-ctx/config.toml".to_string());
        let content = toml::to_string_pretty(self).unwrap_or_default();
        let mut out = format!("Global config: {global_path}\n\n{content}");

        if let Some(root) = Self::find_project_root() {
            let local = Self::local_path(&root);
            if local.exists() {
                out.push_str(&format!("\n\nLocal config (merged): {}\n", local.display()));
            } else {
                out.push_str(&format!(
                    "\n\nLocal config: not found (create {} to override per-project)\n",
                    local.display()
                ));
            }
        }
        out
    }
}
