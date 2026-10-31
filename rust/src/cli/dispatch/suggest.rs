//! "Did you mean?" suggestions for mistyped top-level CLI commands.
//!
//! The dispatch match in [`super`] is the source of truth for what commands
//! exist; this list mirrors the user-facing names (primary spellings plus the
//! common aliases). A missing entry only costs a suggestion — never a wrong
//! dispatch — so it can lag the match slightly without breaking anything.

/// Known top-level command names + their well-known aliases.
pub(crate) const KNOWN_COMMANDS: &[&str] = &[
    "shell",
    "gain",
    "spend",
    "savings",
    "value-report",
    "value_report",
    "evidence-export",
    "shadow",
    "triage",
    "learning",
    "conformance",
    "selftest",
    "health",
    "finops",
    "roi",
    "output-savings",
    "token-report",
    "report-tokens",
    "pack",
    "policy",
    "plugin",
    "plugins",
    "model",
    "addon",
    "addons",
    "rules",
    "proof",
    "value",
    "statusline",
    "inspect",
    "gateway",
    "claude-mod",
    "prompt-segment",
    "verify",
    "verify-cache",
    "cache-selftest",
    "audit",
    "agent",
    "instructions",
    "index",
    "semantic-search",
    "search-code",
    "explore",
    "repomap",
    "repo-map",
    "cep",
    "cockpit",
    "dashboard",
    "provider",
    "serve",
    "watch",
    "proxy",
    "daemon",
    "init",
    "setup",
    "onboard",
    "install",
    "bootstrap",
    "status",
    "read",
    "call",
    "diff",
    "grep",
    "glob",
    "find",
    "ls",
    "deps",
    "discover",
    "ghost",
    "filter",
    "heatmap",
    "graph",
    "smells",
    "session",
    "sessions",
    "ledger",
    "control",
    "plan",
    "compile",
    "knowledge",
    "skillify",
    "summary",
    "overview",
    "compress",
    "wrapped",
    "benchmark",
    "compact",
    "profile",
    "tools",
    "config",
    "allow",
    "allow-path",
    "project",
    "workspace",
    "security",
    "yolo",
    "secure",
    "lockdown",
    "trust",
    "untrust",
    "stats",
    "introspect",
    "cache",
    "theme",
    "tee",
    "terse",
    "compression",
    "cheatsheet",
    "update",
    "upgrade",
    "restart",
    "stop",
    "dev-install",
    "codesign-setup",
    "doctor",
    "harden",
    "export-rules",
    "completions",
    "gotchas",
    "learn",
    "hive",
    "telemetry",
    "wrap",
    "unwrap",
    "help",
    "mcp",
];

/// Returns the closest known command to `input`, or `None` when nothing is
/// near enough to suggest with confidence. Distance + budget live in the
/// shared [`crate::core::levenshtein`] helper (#712), so CLI commands, config
/// keys and MCP tool names all suggest with identical semantics.
pub(super) fn closest_command(input: &str) -> Option<&'static str> {
    // `hive` is not suggested until the release switch is on (3.12.1).
    let visible = KNOWN_COMMANDS
        .iter()
        .copied()
        .filter(|cmd| crate::core::hive::HIVE_AVAILABLE || *cmd != "hive");
    crate::core::levenshtein::closest(input, visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_typos() {
        assert_eq!(closest_command("udpate"), Some("update"));
        assert_eq!(closest_command("doctr"), Some("doctor"));
        assert_eq!(closest_command("statuss"), Some("status"));
        assert_eq!(closest_command("upgrad"), Some("upgrade"));
    }

    #[test]
    fn hive_is_suggested_only_when_available() {
        let hive = closest_command("hiv") == Some("hive");
        assert_eq!(hive, crate::core::hive::HIVE_AVAILABLE);
    }

    #[test]
    fn exact_match_returns_itself() {
        assert_eq!(closest_command("read"), Some("read"));
        assert_eq!(closest_command("doctor"), Some("doctor"));
    }

    #[test]
    fn rejects_unrelated_input() {
        assert_eq!(closest_command("xyzzyplughfoo"), None);
        assert_eq!(closest_command(""), None);
        assert_eq!(closest_command("   "), None);
    }
}
