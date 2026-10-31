use serde::{Deserialize, Serialize};

/// A complete context profile definition.
///
/// Profile files written before 2026-10-16 may still carry sections and keys
/// that never had an effect (`translation`, `layout`, `budget`, `constraints`,
/// `capabilities`, `pipeline`, and single keys such as `read.prefer_cache`).
/// They are ignored when a profile loads and are not written back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub profile: ProfileMeta,
    #[serde(default)]
    pub read: ReadConfig,
    #[serde(default)]
    pub compression: CompressionConfig,
    #[serde(default)]
    pub memory: crate::core::memory_policy::MemoryPolicyOverrides,
    #[serde(default)]
    pub verification: crate::core::output_verification::VerificationConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
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
}

impl ReadConfig {
    pub fn default_mode_effective(&self) -> &str {
        self.default_mode.as_deref().unwrap_or("auto")
    }
}

/// Compression strategy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CompressionConfig {
    pub crp_mode: Option<String>,
}

impl CompressionConfig {
    /// The CRP mode this profile actually produces, after env and
    /// `compression_level` precedence — the same value tools use (#1890).
    pub fn crp_mode_effective(&self) -> &'static str {
        crate::core::protocol::CrpMode::resolve(self.crp_mode.as_deref()).as_str()
    }
}

/// Context-strategy overrides; model selection belongs to the caller.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutingConfig {
    /// If true, reduce context detail under context-window pressure.
    #[serde(default)]
    pub degrade_under_pressure: Option<bool>,
}

impl RoutingConfig {
    pub fn degrade_under_pressure_effective(&self) -> bool {
        self.degrade_under_pressure.unwrap_or(true)
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
    /// Cross-source hints: append issue/PR/schema/page references from the
    /// property graph when reading a file. Default: on (owner 2026-10-31); it
    /// costs tokens only when a provider linked a reference to the file.
    pub cross_source_hint: Option<bool>,
    /// Proactive context: auto-expand previously compressed content when
    /// keyword-relevant to the current read. Default: off (speculative, up to
    /// 2000 tokens per read).
    pub proactive_context: Option<bool>,
}

impl OutputHints {
    pub fn compressed_hint(&self) -> bool {
        self.compressed_hint.unwrap_or(false)
    }
    pub fn archive_hint(&self) -> bool {
        self.archive_hint.unwrap_or(false)
    }
    pub fn verify_footer(&self) -> bool {
        self.verify_footer.unwrap_or(false)
    }
    pub fn related_hint(&self) -> bool {
        self.related_hint.unwrap_or(false)
    }
    pub fn semantic_hint(&self) -> bool {
        self.semantic_hint.unwrap_or(false)
    }
    pub fn elicitation_hint(&self) -> bool {
        self.elicitation_hint.unwrap_or(false)
    }
    pub fn checkpoint_in_output(&self) -> bool {
        self.checkpoint_in_output.unwrap_or(false)
    }
    pub fn graph_context_block(&self) -> bool {
        self.graph_context_block.unwrap_or(false)
    }
    pub fn efficiency_hint(&self) -> bool {
        self.efficiency_hint.unwrap_or(false)
    }
    pub fn cross_source_hint(&self) -> bool {
        self.cross_source_hint.unwrap_or(true)
    }
    pub fn proactive_context(&self) -> bool {
        self.proactive_context.unwrap_or(false)
    }
}

/// Autonomy overrides per profile.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfileAutonomy {
    pub enabled: Option<bool>,
    pub checkpoint_interval: Option<u32>,
}

impl ProfileAutonomy {
    pub fn enabled_effective(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    pub fn checkpoint_interval_effective(&self) -> u32 {
        self.checkpoint_interval.unwrap_or(15)
    }
}
