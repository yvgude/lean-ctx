use std::{collections::HashMap, sync::Arc};

use super::memory_policy::MemoryPolicy;
use super::{error, persona, tool_profiles};

mod defaults;
mod defaults_allowlist;
mod enterprise;
mod enums;
mod loader;
mod logic;
mod memory;
mod merge;
mod model;
mod provenance;
mod proxy;
mod read_dedup;
pub(crate) mod read_redirect;
mod render;
mod response_shaping;
pub mod risk;
pub mod schema;
mod sections;
mod serde_defaults;
pub mod setter;
mod shell_activation;

/// Cache payload for [`Config::load_arc`]: the shared config alongside the
/// content hashes of the global and project-local files plus the environment-
/// selected config profile it was built from, so a later load only reuses an
/// entry with identical inputs (#406, #1159).
type ConfigCacheSlot = Option<(Arc<Config>, Option<String>, Option<String>, Option<String>)>;

pub use defaults::*;
#[allow(unreachable_pub, unused_imports)]
pub use logic::*;
pub use model::*;
pub use render::render_annotated_config;
pub use sections::*;

pub(crate) use defaults_allowlist::{cloud_infra_commands, default_shell_allowlist};
pub use enterprise::EnterpriseConfig;
pub use enums::{
    CognitiveMode, CompressionLevel, Effort, OutputDensity, PermissionInheritance, RecoveryHints,
    ResponseVerbosity, RulesInjection, RulesScope, SessionDegrade, TeeMode, TerseAgent,
};
pub use loader::last_config_parse_error;
pub use loader::local_sensitive_overrides;
pub(crate) use loader::strip_sensitive_overrides;
pub use memory::{
    CompressionAnnotation, MemoryCleanup, MemoryGuardConfig, MemoryProfile, SavingsFooter,
};
pub use provenance::{ConfigProvenance, EnvOverride};
pub use proxy::{
    BaselineConfig, DEFAULT_LOCAL_SHADOW_RATE_PER_MTOK, HistoryMode, PipelineConfig, ProseRanker,
    ProseRole, ProviderEntry, ProxyConfig, ProxyMode, ProxyProvider, ReasoningBudgetConfig,
    ResolvedProvider, RoleAggressiveness, RoutingRules, UpstreamDrift, Upstreams, WireShape,
    diagnose_drift, env_upstream_override, is_local_proxy_url, normalize_url, normalize_url_opt,
    parse_route_target,
};
pub use read_dedup::ReadDedup;
pub use read_redirect::ReadRedirect;
pub use response_shaping::{
    CodeRepetitionConfig, ConfirmationConfig, NarrationConfig, PreambleConfig,
    ResponseShapingConfig,
};
pub use shell_activation::ShellActivation;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_parsing;
