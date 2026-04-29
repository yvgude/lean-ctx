//! # Context Profiles
//!
//! Declarative, version-controlled context strategies ("Context as Code").
//!
//! Profiles configure how lean-ctx processes content for different scenarios:
//! exploration, bugfixing, hotfixes, CI debugging, code review, etc.
//!
//! ## Resolution Order
//!
//! 1. `LEAN_CTX_PROFILE` env var
//! 2. `.lean-ctx/profiles/<name>.toml` (project-local)
//! 3. `~/.lean-ctx/profiles/<name>.toml` (global)
//! 4. Built-in defaults (compiled into the binary)
//!
//! ## Inheritance
//!
//! Profiles can inherit from other profiles via `inherits = "parent"`.
//! Child values override parent values; unset fields fall through.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A complete context profile definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub profile: ProfileMeta,
    #[serde(default)]
    pub read: ReadConfig,
    #[serde(default)]
    pub compression: CompressionConfig,
    #[serde(default)]
    pub verification: crate::core::output_verification::VerificationConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub pipeline: PipelineConfig,
    #[serde(default)]
    pub autonomy: ProfileAutonomy,
}

/// Profile identity and inheritance.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileMeta {
    #[serde(default)]
    pub name: String,
    pub inherits: Option<String>,
    #[serde(default)]
    pub description: String,
}

/// Read behavior configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadConfig {
    #[serde(default = "default_read_mode")]
    pub default_mode: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens_per_file: usize,
    #[serde(default)]
    pub prefer_cache: bool,
}

fn default_read_mode() -> String {
    "auto".to_string()
}
fn default_max_tokens() -> usize {
    50_000
}

impl Default for ReadConfig {
    fn default() -> Self {
        Self {
            default_mode: default_read_mode(),
            max_tokens_per_file: default_max_tokens(),
            prefer_cache: false,
        }
    }
}

/// Compression strategy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    #[serde(default = "default_crp_mode")]
    pub crp_mode: String,
    #[serde(default = "default_output_density")]
    pub output_density: String,
    #[serde(default = "default_entropy_threshold")]
    pub entropy_threshold: f64,
}

fn default_crp_mode() -> String {
    "tdd".to_string()
}
fn default_output_density() -> String {
    "normal".to_string()
}
fn default_entropy_threshold() -> f64 {
    0.3
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            crp_mode: default_crp_mode(),
            output_density: default_output_density(),
            entropy_threshold: default_entropy_threshold(),
        }
    }
}

/// Token and cost budget limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    #[serde(default = "default_context_tokens")]
    pub max_context_tokens: usize,
    #[serde(default = "default_shell_invocations")]
    pub max_shell_invocations: usize,
    #[serde(default = "default_cost_usd")]
    pub max_cost_usd: f64,
}

fn default_context_tokens() -> usize {
    200_000
}
fn default_shell_invocations() -> usize {
    100
}
fn default_cost_usd() -> f64 {
    5.0
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: default_context_tokens(),
            max_shell_invocations: default_shell_invocations(),
            max_cost_usd: default_cost_usd(),
        }
    }
}

/// Pipeline layer activation per profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    #[serde(default = "default_true")]
    pub intent: bool,
    #[serde(default = "default_true")]
    pub relevance: bool,
    #[serde(default = "default_true")]
    pub compression: bool,
    #[serde(default = "default_true")]
    pub translation: bool,
}

fn default_true() -> bool {
    true
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            intent: true,
            relevance: true,
            compression: true,
            translation: true,
        }
    }
}

/// Autonomy overrides per profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileAutonomy {
    #[serde(default = "default_true")]
    pub auto_dedup: bool,
    #[serde(default = "default_checkpoint")]
    pub checkpoint_interval: u32,
}

fn default_checkpoint() -> u32 {
    15
}

impl Default for ProfileAutonomy {
    fn default() -> Self {
        Self {
            auto_dedup: true,
            checkpoint_interval: default_checkpoint(),
        }
    }
}

// ── Built-in Profiles ──────────────────────────────────────

fn builtin_exploration() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "exploration".to_string(),
            inherits: None,
            description: "Broad context for understanding codebases".to_string(),
        },
        read: ReadConfig {
            default_mode: "map".to_string(),
            max_tokens_per_file: 80_000,
            prefer_cache: true,
        },
        compression: CompressionConfig::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: 200_000,
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        autonomy: ProfileAutonomy::default(),
    }
}

