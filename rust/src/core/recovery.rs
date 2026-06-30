//! Recovery-hint policy + grammar.
//!
//! lean-ctx compression is fully reversible, but agents only act on that if we
//! *show* the escape hatch — otherwise they re-read compressed output
//! line-by-line (the "too compressed" complaint). The proactive `RECOVER` rule
//! ([`crate::core::rules_canonical::RECOVER`]) teaches the vocabulary once in the
//! system prompt; this module is the reactive half: it resolves the effective
//! [`RecoveryHints`] tier and owns the canonical, **non-MCP-first** phrasings so
//! every compressed-output site (`ctx_read`, `ctx_shell` tee, archive / firewall
//! / spill handles) renders identical, byte-stable wording.
//!
//! Determinism (#498): the tier is a pure function of profile + config + env,
//! never of per-call session state, and every phrasing is a pure function of its
//! inputs (paths/ids are content-addressed). The footer is therefore "deduped by
//! mode" — only lossy/compressed views carry it; escalating to `full`/`raw` (the
//! recovery action itself) drops it — rather than by fragile session state that
//! would break the byte-stability guards.

use crate::core::config::{Config, RecoveryHints};

/// Header line of the `Full`-tier compressed-view footer. Single source of truth:
/// also surfaced verbatim in `tdd_schema` so the published schema and the runtime
/// affordance can never drift.
pub const COMPACT_VIEW_HEADER: &str =
    "[lean-ctx: compact view — nothing lost, full source on request]";

/// Resolve the effective recovery tier for the current call.
///
/// Resolution order:
/// 1. `LEAN_CTX_RECOVERY_HINTS` env (`off|minimal|full`) — ops / test override.
/// 2. Active profile: `output_hints.compressed_hint = Some(true)` →
///    [`RecoveryHints::Full`] (the `exploration` / `review` profiles);
///    `Some(false)` → [`RecoveryHints::Off`] (explicit profile opt-out).
/// 3. Global `config.recovery_hints` (default [`RecoveryHints::Minimal`]).
#[must_use]
pub fn tier() -> RecoveryHints {
    if let Some(t) = RecoveryHints::from_env() {
        return t;
    }
    match crate::core::profiles::active_profile()
        .output_hints
        .compressed_hint
    {
        Some(true) => RecoveryHints::Full,
        Some(false) => RecoveryHints::Off,
        None => Config::load().recovery_hints,
    }
}

/// Footer appended to a compressed `ctx_read` view. Leads with the MCP-free path
/// ("read the file directly") so orgs that forbid MCP still have a route, then
/// the `ctx_*` shortcuts. `None` when the tier is `Off`.
#[must_use]
pub fn read_footer(file_path: &str) -> Option<String> {
    match tier() {
        RecoveryHints::Off => None,
        RecoveryHints::Minimal => Some(format!(
            "[lean-ctx] full source: read \"{file_path}\" directly (no MCP)  ·  or ctx_read(\"{file_path}\", mode=\"full\")"
        )),
        // The header line is the SSOT [`COMPACT_VIEW_HEADER`] (also in `tdd_schema`);
        // the ladder now leads with the native path before the MCP shortcuts.
        RecoveryHints::Full => Some(format!(
            "{COMPACT_VIEW_HEADER}\n  full: read \"{file_path}\" directly (no MCP)  ·  ctx_read(\"{file_path}\", mode=\"full\")  ·  exact bytes: ctx_read(\"{file_path}\", raw=true)  ·  recover: ctx_retrieve(\"{file_path}\")"
        )),
    }
}

/// Canonical recovery clause for a content-addressed handle (archive / firewall /
/// spill / `ctx_shell` tee). Always names the on-disk path first (MCP-free), then
/// the `ctx_expand(id=...)` shortcut. Unlike [`read_footer`] this is *functional*
/// (it points at where the bytes live), so it is not tier-gated — but `Off`
/// collapses it to the bare path so a `recovery_hints=off` operator still gets the
/// pointer without the coaching.
#[must_use]
pub fn handle_clause(id: &str, on_disk_path: Option<&str>) -> String {
    match (tier(), on_disk_path) {
        (RecoveryHints::Off, Some(p)) => format!("full: {p}"),
        (RecoveryHints::Off, None) => format!("full: ctx_expand(id=\"{id}\")"),
        (_, Some(p)) => format!("full: read {p} directly (no MCP)  ·  or ctx_expand(id=\"{id}\")"),
        (_, None) => format!("full: ctx_expand(id=\"{id}\")  ·  or read the shown path"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::test_env_lock;

    /// The env override forces a tier regardless of profile/config — the knob ops
    /// and tests use to pin behaviour.
    #[test]
    fn env_override_pins_tier() {
        let _lock = test_env_lock();
        crate::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "off");
        assert_eq!(tier(), RecoveryHints::Off);
        crate::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "full");
        assert_eq!(tier(), RecoveryHints::Full);
        crate::test_env::remove_var("LEAN_CTX_RECOVERY_HINTS");
    }

    #[test]
    fn read_footer_leads_with_native_path_and_respects_off() {
        let _lock = test_env_lock();
        crate::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "off");
        assert!(
            read_footer("src/x.rs").is_none(),
            "off suppresses the footer"
        );

        crate::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "minimal");
        let minimal = read_footer("src/x.rs").expect("minimal emits a footer");
        assert!(minimal.lines().count() == 1, "minimal is a single line");
        // Non-MCP path must come before the MCP shortcut.
        let native = minimal.find("read \"src/x.rs\" directly").unwrap();
        let mcp = minimal.find("ctx_read(").unwrap();
        assert!(native < mcp, "native path must precede the MCP route");

        crate::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "full");
        let full = read_footer("src/x.rs").expect("full emits a footer");
        assert!(full.contains("raw=true") && full.contains("ctx_retrieve"));
        assert!(
            full.find("read \"src/x.rs\" directly").unwrap() < full.find("raw=true").unwrap(),
            "full ladder still leads with the native path"
        );
        crate::test_env::remove_var("LEAN_CTX_RECOVERY_HINTS");
    }

    /// Determinism (#498): the footer/clause are pure functions of their inputs,
    /// so repeated calls are byte-identical (provider prompt caching depends on it).
    #[test]
    fn footer_and_clause_are_byte_stable() {
        let _lock = test_env_lock();
        crate::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "minimal");
        assert_eq!(read_footer("src/a.rs"), read_footer("src/a.rs"));
        assert_eq!(
            handle_clause("id1", Some("/tmp/t.log")),
            handle_clause("id1", Some("/tmp/t.log"))
        );
        crate::test_env::remove_var("LEAN_CTX_RECOVERY_HINTS");
    }

    #[test]
    fn handle_clause_is_non_mcp_first() {
        let _lock = test_env_lock();
        crate::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "minimal");
        let with_path = handle_clause("abc123", Some("/tmp/tee/run.log"));
        assert!(
            with_path.find("/tmp/tee/run.log").unwrap() < with_path.find("ctx_expand").unwrap(),
            "on-disk path must precede ctx_expand"
        );
        let no_path = handle_clause("abc123", None);
        assert!(no_path.contains("ctx_expand(id=\"abc123\")"));

        crate::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "off");
        assert_eq!(
            handle_clause("abc123", Some("/tmp/tee/run.log")),
            "full: /tmp/tee/run.log",
            "off keeps the bare functional pointer, drops the coaching"
        );
        crate::test_env::remove_var("LEAN_CTX_RECOVERY_HINTS");
    }
}
