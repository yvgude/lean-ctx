pub mod config;
pub mod drift;
pub mod lint;
pub mod sync;

pub use config::RulesConfig;
pub use drift::{DriftReport, DriftStatus};
pub use lint::{LintSeverity, LintWarning};
pub use sync::SyncReport;

use std::path::Path;

pub struct ContextOps {
    pub home: std::path::PathBuf,
    pub project_root: std::path::PathBuf,
}

impl ContextOps {
    pub fn new(home: &Path, project_root: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            project_root: project_root.to_path_buf(),
        }
    }

    /// Drift between each agent's on-disk rules block and the canonical source.
    ///
    /// Infallible: drift is computed against `rules_canonical` (always present in
    /// the binary), **not** against `.lean-ctx/rules.toml`, so `rules diff` needs
    /// no prior `rules init` and can never fail on a missing config (#548).
    pub fn detect_drift(&self) -> Vec<DriftReport> {
        drift::detect_drift(&self.home)
    }

    pub fn sync_all(&self) -> SyncReport {
        sync::sync_all(&self.home)
    }

    pub fn sync_agent(&self, agent: &str) -> SyncReport {
        sync::sync_agent(&self.home, agent)
    }

    pub fn lint(&self) -> Result<Vec<LintWarning>, String> {
        let config = RulesConfig::load(&self.project_root)?;
        Ok(lint::lint(&config, &self.home))
    }

    pub fn status(&self) -> Vec<crate::rules_inject::RulesTargetStatus> {
        crate::rules_inject::collect_rules_status(&self.home)
    }

    pub fn init(&self) -> Result<RulesConfig, String> {
        RulesConfig::init_from_existing(&self.project_root, &self.home)
    }

    pub fn has_config(&self) -> bool {
        RulesConfig::config_path(&self.project_root).exists()
    }
}

pub fn format_status(statuses: &[crate::rules_inject::RulesTargetStatus]) -> String {
    let mut lines = Vec::new();
    lines.push("Agent Rules Status:".to_string());
    lines.push(String::new());

    for s in statuses {
        let icon = match s.state.as_str() {
            "up_to_date" => "✓",
            "outdated" => "⚠",
            "missing" => "✗",
            "not_detected" => "·",
            _ => "?",
        };
        let detected = if s.detected { "" } else { " (not installed)" };
        lines.push(format!("  [{icon}] {}{detected} — {}", s.name, s.state));
    }

    lines.join("\n")
}

pub fn format_drift(reports: &[DriftReport]) -> String {
    let mut lines = Vec::new();
    lines.push("Drift Report:".to_string());
    lines.push(String::new());

    for r in reports {
        if r.status == DriftStatus::NotDetected {
            continue;
        }
        lines.push(format!("  [{}] {} ({})", r.status, r.target, r.path));
        if let Some(diff) = &r.diff {
            for dl in diff.lines().take(10) {
                lines.push(format!("    {dl}"));
            }
            let total = diff.lines().count();
            if total > 10 {
                lines.push(format!("    ... ({} more lines)", total - 10));
            }
        }
    }

    lines.join("\n")
}

pub fn format_lint(warnings: &[LintWarning]) -> String {
    if warnings.is_empty() {
        return "No lint issues found.".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!("Lint Results ({} issues):", warnings.len()));
    lines.push(String::new());

    for w in warnings {
        let target = w
            .target
            .as_deref()
            .map(|t| format!(" [{t}]"))
            .unwrap_or_default();
        lines.push(format!(
            "  [{severity}] {code}{target}: {msg}",
            severity = w.severity,
            code = w.code,
            msg = w.message,
        ));
    }

    lines.join("\n")
}

pub fn format_sync(report: &SyncReport) -> String {
    let mut lines = Vec::new();
    lines.push("Sync Report:".to_string());
    lines.push(String::new());

    if !report.synced.is_empty() {
        lines.push(format!("  Synced: {}", report.synced.join(", ")));
    }
    if !report.skipped.is_empty() {
        lines.push(format!("  Already in sync: {}", report.skipped.join(", ")));
    }
    if !report.errors.is_empty() {
        lines.push(format!("  Errors: {}", report.errors.join(", ")));
    }
    if report.synced.is_empty() && report.skipped.is_empty() && report.errors.is_empty() {
        lines.push("  No targets found.".to_string());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_ops_has_config_false() {
        let ops = ContextOps::new(
            Path::new("/tmp/fake"),
            Path::new("/tmp/nonexistent_contextops"),
        );
        assert!(!ops.has_config());
    }

    // #548 criterion 2: `rules diff` must not require `.lean-ctx/rules.toml`.
    // `detect_drift` is now infallible and compares against the canonical rule
    // source, so a project with no rules.toml produces a (possibly empty) drift
    // report instead of the old "No rules config found" hard error. Serialized
    // against other tests that read/write the global `CLAUDE_CONFIG_DIR`.
    #[test]
    #[serial_test::serial(claude_config_dir)]
    fn detect_drift_without_rules_toml_does_not_require_init() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let claude_dir = home.path().join(".claude").to_string_lossy().into_owned();
        let _g = crate::setup::EnvVarGuard::set("CLAUDE_CONFIG_DIR", &claude_dir);

        let ops = ContextOps::new(home.path(), project.path());
        assert!(
            !ops.has_config(),
            "sandbox project must have no rules.toml for this regression test"
        );

        // The pre-#548 code path would have errored here on the missing config.
        // Now it returns well-formed reports straight from the canonical source.
        let reports = ops.detect_drift();
        for r in &reports {
            assert!(!r.target.is_empty(), "drift report missing a target name");
        }
    }

    #[test]
    fn format_status_output() {
        let statuses = vec![crate::rules_inject::RulesTargetStatus {
            name: "TestAgent".to_string(),
            detected: true,
            path: "/tmp/test".to_string(),
            state: "up_to_date".to_string(),
            note: None,
        }];
        let output = format_status(&statuses);
        assert!(output.contains("✓"));
        assert!(output.contains("TestAgent"));
    }

    #[test]
    fn format_drift_skips_not_detected() {
        let reports = vec![DriftReport {
            target: "Ghost".to_string(),
            path: "/tmp/ghost".to_string(),
            status: DriftStatus::NotDetected,
            diff: None,
        }];
        let output = format_drift(&reports);
        assert!(!output.contains("Ghost"));
    }

    #[test]
    fn format_lint_empty() {
        let output = format_lint(&[]);
        assert_eq!(output, "No lint issues found.");
    }

    #[test]
    fn format_lint_with_warnings() {
        let warnings = vec![LintWarning {
            severity: LintSeverity::Warning,
            code: "TEST".to_string(),
            message: "test warning".to_string(),
            target: Some("cursor".to_string()),
        }];
        let output = format_lint(&warnings);
        assert!(output.contains("[WARNING]"));
        assert!(output.contains("[cursor]"));
    }

    #[test]
    fn format_sync_empty() {
        let report = SyncReport {
            synced: vec![],
            skipped: vec![],
            errors: vec![],
        };
        let output = format_sync(&report);
        assert!(output.contains("No targets found"));
    }

    #[test]
    fn format_sync_with_results() {
        let report = SyncReport {
            synced: vec!["Cursor".to_string()],
            skipped: vec!["Claude Code".to_string()],
            errors: vec![],
        };
        let output = format_sync(&report);
        assert!(output.contains("Cursor"));
        assert!(output.contains("Claude Code"));
    }
}
