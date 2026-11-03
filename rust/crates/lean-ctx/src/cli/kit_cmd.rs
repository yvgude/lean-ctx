// SPDX-License-Identifier: Apache-2.0
use lean_ctx::core::config::Config;
use lean_ctx::kits;

/// Manages the available local TOML/`.ctxpkg` kit substrate.
pub(crate) fn cmd_kit(args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("list" | "ls") => list_kits(),
        Some("load") => {
            let Some(name) = args.get(1) else {
                print_help();
                std::process::exit(2);
            };
            load_kit(name);
        }
        Some("show") => {
            let Some(name) = args.get(1) else {
                print_help();
                std::process::exit(2);
            };
            show_kit(name);
        }
        Some("unload" | "clear") => unload_kit(),
        Some("help" | "--help" | "-h") => print_help(),
        Some(_) => {
            print_help();
            std::process::exit(2);
        }
    }
}

fn list_kits() {
    println!("Available local kit substrate:");
    for name in kits::list() {
        println!("  {name}");
    }
    println!("\nLoad one locally with: lean-ctx kit load <name>");
}

fn load_kit(name: &str) {
    let loaded = kits::load(name).unwrap_or_else(|error| {
        eprintln!("kit: {error}");
        std::process::exit(1);
    });
    let canonical_name = loaded.kit.kit.name.clone();
    Config::update_global(|config| config.active_kit = Some(canonical_name.clone()))
        .unwrap_or_else(|error| {
            eprintln!("kit: could not persist active kit: {error}");
            std::process::exit(1);
        });

    println!("Loaded local kit substrate: {canonical_name}");
    println!("  Source: {}", loaded.source);
    println!("  Priority: {}", loaded.kit.priority.description);
    println!(
        "  Rust: {}",
        loaded.kit.modes_for_path("src/review.rs").join(" -> ")
    );
    println!(
        "  TOML: {}",
        loaded.kit.modes_for_path("Cargo.toml").join(" -> ")
    );
    println!(
        "  Tests: {}",
        loaded.kit.modes_for_path("tests/review.rs").join(" -> ")
    );
}

fn show_kit(name: &str) {
    let loaded = kits::load(name).unwrap_or_else(|error| {
        eprintln!("kit: {error}");
        std::process::exit(1);
    });
    let kit = loaded.kit;
    println!(
        "Local kit substrate: {} ({})",
        kit.kit.name, kit.kit.version
    );
    println!("Source: {}", loaded.source);
    println!("{}", kit.kit.description);
    println!("\nRead plan:");
    for rule in &kit.read.rules {
        println!("  {}: {}", rule.glob, rule.modes.join(" -> "));
    }
    println!("\nQuality criteria:");
    for criterion in &kit.quality.criteria {
        println!("  - {criterion}");
    }
    println!("\nSystem prompt:\n{}", kit.system_prompt.template.trim());
}

fn unload_kit() {
    Config::update_global(|config| config.active_kit = None).unwrap_or_else(|error| {
        eprintln!("kit: could not clear active kit: {error}");
        std::process::exit(1);
    });
    println!("Unloaded active local kit substrate.");
}

fn print_help() {
    println!(
        "Usage: lean-ctx kit <list|load|show|unload> [name]\n\nAvailable local TOML/.ctxpkg substrate; first-class Context Kit semantics are Research.\n  lean-ctx kit load code-review"
    );
}
