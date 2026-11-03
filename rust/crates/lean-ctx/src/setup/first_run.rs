// SPDX-License-Identifier: Apache-2.0
/// Determine the setup level from a first-run interactive menu.
/// Returns (inject_rules, inject_skills).
pub(crate) fn first_run_setup_level() -> (bool, bool) {
    use std::io::Write;

    let cfg = lean_ctx::core::config::Config::load();
    if cfg.setup.auto_inject_rules.is_some() {
        return (
            cfg.setup.should_inject_rules(),
            cfg.setup.should_inject_skills(),
        );
    }

    println!();
    println!("  \x1b[1mWelcome to lean-ctx!\x1b[0m");
    println!();
    println!("  lean-ctx compresses AI context by 60-99%, saving tokens and money.");
    println!();
    println!("  Choose your setup level:");
    println!(
        "    \x1b[36m[1]\x1b[0m Minimal  \x1b[2m— Just MCP tools, no config file changes (recommended)\x1b[0m"
    );
    println!(
        "    \x1b[36m[2]\x1b[0m Standard \x1b[2m— MCP tools + agent instructions for optimal mode selection\x1b[0m"
    );
    println!(
        "    \x1b[36m[3]\x1b[0m Full     \x1b[2m— Everything (tools + rules + skills + shell hooks)\x1b[0m"
    );
    println!();
    print!("  Your choice \x1b[1m[1]\x1b[0m: ");
    std::io::stdout().flush().ok();

    let mut input = String::new();
    let choice = if std::io::stdin().read_line(&mut input).is_ok() {
        input.trim().parse::<u8>().unwrap_or(1)
    } else {
        1
    };

    match choice {
        3 => (true, true),
        2 => (true, false),
        _ => (false, false),
    }
}

/// Persist the user's setup level choice to config.toml.
pub(crate) fn persist_setup_choice(inject_rules: bool, inject_skills: bool) {
    if let Err(e) = lean_ctx::core::config::Config::update_global(|cfg| {
        cfg.setup.auto_inject_rules = Some(inject_rules);
        cfg.setup.auto_inject_skills = Some(inject_skills);
    }) {
        tracing::warn!("could not persist setup choice: {e}");
    }
}
