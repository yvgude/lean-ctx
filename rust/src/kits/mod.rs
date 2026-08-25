//! Context Kits are versioned, task-specific context plans.
//!
//! Kits complement the general-purpose context profiles: a profile controls
//! broad runtime defaults, while a kit supplies the ordered read plan, shell
//! evidence, review bar, and prompt for one concrete workflow.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;

const CODE_REVIEW_KIT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/kits/code-review/kit.toml"
));

/// A fully parsed Context Kit.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextKit {
    pub format_version: u32,
    pub kit: KitMetadata,
    pub read: ReadPlan,
    pub priority: PriorityPlan,
    pub shell: ShellPlan,
    pub quality: QualityPlan,
    pub system_prompt: SystemPrompt,
}

/// Stable identity and display metadata for a kit.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KitMetadata {
    pub name: String,
    pub description: String,
    pub version: String,
}

/// Ordered read rules. The first matching rule wins.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadPlan {
    #[serde(default)]
    pub default_modes: Vec<String>,
    #[serde(default)]
    pub rules: Vec<ReadRule>,
}

/// A glob and its recommended read stages.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRule {
    pub glob: String,
    pub modes: Vec<String>,
}

/// What must receive attention first.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriorityPlan {
    pub strategy: String,
    pub description: String,
}

/// Shell output relevant to the kit.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellPlan {
    pub default_mode: String,
    #[serde(default)]
    pub commands: Vec<ShellCommand>,
}

/// One command and the evidence retained by its compressor.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellCommand {
    pub command: String,
    #[serde(default)]
    pub focus: Vec<String>,
}

/// Review-output quality bar.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityPlan {
    pub criteria: Vec<String>,
}

/// System instruction supplied to the workflow LLM.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemPrompt {
    pub template: String,
}

/// Where a kit was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KitSource {
    Project(PathBuf),
    User(PathBuf),
    Builtin,
}

impl fmt::Display for KitSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project(path) => write!(f, "project ({})", path.display()),
            Self::User(path) => write!(f, "user ({})", path.display()),
            Self::Builtin => write!(f, "built-in"),
        }
    }
}

/// A kit together with the location that supplied it.
#[derive(Debug, Clone)]
pub struct LoadedKit {
    pub kit: ContextKit,
    pub source: KitSource,
}

/// Errors returned while locating or validating a kit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitError(String);

impl KitError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for KitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KitError {}

impl ContextKit {
    /// Returns the ordered read stages for `path`; the first matching rule wins.
    #[must_use]
    pub fn modes_for_path(&self, path: impl AsRef<Path>) -> &[String] {
        let path = path.as_ref().to_string_lossy();
        self.read
            .rules
            .iter()
            .find(|rule| glob_matches(&rule.glob, &path))
            .map_or(self.read.default_modes.as_slice(), |rule| {
                rule.modes.as_slice()
            })
    }

    /// Returns the first automatic mode for `path`.
    ///
    /// Multi-stage plans are intentionally retained by [`Self::modes_for_path`]
    /// for agents that request the subsequent, more detailed stage explicitly.
    #[must_use]
    pub fn preferred_mode_for_path(&self, path: impl AsRef<Path>) -> Option<&str> {
        self.modes_for_path(path).first().map(String::as_str)
    }
}

/// Loads a named kit from the project, user directory, or built-in catalogue.
pub fn load(name: &str) -> Result<LoadedKit, KitError> {
    validate_name(name)?;

    for path in project_kit_paths(name) {
        if path.is_file() {
            return load_from_path(&path, KitSource::Project(path.clone()));
        }
    }

    if let Some(path) = user_kit_path(name)
        && path.is_file()
    {
        return load_from_path(&path, KitSource::User(path.clone()));
    }

    if name == "code-review" {
        return parse(CODE_REVIEW_KIT, KitSource::Builtin);
    }

    Err(KitError::new(format!("kit '{name}' not found")))
}

/// Loads the configured kit, if one has been selected.
#[must_use]
pub fn active() -> Option<LoadedKit> {
    let name = std::env::var("LEAN_CTX_KIT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| crate::core::config::Config::load().active_kit);
    name.and_then(|name| load(&name).ok())
}

/// Returns the kit-selected automatic read mode for `path`.
#[must_use]
pub fn active_mode_for_path(path: &str) -> Option<String> {
    active().and_then(|loaded| loaded.kit.preferred_mode_for_path(path).map(str::to_string))
}

/// Lists the names in the built-in catalogue plus discoverable local kits.
#[must_use]
pub fn list() -> Vec<String> {
    let mut names = HashSet::from(["code-review".to_string()]);

    for root in project_kit_roots()
        .into_iter()
        .chain(user_kit_root().into_iter())
    {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("kit.toml").is_file()
                && let Some(name) = path.file_name().and_then(|name| name.to_str())
                && validate_name(name).is_ok()
            {
                names.insert(name.to_string());
            }
        }
    }

    let mut names: Vec<_> = names.into_iter().collect();
    names.sort();
    names
}

