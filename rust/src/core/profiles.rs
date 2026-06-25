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
use std::sync::RwLock;

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
    pub translation: TranslationConfig,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub memory: crate::core::memory_policy::MemoryPolicyOverrides,
    #[serde(default)]
    pub verification: crate::core::output_verification::VerificationConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub pipeline: PipelineConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub degradation: DegradationConfig,
    #[serde(default)]
    pub autonomy: ProfileAutonomy,
    #[serde(default)]
    pub output_hints: OutputHints,
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
///
/// Fields are `Option<T>` for field-level profile inheritance.
/// Use `_effective()` methods to get the resolved value with defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ReadConfig {
    pub default_mode: Option<String>,
    pub max_tokens_per_file: Option<usize>,
    pub prefer_cache: Option<bool>,
}

impl ReadConfig {
    #[must_use]
    pub fn default_mode_effective(&self) -> &str {
        self.default_mode.as_deref().unwrap_or("auto")
    }
    #[must_use]
    pub fn max_tokens_per_file_effective(&self) -> usize {
        self.max_tokens_per_file.unwrap_or(50_000)
    }
    #[must_use]
    pub fn prefer_cache_effective(&self) -> bool {
        self.prefer_cache.unwrap_or(false)
    }
}

/// Compression strategy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CompressionConfig {
    pub crp_mode: Option<String>,
    pub output_density: Option<String>,
    pub entropy_threshold: Option<f64>,
    pub terse_mode: Option<bool>,
}

impl CompressionConfig {
    #[must_use]
    pub fn crp_mode_effective(&self) -> &str {
        self.crp_mode.as_deref().unwrap_or("tdd")
    }
    #[must_use]
    pub fn output_density_effective(&self) -> &str {
        self.output_density.as_deref().unwrap_or("normal")
    }
    #[must_use]
    pub fn entropy_threshold_effective(&self) -> f64 {
        self.entropy_threshold.unwrap_or(0.3)
    }
    #[must_use]
    pub fn terse_mode_effective(&self) -> bool {
        self.terse_mode.unwrap_or(false)
    }
}

/// Translation (tokenizer-aware) configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TranslationConfig {
    /// If false, preserve legacy CRP/TDD formats without post-translation.
    pub enabled: Option<bool>,
    /// legacy|ascii|auto
    pub ruleset: Option<String>,
}

impl TranslationConfig {
    #[must_use]
    pub fn enabled_effective(&self) -> bool {
        self.enabled.unwrap_or(false)
    }
    #[must_use]
    pub fn ruleset_effective(&self) -> &str {
        self.ruleset.as_deref().unwrap_or("legacy")
    }
}

/// Layout (attention-aware reorder) configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LayoutConfig {
    /// If false, preserve original order.
    pub enabled: Option<bool>,
    /// Minimum line count for enabling reorder.
    pub min_lines: Option<usize>,
}

impl LayoutConfig {
    #[must_use]
    pub fn enabled_effective(&self) -> bool {
        self.enabled.unwrap_or(false)
    }
    #[must_use]
    pub fn min_lines_effective(&self) -> usize {
        self.min_lines.unwrap_or(15)
    }
}

/// Routing policy overrides (intent → model tier → read mode/budgets).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutingConfig {
    /// Hard cap for recommended model tier: fast|standard|premium.
    #[serde(default)]
    pub max_model_tier: Option<String>,
    /// If true, apply deterministic routing degradation under budget/pressure.
    #[serde(default)]
    pub degrade_under_pressure: Option<bool>,
}

impl RoutingConfig {
    #[must_use]
    pub fn max_model_tier_effective(&self) -> &str {
        self.max_model_tier.as_deref().unwrap_or("premium")
    }

    #[must_use]
    pub fn degrade_under_pressure_effective(&self) -> bool {
        self.degrade_under_pressure.unwrap_or(true)
    }
}

/// Budget/SLO degradation policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DegradationConfig {
    /// If true, enforce throttling/blocking decisions. Default is warn-only.
    #[serde(default)]
    pub enforce: Option<bool>,
    /// Throttle duration (ms) when policy verdict is Throttle. Default: 250ms.
    #[serde(default)]
    pub throttle_ms: Option<u64>,
}

