use crate::core::contextops::{self, ContextOps};

pub fn cmd_rules(args: &[String]) {
    let action = args.first().map_or("help", String::as_str);

    let Some(home) = dirs::home_dir() else {
        eprintln!("Error: could not determine home directory");
        std::process::exit(1);
    };

    let project_root = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let ops = ContextOps::new(&home, &project_root);

    match action {
        "sync" => cmd_sync(&ops, args),
        "diff" => cmd_diff(&ops),
        "lint" => cmd_lint(&ops),
        "status" => cmd_status(&ops),
        "init" => cmd_init(&ops),
        "help" | "--help" | "-h" => print_help(),
        _ => {
            eprintln!("Unknown rules action: {action}");
            print_help();
            std::process::exit(1);
        }
    }
}

fn cmd_sync(ops: &ContextOps, args: &[String]) {
    let agent = args.get(1).map(String::as_str);

    let report = if let Some(agent_name) = agent {
        println!("Syncing rules for {agent_name}...");
        ops.sync_agent(agent_name)
    } else {
        println!("Syncing rules to all detected agents...");
        ops.sync_all()
    };

    println!("{}", contextops::format_sync(&report));

    if !report.errors.is_empty() {
        std::process::exit(1);
    }
}

fn cmd_diff(ops: &ContextOps) {
    match ops.detect_drift() {
        Ok(reports) => {
            println!("{}", contextops::format_drift(&reports));

            let drifted = reports
                .iter()
                .filter(|r| r.status == contextops::DriftStatus::Drifted)
                .count();
            if drifted > 0 {
                println!("\n{drifted} target(s) drifted. Run `lean-ctx rules sync` to fix.");
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            println!("\nFalling back to status-based drift check...");
            let statuses = ops.status();
            let outdated: Vec<_> = statuses.iter().filter(|s| s.state == "outdated").collect();
            if outdated.is_empty() {
                println!("All detected targets appear up to date.");
            } else {
                for s in &outdated {
                    println!("  [OUTDATED] {} ({})", s.name, s.path);
                }
            }
        }
    }
}

fn cmd_lint(ops: &ContextOps) {
    match ops.lint() {
        Ok(warnings) => {
            println!("{}", contextops::format_lint(&warnings));
            let errors = warnings
                .iter()
                .filter(|w| w.severity == contextops::LintSeverity::Error)
                .count();
            if errors > 0 {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            eprintln!("Run `lean-ctx rules init` first to create .lean-ctx/rules.toml");
            std::process::exit(1);
        }
    }
}

fn cmd_status(ops: &ContextOps) {
    let statuses = ops.status();
    println!("{}", contextops::format_status(&statuses));

    let has_config = ops.has_config();
    println!();
    if has_config {
        println!("Central config: ✓ (.lean-ctx/rules.toml)");
    } else {
        println!("Central config: ✗ (run `lean-ctx rules init` to create)");
    }
}

fn cmd_init(ops: &ContextOps) {
    if ops.has_config() {
        eprintln!("Config already exists at .lean-ctx/rules.toml");
        eprintln!("Delete it first if you want to reinitialize.");
        std::process::exit(1);
    }

    match ops.init() {
        Ok(_config) => {
            println!("Created .lean-ctx/rules.toml from existing rules.");
            println!();
            println!("Next steps:");
            println!("  1. Review .lean-ctx/rules.toml");
            println!("  2. Run `lean-ctx rules lint` to check consistency");
            println!("  3. Run `lean-ctx rules sync` to distribute");
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn print_help() {
    eprintln!(
        "lean-ctx rules — Cross-agent rules governance (ContextOps)\n\
         \n\
         USAGE:\n    \
             lean-ctx rules <action> [args]\n\
         \n\
         ACTIONS:\n    \
             sync [agent]      Sync central rules to all (or one) agent config(s)\n    \
             diff              Show drift between central and distributed rules\n    \
             lint              Check rules for consistency and completeness\n    \
             status            Show sync status for all targets\n    \
             init              Create .lean-ctx/rules.toml from existing rules\n    \
             help              Show this help"
    );
}