fn builtin_bugfix() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "bugfix".to_string(),
            inherits: None,
            description: "Focused context for debugging specific issues".to_string(),
        },
        read: ReadConfig {
            default_mode: "auto".to_string(),
            max_tokens_per_file: 30_000,
            prefer_cache: false,
        },
        compression: CompressionConfig {
            crp_mode: "tdd".to_string(),
            output_density: "terse".to_string(),
            ..CompressionConfig::default()
        },
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: 100_000,
            max_shell_invocations: 50,
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        autonomy: ProfileAutonomy {
            checkpoint_interval: 10,
            ..ProfileAutonomy::default()
        },
    }
}

fn builtin_hotfix() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "hotfix".to_string(),
            inherits: None,
            description: "Minimal context, fast iteration for urgent fixes".to_string(),
        },
        read: ReadConfig {
            default_mode: "signatures".to_string(),
            max_tokens_per_file: 2_000,
            prefer_cache: true,
        },
        compression: CompressionConfig {
            crp_mode: "tdd".to_string(),
            output_density: "ultra".to_string(),
            ..CompressionConfig::default()
        },
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: 30_000,
            max_shell_invocations: 20,
            max_cost_usd: 1.0,
        },
        pipeline: PipelineConfig::default(),
        autonomy: ProfileAutonomy {
            checkpoint_interval: 5,
            ..ProfileAutonomy::default()
        },
    }
}

fn builtin_ci_debug() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "ci-debug".to_string(),
            inherits: None,
            description: "CI/CD debugging with shell-heavy workflows".to_string(),
        },
        read: ReadConfig {
            default_mode: "auto".to_string(),
            max_tokens_per_file: 50_000,
            prefer_cache: false,
        },
        compression: CompressionConfig {
            output_density: "terse".to_string(),
            ..CompressionConfig::default()
        },
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: 150_000,
            max_shell_invocations: 200,
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        autonomy: ProfileAutonomy::default(),
    }
}

fn builtin_review() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "review".to_string(),
            inherits: None,
            description: "Code review with broad read-only context".to_string(),
        },
        read: ReadConfig {
            default_mode: "map".to_string(),
            max_tokens_per_file: 60_000,
            prefer_cache: true,
        },
        compression: CompressionConfig {
            crp_mode: "compact".to_string(),
            ..CompressionConfig::default()
        },
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: 150_000,
            max_shell_invocations: 30,
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        autonomy: ProfileAutonomy::default(),
    }
}

/// Returns all built-in profile definitions.
pub fn builtin_profiles() -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    for p in [
        builtin_exploration(),
        builtin_bugfix(),
        builtin_hotfix(),
        builtin_ci_debug(),
        builtin_review(),
    ] {
        map.insert(p.profile.name.clone(), p);
    }
    map
}

// ── Loading ────────────────────────────────────────────────

fn profiles_dir_global() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("profiles"))
}