impl DegradationConfig {
    #[must_use]
    pub fn enforce_effective(&self) -> bool {
        self.enforce.unwrap_or(false)
    }

    #[must_use]
    pub fn throttle_ms_effective(&self) -> u64 {
        self.throttle_ms.unwrap_or(250)
    }
}

/// Controls which optional hints/footers are appended to tool output.
/// All default to `false` for minimal output overhead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputHints {
    pub compressed_hint: Option<bool>,
    pub archive_hint: Option<bool>,
    pub verify_footer: Option<bool>,
    pub related_hint: Option<bool>,
    pub semantic_hint: Option<bool>,
    pub elicitation_hint: Option<bool>,
    pub checkpoint_in_output: Option<bool>,
    pub graph_context_block: Option<bool>,
    pub efficiency_hint: Option<bool>,
}

impl OutputHints {
    #[must_use]
    pub fn compressed_hint(&self) -> bool {
        self.compressed_hint.unwrap_or(false)
    }
    #[must_use]
    pub fn archive_hint(&self) -> bool {
        self.archive_hint.unwrap_or(false)
    }
    #[must_use]
    pub fn verify_footer(&self) -> bool {
        self.verify_footer.unwrap_or(false)
    }
    #[must_use]
    pub fn related_hint(&self) -> bool {
        self.related_hint.unwrap_or(false)
    }
    #[must_use]
    pub fn semantic_hint(&self) -> bool {
        self.semantic_hint.unwrap_or(false)
    }
    #[must_use]
    pub fn elicitation_hint(&self) -> bool {
        self.elicitation_hint.unwrap_or(false)
    }
    #[must_use]
    pub fn checkpoint_in_output(&self) -> bool {
        self.checkpoint_in_output.unwrap_or(false)
    }
    #[must_use]
    pub fn graph_context_block(&self) -> bool {
        self.graph_context_block.unwrap_or(false)
    }
    #[must_use]
    pub fn efficiency_hint(&self) -> bool {
        self.efficiency_hint.unwrap_or(false)
    }
}

/// Token and cost budget limits.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BudgetConfig {
    pub max_context_tokens: Option<usize>,
    pub max_shell_invocations: Option<usize>,
    pub max_cost_usd: Option<f64>,
}

impl BudgetConfig {
    #[must_use]
    pub fn max_context_tokens_effective(&self) -> usize {
        self.max_context_tokens.unwrap_or(200_000)
    }
    #[must_use]
    pub fn max_shell_invocations_effective(&self) -> usize {
        self.max_shell_invocations.unwrap_or(100)
    }
    #[must_use]
    pub fn max_cost_usd_effective(&self) -> f64 {
        self.max_cost_usd.unwrap_or(5.0)
    }
}

/// Pipeline layer activation per profile.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PipelineConfig {
    pub intent: Option<bool>,
    pub relevance: Option<bool>,
    pub compression: Option<bool>,
    pub translation: Option<bool>,
}

impl PipelineConfig {
    #[must_use]
    pub fn intent_effective(&self) -> bool {
        self.intent.unwrap_or(true)
    }
    #[must_use]
    pub fn relevance_effective(&self) -> bool {
        self.relevance.unwrap_or(true)
    }
    #[must_use]
    pub fn compression_effective(&self) -> bool {
        self.compression.unwrap_or(true)
    }
    #[must_use]
    pub fn translation_effective(&self) -> bool {
        self.translation.unwrap_or(true)
    }
}

/// Autonomy overrides per profile.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfileAutonomy {
    pub enabled: Option<bool>,
    pub auto_preload: Option<bool>,
    pub auto_dedup: Option<bool>,
    pub auto_related: Option<bool>,
    pub silent_preload: Option<bool>,
    /// Enable bounded prefetch after reads (opt-in by default).
    pub auto_prefetch: Option<bool>,
    /// Enable response shaping for large outputs (opt-in by default).
    pub auto_response: Option<bool>,
    pub dedup_threshold: Option<usize>,
    pub prefetch_max_files: Option<usize>,
    pub prefetch_budget_tokens: Option<usize>,
    pub response_min_tokens: Option<usize>,
    pub checkpoint_interval: Option<u32>,
}

