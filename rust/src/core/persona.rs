//! Context personas (`persona-spec-v1`).
//!
//! A persona is a declarative bundle that shapes the *whole* context surface for
//! a domain — not just coding. It composes:
//! - **tool surface** (a [`ToolProfile`]: built-in tier or custom list),
//! - **default read-mode**,
//! - **compressor** + **chunker** (names from the extension registry, 12.9),
//! - **intent taxonomy** (the task labels meaningful for the domain),
//! - **sensitivity floor** (minimum classification to enforce).
//!
//! Personas build on the existing tool profiles and are selectable per
//! workspace/channel/session via config (`persona = "…"`) or the
//! `LEAN_CTX_PERSONA` env var. The built-in `coding` persona reproduces today's
//! default behavior; further presets are added in 12.16.

use std::path::PathBuf;

use serde::Deserialize;

use super::sensitivity::SensitivityLevel;
use super::tool_profiles::ToolProfile;

/// A resolved persona ready to drive the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persona {
    pub name: String,
    pub description: String,
    pub tool_profile: ToolProfile,
    pub default_read_mode: String,
    pub compressor: String,
    pub chunker: String,
    pub intent_taxonomy: Vec<String>,
    pub sensitivity_floor: SensitivityLevel,
}

/// The on-disk / declarative form of a persona (`persona-spec-v1`).
#[derive(Debug, Clone, Deserialize)]
pub struct PersonaSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_tool_profile")]
    pub tool_profile: String,
    /// Explicit tool list when `tool_profile = "custom"`.
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_read_mode")]
    pub default_read_mode: String,
    #[serde(default = "default_compressor")]
    pub compressor: String,
    #[serde(default = "default_chunker")]
    pub chunker: String,
    #[serde(default)]
    pub intent_taxonomy: Vec<String>,
    #[serde(default)]
    pub sensitivity_floor: Option<String>,
}

fn default_tool_profile() -> String {
    "power".to_string()
}
fn default_read_mode() -> String {
    "auto".to_string()
}
fn default_compressor() -> String {
    "identity".to_string()
}
fn default_chunker() -> String {
    "lines".to_string()
}

fn labels(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// Error parsing a persona spec.
#[derive(Debug, thiserror::Error)]
pub enum PersonaError {
    #[error("invalid persona spec: {0}")]
    Validation(String),
    #[error("failed to parse persona: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("failed to read persona at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl PersonaSpec {
    /// Parse a spec from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, PersonaError> {
        let spec: Self = toml::from_str(text)?;
        spec.validate()?;
        Ok(spec)
    }

    fn validate(&self) -> Result<(), PersonaError> {
        if self.name.trim().is_empty() {
            return Err(PersonaError::Validation("name must not be empty".into()));
        }
        if self.tool_profile.eq_ignore_ascii_case("custom") && self.tools.is_empty() {
            return Err(PersonaError::Validation(format!(
                "persona '{}' uses tool_profile=custom but lists no tools",
                self.name
            )));
        }
        Ok(())
    }

    /// Resolve the declarative spec into a usable [`Persona`].
    #[must_use]
    pub fn into_persona(self) -> Persona {
        let tool_profile = if self.tool_profile.eq_ignore_ascii_case("custom") {
            ToolProfile::Custom(self.tools)
        } else {
            ToolProfile::parse(&self.tool_profile).unwrap_or(ToolProfile::Power)
        };
        let sensitivity_floor = self
            .sensitivity_floor
            .as_deref()
            .and_then(SensitivityLevel::parse)
            .unwrap_or_default();
        Persona {
            name: self.name,
            description: self.description,
            tool_profile,
            default_read_mode: self.default_read_mode,
            compressor: self.compressor,
            chunker: self.chunker,
            intent_taxonomy: self.intent_taxonomy,
            sensitivity_floor,
        }
    }
}

/// The default persona name when nothing is configured.
pub const DEFAULT_PERSONA: &str = "coding";

impl Persona {
    /// The built-in `coding` persona — reproduces today's default behavior so
    /// existing installs see no change.
    #[must_use]
    pub fn coding() -> Self {
        Persona {
            name: "coding".to_string(),
            description: "Software engineering on a code repository (default).".to_string(),
            tool_profile: ToolProfile::Power,
            default_read_mode: "auto".to_string(),
            compressor: "identity".to_string(),
            chunker: "lines".to_string(),
            intent_taxonomy: super::intent_engine::TaskType::all()
                .iter()
                .map(|t| t.as_str().to_string())
                .collect(),
            sensitivity_floor: SensitivityLevel::Public,
        }
    }

