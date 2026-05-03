use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub version: String,
    pub setup_report: Option<crate::core::setup_report::SetupReport>,
    pub doctor_compact_passed: u32,
    pub doctor_compact_total: u32,
    pub mcp_targets: Vec<McpTargetStatus>,
    pub rules_targets: Vec<crate::rules_inject::RulesTargetStatus>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTargetStatus {
    pub name: String,
    pub detected: bool,
    pub config_path: String,
    pub state: String,
    pub note: Option<String>,
}

pub fn run_cli(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let help = args.iter().any(|a| a == "--help" || a == "-h");
    if help {
        println!("Usage:");
        println!("  lean-ctx status [--json]");
        return 0;
    }

    match build_status_report() {
        Ok((report, path)) => {
            let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
            let _ = crate::config_io::write_atomic_with_backup(&path, &text);

            if json {
                println!("{text}");
            } else {
                print_human(&report, &path);
            }

            i32::from(!report.errors.is_empty())
        }
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

fn build_status_report() -> Result<(StatusReport, std::path::PathBuf), String> {
    let generated_at = Utc::now();
    let version = env!("CARGO_PKG_VERSION").to_string();
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;

    let mut warnings: Vec<String> = Vec::new();
    let errors: Vec<String> = Vec::new();

    let setup_report = {
        let path = crate::core::setup_report::SetupReport::default_path()?;
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(s) => match serde_json::from_str::<crate::core::setup_report::SetupReport>(&s) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        warnings.push(format!("setup report parse error: {e}"));
                        None
                    }
                },
                Err(e) => {
                    warnings.push(format!("setup report read error: {e}"));
                    None
                }
            }
        } else {
            None
        }
    };

    let (doctor_compact_passed, doctor_compact_total) = crate::doctor::compact_score();

    // MCP targets (registry based)
    let targets = crate::core::editor_registry::build_targets(&home);
    let mut mcp_targets: Vec<McpTargetStatus> = Vec::new();
    for t in &targets {
        let detected = t.detect_path.exists();
        let config_path = t.config_path.to_string_lossy().to_string();

        let state = if !detected {
            "not_detected".to_string()
        } else if !t.config_path.exists() {
            "missing_file".to_string()
        } else {
            match std::fs::read_to_string(&t.config_path) {
                Ok(s) => {
                    if s.contains("lean-ctx") {
                        "configured".to_string()
                    } else {
                        "missing_entry".to_string()
                    }
                }
                Err(e) => {
                    warnings.push(format!("mcp config read error for {}: {e}", t.name));
                    "read_error".to_string()
                }
            }
        };

        if detected {
            mcp_targets.push(McpTargetStatus {
                name: t.name.to_string(),
                detected,
                config_path,
                state,
                note: None,
            });
        }
    }

    if mcp_targets.is_empty() {
        warnings.push("no supported AI tools detected".to_string());
    }

    let rules_targets = crate::rules_inject::collect_rules_status(&home);

    let path = crate::core::setup_report::status_report_path()?;

    let report = StatusReport {
        schema_version: 1,
        generated_at,
        version,
        setup_report,
        doctor_compact_passed,
        doctor_compact_total,
        mcp_targets,
        rules_targets,
        warnings,
        errors,
    };

    Ok((report, path))
}

fn print_human(report: &StatusReport, path: &std::path::Path) {
    println!("lean-ctx status  v{}", report.version);
    println!(
        "  doctor: {}/{}",
        report.doctor_compact_passed, report.doctor_compact_total
    );

    if let Some(setup) = &report.setup_report {
        println!(
            "  last setup: {}  success={}",
            setup.finished_at.to_rfc3339(),
            setup.success
        );
    } else if report.doctor_compact_passed == report.doctor_compact_total {
        println!("  last setup: (manual install — all checks pass)");
    } else {
        println!("  last setup: (none) — run \x1b[1mlean-ctx setup\x1b[0m to configure");
    }

    let detected = report.mcp_targets.len();
    let configured = report
        .mcp_targets
        .iter()
        .filter(|t| t.state == "configured")
        .count();
    println!("  mcp: {configured}/{detected} configured (detected tools)");

    let rules_detected = report.rules_targets.iter().filter(|t| t.detected).count();
    let rules_up_to_date = report
        .rules_targets
        .iter()
        .filter(|t| t.detected && t.state == "up_to_date")
        .count();
    println!("  rules: {rules_up_to_date}/{rules_detected} up-to-date (detected tools)");

    if !report.warnings.is_empty() {
        println!("  warnings: {}", report.warnings.len());
    }
    if !report.errors.is_empty() {
        println!("  errors: {}", report.errors.len());
    }
    println!("  report saved: {}", path.display());
}