impl ProfileAutonomy {
    #[must_use]
    pub fn enabled_effective(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    #[must_use]
    pub fn auto_preload_effective(&self) -> bool {
        self.auto_preload.unwrap_or(true)
    }
    #[must_use]
    pub fn auto_dedup_effective(&self) -> bool {
        self.auto_dedup.unwrap_or(true)
    }
    #[must_use]
    pub fn auto_related_effective(&self) -> bool {
        self.auto_related.unwrap_or(true)
    }
    #[must_use]
    pub fn silent_preload_effective(&self) -> bool {
        self.silent_preload.unwrap_or(true)
    }
    #[must_use]
    pub fn auto_prefetch_effective(&self) -> bool {
        self.auto_prefetch.unwrap_or(false)
    }
    #[must_use]
    pub fn auto_response_effective(&self) -> bool {
        self.auto_response.unwrap_or(false)
    }
    #[must_use]
    pub fn dedup_threshold_effective(&self) -> usize {
        self.dedup_threshold.unwrap_or(8)
    }
    #[must_use]
    pub fn prefetch_max_files_effective(&self) -> usize {
        self.prefetch_max_files.unwrap_or(3)
    }
    #[must_use]
    pub fn prefetch_budget_tokens_effective(&self) -> usize {
        self.prefetch_budget_tokens.unwrap_or(4000)
    }
    #[must_use]
    pub fn response_min_tokens_effective(&self) -> usize {
        self.response_min_tokens.unwrap_or(600)
    }
    #[must_use]
    pub fn checkpoint_interval_effective(&self) -> u32 {
        self.checkpoint_interval.unwrap_or(15)
    }
}

// ── Built-in Profiles ──────────────────────────────────────

fn builtin_coder() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "coder".to_string(),
            inherits: None,
            description: "Default coding workflow with guarded autonomy drivers".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("auto".to_string()),
            max_tokens_per_file: Some(50_000),
            prefer_cache: Some(true),
        },
        compression: CompressionConfig {
            crp_mode: Some("tdd".to_string()),
            output_density: Some("terse".to_string()),
            terse_mode: Some(true),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig {
            enabled: Some(true),
            ruleset: Some("auto".to_string()),
        },
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(150_000),
            max_shell_invocations: Some(100),
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig::default(),
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy {
            auto_prefetch: Some(true),
            auto_response: Some(true),
            checkpoint_interval: Some(10),
            ..ProfileAutonomy::default()
        },
        output_hints: OutputHints::default(),
    }
}

