use super::builtins::{builtin_coder, builtin_profile, builtin_profiles};
use super::types::{
    CompressionConfig, OutputHints, Profile, ProfileAutonomy, ProfileMeta, ReadConfig,
    RoutingConfig,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

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
pub(crate) fn load_profile(name: &str) -> Option<Profile> {
    load_profile_recursive(name, 0)
}

fn load_profile_recursive(name: &str, depth: usize) -> Option<Profile> {
    if depth > 5 {
        return None;
    }

    let mut profile = load_profile_from_disk(name).or_else(|| builtin_profile(name))?;
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
    let table: toml::Table = toml::from_str(&content).ok()?;
    warn_retired_autonomy_keys(path, &table);
    table.try_into().ok()
}

/// A profile `[autonomy]` key retired with its driver is ignored on load;
/// name it instead of dropping it silently.
fn warn_retired_autonomy_keys(path: &Path, table: &toml::Table) {
    let Some(autonomy) = table.get("autonomy").and_then(toml::Value::as_table) else {
        return;
    };
    for key in autonomy.keys() {
        let dotted = format!("autonomy.{key}");
        if let Some(retired) = crate::core::config::retired::retired_key(&dotted) {
            tracing::warn!(
                "profile {}: `{dotted}` is retired and has no effect ({})",
                path.display(),
                retired.note
            );
        }
    }
}

/// Merges parent into child: child values take precedence,
/// parent provides defaults for unspecified fields.
///
/// ALL sections are merged field-by-field using `Option::or()`.
/// A child profile only needs to set the fields it wants to override.
pub(super) fn merge_profiles(parent: Profile, child: Profile) -> Profile {
    let read = ReadConfig {
        default_mode: child.read.default_mode.or(parent.read.default_mode),
    };
    let compression = CompressionConfig {
        crp_mode: child.compression.crp_mode.or(parent.compression.crp_mode),
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
    let routing = RoutingConfig {
        degrade_under_pressure: child
            .routing
            .degrade_under_pressure
            .or(parent.routing.degrade_under_pressure),
    };
    let autonomy = ProfileAutonomy {
        enabled: child.autonomy.enabled.or(parent.autonomy.enabled),
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
        cross_source_hint: child
            .output_hints
            .cross_source_hint
            .or(parent.output_hints.cross_source_hint),
        proactive_context: child
            .output_hints
            .proactive_context
            .or(parent.output_hints.proactive_context),
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
        memory,
        verification,
        routing,
        autonomy,
        output_hints,
    }
}

/// Reads the `profile` key directly from `config.toml` without going through
/// `Config::load()`. This avoids a reentrancy deadlock: `Config::load()` →
/// `find_project_root()` (OnceLock) → `SessionState::load_latest()` →
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
pub(crate) fn active_profile_name() -> String {
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
pub(crate) fn active_profile() -> Profile {
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
pub(crate) fn set_active_profile(name: &str) -> Result<Profile, String> {
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
pub(crate) fn list_profiles() -> Vec<ProfileInfo> {
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
pub(crate) struct ProfileInfo {
    pub name: String,
    pub description: String,
    pub source: ProfileSource,
}

/// Where a profile was loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileSource {
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
pub(crate) fn format_as_toml(profile: &Profile) -> String {
    toml::to_string_pretty(profile).unwrap_or_else(|_| "[error serializing profile]".to_string())
}