fn profiles_dir_project() -> Option<PathBuf> {
    let mut current = std::env::current_dir().ok()?;
    for _ in 0..12 {
        let candidate = current.join(".lean-ctx").join("profiles");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Loads a profile by name with full resolution:
/// 1. Project-local `.lean-ctx/profiles/<name>.toml`
/// 2. Global `~/.lean-ctx/profiles/<name>.toml`
/// 3. Built-in defaults
///
/// Applies inheritance chain (max depth 5 to prevent cycles).
pub fn load_profile(name: &str) -> Option<Profile> {
    load_profile_recursive(name, 0)
}

fn load_profile_recursive(name: &str, depth: usize) -> Option<Profile> {
    if depth > 5 {
        return None;
    }

    let mut profile = load_profile_from_disk(name).or_else(|| builtin_profiles().remove(name))?;
    profile.profile.name = name.to_string();

    if let Some(ref parent_name) = profile.profile.inherits.clone() {
        if let Some(parent) = load_profile_recursive(parent_name, depth + 1) {
            profile = merge_profiles(parent, profile);
        }
    }

    Some(profile)
}

fn load_profile_from_disk(name: &str) -> Option<Profile> {
    let filename = format!("{name}.toml");

    if let Some(project_dir) = profiles_dir_project() {
        let path = project_dir.join(&filename);
        if let Some(p) = try_load_toml(&path) {
            return Some(p);
        }
    }

    if let Some(global_dir) = profiles_dir_global() {
        let path = global_dir.join(&filename);
        if let Some(p) = try_load_toml(&path) {
            return Some(p);
        }
    }

    None
}

fn try_load_toml(path: &Path) -> Option<Profile> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

/// Merges parent into child: child values take precedence,
/// parent provides defaults for unspecified fields.
fn merge_profiles(parent: Profile, child: Profile) -> Profile {
    Profile {
        profile: ProfileMeta {
            name: child.profile.name,
            inherits: child.profile.inherits,
            description: if child.profile.description.is_empty() {
                parent.profile.description
            } else {
                child.profile.description
            },
        },
        read: child.read,
        compression: child.compression,
        verification: child.verification,
        budget: child.budget,
        pipeline: child.pipeline,
        autonomy: child.autonomy,
    }
}

/// Returns the currently active profile name from env or default.
pub fn active_profile_name() -> String {
    std::env::var("LEAN_CTX_PROFILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "exploration".to_string())
}

/// Loads the currently active profile.
pub fn active_profile() -> Profile {
    let name = active_profile_name();
    load_profile(&name).unwrap_or_else(builtin_exploration)
}

/// Sets the active profile for the current process by updating `LEAN_CTX_PROFILE`.
///
/// Returns the resolved profile after applying inheritance.
pub fn set_active_profile(name: &str) -> Result<Profile, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("profile name is empty".to_string());
    }
    let prev = active_profile_name();
    let profile = load_profile(name).ok_or_else(|| format!("profile '{name}' not found"))?;
    std::env::set_var("LEAN_CTX_PROFILE", name);
    if prev != name {
        crate::core::events::emit_profile_changed(&prev, name);
    }
    Ok(profile)
}

/// Lists all available profile names (built-in + on-disk).
pub fn list_profiles() -> Vec<ProfileInfo> {
    let mut profiles: HashMap<String, ProfileInfo> = HashMap::new();

    for (name, p) in builtin_profiles() {
        profiles.insert(
            name.clone(),
            ProfileInfo {
                name,
                description: p.profile.description,
                source: ProfileSource::Builtin,
            },
        );
    }

    for (source, dir) in [
        (ProfileSource::Global, profiles_dir_global()),
        (ProfileSource::Project, profiles_dir_project()),
    ] {
        if let Some(dir) = dir {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            let name = stem.to_string();
                            let desc = try_load_toml(&path)
                                .map(|p| p.profile.description)
                                .unwrap_or_default();
                            profiles.insert(
                                name.clone(),
                                ProfileInfo {
                                    name,
                                    description: desc,
                                    source,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    let mut result: Vec<ProfileInfo> = profiles.into_values().collect();
    result.sort_by_key(|p| p.name.clone());
    result
}

/// Information about an available profile.
#[derive(Debug, Clone)]
pub struct ProfileInfo {
    pub name: String,
    pub description: String,
    pub source: ProfileSource,
}

/// Where a profile was loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    Builtin,
    Global,
    Project,
}

impl std::fmt::Display for ProfileSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin => write!(f, "built-in"),
            Self::Global => write!(f, "global"),
            Self::Project => write!(f, "project"),
        }
    }
}

/// Formats a profile as TOML for display or file creation.
pub fn format_as_toml(profile: &Profile) -> String {
    toml::to_string_pretty(profile).unwrap_or_else(|_| "[error serializing profile]".to_string())
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles_has_five() {
        let builtins = builtin_profiles();
        assert_eq!(builtins.len(), 5);
        assert!(builtins.contains_key("exploration"));
        assert!(builtins.contains_key("bugfix"));
        assert!(builtins.contains_key("hotfix"));
        assert!(builtins.contains_key("ci-debug"));
        assert!(builtins.contains_key("review"));
    }

    #[test]
    fn hotfix_has_minimal_budget() {
        let p = builtin_profiles().remove("hotfix").unwrap();
        assert_eq!(p.budget.max_context_tokens, 30_000);
        assert_eq!(p.budget.max_shell_invocations, 20);
        assert_eq!(p.read.default_mode, "signatures");
        assert_eq!(p.compression.output_density, "ultra");
    }

    #[test]
    fn exploration_has_broad_context() {
        let p = builtin_profiles().remove("exploration").unwrap();
        assert_eq!(p.budget.max_context_tokens, 200_000);
        assert_eq!(p.read.default_mode, "map");
        assert!(p.read.prefer_cache);
    }

    #[test]
    fn profile_roundtrip_toml() {
        let original = builtin_exploration();
        let toml_str = format_as_toml(&original);
        let parsed: Profile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.profile.name, "exploration");
        assert_eq!(parsed.read.default_mode, "map");
        assert_eq!(parsed.budget.max_context_tokens, 200_000);
    }

    #[test]
    fn merge_child_overrides_parent() {
        let parent = builtin_exploration();
        let child = Profile {
            profile: ProfileMeta {
                name: "custom".to_string(),
                inherits: Some("exploration".to_string()),
                description: String::new(),
            },
            read: ReadConfig {
                default_mode: "signatures".to_string(),
                ..ReadConfig::default()
            },
            compression: CompressionConfig::default(),
            verification: crate::core::output_verification::VerificationConfig::default(),
            budget: BudgetConfig {
                max_context_tokens: 10_000,
                ..BudgetConfig::default()
            },
            pipeline: PipelineConfig::default(),
            autonomy: ProfileAutonomy::default(),
        };

        let merged = merge_profiles(parent, child);
        assert_eq!(merged.read.default_mode, "signatures");
        assert_eq!(merged.budget.max_context_tokens, 10_000);
        assert_eq!(
            merged.profile.description,
            "Broad context for understanding codebases"
        );
    }

    #[test]
    fn load_builtin_by_name() {
        let p = load_profile("hotfix").unwrap();
        assert_eq!(p.profile.name, "hotfix");
        assert_eq!(p.read.default_mode, "signatures");
    }

    #[test]
    fn load_nonexistent_returns_none() {
        assert!(load_profile("does-not-exist-xyz").is_none());
    }

    #[test]
    fn list_profiles_includes_builtins() {
        let list = list_profiles();
        assert!(list.len() >= 5);
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"exploration"));
        assert!(names.contains(&"hotfix"));
        assert!(names.contains(&"review"));
    }

    #[test]
    fn active_profile_defaults_to_exploration() {
        std::env::remove_var("LEAN_CTX_PROFILE");
        let p = active_profile();
        assert_eq!(p.profile.name, "exploration");
    }

    #[test]
    fn active_profile_from_env() {
        std::env::set_var("LEAN_CTX_PROFILE", "hotfix");
        let name = active_profile_name();
        assert_eq!(name, "hotfix");
        std::env::remove_var("LEAN_CTX_PROFILE");
    }

    #[test]
    fn profile_source_display() {
        assert_eq!(ProfileSource::Builtin.to_string(), "built-in");
        assert_eq!(ProfileSource::Global.to_string(), "global");
        assert_eq!(ProfileSource::Project.to_string(), "project");
    }

    #[test]
    fn default_profile_has_sane_values() {
        let p = Profile {
            profile: ProfileMeta::default(),
            read: ReadConfig::default(),
            compression: CompressionConfig::default(),
            verification: crate::core::output_verification::VerificationConfig::default(),
            budget: BudgetConfig::default(),
            pipeline: PipelineConfig::default(),
            autonomy: ProfileAutonomy::default(),
        };
        assert_eq!(p.read.default_mode, "auto");
        assert_eq!(p.compression.crp_mode, "tdd");
        assert_eq!(p.budget.max_context_tokens, 200_000);
        assert!(p.pipeline.compression);
        assert!(p.pipeline.intent);
    }

    #[test]
    fn pipeline_layers_configurable() {
        let toml_str = r#"
[profile]
name = "no-intent"

[pipeline]
intent = false
relevance = false
"#;
        let p: Profile = toml::from_str(toml_str).unwrap();
        assert!(!p.pipeline.intent);
        assert!(!p.pipeline.relevance);
        assert!(p.pipeline.compression);
        assert!(p.pipeline.translation);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let toml_str = r#"
[profile]
name = "minimal"

[read]
default_mode = "entropy"
"#;
        let p: Profile = toml::from_str(toml_str).unwrap();
        assert_eq!(p.read.default_mode, "entropy");
        assert_eq!(p.read.max_tokens_per_file, 50_000);
        assert_eq!(p.budget.max_context_tokens, 200_000);
        assert_eq!(p.compression.crp_mode, "tdd");
    }
}