fn load_from_path(path: &Path, source: KitSource) -> Result<LoadedKit, KitError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| KitError::new(format!("cannot read {}: {error}", path.display())))?;
    parse(&content, source)
}

/// Parses and validates a Context Kit document without loading or activating it.
pub fn parse_document(content: &str) -> Result<ContextKit, KitError> {
    let kit = toml::from_str::<ContextKit>(content)
        .map_err(|error| KitError::new(format!("invalid kit TOML: {error}")))?;
    validate(&kit)?;
    Ok(kit)
}

fn parse(content: &str, source: KitSource) -> Result<LoadedKit, KitError> {
    let kit = parse_document(content)?;
    Ok(LoadedKit { kit, source })
}

fn validate(kit: &ContextKit) -> Result<(), KitError> {
    if kit.format_version != 1 {
        return Err(KitError::new(format!(
            "unsupported kit format version {}; expected 1",
            kit.format_version
        )));
    }
    validate_name(&kit.kit.name)?;
    if kit.kit.description.trim().is_empty() || kit.kit.version.trim().is_empty() {
        return Err(KitError::new(
            "kit description and version must not be empty",
        ));
    }
    validate_modes(&kit.read.default_modes, "read.default_modes")?;
    for rule in &kit.read.rules {
        if rule.glob.trim().is_empty() {
            return Err(KitError::new("read rule glob must not be empty"));
        }
        validate_modes(&rule.modes, &format!("read rule '{}'.modes", rule.glob))?;
    }
    if kit.priority.strategy != "git_changed" {
        return Err(KitError::new(
            "priority.strategy must be 'git_changed' for the current kit format",
        ));
    }
    if kit.priority.description.trim().is_empty() {
        return Err(KitError::new("priority.description must not be empty"));
    }
    if kit.shell.default_mode != "compressed" {
        return Err(KitError::new(
            "shell.default_mode must be 'compressed' for the current kit format",
        ));
    }
    if kit
        .shell
        .commands
        .iter()
        .any(|command| command.command.trim().is_empty())
    {
        return Err(KitError::new("shell command must not be empty"));
    }
    if kit.quality.criteria.is_empty()
        || kit
            .quality
            .criteria
            .iter()
            .any(|criterion| criterion.trim().is_empty())
    {
        return Err(KitError::new(
            "quality.criteria must contain non-empty criteria",
        ));
    }
    if kit.system_prompt.template.trim().is_empty() {
        return Err(KitError::new("system_prompt.template must not be empty"));
    }
    Ok(())
}

fn validate_modes(modes: &[String], field: &str) -> Result<(), KitError> {
    if modes.is_empty() {
        return Err(KitError::new(format!("{field} must not be empty")));
    }
    for mode in modes {
        crate::tools::ctx_read::ReadMode::from_str(mode).map_err(|error| {
            KitError::new(format!(
                "{field} contains unsupported mode '{mode}': {error}"
            ))
        })?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), KitError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(KitError::new(
            "kit name must contain only ASCII letters, digits, '-' or '_'",
        ));
    }
    Ok(())
}

fn project_kit_paths(name: &str) -> Vec<PathBuf> {
    project_kit_roots()
        .into_iter()
        .map(|root| root.join(name).join("kit.toml"))
        .collect()
}

fn project_kit_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let Ok(mut current) = std::env::current_dir() else {
        return roots;
    };
    for _ in 0..12 {
        roots.push(current.join("kits"));
        roots.push(current.join(".lean-ctx").join("kits"));
        if !current.pop() {
            break;
        }
    }
    roots
}

fn user_kit_root() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|directory| directory.join("kits"))
}

fn user_kit_path(name: &str) -> Option<PathBuf> {
    user_kit_root().map(|root| root.join(name).join("kit.toml"))
}