    /// Built-in presets by name (`sales` is an alias of `lead-gen`).
    #[must_use]
    pub fn builtin(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "coding" => Some(Self::coding()),
            "research" => Some(Self::research()),
            "lead-gen" | "lead_gen" | "sales" => Some(Self::lead_gen()),
            "support" => Some(Self::support()),
            "data-analysis" | "data_analysis" => Some(Self::data_analysis()),
            _ => None,
        }
    }

    /// Names of the built-in presets (sorted, canonical names only).
    #[must_use]
    pub fn builtin_names() -> Vec<String> {
        vec![
            "coding".to_string(),
            "data-analysis".to_string(),
            "lead-gen".to_string(),
            "research".to_string(),
            "support".to_string(),
        ]
    }

    /// `research`: reading the web/docs and synthesizing cited findings.
    #[must_use]
    pub fn research() -> Self {
        Persona {
            name: "research".to_string(),
            description: "Web/document research with cited synthesis.".to_string(),
            tool_profile: ToolProfile::Standard,
            default_read_mode: "map".to_string(),
            compressor: "markdown".to_string(),
            chunker: "paragraph".to_string(),
            intent_taxonomy: labels(&["explore", "summarize", "compare", "cite", "synthesize"]),
            sensitivity_floor: SensitivityLevel::Public,
        }
    }

    /// `lead-gen` (alias `sales`): prospecting + enriching sales leads.
    #[must_use]
    pub fn lead_gen() -> Self {
        Persona {
            name: "lead-gen".to_string(),
            description: "Outbound sales lead research + enrichment.".to_string(),
            tool_profile: ToolProfile::Custom(labels(&[
                "ctx_read",
                "ctx_search",
                "ctx_url_read",
                "ctx_knowledge",
                "ctx_semantic_search",
                "ctx_session",
            ])),
            default_read_mode: "map".to_string(),
            compressor: "prose".to_string(),
            chunker: "paragraph".to_string(),
            intent_taxonomy: labels(&["prospect", "qualify", "enrich", "outreach"]),
            sensitivity_floor: SensitivityLevel::Confidential,
        }
    }

    /// `support`: customer-support triage and resolution.
    #[must_use]
    pub fn support() -> Self {
        Persona {
            name: "support".to_string(),
            description: "Customer-support triage, diagnosis, resolution.".to_string(),
            tool_profile: ToolProfile::Standard,
            default_read_mode: "auto".to_string(),
            compressor: "prose".to_string(),
            chunker: "paragraph".to_string(),
            intent_taxonomy: labels(&["triage", "diagnose", "resolve", "escalate", "document"]),
            sensitivity_floor: SensitivityLevel::Internal,
        }
    }

    /// `data-analysis`: structured-data ingestion and reporting.
    #[must_use]
    pub fn data_analysis() -> Self {
        Persona {
            name: "data-analysis".to_string(),
            description: "Structured-data ingestion, analysis, reporting.".to_string(),
            tool_profile: ToolProfile::Standard,
            default_read_mode: "map".to_string(),
            compressor: "identity".to_string(),
            chunker: "lines".to_string(),
            intent_taxonomy: labels(&["ingest", "clean", "analyze", "visualize", "report"]),
            sensitivity_floor: SensitivityLevel::Internal,
        }
    }

    /// Resolve the active persona for this config.
    ///
    /// Priority: `LEAN_CTX_PERSONA` env > config `persona` > [`DEFAULT_PERSONA`].
    /// A name is resolved against built-ins first, then a `<personas_dir>/<name>.toml`
    /// file. Unknown/invalid names fall back to `coding` (never an error at a
    /// call site — selection is best-effort).
    #[must_use]
    pub fn resolve(cfg: &super::config::Config) -> Self {
        let name = std::env::var("LEAN_CTX_PERSONA")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| cfg.persona.clone())
            .unwrap_or_else(|| DEFAULT_PERSONA.to_string());

        if let Some(p) = Self::builtin(&name) {
            return p;
        }
        match load_from_dir(&name) {
            Ok(Some(p)) => p,
            Ok(None) => {
                tracing::warn!("persona '{name}' not found; falling back to coding");
                Self::coding()
            }
            Err(e) => {
                tracing::warn!("failed to load persona '{name}': {e}; falling back to coding");
                Self::coding()
            }
        }
    }

    /// The effective tool surface: an explicit tool-profile setting (env/config)
    /// always wins (backward compatible); otherwise the persona supplies it.
    #[must_use]
    pub fn effective_tool_profile(&self, cfg: &super::config::Config) -> ToolProfile {
        if tool_profile_is_explicit(cfg) {
            ToolProfile::from_config(cfg)
        } else {
            self.tool_profile.clone()
        }
    }
}

/// Whether the user explicitly pinned a tool profile (vs. leaving it to the
/// persona default).
fn tool_profile_is_explicit(cfg: &super::config::Config) -> bool {
    std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok()
        || cfg.tool_profile.is_some()
        || !cfg.tools_enabled.is_empty()
}

