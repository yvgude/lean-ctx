//! `lean-ctx introspect` — report which cognition subsystems are wired and
//! actually active at runtime. Reads the shared activity registry persisted by
//! the running server (see [`crate::core::introspect`]).

use crate::core::introspect;

pub(crate) fn cmd_introspect(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let sub = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map_or("cognition", String::as_str);

    match sub {
        "cognition" => {
            if json {
                println!("{}", introspect::snapshot_json());
            } else {
                print!("{}", introspect::format_report());
            }
        }
        other => {
            eprintln!("Unknown introspect target: {other}");
            eprintln!("Usage: lean-ctx introspect [cognition] [--json]");
            std::process::exit(2);
        }
    }
}
