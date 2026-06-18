use std::fmt;

/// Controls which MCP tools are exposed to agents.
///
/// Three built-in tiers reduce tool-list overwhelm for new users
/// while letting power users keep everything.
///
/// When NO profile is pinned (no config key, no env var), the server
/// advertises only the lazy core set (`CORE_TOOL_NAMES`) and the
/// effective profile falls back to `Power` — which acts as a pure call-gate
/// ("everything reachable via ctx_call"), not as an advertisement list.
/// Pinning a profile makes the advertised set explicit and authoritative
/// (#358), which costs schema tokens: `standard` advertises 22 full schemas,
/// `power` the whole registry (#575).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolProfile {
    Minimal,
    Standard,
    Power,
    Custom(Vec<String>),
}

impl ToolProfile {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "minimal" | "min" => Some(Self::Minimal),
            "standard" | "std" | "default" => Some(Self::Standard),
            "power" | "full" | "all" => Some(Self::Power),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Minimal => "minimal",
            Self::Standard => "standard",
            Self::Power => "power",
            Self::Custom(_) => "custom",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::Minimal => "6 essential tools for new users",
            Self::Standard => "22 balanced tools (recommended)",
            Self::Power => "All tools exposed",
            Self::Custom(v) => {
                if v.is_empty() {
                    "Custom tool list (empty)"
                } else {
                    "Custom tool list"
                }
            }
        }
    }

    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        match self {
            Self::Power => true,
            Self::Minimal => MINIMAL_TOOLS.contains(&tool_name),
            Self::Standard => STANDARD_TOOLS.contains(&tool_name),
            Self::Custom(list) => list.iter().any(|t| t == tool_name),
        }
    }

    pub fn tool_count(&self) -> usize {
        match self {
            Self::Minimal => MINIMAL_TOOLS.len(),
            Self::Standard => STANDARD_TOOLS.len(),
            Self::Power => 0, // dynamic — caller should use registry count
            Self::Custom(list) => list.len(),
        }
    }

    pub fn tool_names(&self) -> Vec<&str> {
        match self {
            Self::Minimal => MINIMAL_TOOLS.to_vec(),
            Self::Standard => STANDARD_TOOLS.to_vec(),
            Self::Power | Self::Custom(_) => vec![],
        }
    }

    /// Resolves the active tool profile from environment, then config.
    ///
    /// Priority: `LEAN_CTX_TOOL_PROFILE` env > config `tool_profile` > config `tools.enabled` > default.
    /// Existing installs default to `power` (backward compat).
    /// New installs set `standard` during setup.
    pub fn from_config(cfg: &super::config::Config) -> Self {
        if let Ok(val) = std::env::var("LEAN_CTX_TOOL_PROFILE") {
            let trimmed = val.trim();
            if let Some(profile) = Self::parse(trimmed) {
                return profile;
            }
            // Same "unpin" sentinel handling as for the config key below (#431).
            if !trimmed.is_empty() && !is_unpinned_alias(trimmed) {
                tracing::warn!("Unknown LEAN_CTX_TOOL_PROFILE value '{trimmed}', using config");
            }
        }

        if let Some(ref profile_name) = cfg.tool_profile {
            if let Some(profile) = Self::parse(profile_name) {
                return profile;
            }
            // `lean`/`lazy`/`reset` are the *unpinned* sentinel (lazy core
            // advertised, everything reachable via ctx_call) — not a pinned
            // tier. They can legitimately land in config (older versions, the
            // dashboard's "Lean" button, manual edits), so resolve them
            // silently to the default instead of warning + falling back (#431).
            if !is_unpinned_alias(profile_name) {
                tracing::warn!("Unknown tool_profile '{profile_name}' in config, using default");
            }
        }

        if !cfg.tools_enabled.is_empty() {
            return Self::Custom(cfg.tools_enabled.clone());
        }

        Self::Power
    }
}

impl fmt::Display for ToolProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// `lean`/`lazy`/`reset` are not pinned tiers — they are the *unpin* sentinel
/// that clears any pin so the default returns (lazy core advertised, everything
/// callable via `ctx_call`). Centralised so the config loader, the CLI
/// (`lean-ctx profile lean`) and the dashboard all agree on the same set (#431).
pub fn is_unpinned_alias(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "lean" | "lazy" | "reset"
    )
}