/// Root directory holding `<name>.toml` persona files. `LEAN_CTX_PERSONAS_DIR`
/// overrides the default so containers/CI/tests can isolate it.
#[must_use]
pub fn personas_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LEAN_CTX_PERSONAS_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Some(config_dir) = dirs::config_dir() {
        config_dir.join("lean-ctx").join("personas")
    } else {
        PathBuf::from("~/.config/lean-ctx/personas")
    }
}

/// Load a persona from `<personas_dir>/<name>.toml`. `Ok(None)` if absent.
fn load_from_dir(name: &str) -> Result<Option<Persona>, PersonaError> {
    let path = personas_dir().join(format!("{name}.toml"));
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|source| PersonaError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(Some(PersonaSpec::from_toml(&text)?.into_persona()))
}

/// All persona names available on this instance (built-ins + discovered files).
#[must_use]
pub fn list_personas() -> Vec<String> {
    let mut names = Persona::builtin_names();
    if let Ok(entries) = std::fs::read_dir(personas_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if !names.iter().any(|n| n == stem) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_preset_matches_today_defaults() {
        let p = Persona::coding();
        assert_eq!(p.name, "coding");
        assert_eq!(p.tool_profile, ToolProfile::Power);
        assert_eq!(p.default_read_mode, "auto");
        assert_eq!(p.sensitivity_floor, SensitivityLevel::Public);
        assert!(p.intent_taxonomy.contains(&"generate".to_string()));
    }

    #[test]
    fn spec_parses_and_resolves_custom_tool_surface() {
        let spec = PersonaSpec::from_toml(
            r#"
name = "lead-gen"
description = "Sales lead research"
tool_profile = "custom"
tools = ["ctx_read", "ctx_search", "ctx_url_read"]
default_read_mode = "map"
compressor = "whitespace"
chunker = "paragraph"
sensitivity_floor = "confidential"
intent_taxonomy = ["prospect", "qualify", "enrich"]
"#,
        )
        .unwrap();
        let persona = spec.into_persona();
        assert_eq!(
            persona.tool_profile,
            ToolProfile::Custom(vec![
                "ctx_read".into(),
                "ctx_search".into(),
                "ctx_url_read".into(),
            ])
        );
        assert_eq!(persona.default_read_mode, "map");
        assert_eq!(persona.compressor, "whitespace");
        assert_eq!(persona.sensitivity_floor, SensitivityLevel::Confidential);
        // A custom persona genuinely changes the tool surface.
        assert!(persona.tool_profile.is_tool_enabled("ctx_url_read"));
        assert!(!persona.tool_profile.is_tool_enabled("ctx_refactor"));
    }

    #[test]
    fn builtin_presets_are_shipped_and_resolvable() {
        let names = Persona::builtin_names();
        for expected in ["coding", "research", "lead-gen", "support", "data-analysis"] {
            assert!(
                names.contains(&expected.to_string()),
                "missing preset {expected}"
            );
            assert!(
                Persona::builtin(expected).is_some(),
                "unresolvable preset {expected}"
            );
        }
        // `sales` is an alias of lead-gen.
        assert_eq!(Persona::builtin("sales").unwrap().name, "lead-gen");
    }

    #[test]
    fn intent_taxonomy_varies_by_persona() {
        let coding = Persona::coding().intent_taxonomy;
        let research = Persona::research().intent_taxonomy;
        let lead = Persona::lead_gen().intent_taxonomy;
        assert_ne!(coding, research);
        assert_ne!(coding, lead);
        assert!(research.contains(&"synthesize".to_string()));
        assert!(lead.contains(&"prospect".to_string()));
    }

    #[test]
    fn presets_change_tool_surface() {
        // lead-gen exposes web research tools, not refactoring tools.
        let lead = Persona::lead_gen();
        assert!(lead.tool_profile.is_tool_enabled("ctx_url_read"));
        assert!(!lead.tool_profile.is_tool_enabled("ctx_refactor"));
    }

    #[test]
    fn custom_profile_without_tools_is_rejected() {
        let err = PersonaSpec::from_toml("name = \"x\"\ntool_profile = \"custom\"\n").unwrap_err();
        assert!(matches!(err, PersonaError::Validation(_)));
    }

    #[test]
    fn loader_reads_persona_file_and_selection_picks_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("research.toml"),
            "name = \"research\"\ntool_profile = \"standard\"\ndefault_read_mode = \"map\"\n",
        )
        .unwrap();
        std::env::set_var("LEAN_CTX_PERSONAS_DIR", dir.path());

        let loaded = load_from_dir("research").unwrap().unwrap();
        assert_eq!(loaded.name, "research");
        assert_eq!(loaded.tool_profile, ToolProfile::Standard);

        let names = list_personas();
        assert!(names.contains(&"research".to_string()));
        assert!(names.contains(&"coding".to_string()));

        std::env::remove_var("LEAN_CTX_PERSONAS_DIR");
    }
}
