//! Shell activation mode — controls when lean-ctx aliases auto-activate.

use serde::{Deserialize, Serialize};

use super::Config;

/// Controls when the shell hook auto-activates command aliases.
///
/// - `AgentsOnly`: (Default since #699) Aliases only activate when an AI agent
///   env var is detected (`LEAN_CTX_AGENT`, `CURSOR_AGENT`, `CLAUDECODE`,
///   `CODEBUDDY`, `CODEX_CLI_SESSION`, `GEMINI_SESSION`). lean-ctx exists to
///   save *agent* tokens — in a plain human terminal the aliases add overhead
///   and surface allowlist diagnostics with no benefit (GH #699).
/// - `Always`: Aliases are active in every interactive shell — the pre-#699
///   default, still available for `lean-ctx wrapped` fans who want their own
///   shell usage tracked.
/// - `Off`: Aliases never auto-activate. The user must call `lean-ctx-on` manually.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ShellActivation {
    Always,
    #[default]
    AgentsOnly,
    Off,
}

impl ShellActivation {
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

    pub fn effective(config: &Config) -> Self {
        if let Some(env_val) = Self::from_env() {
            return env_val;
        }
        config.shell_activation.clone()
    }

    /// Returns the shell condition snippet that guards auto-activation.
    /// Used in generated shell hooks (posix, fish, powershell).
    pub fn posix_guard(&self) -> &'static str {
        match self {
            Self::Always => {
                r#"if [ -z "${LEAN_CTX_ACTIVE:-}" ] && [ -z "${LEAN_CTX_DISABLED:-}" ] && [ "${LEAN_CTX_ENABLED:-1}" != "0" ]; then"#
            }
            Self::AgentsOnly => {
                r#"if [ -z "${LEAN_CTX_ACTIVE:-}" ] && [ -z "${LEAN_CTX_DISABLED:-}" ] && [ "${LEAN_CTX_ENABLED:-1}" != "0" ] && { [ -n "${LEAN_CTX_AGENT:-}" ] || [ -n "${CURSOR_AGENT:-}" ] || [ -n "${CLAUDECODE:-}" ] || [ -n "${CODEBUDDY:-}" ] || [ -n "${CODEX_CLI_SESSION:-}" ] || [ -n "${GEMINI_SESSION:-}" ]; }; then"#
            }
            Self::Off => "",
        }
    }

    pub fn fish_guard(&self) -> &'static str {
        match self {
            Self::Always => {
                "if not set -q LEAN_CTX_ACTIVE; and not set -q LEAN_CTX_DISABLED; and test (set -q LEAN_CTX_ENABLED; and echo $LEAN_CTX_ENABLED; or echo 1) != '0'"
            }
            Self::AgentsOnly => {
                "if not set -q LEAN_CTX_ACTIVE; and not set -q LEAN_CTX_DISABLED; and test (set -q LEAN_CTX_ENABLED; and echo $LEAN_CTX_ENABLED; or echo 1) != '0'; and begin; set -q LEAN_CTX_AGENT; or set -q CURSOR_AGENT; or set -q CLAUDECODE; or set -q CODEBUDDY; or set -q CODEX_CLI_SESSION; or set -q GEMINI_SESSION; end"
            }
            Self::Off => "",
        }
    }

    pub fn powershell_guard(&self) -> &'static str {
        match self {
            Self::Always => {
                "if (-not $env:LEAN_CTX_ACTIVE -and -not $env:LEAN_CTX_DISABLED -and -not $env:LEAN_CTX_NO_HOOK)"
            }
            Self::AgentsOnly => {
                "if (-not $env:LEAN_CTX_ACTIVE -and -not $env:LEAN_CTX_DISABLED -and -not $env:LEAN_CTX_NO_HOOK -and ($env:LEAN_CTX_AGENT -or $env:CURSOR_AGENT -or $env:CLAUDECODE -or $env:CODEBUDDY -or $env:CODEX_CLI_SESSION -or $env:GEMINI_SESSION))"
            }
            Self::Off => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GH #699: lean-ctx must be transparent in a plain human terminal —
    /// aliases (and their allowlist diagnostics) only belong in agent
    /// sessions unless the user explicitly opts into `always`.
    #[test]
    fn default_is_agents_only() {
        assert_eq!(ShellActivation::default(), ShellActivation::AgentsOnly);
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
        assert!(
            !ShellActivation::Always.posix_guard().is_empty(),
            "`ShellActivation::Always.posix_guard()` is empty"
        );
    }

    #[test]
    fn posix_guard_agents_checks_env_vars() {
        let guard = ShellActivation::AgentsOnly.posix_guard();
        assert!(guard.contains("LEAN_CTX_AGENT"));
        assert!(guard.contains("CURSOR_AGENT"));
        assert!(guard.contains("CLAUDECODE"));
        assert!(guard.contains("CODEBUDDY"));
        assert!(guard.contains("CODEX_CLI_SESSION"));
        assert!(guard.contains("GEMINI_SESSION"));
    }

    /// The agents-only default only works if every guard flavor recognizes
    /// the same agent markers — a shell where Cursor's env var is missing
    /// from one variant silently loses the hook there.
    #[test]
    fn all_guards_recognize_cursor_agent() {
        assert!(
            ShellActivation::AgentsOnly
                .fish_guard()
                .contains("CURSOR_AGENT")
        );
        assert!(
            ShellActivation::AgentsOnly
                .powershell_guard()
                .contains("CURSOR_AGENT")
        );
    }

    #[test]
    fn posix_guard_off_is_empty() {
        {
            let found = &ShellActivation::Off.posix_guard();
            assert!(found.is_empty(), "{found:?}");
        }
    }
}