const MINIMAL_TOOLS: &[&str] = &[
    "ctx_read",
    "ctx_shell",
    "shell",
    "ctx_search",
    "ctx_tree",
    "ctx_session",
];

const STANDARD_TOOLS: &[&str] = &[
    // Everything in minimal
    "ctx_read",
    "ctx_shell",
    "shell",
    "ctx_search",
    "ctx_tree",
    "ctx_session",
    // Plus balanced additions
    "ctx_semantic_search",
    "ctx_knowledge",
    "ctx_overview",
    "ctx_repomap",
    "ctx_callgraph",
    "ctx_impact",
    "ctx_compress",
    "ctx_multi_read",
    "ctx_delta",
    "ctx_edit",
    "ctx_agent",
    "ctx_architecture",
    "ctx_pack",
    "ctx_routes",
    "ctx_refactor",
    // Web/research context: fetch + cite external pages and YouTube transcripts
    "ctx_url_read",
];

/// Available built-in profile names.
pub const PROFILE_NAMES: &[&str] = &["minimal", "standard", "power"];

pub struct ProfileInfo {
    pub name: &'static str,
    pub tool_count: &'static str,
    pub description: &'static str,
}

pub fn list_profiles() -> Vec<ProfileInfo> {
    vec![
        ProfileInfo {
            name: "minimal",
            tool_count: "6",
            description: "Essential tools for new users / skeptics",
        },
        ProfileInfo {
            name: "standard",
            tool_count: "22",
            description: "Balanced set (recommended for most users)",
        },
        ProfileInfo {
            name: "power",
            tool_count: "all",
            description: "Every tool exposed (backward compatible)",
        },
    ]
}

/// Writes the `tool_profile` setting to config.toml, preserving all comments,
/// formatting, and unrelated keys (robust against substring/comment matches).
pub fn set_profile_in_config(profile_name: &str) -> Result<(), String> {
    // Canonical config location (RO-safe config category, GH #408). Writing it
    // anywhere else than `Config::load` reads would split-brain once the data
    // default flips to `$XDG_DATA_HOME`.
    let config_path = crate::core::config::Config::path()
        .ok_or_else(|| "Cannot determine config dir".to_string())?;

    let mut doc = crate::config_io::load_toml_document(&config_path);
    doc["tool_profile"] = toml_edit::value(profile_name);
    crate::config_io::write_toml_document(&config_path, &doc)?;
    Ok(())
}

