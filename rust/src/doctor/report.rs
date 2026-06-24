//! Structured installation-health report for the dashboard doctor signal (#466).
//!
//! `lean-ctx doctor`'s terminal renderer ([`super::run`]) emits ANSI-coloured
//! lines that are unfit for JSON. This module re-derives the same pass/fail
//! predicates that [`super::compact_score`] counts into a clean, serializable
//! shape the dashboard renders as a three-level health badge
//! (good / warnings / issues) with a per-check breakdown — without shelling out
//! or scraping coloured stdout, so the CLI and the dashboard stay in lockstep
//! from a single source of truth.

use serde::Serialize;

use super::checks::{
    capacity_warnings, mcp_config_outcome, mcp_server_cwd_outcome, shell_aliases_outcome,
    skill_files_outcome,
};
use super::common::{path_in_path_env, resolve_lean_ctx_binary};
use super::deprecations::deprecations_outcome;

/// Three-level health signal mirroring the issue's badge states (#466).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum HealthLevel {
    /// Every scored check passed and no advisories fired.
    Good,
    /// All scored checks pass, but non-critical advisories exist.
    Warnings,
    /// At least one scored check failed — needs attention.
    Issues,
}

/// One scored install check, dashboard-ready (plain text, no ANSI).
#[derive(Serialize)]
pub struct HealthCheck {
    pub id: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// The structured payload served at `GET /api/doctor`.
#[derive(Serialize)]
pub struct HealthReport {
    pub level: HealthLevel,
    pub passed: u32,
    pub total: u32,
    pub checks: Vec<HealthCheck>,
    pub warnings: Vec<String>,
}

impl HealthReport {
    /// Map scored checks + advisories onto the three-level badge: a failed check
    /// is always `Issues`; otherwise advisories (capacity, deprecations) demote a
    /// clean install to `Warnings`; a spotless install is `Good`.
    fn classify(passed: u32, total: u32, has_warnings: bool) -> HealthLevel {
        if passed < total {
            HealthLevel::Issues
        } else if has_warnings {
            HealthLevel::Warnings
        } else {
            HealthLevel::Good
        }
    }
}

fn check(id: &'static str, ok: bool, pass: &str, fail: &str) -> HealthCheck {
    HealthCheck {
        id,
        ok,
        detail: if ok { pass } else { fail }.to_string(),
    }
}

/// Strip ANSI SGR sequences (`ESC[…m`) and collapse whitespace so a
/// terminal-formatted doctor line becomes a single clean JSON/text string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip the CSI sequence up to and including its final 'm'.
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Build the structured installation-health report.
#[must_use]
pub fn health_report() -> HealthReport {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok();

    let binary_ok = resolve_lean_ctx_binary().is_some() || path_in_path_env();
    let data_dir_ok = data_dir.as_ref().is_some_and(|p| p.is_dir());
    let stats_ok = data_dir
        .as_ref()
        .map(|d| d.join("stats.json"))
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| m.is_file());

    // These three are the authoritative pass/fail predicates the terminal doctor
    // and `compact_score` already use — reuse `.ok` so the badge can never drift
    // from `lean-ctx doctor`; only the human-readable text is dashboard-specific.
    let shell_ok = shell_aliases_outcome().ok;
    let mcp_ok = mcp_config_outcome().ok;
    let skills_ok = skill_files_outcome().ok;

    let checks = vec![
        check(
            "binary",
            binary_ok,
            "lean-ctx is on your PATH",
            "lean-ctx is not on your PATH — run `lean-ctx init`",
        ),
        check(
            "data_dir",
            data_dir_ok,
            "data directory present",
            "data directory missing — it is created on first use",
        ),
        check(
            "stats",
            stats_ok,
            "usage statistics are being recorded",
            "no usage statistics yet — route a few commands through lean-ctx",
        ),
        check(
            "shell",
            shell_ok,
            "shell integration active",
            "shell integration not detected — run `lean-ctx init --global`",
        ),
        check(
            "mcp",
            mcp_ok,
            "MCP server registered",
            "MCP server not registered — click Fix or run `lean-ctx doctor --fix`",
        ),
        check(
            "skills",
            skills_ok,
            "agent skill files installed",
            "agent skill files missing — click Fix or run `lean-ctx doctor --fix`",
        ),
    ];