/// Matches a kit rule using the project's standard glob implementation.
fn glob_matches(pattern: &str, path: &str) -> bool {
    glob::Pattern::new(pattern).is_ok_and(|pattern| pattern.matches_path(Path::new(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_review_kit_has_the_requested_read_plan() {
        let loaded = load("code-review").expect("built-in kit loads");
        let kit = loaded.kit;

        assert_eq!(kit.modes_for_path("src/lib.rs"), ["map", "signatures"]);
        assert_eq!(kit.modes_for_path("tests/review.rs"), ["signatures"]);
        assert_eq!(kit.modes_for_path("Cargo.toml"), ["full"]);
        assert_eq!(kit.priority.strategy, "git_changed");
        assert!(
            kit.shell
                .commands
                .iter()
                .any(|command| command.command == "cargo test")
        );
        assert!(
            kit.shell
                .commands
                .iter()
                .any(|command| command.command == "cargo clippy")
        );
        assert!(!kit.quality.criteria.is_empty());
        assert!(kit.system_prompt.template.contains("actionable findings"));
    }

    #[test]
    fn kit_rejects_unknown_read_mode() {
        let invalid = CODE_REVIEW_KIT.replacen("modes = [\"full\"]", "modes = [\"outline\"]", 1);
        let error = parse(&invalid, KitSource::Builtin).expect_err("mode must be valid");
        assert!(error.to_string().contains("unsupported mode 'outline'"));
    }

    #[test]
    fn document_parser_preserves_builtin_contract_and_first_match() {
        let kit = parse_document(CODE_REVIEW_KIT).expect("built-in document parses");

        assert_eq!(kit.kit.name, "code-review");
        assert_eq!(kit.modes_for_path("src/tests/review.rs"), ["signatures"]);
        assert_eq!(kit.modes_for_path("src/lib.rs"), ["map", "signatures"]);
    }

    #[test]
    fn document_parser_rejects_unknown_fields_and_invalid_values_deterministically() {
        let cases = [
            (
                format!("{CODE_REVIEW_KIT}\nunexpected = true\n"),
                "unknown field",
            ),
            (
                CODE_REVIEW_KIT.replacen("format_version = 1", "format_version = 2", 1),
                "unsupported kit format version 2",
            ),
            (
                CODE_REVIEW_KIT.replacen("name = \"code-review\"", "name = \"../code-review\"", 1),
                "kit name must contain only ASCII",
            ),
            (
                CODE_REVIEW_KIT.replacen(
                    "default_modes = [\"map\", \"signatures\"]",
                    "default_modes = []",
                    1,
                ),
                "read.default_modes must not be empty",
            ),
            (
                CODE_REVIEW_KIT.replacen("command = \"cargo test\"", "command = \" \"", 1),
                "shell command must not be empty",
            ),
            (
                CODE_REVIEW_KIT.replacen(
                    "\"Report only actionable defects introduced or exposed by the change.\"",
                    "\"\"",
                    1,
                ),
                "quality.criteria must contain non-empty criteria",
            ),
        ];

        for (document, expected) in cases {
            let first = parse_document(&document).expect_err("document must be rejected");
            let second = parse_document(&document).expect_err("repeated parse must be rejected");
            assert_eq!(first, second);
            assert!(first.to_string().contains(expected), "{first}");
        }

        let mut empty_template = CODE_REVIEW_KIT.to_string();
        let prefix = "template = '''";
        let start = empty_template.find(prefix).expect("template starts") + prefix.len();
        let end = start + empty_template[start..].find("'''").expect("template ends");
        empty_template.replace_range(start..end, "   ");
        let error = parse_document(&empty_template).expect_err("template must not be empty");
        assert_eq!(
            error.to_string(),
            "system_prompt.template must not be empty"
        );
    }

    #[test]
    fn document_parser_never_executes_declared_commands() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let marker = directory.path().join("executed");
        let marker_toml = marker
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let command = format!("command = \"touch {marker_toml}\"");
        let document = CODE_REVIEW_KIT.replacen("command = \"cargo test\"", &command, 1);

        parse_document(&document).expect("command remains inert data");

        assert!(!marker.exists());
    }

    #[test]
    fn glob_matching_honors_path_segments() {
        assert!(glob_matches("**/tests/**/*.rs", "tests/unit/review.rs"));
        assert!(glob_matches("**/tests/**/*.rs", "src/tests/review.rs"));
        assert!(!glob_matches("**/tests/**/*.rs", "src/test_helpers.rs"));
        assert!(glob_matches("**/*_test.rs", "src/parser_test.rs"));
    }

    #[test]
    fn kit_name_cannot_escape_its_directory() {
        assert!(load("../code-review").is_err());
    }
}