fn builtin_exploration() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "exploration".to_string(),
            inherits: None,
            description: "Broad context for understanding codebases".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("map".to_string()),
            max_tokens_per_file: Some(80_000),
            prefer_cache: Some(true),
        },
        compression: CompressionConfig {
            terse_mode: Some(true),
            output_density: Some("terse".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(200_000),
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig::default(),
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints {
            related_hint: Some(true),
            compressed_hint: Some(true),
            ..OutputHints::default()
        },
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
            default_mode: Some("auto".to_string()),
            max_tokens_per_file: Some(30_000),
            prefer_cache: Some(false),
        },
        compression: CompressionConfig {
            crp_mode: Some("tdd".to_string()),
            output_density: Some("terse".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(100_000),
            max_shell_invocations: Some(50),
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig {
            max_model_tier: Some("standard".to_string()),
            ..RoutingConfig::default()
        },
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy {
            checkpoint_interval: Some(10),
            ..ProfileAutonomy::default()
        },
        output_hints: OutputHints::default(),
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
            default_mode: Some("signatures".to_string()),
            max_tokens_per_file: Some(2_000),
            prefer_cache: Some(true),
        },
        compression: CompressionConfig {
            crp_mode: Some("tdd".to_string()),
            output_density: Some("ultra".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(30_000),
            max_shell_invocations: Some(20),
            max_cost_usd: Some(1.0),
        },
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig {
            max_model_tier: Some("fast".to_string()),
            ..RoutingConfig::default()
        },
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy {
            checkpoint_interval: Some(5),
            ..ProfileAutonomy::default()
        },
        output_hints: OutputHints::default(),
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
            default_mode: Some("auto".to_string()),
            max_tokens_per_file: Some(50_000),
            prefer_cache: Some(false),
        },
        compression: CompressionConfig {
            output_density: Some("terse".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(150_000),
            max_shell_invocations: Some(200),
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig {
            max_model_tier: Some("standard".to_string()),
            ..RoutingConfig::default()
        },
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints::default(),
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
            default_mode: Some("map".to_string()),
            max_tokens_per_file: Some(60_000),
            prefer_cache: Some(true),
        },
        compression: CompressionConfig {
            crp_mode: Some("compact".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig {
            enabled: Some(true),
            ..LayoutConfig::default()
        },
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(150_000),
            max_shell_invocations: Some(30),
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig {
            max_model_tier: Some("standard".to_string()),
            ..RoutingConfig::default()
        },
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints {
            verify_footer: Some(true),
            related_hint: Some(true),
            compressed_hint: Some(true),
            ..OutputHints::default()
        },
    }
}

fn builtin_passthrough() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "passthrough".to_string(),
            inherits: None,
            description: "No output modification — always full content, no compression".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("full".to_string()),
            max_tokens_per_file: Some(10_000_000),
            prefer_cache: Some(false),
        },
        compression: CompressionConfig {
            crp_mode: Some("off".to_string()),
            output_density: Some("normal".to_string()),
            entropy_threshold: None,
            terse_mode: Some(false),
        },
        translation: TranslationConfig {
            enabled: Some(false),
            ..TranslationConfig::default()
        },
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(1_000_000),
            ..BudgetConfig::default()
        },
        pipeline: PipelineConfig {
            intent: Some(false),
            relevance: Some(false),
            compression: Some(false),
            translation: Some(false),
        },
        routing: RoutingConfig::default(),
        degradation: DegradationConfig {
            enforce: Some(false),
            ..DegradationConfig::default()
        },
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints::default(),
    }
}

