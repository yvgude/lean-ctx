//! "Did you mean?" suggestions for mistyped top-level CLI commands.
//!
//! The dispatch match in [`super`] is the source of truth for what commands
//! exist; this list mirrors the user-facing names (primary spellings plus the
//! common aliases). A missing entry only costs a suggestion — never a wrong
//! dispatch — so it can lag the match slightly without breaking anything.

/// Known top-level command names + their well-known aliases.
const KNOWN_COMMANDS: &[&str] = &[
    "shell",
    "gain",
    "spend",
    "savings",
    "learning",
    "conformance",
    "selftest",
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
    "addon",
    "addons",
    "rules",
    "proof",
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
    "gotchas",
    "learn",
    "buddy",
    "cloud",
    "help",
    "mcp",
];

/// Returns the closest known command to `input`, or `None` when nothing is
/// near enough to suggest with confidence.
///
/// The edit-distance budget scales with length (short words tolerate one edit,
/// longer ones roughly a third) so `udpate` -> `update` is offered while a
/// genuinely unrelated word stays unsuggested.
pub(super) fn closest_command(input: &str) -> Option<&'static str> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let budget = (input.chars().count() / 3).max(1);
    KNOWN_COMMANDS
        .iter()
        .map(|&cmd| (cmd, levenshtein(input, cmd)))
        .filter(|&(_, dist)| dist <= budget)
        .min_by_key(|&(_, dist)| dist)
        .map(|(cmd, _)| cmd)
}

/// Classic Wagner-Fischer edit distance over Unicode scalar values, with a
/// single rolling row (O(min) memory) since command names are tiny.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basic_distances() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("abc", "abd"), 1);
        assert_eq!(levenshtein("udpate", "update"), 2);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

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