/// Removes the `tool_profile` key from config.toml, restoring the lean
/// default: only the lazy core set is advertised in `tools/list`,
/// while every registered tool stays reachable through `ctx_call`. This is
/// the recommended low-overhead mode (#575).
pub fn clear_profile_in_config() -> Result<(), String> {
    let config_path = crate::core::config::Config::path()
        .ok_or_else(|| "Cannot determine config dir".to_string())?;
    if !config_path.exists() {
        return Ok(());
    }

    let mut doc = crate::config_io::load_toml_document(&config_path);
    if doc.remove("tool_profile").is_none() {
        return Ok(());
    }
    crate::config_io::write_toml_document(&config_path, &doc)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_profiles() {
        assert_eq!(ToolProfile::parse("minimal"), Some(ToolProfile::Minimal));
        assert_eq!(ToolProfile::parse("min"), Some(ToolProfile::Minimal));
        assert_eq!(ToolProfile::parse("standard"), Some(ToolProfile::Standard));
        assert_eq!(ToolProfile::parse("std"), Some(ToolProfile::Standard));
        assert_eq!(ToolProfile::parse("default"), Some(ToolProfile::Standard));
        assert_eq!(ToolProfile::parse("power"), Some(ToolProfile::Power));
        assert_eq!(ToolProfile::parse("full"), Some(ToolProfile::Power));
        assert_eq!(ToolProfile::parse("all"), Some(ToolProfile::Power));
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(ToolProfile::parse("MINIMAL"), Some(ToolProfile::Minimal));
        assert_eq!(ToolProfile::parse("Standard"), Some(ToolProfile::Standard));
        assert_eq!(ToolProfile::parse("POWER"), Some(ToolProfile::Power));
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert_eq!(ToolProfile::parse("unknown"), None);
        assert_eq!(ToolProfile::parse(""), None);
    }

    #[test]
    fn minimal_has_6_tools() {
        assert_eq!(MINIMAL_TOOLS.len(), 6);
    }

    #[test]
    fn minimal_profile_schema_budget() {
        // tool_profile=minimal advertises the 6-tool lean set; the schemas they
        // re-send every turn (description + input schema) must stay small — this
        // is the tool-side of the faithful-arm per-turn prefix tax (#361). We
        // sum the registry's *uncompressed* schemas (a strict upper bound on the
        // advertised, description-compressed surface).
        const MINIMAL_SCHEMA_BUDGET_TOKENS: usize = 1500;
        let defs = crate::server::registry::build_registry().tool_defs();
        let total: usize = defs
            .iter()
            .filter(|t| MINIMAL_TOOLS.contains(&t.name.as_ref()))
            .map(crate::core::context_overhead::tool_tokens)
            .sum();
        assert!(total > 0, "minimal tools must exist in the registry");
        assert!(
            total <= MINIMAL_SCHEMA_BUDGET_TOKENS,
            "minimal-profile tool schemas = {total} tok, budget {MINIMAL_SCHEMA_BUDGET_TOKENS}"
        );
    }

    #[test]
    fn standard_has_22_tools() {
        assert_eq!(STANDARD_TOOLS.len(), 22);
    }

    #[test]
    fn minimal_is_subset_of_standard() {
        for tool in MINIMAL_TOOLS {
            assert!(
                STANDARD_TOOLS.contains(tool),
                "minimal tool {tool} missing from standard"
            );
        }
    }

    #[test]
    fn power_enables_everything() {
        let profile = ToolProfile::Power;
        assert!(profile.is_tool_enabled("ctx_read"));
        assert!(profile.is_tool_enabled("ctx_anything"));
        assert!(profile.is_tool_enabled("nonexistent_tool"));
    }

    #[test]
    fn minimal_filters_correctly() {
        let profile = ToolProfile::Minimal;
        assert!(profile.is_tool_enabled("ctx_read"));
        assert!(profile.is_tool_enabled("ctx_shell"));
        assert!(profile.is_tool_enabled("ctx_search"));
        assert!(profile.is_tool_enabled("ctx_tree"));
        assert!(profile.is_tool_enabled("ctx_session"));
        assert!(!profile.is_tool_enabled("ctx_semantic_search"));
        assert!(!profile.is_tool_enabled("ctx_architecture"));
        assert!(!profile.is_tool_enabled("ctx_benchmark"));
    }

    #[test]
    fn standard_filters_correctly() {
        let profile = ToolProfile::Standard;
        assert!(profile.is_tool_enabled("ctx_read"));
        assert!(profile.is_tool_enabled("ctx_semantic_search"));
        assert!(profile.is_tool_enabled("ctx_architecture"));
        assert!(!profile.is_tool_enabled("ctx_benchmark"));
        assert!(!profile.is_tool_enabled("ctx_analyze"));
        assert!(!profile.is_tool_enabled("ctx_smells"));
    }

    #[test]
    fn custom_profile_uses_provided_list() {
        let profile = ToolProfile::Custom(vec!["ctx_read".to_string(), "ctx_shell".to_string()]);
        assert!(profile.is_tool_enabled("ctx_read"));
        assert!(profile.is_tool_enabled("ctx_shell"));
        assert!(!profile.is_tool_enabled("ctx_search"));
    }

    #[test]
    fn profile_display_counts_match_tool_arrays() {
        // The numbers shown by `lean-ctx tools` must equal the actual array
        // lengths, so adding/removing a profile tool (e.g. the `shell` alias)
        // can never silently desync the advertised count from reality.
        let profiles = list_profiles();
        assert_eq!(
            profiles[0].tool_count.parse::<usize>().unwrap(),
            MINIMAL_TOOLS.len(),
            "minimal count must match MINIMAL_TOOLS length",
        );
        assert_eq!(
            profiles[1].tool_count.parse::<usize>().unwrap(),
            STANDARD_TOOLS.len(),
            "standard count must match STANDARD_TOOLS length",
        );
        assert_eq!(profiles[2].tool_count, "all");
    }

    #[test]
    fn custom_empty_enables_nothing() {
        let profile = ToolProfile::Custom(vec![]);
        assert!(!profile.is_tool_enabled("ctx_read"));
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(format!("{}", ToolProfile::Minimal), "minimal");
        assert_eq!(format!("{}", ToolProfile::Standard), "standard");
        assert_eq!(format!("{}", ToolProfile::Power), "power");
        assert_eq!(
            format!("{}", ToolProfile::Custom(vec!["ctx_read".into()])),
            "custom"
        );
    }

    #[test]
    fn tool_count_matches_list_length() {
        assert_eq!(ToolProfile::Minimal.tool_count(), MINIMAL_TOOLS.len());
        assert_eq!(ToolProfile::Standard.tool_count(), STANDARD_TOOLS.len());
        assert_eq!(ToolProfile::Power.tool_count(), 0);
    }

    #[test]
    fn from_config_defaults_to_power_for_backward_compat() {
        if std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok() {
            return;
        }
        let cfg = crate::core::config::Config {
            tool_profile: None,
            tools_enabled: vec![],
            ..Default::default()
        };
        assert_eq!(ToolProfile::from_config(&cfg), ToolProfile::Power);
    }

    #[test]
    fn from_config_respects_tool_profile_field() {
        if std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok() {
            return;
        }
        let cfg = crate::core::config::Config {
            tool_profile: Some("minimal".to_string()),
            tools_enabled: vec![],
            ..Default::default()
        };
        assert_eq!(ToolProfile::from_config(&cfg), ToolProfile::Minimal);
    }

    #[test]
    fn from_config_tools_enabled_creates_custom() {
        if std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok() {
            return;
        }
        let cfg = crate::core::config::Config {
            tool_profile: None,
            tools_enabled: vec!["ctx_read".to_string(), "ctx_shell".to_string()],
            ..Default::default()
        };
        let profile = ToolProfile::from_config(&cfg);
        assert_eq!(
            profile,
            ToolProfile::Custom(vec!["ctx_read".to_string(), "ctx_shell".to_string()])
        );
    }

    #[test]
    fn tool_profile_takes_precedence_over_tools_enabled() {
        if std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok() {
            return;
        }
        let cfg = crate::core::config::Config {
            tool_profile: Some("standard".to_string()),
            tools_enabled: vec!["ctx_read".to_string()],
            ..Default::default()
        };
        assert_eq!(ToolProfile::from_config(&cfg), ToolProfile::Standard);
    }

    #[test]
    fn all_profile_names_are_parseable() {
        for name in PROFILE_NAMES {
            assert!(
                ToolProfile::parse(name).is_some(),
                "profile name '{name}' should be parseable"
            );
        }
    }

    #[test]
    fn list_profiles_returns_three_entries() {
        let profiles = list_profiles();
        assert_eq!(profiles.len(), 3);
    }

    #[test]
    fn standard_includes_edit_and_delta() {
        let profile = ToolProfile::Standard;
        assert!(
            profile.is_tool_enabled("ctx_edit"),
            "ctx_edit must be in standard"
        );
        assert!(
            profile.is_tool_enabled("ctx_delta"),
            "ctx_delta must be in standard"
        );
    }

    #[test]
    fn standard_includes_url_read() {
        let profile = ToolProfile::Standard;
        assert!(
            profile.is_tool_enabled("ctx_url_read"),
            "ctx_url_read must be in standard (web/research context)"
        );
    }

    #[test]
    fn clear_profile_removes_key_and_is_idempotent() {
        let iso = crate::core::data_dir::isolated_data_dir();
        set_profile_in_config("power").unwrap();
        let config_path = iso.path().join("config.toml");
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("tool_profile"),
            "set_profile_in_config must write the key"
        );

        clear_profile_in_config().unwrap();
        assert!(
            !std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("tool_profile"),
            "clear_profile_in_config must remove the key (lean default, #575)"
        );

        // Idempotent: clearing again (and on a missing file) must not fail.
        clear_profile_in_config().unwrap();
    }

    #[test]
    fn clear_profile_on_missing_config_is_ok() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        clear_profile_in_config().unwrap();
    }
}