    let passed = u32::try_from(checks.iter().filter(|c| c.ok).count()).unwrap_or(u32::MAX);
    let total = u32::try_from(checks.len()).unwrap_or(u32::MAX);

    // Advisories never fail the install (the checks above are green) but warrant
    // a ⚠ badge: memory stores under capacity pressure and active deprecations.
    let mut warnings: Vec<String> = capacity_warnings()
        .into_iter()
        .filter(|o| !o.ok)
        .map(|o| strip_ansi(&o.line))
        .collect();
    let dep = deprecations_outcome();
    if !dep.ok {
        warnings.push(strip_ansi(&dep.line));
    }
    let mcp_cwd = mcp_server_cwd_outcome();
    if !mcp_cwd.ok {
        warnings.push(strip_ansi(&mcp_cwd.line));
    }

    let level = HealthReport::classify(passed, total, !warnings.is_empty());

    HealthReport {
        level,
        passed,
        total,
        checks,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_colour_and_collapses_whitespace() {
        let raw = "\u{1b}[1mShell\u{1b}[0m  \u{1b}[32mconfigured\u{1b}[0m\n      in ~/.zshrc";
        assert_eq!(strip_ansi(raw), "Shell configured in ~/.zshrc");
    }

    #[test]
    fn strip_ansi_is_idempotent_on_plain_text() {
        assert_eq!(strip_ansi("already clean"), "already clean");
    }

    #[test]
    fn classify_issues_when_any_check_failed() {
        assert_eq!(
            HealthReport::classify(5, 6, false),
            HealthLevel::Issues,
            "a failed scored check always wins over advisories"
        );
        assert_eq!(HealthReport::classify(5, 6, true), HealthLevel::Issues);
    }

    #[test]
    fn classify_warnings_when_clean_but_advisories() {
        assert_eq!(HealthReport::classify(6, 6, true), HealthLevel::Warnings);
    }

    #[test]
    fn classify_good_when_spotless() {
        assert_eq!(HealthReport::classify(6, 6, false), HealthLevel::Good);
    }

    /// Read-only invariant: whatever the host state, the report is internally
    /// consistent (counts match the checks, level matches the predicates) and
    /// every detail string is ANSI-free.
    #[test]
    fn health_report_is_self_consistent() {
        let r = health_report();
        assert_eq!(r.total, 6, "six scored install checks");
        assert_eq!(
            r.passed,
            u32::try_from(r.checks.iter().filter(|c| c.ok).count()).unwrap(),
            "passed must equal the number of green checks"
        );
        let expected = HealthReport::classify(r.passed, r.total, !r.warnings.is_empty());
        assert_eq!(
            r.level, expected,
            "level must follow the classification rule"
        );
        for c in &r.checks {
            assert!(!c.detail.contains('\u{1b}'), "no ANSI in check detail");
            assert!(!c.detail.is_empty());
        }
        for w in &r.warnings {
            assert!(!w.contains('\u{1b}'), "no ANSI in warnings");
        }
    }

    #[test]
    fn report_serializes_with_lowercase_level() {
        let report = HealthReport {
            level: HealthLevel::Warnings,
            passed: 6,
            total: 6,
            checks: vec![check("binary", true, "ok", "bad")],
            warnings: vec!["facts: 206/200 (103%)".to_string()],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains(r#""level":"warnings""#));
        assert!(json.contains(r#""id":"binary""#));
        assert!(json.contains(r#""passed":6"#));
    }
}
