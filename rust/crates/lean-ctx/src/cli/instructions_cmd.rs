// SPDX-License-Identifier: Apache-2.0
use lean_ctx::core::instruction_compiler::{self, CompileOptions};
use lean_ctx_server::tools::CrpMode;

pub(crate) fn cmd_instructions(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }
    if args.iter().any(|a| a == "--list-clients") {
        for c in lean_ctx::core::client_constraints::ALL_CLIENTS {
            println!("{}", c.id);
        }
        return;
    }

    let client = value_arg(args, "--client");
    let profile = value_arg(args, "--profile");

    let unified = args.iter().any(|a| a == "--unified");
    let json = args.iter().any(|a| a == "--json");
    let include_rules = args.iter().any(|a| a == "--include-rules");

    let crp_mode_override = value_arg(args, "--crp").and_then(|v| CrpMode::parse(&v));

    let client = client.unwrap_or_default();
    let profile = profile.unwrap_or_else(lean_ctx::core::profiles::active_profile_name);

    if client.trim().is_empty() {
        eprintln!("Missing --client.");
        print_help();
        std::process::exit(2);
    }

    let res = instruction_compiler::compile(
        &client,
        &profile,
        CompileOptions {
            unified,
            include_rules_files: include_rules,
            crp_mode_override,
        },
    );
    let out = match res {
        Ok(v) => v,
        Err(e) => {
            eprintln!("instructions: {e}");
            std::process::exit(2);
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print!("{}", out.mcp_instructions);
        if !out.mcp_instructions.ends_with('\n') {
            println!();
        }
    }
}

fn value_arg(args: &[String], key: &str) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&format!("{key}=")) {
            return Some(v.to_string());
        }
        if a == key {
            return args.get(i + 1).cloned();
        }
    }
    None
}

fn print_help() {
    println!(
        "\
lean-ctx instructions

Usage:
  lean-ctx instructions --client <id> [--profile <name>] [--crp off|compact|tdd] [--unified] [--json] [--include-rules]
  lean-ctx instructions --list-clients

Notes:
  - Output is deterministic for the same inputs (profile + client + flags).
  - Use --json to include metadata (and optionally rules file contents).
"
    );
}
