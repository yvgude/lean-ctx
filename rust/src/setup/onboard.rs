use super::options::SetupOptions;
use super::with_options::run_setup_with_options;

/// Friendly, non-interactive "golden path" onboarding.
///
/// Unlike `run_setup` (the full 12-step interactive wizard), `onboard` makes
/// every decision for the user with sensible defaults — connect detected AI
/// tools, install the shell hook, set the `standard` tool profile — then prints
/// one clear "you're all set" message with a single obvious next step. This is
/// the recommended first-run path: time-to-value in seconds. The one question
/// it asks — which detected tools to connect — only appears on a terminal and
/// when `--agents` did not already answer it (`ask_agents`); unattended runs
/// connect every detected tool.
pub fn run_onboard(ask_agents: bool) {
    use crate::terminal_ui;

    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let green = "\x1b[1;32m";
    let yellow = "\x1b[33m";
    let rst = "\x1b[0m";

    println!();
    println!("  {bold}Connecting lean-ctx to your AI tools…{rst}");
    if ask_agents
        && !crate::shell::is_non_interactive()
        && let Some(home) = crate::core::home::resolve_home_dir()
    {
        let targets = crate::core::editor_registry::build_targets(&home);
        let current = crate::core::config::Config::load().setup.agents;
        println!();
        let chosen = super::agent_select::prompt_agent_selection(&targets, current.as_deref());
        if chosen != current
            && let Err(e) = super::agent_select::persist_agent_selection(chosen)
        {
            terminal_ui::print_status_warn(&e);
        }
    } else {
        let selection = crate::core::config::Config::load().setup.agents;
        println!(
            "  {dim}Tools: {} · choose with `lean-ctx onboard --agents claude,cursor`{rst}",
            super::agent_select::describe_selection(selection.as_deref())
        );
    }
    println!("  {dim}Recommended defaults otherwise. Run `lean-ctx setup` for full control.{rst}");
    println!();

    let opts = SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        ..Default::default()
    };

    let report = match run_setup_with_options(opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  {yellow}Onboarding could not complete: {e}{rst}");
            eprintln!("  {dim}Try the guided setup instead: lean-ctx setup{rst}");
            std::process::exit(1);
        }
    };

    let connected: Vec<String> = report
        .steps
        .iter()
        .find(|s| s.name == "editors")
        .map(|s| {
            s.items
                .iter()
                .filter(|i| matches!(i.status.as_str(), "created" | "updated" | "already"))
                .map(|i| i.name.clone())
                .collect()
        })
        .unwrap_or_default();

    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map_or_else(|_| "~/.lean-ctx".to_string(), |p| p.display().to_string());

    println!();
    if connected.is_empty() {
        println!("  {yellow}No AI tools detected yet.{rst}");
        println!(
            "  {dim}Install Cursor, Claude Code, VS Code, etc., then re-run: lean-ctx onboard{rst}"
        );
    } else {
        println!("  {green}✓ lean-ctx is connected.{rst}");
        println!();
        println!("  {bold}Connected:{rst} {}", connected.join(", "));
    }
    println!("  {dim}Data dir:{rst}  {data_dir}");

    let source_cmd = crate::shell_hook::shell_source_command().unwrap_or("Restart your shell");
    println!();
    println!("  {bold}One last step:{rst}");
    println!("  {cyan}1.{rst} Reload your shell:  {bold}{source_cmd}{rst}");
    if !connected.is_empty() {
        println!(
            "  {cyan}2.{rst} {yellow}Fully restart your AI tool{rst} {dim}(so it reconnects to lean-ctx){rst}"
        );
        println!(
            "  {cyan}3.{rst} Ask your AI to read a file — lean-ctx optimizes it automatically."
        );
    }
    println!();
    println!(
        "  {dim}Check anytime:{rst}  {bold}lean-ctx doctor{rst}  {dim}·{rst}  {bold}lean-ctx gain{rst}"
    );
    println!();
    terminal_ui::print_command_box();

    crate::cli::show_first_run_wow();
}