/// Returns all built-in profile definitions.
#[must_use]
pub fn builtin_profiles() -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    for p in [
        builtin_coder(),
        builtin_exploration(),
        builtin_bugfix(),
        builtin_hotfix(),
        builtin_ci_debug(),
        builtin_review(),
        builtin_passthrough(),
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
#[must_use]
pub fn load_profile(name: &str) -> Option<Profile> {
    load_profile_recursive(name, 0)
}

fn load_profile_recursive(name: &str, depth: usize) -> Option<Profile> {
    if depth > 5 {
        return None;
    }

    let mut profile = load_profile_from_disk(name).or_else(|| builtin_profiles().remove(name))?;
    profile.profile.name = name.to_string();

    if let Some(ref parent_name) = profile.profile.inherits.clone()
        && let Some(parent) = load_profile_recursive(parent_name, depth + 1)
    {
        profile = merge_profiles(parent, profile);
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
///
/// ALL sections are merged field-by-field using `Option::or()`.
/// A child profile only needs to set the fields it wants to override.
fn merge_profiles(parent: Profile, child: Profile) -> Profile {
    let read = ReadConfig {
        default_mode: child.read.default_mode.or(parent.read.default_mode),
        max_tokens_per_file: child
            .read
            .max_tokens_per_file
            .or(parent.read.max_tokens_per_file),
        prefer_cache: child.read.prefer_cache.or(parent.read.prefer_cache),
    };
    let compression = CompressionConfig {
        crp_mode: child.compression.crp_mode.or(parent.compression.crp_mode),
        output_density: child
            .compression
            .output_density
            .or(parent.compression.output_density),
        entropy_threshold: child
            .compression
            .entropy_threshold
            .or(parent.compression.entropy_threshold),
        terse_mode: child
            .compression
            .terse_mode
            .or(parent.compression.terse_mode),
    };
    let translation = TranslationConfig {
        enabled: child.translation.enabled.or(parent.translation.enabled),
        ruleset: child.translation.ruleset.or(parent.translation.ruleset),
    };
    let layout = LayoutConfig {
        enabled: child.layout.enabled.or(parent.layout.enabled),
        min_lines: child.layout.min_lines.or(parent.layout.min_lines),
    };
    let memory = crate::core::memory_policy::MemoryPolicyOverrides {
        knowledge: crate::core::memory_policy::KnowledgePolicyOverrides {
            max_facts: child
                .memory
                .knowledge
                .max_facts
                .or(parent.memory.knowledge.max_facts),
            max_patterns: child
                .memory
                .knowledge
                .max_patterns
                .or(parent.memory.knowledge.max_patterns),
            max_history: child
                .memory
                .knowledge
                .max_history
                .or(parent.memory.knowledge.max_history),
            contradiction_threshold: child
                .memory
                .knowledge
                .contradiction_threshold
                .or(parent.memory.knowledge.contradiction_threshold),
            recall_facts_limit: child
                .memory
                .knowledge
                .recall_facts_limit
                .or(parent.memory.knowledge.recall_facts_limit),
            rooms_limit: child
                .memory
                .knowledge
                .rooms_limit
                .or(parent.memory.knowledge.rooms_limit),
            timeline_limit: child
                .memory
                .knowledge
                .timeline_limit
                .or(parent.memory.knowledge.timeline_limit),
            relations_limit: child
                .memory
                .knowledge
                .relations_limit
                .or(parent.memory.knowledge.relations_limit),
        },
        lifecycle: crate::core::memory_policy::LifecyclePolicyOverrides {
            decay_rate: child
                .memory
                .lifecycle
                .decay_rate
                .or(parent.memory.lifecycle.decay_rate),
            low_confidence_threshold: child
                .memory
                .lifecycle
                .low_confidence_threshold
                .or(parent.memory.lifecycle.low_confidence_threshold),
            stale_days: child
                .memory
                .lifecycle
                .stale_days
                .or(parent.memory.lifecycle.stale_days),
            similarity_threshold: child
                .memory
                .lifecycle
                .similarity_threshold
                .or(parent.memory.lifecycle.similarity_threshold),
            forgetting_model: child
                .memory
                .lifecycle
                .forgetting_model
                .clone()
                .or_else(|| parent.memory.lifecycle.forgetting_model.clone()),
            base_stability_days: child
                .memory
                .lifecycle
                .base_stability_days
                .or(parent.memory.lifecycle.base_stability_days),
            archetype_aware_decay: child
                .memory
                .lifecycle
                .archetype_aware_decay
                .or(parent.memory.lifecycle.archetype_aware_decay),
        },
    };
    let verification = crate::core::output_verification::VerificationConfig {
        enabled: child.verification.enabled.or(parent.verification.enabled),
        mode: child.verification.mode.or(parent.verification.mode),
        strict_mode: child
            .verification
            .strict_mode
            .or(parent.verification.strict_mode),
        check_paths: child
            .verification
            .check_paths
            .or(parent.verification.check_paths),
        check_identifiers: child
            .verification
            .check_identifiers
            .or(parent.verification.check_identifiers),
        check_line_numbers: child
            .verification
            .check_line_numbers
            .or(parent.verification.check_line_numbers),
        check_structure: child
            .verification
            .check_structure
            .or(parent.verification.check_structure),
    };
    let budget = BudgetConfig {
        max_context_tokens: child
            .budget
            .max_context_tokens
            .or(parent.budget.max_context_tokens),
        max_shell_invocations: child
            .budget
            .max_shell_invocations
            .or(parent.budget.max_shell_invocations),
        max_cost_usd: child.budget.max_cost_usd.or(parent.budget.max_cost_usd),
    };
    let pipeline = PipelineConfig {
        intent: child.pipeline.intent.or(parent.pipeline.intent),
        relevance: child.pipeline.relevance.or(parent.pipeline.relevance),
        compression: child.pipeline.compression.or(parent.pipeline.compression),
        translation: child.pipeline.translation.or(parent.pipeline.translation),
    };
    let routing = RoutingConfig {
        max_model_tier: child
            .routing
            .max_model_tier
            .or(parent.routing.max_model_tier),
        degrade_under_pressure: child
            .routing
            .degrade_under_pressure
            .or(parent.routing.degrade_under_pressure),
    };
    let degradation = DegradationConfig {
        enforce: child.degradation.enforce.or(parent.degradation.enforce),
        throttle_ms: child
            .degradation
            .throttle_ms
            .or(parent.degradation.throttle_ms),
    };
    let autonomy = ProfileAutonomy {
        enabled: child.autonomy.enabled.or(parent.autonomy.enabled),
        auto_preload: child.autonomy.auto_preload.or(parent.autonomy.auto_preload),
        auto_dedup: child.autonomy.auto_dedup.or(parent.autonomy.auto_dedup),
        auto_related: child.autonomy.auto_related.or(parent.autonomy.auto_related),
        silent_preload: child
            .autonomy
            .silent_preload
            .or(parent.autonomy.silent_preload),
        auto_prefetch: child
            .autonomy
            .auto_prefetch
            .or(parent.autonomy.auto_prefetch),
        auto_response: child
            .autonomy
            .auto_response
            .or(parent.autonomy.auto_response),
        dedup_threshold: child
            .autonomy
            .dedup_threshold
            .or(parent.autonomy.dedup_threshold),
        prefetch_max_files: child
            .autonomy
            .prefetch_max_files
            .or(parent.autonomy.prefetch_max_files),
        prefetch_budget_tokens: child
            .autonomy
            .prefetch_budget_tokens
            .or(parent.autonomy.prefetch_budget_tokens),
        response_min_tokens: child
            .autonomy
            .response_min_tokens
            .or(parent.autonomy.response_min_tokens),
        checkpoint_interval: child
            .autonomy
            .checkpoint_interval
            .or(parent.autonomy.checkpoint_interval),
    };
    let output_hints = OutputHints {
        compressed_hint: child
            .output_hints
            .compressed_hint
            .or(parent.output_hints.compressed_hint),
        archive_hint: child
            .output_hints
            .archive_hint
            .or(parent.output_hints.archive_hint),
        verify_footer: child
            .output_hints
            .verify_footer
            .or(parent.output_hints.verify_footer),
        related_hint: child
            .output_hints
            .related_hint
            .or(parent.output_hints.related_hint),
        semantic_hint: child
            .output_hints
            .semantic_hint
            .or(parent.output_hints.semantic_hint),
        elicitation_hint: child
            .output_hints
            .elicitation_hint
            .or(parent.output_hints.elicitation_hint),
        checkpoint_in_output: child
            .output_hints
            .checkpoint_in_output
            .or(parent.output_hints.checkpoint_in_output),
        graph_context_block: child
            .output_hints
            .graph_context_block
            .or(parent.output_hints.graph_context_block),
        efficiency_hint: child
            .output_hints
            .efficiency_hint
            .or(parent.output_hints.efficiency_hint),
    };
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
        read,
        compression,
        translation,
        layout,
        memory,
        verification,
        budget,
        pipeline,
        routing,
        degradation,
        autonomy,
        output_hints,
    }
}

/// Reads the `profile` key directly from `config.toml` without going through
/// `Config::load()`. This avoids a reentrancy deadlock: `Config::load()` →
/// `find_project_root()` (`OnceLock`) → `SessionState::load_latest()` →
/// `normalize_loaded_session()` → `active_profile()` → here → `Config::load()`.
fn profile_name_from_config_file() -> Option<String> {
    let path = crate::core::config::Config::path()?;
    let content = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = toml::from_str(&content).ok()?;
    table
        .get("profile")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Process-wide active-profile override set by [`set_active_profile`].
///
/// Takes precedence over `LEAN_CTX_PROFILE`. Storing the runtime selection in an
/// in-process cell (rather than mutating the environment) keeps profile
/// switching thread-safe inside the multi-threaded MCP server, where
/// `set_active_profile` may run on a blocking-pool worker while other workers
/// resolve the active profile concurrently.
static ACTIVE_PROFILE_OVERRIDE: RwLock<Option<String>> = RwLock::new(None);

/// Returns the currently active profile name.
///
/// Resolution order: in-process override (see [`set_active_profile`]) →
/// `LEAN_CTX_PROFILE` env var → config.toml `profile` field → "coder".
pub fn active_profile_name() -> String {
    if let Some(name) = ACTIVE_PROFILE_OVERRIDE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return name;
    }
    if let Ok(v) = std::env::var("LEAN_CTX_PROFILE") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(name) = profile_name_from_config_file() {
        return name;
    }
    "coder".to_string()
}

/// Loads the currently active profile.
pub fn active_profile() -> Profile {
    let name = active_profile_name();
    if let Some(p) = load_profile(&name) {
        p
    } else {
        if name != "coder" {
            tracing::warn!(
                "Profile '{name}' not found (no built-in or disk file). \
                 Falling back to 'coder'. Create it with: lean-ctx profile create {name}"
            );
        }
        builtin_coder()
    }
}

/// Sets the active profile for the current process.
///
/// Records the selection in a thread-safe in-process override (see
/// [`active_profile_name`]) and returns the resolved profile after applying
/// inheritance.
pub fn set_active_profile(name: &str) -> Result<Profile, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("profile name is empty".to_string());
    }
    let prev = active_profile_name();
    let profile = load_profile(name).ok_or_else(|| format!("profile '{name}' not found"))?;
    *ACTIVE_PROFILE_OVERRIDE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(name.to_string());
    if prev != name {
        crate::core::events::emit_profile_changed(&prev, name);
    }
    Ok(profile)
}

