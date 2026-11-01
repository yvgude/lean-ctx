//! `lean-ctx learning` — inspect the local learning-efficacy evidence (#550,
//! VIS-4). `export` and `import` carried the retired LITM placement calibration
//! (#539); they stay accepted as no-ops until their announced removal
//! (DEPRECATIONS.toml `cli-learning-export-import`).

use crate::core;

/// What `learning export|import` answers since the calibration was retired.
const RETIRED_NOTE: &str = "is retired and has no effect: the LITM placement calibration it \
carried is gone and session blocks use a static begin/end layout. It will be removed \
(see `lean-ctx doctor`).";

fn retired_notice(action: &str) -> String {
    format!("lean-ctx learning {action} {RETIRED_NOTE}")
}

pub(in crate::cli::dispatch) fn cmd_learning(rest: &[String]) {
    let action = rest.first().map_or("status", String::as_str);
    match action {
        "export" | "import" => eprintln!("{}", retired_notice(action)),
        "status" | "" => {
            let efficacy = core::efficacy::report();
            println!("Adaptive learning state\n{}", "=".repeat(40));
            if efficacy.is_empty() {
                println!("\n(no efficacy evidence recorded yet)");
            } else {
                println!("\nEfficacy:");
                for l in efficacy {
                    println!("  {l}");
                }
            }
        }
        _ => {
            eprintln!("Usage: lean-ctx learning [status]");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_and_import_say_they_are_retired() {
        for action in ["export", "import"] {
            let notice = retired_notice(action);
            assert!(
                notice.starts_with(&format!("lean-ctx learning {action} is retired")),
                "{notice}"
            );
            assert!(notice.contains("no effect"), "{notice}");
        }
    }
}
