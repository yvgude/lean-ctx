//! Shell activation mode — controls when lean-ctx aliases auto-activate.

use serde::{Deserialize, Serialize};

use super::Config;

/// Controls when the shell hook auto-activates command aliases.
///
/// - `Always`: (Default) Aliases are active in every interactive shell.
/// - `AgentsOnly`: Aliases only activate when an AI agent env var is detected
///   (e.g. `LEAN_CTX_AGENT`, `CLAUDECODE`, `CODEX_CLI_SESSION`, `GEMINI_SESSION`).
///   Perfect for users who only want lean-ctx when AI agents run shell commands.
/// - `Off`: Aliases never auto-activate. The user must call `lean-ctx-on` manually.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ShellActivation {
    #[default]
    Always,
    AgentsOnly,
    Off,
}

impl ShellActivation {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_SHELL_ACTIVATION")
            .ok()
            .and_then(|v| match v.trim().to_lowercase().as_str() {
                "always" => Some(Self::Always),
                "agents-only" | "agents_only" | "agentsonly" => Some(Self::AgentsOnly),
                "off" | "none" | "manual" => Some(Self::Off),
                _ => None,
            })
    }

    #[must_use]
    pub fn effective(config: &Config) -> Self {
        if let Some(env_val) = Self::from_env() {
            return env_val;
        }
        config.shell_activation.clone()
    }

    /// Returns the shell condition snippet that guards auto-activation.
    /// Used in generated shell hooks (posix, fish, powershell).
    #[must_use]
    pub fn posix_guard(&self) -> &'static str {
        match self {
            Self::Always => {
                r#"if [ -z "${LEAN_CTX_ACTIVE:-}" ] && [ -z "${LEAN_CTX_DISABLED:-}" ] && [ "${LEAN_CTX_ENABLED:-1}" != "0" ]; then"#
            }
            Self::AgentsOnly => {
                r#"if [ -z "${LEAN_CTX_ACTIVE:-}" ] && [ -z "${LEAN_CTX_DISABLED:-}" ] && [ "${LEAN_CTX_ENABLED:-1}" != "0" ] && { [ -n "${LEAN_CTX_AGENT:-}" ] || [ -n "${CLAUDECODE:-}" ] || [ -n "${CODEBUDDY:-}" ] || [ -n "${CODEX_CLI_SESSION:-}" ] || [ -n "${GEMINI_SESSION:-}" ]; }; then"#
            }
            Self::Off => "",
        }
    }

    #[must_use]
    pub fn fish_guard(&self) -> &'static str {
        match self {
            Self::Always => {
                "if not set -q LEAN_CTX_ACTIVE; and not set -q LEAN_CTX_DISABLED; and test (set -q LEAN_CTX_ENABLED; and echo $LEAN_CTX_ENABLED; or echo 1) != '0'"
            }
            Self::AgentsOnly => {
                "if not set -q LEAN_CTX_ACTIVE; and not set -q LEAN_CTX_DISABLED; and test (set -q LEAN_CTX_ENABLED; and echo $LEAN_CTX_ENABLED; or echo 1) != '0'; and begin; set -q LEAN_CTX_AGENT; or set -q CLAUDECODE; or set -q CODEBUDDY; or set -q CODEX_CLI_SESSION; or set -q GEMINI_SESSION; end"
            }
            Self::Off => "",
        }
    }

    #[must_use]
    pub fn powershell_guard(&self) -> &'static str {
        match self {
            Self::Always => {
                "if (-not $env:LEAN_CTX_ACTIVE -and -not $env:LEAN_CTX_DISABLED -and -not $env:LEAN_CTX_NO_HOOK)"
            }
            Self::AgentsOnly => {
                "if (-not $env:LEAN_CTX_ACTIVE -and -not $env:LEAN_CTX_DISABLED -and -not $env:LEAN_CTX_NO_HOOK -and ($env:LEAN_CTX_AGENT -or $env:CLAUDECODE -or $env:CODEBUDDY -or $env:CODEX_CLI_SESSION -or $env:GEMINI_SESSION))"
            }
            Self::Off => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_always() {
        assert_eq!(ShellActivation::default(), ShellActivation::Always);
    }

    #[test]
    fn serde_roundtrip() {
        let toml_str = r#"shell_activation = "agents-only""#;
        #[derive(Deserialize)]
        struct Wrapper {
            shell_activation: ShellActivation,
        }
        let w: Wrapper = toml::from_str(toml_str).unwrap();
        assert_eq!(w.shell_activation, ShellActivation::AgentsOnly);
    }

    #[test]
    fn posix_guard_always_has_content() {
        assert!(!ShellActivation::Always.posix_guard().is_empty());
    }

    #[test]
    fn posix_guard_agents_checks_env_vars() {
        let guard = ShellActivation::AgentsOnly.posix_guard();
        assert!(guard.contains("LEAN_CTX_AGENT"));
        assert!(guard.contains("CLAUDECODE"));
        assert!(guard.contains("CODEBUDDY"));
        assert!(guard.contains("CODEX_CLI_SESSION"));
        assert!(guard.contains("GEMINI_SESSION"));
    }

    #[test]
    fn posix_guard_off_is_empty() {
        assert!(ShellActivation::Off.posix_guard().is_empty());
    }
}