/// Lists all available profile names (built-in + on-disk).
#[must_use]
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
        if let Some(dir) = dir
            && let Ok(entries) = std::fs::read_dir(&dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("toml")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
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
#[must_use]
pub fn format_as_toml(profile: &Profile) -> String {
    toml::to_string_pretty(profile).unwrap_or_else(|_| "[error serializing profile]".to_string())
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles_count() {
        let builtins = builtin_profiles();
        assert_eq!(builtins.len(), 7);
        assert!(builtins.contains_key("coder"));
        assert!(builtins.contains_key("exploration"));
        assert!(builtins.contains_key("bugfix"));
        assert!(builtins.contains_key("hotfix"));
        assert!(builtins.contains_key("ci-debug"));
        assert!(builtins.contains_key("review"));
        assert!(builtins.contains_key("passthrough"));
    }

    #[test]
    fn hotfix_has_minimal_budget() {
        let p = builtin_profiles().remove("hotfix").unwrap();
        assert_eq!(p.budget.max_context_tokens_effective(), 30_000);
        assert_eq!(p.budget.max_shell_invocations_effective(), 20);
        assert_eq!(p.read.default_mode_effective(), "signatures");
        assert_eq!(p.compression.output_density_effective(), "ultra");
    }

    #[test]
    fn exploration_has_broad_context() {
        let p = builtin_profiles().remove("exploration").unwrap();
        assert_eq!(p.budget.max_context_tokens_effective(), 200_000);
        assert_eq!(p.read.default_mode_effective(), "map");
        assert!(p.read.prefer_cache_effective());
    }

    #[test]
    fn profile_roundtrip_toml() {
        let original = builtin_exploration();
        let toml_str = format_as_toml(&original);
        let parsed: Profile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.profile.name, "exploration");
        assert_eq!(parsed.read.default_mode_effective(), "map");
        assert_eq!(parsed.budget.max_context_tokens_effective(), 200_000);
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
                default_mode: Some("signatures".to_string()),
                ..ReadConfig::default()
            },
            compression: CompressionConfig::default(),
            translation: TranslationConfig::default(),
            layout: LayoutConfig::default(),
            memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
            verification: crate::core::output_verification::VerificationConfig::default(),
            budget: BudgetConfig {
                max_context_tokens: Some(10_000),
                ..BudgetConfig::default()
            },
            pipeline: PipelineConfig::default(),
            routing: RoutingConfig::default(),
            degradation: DegradationConfig::default(),
            autonomy: ProfileAutonomy::default(),
            output_hints: OutputHints::default(),
        };

        let merged = merge_profiles(parent, child);
        assert_eq!(merged.read.default_mode_effective(), "signatures");
        assert_eq!(merged.budget.max_context_tokens_effective(), 10_000);
        assert_eq!(
            merged.profile.description,
            "Broad context for understanding codebases"
        );
    }

    #[test]
    fn merge_partial_child_inherits_parent_fields() {
        let parent = builtin_exploration();
        let child = Profile {
            profile: ProfileMeta {
                name: "partial".to_string(),
                inherits: Some("exploration".to_string()),
                description: String::new(),
            },
            read: ReadConfig {
                default_mode: Some("map".to_string()),
                ..ReadConfig::default()
            },
            compression: CompressionConfig::default(),
            translation: TranslationConfig::default(),
            layout: LayoutConfig::default(),
            memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
            verification: crate::core::output_verification::VerificationConfig::default(),
            budget: BudgetConfig::default(),
            pipeline: PipelineConfig::default(),
            routing: RoutingConfig::default(),
            degradation: DegradationConfig::default(),
            autonomy: ProfileAutonomy::default(),
            output_hints: OutputHints::default(),
        };

        let merged = merge_profiles(parent, child);
        assert_eq!(merged.read.default_mode_effective(), "map");
        assert_eq!(
            merged.read.max_tokens_per_file_effective(),
            80_000,
            "should inherit max_tokens_per_file from parent"
        );
        assert!(
            merged.read.prefer_cache_effective(),
            "should inherit prefer_cache from parent"
        );
        assert_eq!(
            merged.budget.max_context_tokens_effective(),
            200_000,
            "should inherit budget from parent"
        );
    }

    #[test]
    fn load_builtin_by_name() {
        let p = load_profile("hotfix").unwrap();
        assert_eq!(p.profile.name, "hotfix");
        assert_eq!(p.read.default_mode_effective(), "signatures");
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
    fn active_profile_defaults_to_coder() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROFILE");
        let p = active_profile();
        assert_eq!(p.profile.name, "coder");
    }

    #[test]
    fn active_profile_from_env() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_PROFILE", "hotfix");
        let name = active_profile_name();
        assert_eq!(name, "hotfix");
        crate::test_env::remove_var("LEAN_CTX_PROFILE");
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
            translation: TranslationConfig::default(),
            layout: LayoutConfig::default(),
            memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
            verification: crate::core::output_verification::VerificationConfig::default(),
            budget: BudgetConfig::default(),
            pipeline: PipelineConfig::default(),
            routing: RoutingConfig::default(),
            degradation: DegradationConfig::default(),
            autonomy: ProfileAutonomy::default(),
            output_hints: OutputHints::default(),
        };
        assert_eq!(p.read.default_mode_effective(), "auto");
        assert_eq!(p.compression.crp_mode_effective(), "tdd");
        assert_eq!(p.budget.max_context_tokens_effective(), 200_000);
        assert!(p.pipeline.compression_effective());
        assert!(p.pipeline.intent_effective());
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
        assert!(!p.pipeline.intent_effective());
        assert!(!p.pipeline.relevance_effective());
        assert!(p.pipeline.compression_effective());
        assert!(p.pipeline.translation_effective());
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
        assert_eq!(p.read.default_mode_effective(), "entropy");
        assert_eq!(p.read.max_tokens_per_file_effective(), 50_000);
        assert_eq!(p.budget.max_context_tokens_effective(), 200_000);
        assert_eq!(p.compression.crp_mode_effective(), "tdd");
    }

    #[test]
    fn partial_toml_leaves_unset_as_none() {
        let toml_str = r#"
[profile]
name = "sparse"

[read]
default_mode = "map"
"#;
        let p: Profile = toml::from_str(toml_str).unwrap();
        assert_eq!(p.read.default_mode, Some("map".to_string()));
        assert_eq!(p.read.max_tokens_per_file, None);
        assert_eq!(p.read.prefer_cache, None);
        assert_eq!(p.budget.max_context_tokens, None);
        assert_eq!(p.compression.crp_mode, None);
    }
}
