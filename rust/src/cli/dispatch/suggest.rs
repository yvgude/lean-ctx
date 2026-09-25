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
    "measure",
    "learning",
    "conformance",
    "selftest",
    "health",
    "billing",
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
    "prove",
    "value",
    "verify",
    "eval",
    "verify-cache",
    "cache-selftest",
    "visualize",
    "audit",
    "compliance",
    "agent",
    "instructions",
    "index",
    "semantic-search",
    "search-code",
    "explore",
    "repomap",
    "repo-map",
    "cep",
    "dashboard",
    "team",
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
    "benchmark-run",
    "calibrate",
    "pair",
    "compact",
    "profile",
    "tools",
    "config",
    "allow",
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
    "buddy",
    "cloud",
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
    crate::core::levenshtein::closest(input, KNOWN_COMMANDS.iter().copied())
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
