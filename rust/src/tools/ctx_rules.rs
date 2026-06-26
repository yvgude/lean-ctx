use crate::core::contextops::{self, ContextOps};

pub fn handle(action: &str, agent: Option<&str>) -> String {
    let Some(home) = dirs::home_dir() else {
        return "Error: could not determine home directory".to_string();
    };

    let project_root = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let ops = ContextOps::new(&home, &project_root);

    match action {
        "sync" => {
            let report = if let Some(agent_name) = agent {
                ops.sync_agent(agent_name)
            } else {
                ops.sync_all()
            };
            contextops::format_sync(&report)
        }
        "diff" => {
            // Drift is measured against the canonical rule source, so this needs
            // no `.lean-ctx/rules.toml` and cannot error on a missing config (#548).
            let reports = ops.detect_drift();
            let mut output = contextops::format_drift(&reports);
            let drifted = reports
                .iter()
                .filter(|r| r.status == contextops::DriftStatus::Drifted)
                .count();
            if drifted > 0 {
                output.push_str(&format!(
                    "\n\n{drifted} target(s) drifted. Run ctx_rules(action=\"sync\") to fix."
                ));
            }
            output
        }
        "lint" => match ops.lint() {
            Ok(warnings) => contextops::format_lint(&warnings),
            Err(e) => {
                format!("Error: {e}\nRun ctx_rules(action=\"init\") to create .lean-ctx/rules.toml")
            }
        },
        "status" => {
            let statuses = ops.status();
            let mut output = contextops::format_status(&statuses);
            output.push('\n');
            if ops.has_config() {
                output.push_str("\nCentral config: present (.lean-ctx/rules.toml)");
            } else {
                output.push_str(
                    "\nCentral config: missing (run ctx_rules(action=\"init\") to create)",
                );
            }
            output
        }
        "init" => {
            if ops.has_config() {
                return "Config already exists at .lean-ctx/rules.toml. Delete it first to reinitialize.".to_string();
            }
            match ops.init() {
                Ok(_) => "Created .lean-ctx/rules.toml from existing rules. It feeds ctx_rules(action=\"lint\") for cross-agent consistency and is a user-editable inventory; it is NOT the source for sync/diff, which (re)generate from lean-ctx's built-in canonical rules. Next: ctx_rules(action=\"lint\") to check, then ctx_rules(action=\"sync\") to (re)write the canonical block.".to_string(),
                Err(e) => format!("Error: {e}"),
            }
        }
        _ => "Unknown action. Use: sync, diff, lint, status, init".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_status() {
        let result = handle("status", None);
        assert!(result.contains("Agent Rules Status"));
    }

    #[test]
    fn handle_unknown_action() {
        let result = handle("unknown_xyz", None);
        assert!(result.contains("Unknown action"));
    }

    #[test]
    fn handle_lint_without_config() {
        let result = handle("lint", None);
        assert!(
            result.contains("Error") || result.contains("Lint"),
            "Should show error or lint results: {result}"
        );
    }

    #[test]
    fn handle_diff_without_config() {
        let result = handle("diff", None);
        assert!(!result.is_empty());
    }
}
